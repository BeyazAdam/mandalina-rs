//! PDF optimizasyonu: gömülü görselleri yeniden örnekler ve JPEG olarak
//! yeniden kodlar, kullanılmayan nesneleri temizler, akışları sıkıştırır.
//!
//! Desteklenen görsel akışları:
//! - `DCTDecode` (JPEG): DeviceRGB / DeviceGray / ICCBased renk uzayları
//! - `FlateDecode`: 8 bit DeviceRGB / DeviceGray / ICCBased ham pikseller
//!
//! CMYK, Indexed, Separation/DeviceN, maskeler (`ImageMask`), `Decode`
//! dizisi olan ve JBIG2/JPX gibi diğer filtrelerdeki görseller olduğu gibi
//! bırakılır; böylece renk bozulması riski yoktur.

use std::fs;
use std::path::{Path, PathBuf};

use image::{DynamicImage, GrayImage, ImageFormat, RgbImage};
use lopdf::{dictionary, Document, Object, Stream};
use rayon::prelude::*;

use crate::error::Result;
use crate::image_compressor::{self, CompressOutcome, ImageFormatKind, ImageOptions};

#[derive(Debug, Clone, Copy)]
pub struct PdfOptions {
    pub image: ImageOptions,
    /// Bundan küçük görsel akışlarına dokunma (bayt). Küçük ikonlar için
    /// yeniden kodlama kazanç sağlamaz, yalnızca kaliteyi düşürür.
    pub min_image_bytes: usize,
}

impl Default for PdfOptions {
    fn default() -> Self {
        Self {
            image: ImageOptions {
                max_width: Some(2000),
                max_height: Some(2000),
                ..ImageOptions::default()
            },
            min_image_bytes: 16 * 1024,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PdfOutcome {
    pub file: CompressOutcome,
    pub images_found: usize,
    pub images_recompressed: usize,
}

pub fn compress_pdf(input: &Path, output: &Path, opts: &PdfOptions) -> Result<PdfOutcome> {
    let original_bytes = fs::metadata(input)?.len();
    let mut doc = Document::load(input)?;

    let (images_found, images_recompressed) = optimize_images(&mut doc, opts);

    doc.prune_objects();
    doc.delete_zero_length_streams();
    doc.compress();

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    doc.save(output)?;

    let mut compressed_bytes = fs::metadata(output)?.len();
    let kept_original = compressed_bytes >= original_bytes;
    if kept_original {
        compressed_bytes = fs::copy(input, output)?;
    }

    Ok(PdfOutcome {
        file: CompressOutcome {
            original_bytes,
            compressed_bytes,
            output: output.to_path_buf(),
            kept_original,
        },
        images_found,
        images_recompressed,
    })
}

/// Görselleri sırasıyla birer sayfa olarak tek bir PDF'e yazar. Görseller
/// JPEG (DCTDecode) olarak gömülür; sayfa uzun kenarı A4 uzun kenarına (842 pt) eşitlenir.
pub fn images_to_pdf(inputs: &[PathBuf], output: &Path, opts: &ImageOptions) -> Result<CompressOutcome> {
    const PAGE_LONG_SIDE_PT: f32 = 842.0;

    let mut original_bytes = 0;
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let mut kids = Vec::with_capacity(inputs.len());

    for input in inputs {
        original_bytes += fs::metadata(input)?.len();
        let img = image_compressor::load_oriented(input)?;
        let img = image_compressor::downscale(img, opts.max_width, opts.max_height);
        let (w, h) = (img.width(), img.height());
        let jpeg = image_compressor::encode(&img, ImageFormatKind::Jpeg, opts.quality)?;
        drop(img);

        let color_space = if jpeg_component_count(&jpeg) == Some(1) {
            "DeviceGray"
        } else {
            "DeviceRGB"
        };
        let image_id = doc.add_object(
            Stream::new(
                dictionary! {
                    "Type" => "XObject",
                    "Subtype" => "Image",
                    "Width" => i64::from(w),
                    "Height" => i64::from(h),
                    "ColorSpace" => color_space,
                    "BitsPerComponent" => 8,
                    "Filter" => "DCTDecode",
                },
                jpeg,
            )
            .with_compression(false),
        );

        let scale = PAGE_LONG_SIDE_PT / w.max(h) as f32;
        let (pw, ph) = (w as f32 * scale, h as f32 * scale);
        let content = format!("q {pw:.2} 0 0 {ph:.2} 0 0 cm /Im0 Do Q");
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.into_bytes()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), Object::Real(pw), Object::Real(ph)],
            "Contents" => content_id,
            "Resources" => dictionary! { "XObject" => dictionary! { "Im0" => image_id } },
        });
        kids.push(Object::Reference(page_id));
    }

    let count = kids.len() as i64;
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => count }),
    );
    let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog_id);
    doc.compress();

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    doc.save(output)?;

    Ok(CompressOutcome {
        original_bytes,
        compressed_bytes: fs::metadata(output)?.len(),
        output: output.to_path_buf(),
        kept_original: false,
    })
}

/// Tüm görsel akışlarını `rayon` ile paralel işler.
/// Dönüş: (bulunan görsel sayısı, yeniden kodlanan görsel sayısı)
fn optimize_images(doc: &mut Document, opts: &PdfOptions) -> (usize, usize) {
    let results: Vec<bool> = doc
        .objects
        .par_iter_mut()
        .filter_map(|(id, object)| {
            let Object::Stream(stream) = object else {
                return None;
            };
            if !is_image(stream) {
                return None;
            }
            Some(recompress_image(stream, opts).unwrap_or_else(|e| {
                log::debug!("PDF görseli {id:?} atlandı: {e}");
                false
            }))
        })
        .collect();

    let recompressed = results.iter().filter(|&&changed| changed).count();
    (results.len(), recompressed)
}

fn is_image(stream: &Stream) -> bool {
    stream
        .dict
        .get(b"Subtype")
        .and_then(Object::as_name)
        .is_ok_and(|name| name == b"Image")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Channels {
    Gray,
    Rgb,
}

impl Channels {
    fn count(self) -> usize {
        match self {
            Self::Gray => 1,
            Self::Rgb => 3,
        }
    }
}

/// Renk uzayını güvenle yeniden kodlayabileceğimiz kanal sayısına çevirir.
fn supported_channels(stream: &Stream) -> Option<Channels> {
    match stream.dict.get(b"ColorSpace").ok()? {
        Object::Name(name) => match name.as_slice() {
            b"DeviceRGB" => Some(Channels::Rgb),
            b"DeviceGray" => Some(Channels::Gray),
            _ => None,
        },
        // [/ICCBased <akış referansı>]: kanal sayısı ICC akışının /N değerindedir.
        // Kanal sayısını değiştirmediğimiz sürece renk uzayına dokunmamız gerekmez;
        // gerçek sayıyı kodlanmış veriden (JPEG) ya da veri uzunluğundan (Flate) doğrularız.
        Object::Array(items) => match items.first()?.as_name().ok()? {
            b"ICCBased" => Some(Channels::Rgb),
            _ => None,
        },
        // Dolaylı referanslar paralel döngüde çözülemez (belgeye erişim yok).
        _ => None,
    }
}

fn recompress_image(stream: &mut Stream, opts: &PdfOptions) -> anyhow::Result<bool> {
    if stream.content.len() < opts.min_image_bytes
        || stream.dict.has(b"Decode")
        || stream.dict.get(b"ImageMask").and_then(Object::as_bool).unwrap_or(false)
    {
        return Ok(false);
    }
    let Some(declared) = supported_channels(stream) else {
        return Ok(false);
    };

    let filters: Vec<Vec<u8>> = stream
        .filters()
        .unwrap_or_default()
        .into_iter()
        .map(<[u8]>::to_vec)
        .collect();

    let img = match filters.as_slice() {
        [f] if f == b"DCTDecode" => decode_jpeg(stream, declared)?,
        [f] if f == b"FlateDecode" => decode_flate(stream, declared)?,
        _ => return Ok(false),
    };
    let Some(img) = img else {
        return Ok(false);
    };

    let (orig_w, orig_h) = (img.width(), img.height());
    let img = image_compressor::downscale(img, opts.image.max_width, opts.image.max_height);
    let resized = (img.width(), img.height()) != (orig_w, orig_h);
    let encoded = image_compressor::encode(&img, ImageFormatKind::Jpeg, opts.image.quality)?;

    // Kazanç yoksa (ve boyut değişmediyse) orijinal akışı koru.
    if encoded.len() >= stream.content.len() && !resized {
        return Ok(false);
    }

    stream.dict.set("Filter", Object::Name(b"DCTDecode".to_vec()));
    stream.dict.remove(b"DecodeParms");
    stream.dict.set("Width", Object::Integer(i64::from(img.width())));
    stream.dict.set("Height", Object::Integer(i64::from(img.height())));
    stream.dict.set("BitsPerComponent", Object::Integer(8));
    stream.set_content(encoded);
    // JPEG verisini `Document::compress` tekrar Flate ile sarmasın.
    stream.allows_compression = false;
    Ok(true)
}

fn decode_jpeg(stream: &Stream, declared: Channels) -> anyhow::Result<Option<DynamicImage>> {
    // Kod çözücü CMYK'yı sessizce RGB'ye çevirebilir; bu yüzden gerçek bileşen
    // sayısını çözmeden önce JPEG başlığından okuyoruz (ICCBased N=4 durumu).
    let ok = match jpeg_component_count(&stream.content) {
        Some(1) => true,
        Some(3) => declared == Channels::Rgb,
        _ => false,
    };
    if !ok {
        return Ok(None);
    }
    let img = image::load_from_memory_with_format(&stream.content, ImageFormat::Jpeg)?;
    Ok(Some(img))
}

/// JPEG SOF segmentinden bileşen (kanal) sayısını okur.
fn jpeg_component_count(data: &[u8]) -> Option<u8> {
    if data.get(..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut i = 2;
    loop {
        // Marker'dan önceki 0xFF dolgu baytlarını atla.
        while *data.get(i)? == 0xFF && *data.get(i + 1)? == 0xFF {
            i += 1;
        }
        if *data.get(i)? != 0xFF {
            return None;
        }
        let marker = *data.get(i + 1)?;
        let len = usize::from(u16::from_be_bytes([*data.get(i + 2)?, *data.get(i + 3)?]));
        match marker {
            // SOF0..SOF15; C4 (DHT), C8 (JPG), CC (DAC) SOF değildir.
            0xC0..=0xCF if !matches!(marker, 0xC4 | 0xC8 | 0xCC) => return data.get(i + 9).copied(),
            // SOS: SOF görmeden tarama verisine geldik.
            0xDA => return None,
            _ => i += 2 + len,
        }
    }
}

fn decode_flate(stream: &Stream, declared: Channels) -> anyhow::Result<Option<DynamicImage>> {
    let bpc = stream.dict.get(b"BitsPerComponent").and_then(Object::as_i64)?;
    if bpc != 8 {
        return Ok(None);
    }
    let width = u32::try_from(stream.dict.get(b"Width").and_then(Object::as_i64)?)?;
    let height = u32::try_from(stream.dict.get(b"Height").and_then(Object::as_i64)?)?;

    let pixels = width as usize * height as usize;
    // Sıkıştırma bombalarına karşı: beklenen ham boyutun biraz fazlasıyla sınırla.
    let limit = pixels * Channels::Rgb.count() + 1024;
    let raw = stream.decompressed_content_with_limit(limit)?;

    // ICCBased için kanal sayısını veri uzunluğundan çıkar.
    let channels = if raw.len() == pixels * Channels::Rgb.count() {
        Channels::Rgb
    } else if raw.len() == pixels * Channels::Gray.count() {
        Channels::Gray
    } else {
        return Ok(None);
    };
    if declared == Channels::Gray && channels != Channels::Gray {
        return Ok(None);
    }

    Ok(match channels {
        Channels::Rgb => RgbImage::from_raw(width, height, raw).map(DynamicImage::ImageRgb8),
        Channels::Gray => GrayImage::from_raw(width, height, raw).map(DynamicImage::ImageLuma8),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_jpeg_component_count() {
        let rgb = DynamicImage::ImageRgb8(RgbImage::new(16, 16));
        let gray = DynamicImage::ImageLuma8(GrayImage::new(16, 16));
        let rgb_jpeg = image_compressor::encode(&rgb, ImageFormatKind::Jpeg, 80).unwrap();
        let gray_jpeg = image_compressor::encode(&gray, ImageFormatKind::Jpeg, 80).unwrap();
        assert_eq!(jpeg_component_count(&rgb_jpeg), Some(3));
        assert_eq!(jpeg_component_count(&gray_jpeg), Some(1));
        assert_eq!(jpeg_component_count(b"not a jpeg"), None);
    }

    /// Tek sayfalı, içinde Flate sıkıştırılmış 1200x900 RGB görsel olan bir PDF üretir.
    fn sample_pdf(path: &Path) {
        let (w, h) = (1200_u32, 900_u32);
        let mut seed = 0x1234_5678_u32;
        let raw: Vec<u8> = (0..w * h)
            .flat_map(|i| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = (seed >> 28) as u8;
                let (x, y) = ((i % w) as u8, (i / w) as u8);
                [x.wrapping_add(noise), y.wrapping_add(noise), x ^ y]
            })
            .collect();

        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let mut image = Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => i64::from(w),
                "Height" => i64::from(h),
                "ColorSpace" => "DeviceRGB",
                "BitsPerComponent" => 8,
            },
            raw,
        );
        image.compress().unwrap();
        let image_id = doc.add_object(image);
        let content_id = doc.add_object(Stream::new(dictionary! {}, b"q 600 0 0 450 0 0 cm /Im1 Do Q".to_vec()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => content_id,
            "MediaBox" => vec![0.into(), 0.into(), 600.into(), 450.into()],
            "Resources" => dictionary! { "XObject" => dictionary! { "Im1" => image_id } },
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page_id.into()],
                "Count" => 1,
            }),
        );
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);
        doc.save(path).unwrap();
    }

    #[test]
    fn compresses_flate_image_inside_pdf() {
        let dir = std::env::temp_dir().join(format!("mandalina-pdf-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let input = dir.join("girdi.pdf");
        let output = dir.join("Sıkıştırılmış").join("girdi.pdf");
        sample_pdf(&input);

        let opts = PdfOptions {
            image: ImageOptions {
                max_width: Some(600),
                max_height: Some(600),
                ..ImageOptions::default()
            },
            ..PdfOptions::default()
        };
        let outcome = compress_pdf(&input, &output, &opts).unwrap();

        assert_eq!(outcome.images_found, 1);
        assert_eq!(outcome.images_recompressed, 1);
        assert!(!outcome.file.kept_original);
        assert!(outcome.file.compressed_bytes < outcome.file.original_bytes);

        // Çıktı geçerli bir PDF olmalı ve görsel JPEG'e dönüşmüş, küçülmüş olmalı.
        let doc = Document::load(&output).unwrap();
        let image = doc
            .objects
            .values()
            .filter_map(|o| o.as_stream().ok())
            .find(|s| is_image(s))
            .expect("çıktıda görsel bulunamadı");
        assert_eq!(image.filters().unwrap(), vec![b"DCTDecode".as_slice()]);
        assert_eq!(image.dict.get(b"Width").and_then(Object::as_i64).unwrap(), 600);
        assert_eq!(image.dict.get(b"Height").and_then(Object::as_i64).unwrap(), 450);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn merges_images_into_pdf_pages() {
        let dir = std::env::temp_dir().join(format!("mandalina-merge-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let wide = dir.join("genis.png");
        let gray = dir.join("gri.png");
        // Not: `image` 8 bit gri BMP'yi paletli okuyup RGB'ye çevirir; gri yolu PNG ile sınanır.
        let bmp = dir.join("uzun.bmp");
        RgbImage::from_pixel(400, 200, image::Rgb([200, 120, 40]))
            .save(&wide)
            .unwrap();
        GrayImage::from_pixel(100, 300, image::Luma([90])).save(&gray).unwrap();
        RgbImage::from_pixel(120, 360, image::Rgb([10, 80, 160]))
            .save(&bmp)
            .unwrap();

        let output = dir.join("birlesik.pdf");
        let outcome = images_to_pdf(&[wide, gray, bmp], &output, &ImageOptions::default()).unwrap();
        assert!(outcome.compressed_bytes > 0);

        let doc = Document::load(&output).unwrap();
        let pages = doc.get_pages();
        assert_eq!(pages.len(), 3);
        let color_spaces: Vec<Vec<u8>> = doc
            .objects
            .values()
            .filter_map(|o| o.as_stream().ok())
            .filter(|s| is_image(s))
            .filter_map(|s| {
                s.dict
                    .get(b"ColorSpace")
                    .and_then(Object::as_name)
                    .ok()
                    .map(<[u8]>::to_vec)
            })
            .collect();
        assert!(color_spaces.contains(&b"DeviceRGB".to_vec()));
        assert!(color_spaces.contains(&b"DeviceGray".to_vec()));
        fs::remove_dir_all(&dir).ok();
    }
}
