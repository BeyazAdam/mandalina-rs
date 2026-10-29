//! PNG / JPEG / WebP sıkıştırma, yeniden boyutlandırma ve format dönüştürme.
//!
//! EXIF ve diğer meta veriler: görsel çözülüp yeniden kodlandığında `image`
//! crate'i hiçbir meta veriyi yazmaz, yani EXIF/XMP/ICC blokları kendiliğinden
//! atılır. Fotoğrafın yan dönmemesi için EXIF yönlendirmesi atılmadan önce
//! piksellere uygulanır.

use std::borrow::Cow;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::codecs::avif::AvifEncoder;
use image::codecs::bmp::BmpEncoder;
use image::codecs::ico::IcoEncoder;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
use image::codecs::tiff::TiffEncoder;
use image::codecs::webp::WebPEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ImageDecoder, ImageReader, Rgb, RgbImage};

use crate::error::{CompressError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormatKind {
    Png,
    Jpeg,
    WebP,
    Avif,
    Tiff,
    Bmp,
    Ico,
}

impl ImageFormatKind {
    /// Uzantıdan formatı bulur. Bu, dosyanın *okunabildiği* anlamına gelmez;
    /// bkz. [`Self::is_decodable`].
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "png" => Some(Self::Png),
            "jpg" | "jpeg" | "jfif" => Some(Self::Jpeg),
            "webp" => Some(Self::WebP),
            "avif" => Some(Self::Avif),
            "tif" | "tiff" => Some(Self::Tiff),
            "bmp" => Some(Self::Bmp),
            "ico" => Some(Self::Ico),
            _ => None,
        }
    }

    /// Girdi olarak okunabilen dosya mı? AVIF yalnızca yazılabilir: çözücüsü
    /// (dav1d) bir C kütüphanesi gerektirir.
    pub fn decodable_from_path(path: &Path) -> Option<Self> {
        Self::from_path(path).filter(|f| f.is_decodable())
    }

    pub fn is_decodable(self) -> bool {
        self != Self::Avif
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::WebP => "webp",
            Self::Avif => "avif",
            Self::Tiff => "tiff",
            Self::Bmp => "bmp",
            Self::Ico => "ico",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Jpeg => "JPG",
            Self::WebP => "WEBP",
            Self::Avif => "AVIF",
            Self::Tiff => "TIFF",
            Self::Bmp => "BMP",
            Self::Ico => "ICO",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ImageOptions {
    /// JPEG kalitesi (1-100). PNG'de 100'ün altı 16 bit -> 8 bit indirgemeyi açar.
    pub quality: u8,
    pub max_width: Option<u32>,
    pub max_height: Option<u32>,
    pub convert_to: Option<ImageFormatKind>,
}

impl Default for ImageOptions {
    fn default() -> Self {
        Self {
            quality: 80,
            max_width: None,
            max_height: None,
            convert_to: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CompressOutcome {
    pub original_bytes: u64,
    pub compressed_bytes: u64,
    pub output: PathBuf,
    /// Yeniden kodlama dosyayı büyüttüyse orijinal kopyalanır.
    pub kept_original: bool,
}

impl CompressOutcome {
    pub fn saved_bytes(&self) -> u64 {
        self.original_bytes.saturating_sub(self.compressed_bytes)
    }
}

/// `input` dosyasını sıkıştırıp `output` yoluna yazar (ara klasörler oluşturulur).
pub fn compress_file(input: &Path, output: &Path, opts: &ImageOptions) -> Result<CompressOutcome> {
    let source_format =
        ImageFormatKind::decodable_from_path(input).ok_or_else(|| CompressError::Unsupported(input.to_path_buf()))?;
    let target_format = opts.convert_to.unwrap_or(source_format);
    let original_bytes = fs::metadata(input)?.len();

    let img = load_oriented(input)?;
    let img = downscale(img, opts.max_width, opts.max_height);
    let encoded = encode(&img, target_format, opts.quality)?;
    drop(img); // Büyük piksel tamponunu dosyaya yazmadan önce serbest bırak.

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }

    // Aynı formatta kalıp büyüdüysek orijinali koru. Format dönüşümünde ise
    // kullanıcı yeni formatı açıkça istediği için sonucu her durumda yazarız.
    let kept_original = target_format == source_format && encoded.len() as u64 >= original_bytes;
    let compressed_bytes = if kept_original {
        fs::copy(input, output)?
    } else {
        fs::write(output, &encoded)?;
        encoded.len() as u64
    };

    Ok(CompressOutcome {
        original_bytes,
        compressed_bytes,
        output: output.to_path_buf(),
        kept_original,
    })
}

/// Dosyayı çözer ve EXIF yönlendirmesini piksellere uygular.
pub fn load_oriented(path: &Path) -> Result<DynamicImage> {
    let mut decoder = ImageReader::open(path)?.with_guessed_format()?.into_decoder()?;
    let orientation = decoder.orientation()?;
    let mut img = DynamicImage::from_decoder(decoder)?;
    img.apply_orientation(orientation);
    Ok(img)
}

/// Görsel sınırları aşıyorsa en-boy oranını koruyarak küçültür; asla büyütmez.
pub fn downscale(img: DynamicImage, max_width: Option<u32>, max_height: Option<u32>) -> DynamicImage {
    let (w, h) = (img.width(), img.height());
    let max_w = max_width.unwrap_or(u32::MAX).min(w);
    let max_h = max_height.unwrap_or(u32::MAX).min(h);
    if w <= max_w && h <= max_h {
        return img;
    }
    img.resize(max_w, max_h, FilterType::Lanczos3)
}

/// Görseli bellekte istenen formata kodlar. Hiçbir meta veri yazılmaz.
pub fn encode(img: &DynamicImage, format: ImageFormatKind, quality: u8) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    match format {
        ImageFormatKind::Jpeg => {
            let flat = flatten_for_jpeg(img);
            flat.write_with_encoder(JpegEncoder::new_with_quality(&mut buf, quality.clamp(1, 100)))?;
        }
        ImageFormatKind::Png => {
            let reduced = reduce_for_png(img, quality);
            reduced.write_with_encoder(PngEncoder::new_with_quality(
                &mut buf,
                CompressionType::Best,
                PngFilter::Adaptive,
            ))?;
        }
        ImageFormatKind::WebP => {
            // `image` crate'i yalnızca kayıpsız WebP kodlar. Kayıplı WebP için
            // `webp` (libwebp) crate'i eklenebilir; C derleyicisi gerektirir.
            let rgb_or_rgba = if is_opaque(img) {
                DynamicImage::ImageRgb8(img.to_rgb8())
            } else {
                DynamicImage::ImageRgba8(img.to_rgba8())
            };
            rgb_or_rgba.write_with_encoder(WebPEncoder::new_lossless(&mut buf))?;
        }
        ImageFormatKind::Avif => {
            // Hız 7: rav1e için iyi bir boyut/süre dengesi (1 = en yavaş, 10 = en hızlı).
            let encoder = AvifEncoder::new_with_speed_quality(&mut buf, 7, quality.clamp(1, 100));
            to_rgb_or_rgba8(img).write_with_encoder(encoder)?;
        }
        ImageFormatKind::Tiff => {
            // TIFF kodlayıcısı `Seek` ister.
            let mut cursor = Cursor::new(&mut buf);
            to_rgb_or_rgba8(img).write_with_encoder(TiffEncoder::new(&mut cursor))?;
        }
        ImageFormatKind::Bmp => {
            to_rgb_or_rgba8(img).write_with_encoder(BmpEncoder::new(&mut buf))?;
        }
        ImageFormatKind::Ico => {
            // ICO tek görüntüde en fazla 256x256 destekler.
            let fitted = if img.width() > ICO_MAX || img.height() > ICO_MAX {
                Cow::Owned(img.resize(ICO_MAX, ICO_MAX, FilterType::Lanczos3))
            } else {
                Cow::Borrowed(img)
            };
            DynamicImage::ImageRgba8(fitted.to_rgba8()).write_with_encoder(IcoEncoder::new(&mut buf))?;
        }
    }
    Ok(buf)
}

const ICO_MAX: u32 = 256;

/// Çoğu kodlayıcının kabul ettiği 8 bit RGB/RGBA biçimine indirger.
fn to_rgb_or_rgba8(img: &DynamicImage) -> Cow<'_, DynamicImage> {
    match img {
        DynamicImage::ImageRgb8(_) | DynamicImage::ImageRgba8(_) => Cow::Borrowed(img),
        other if is_opaque(other) => Cow::Owned(DynamicImage::ImageRgb8(other.to_rgb8())),
        other => Cow::Owned(DynamicImage::ImageRgba8(other.to_rgba8())),
    }
}

/// Görselde gerçekten saydam piksel var mı?
pub fn is_opaque(img: &DynamicImage) -> bool {
    match img {
        DynamicImage::ImageRgba8(buf) => buf.pixels().all(|p| p.0[3] == u8::MAX),
        DynamicImage::ImageLumaA8(buf) => buf.pixels().all(|p| p.0[1] == u8::MAX),
        other => !other.color().has_alpha(),
    }
}

fn is_high_depth(img: &DynamicImage) -> bool {
    img.color().bytes_per_pixel() / img.color().channel_count() > 1
}

/// JPEG saydamlık desteklemez: alfa kanalı beyaz zemin üzerine birleştirilir.
fn flatten_for_jpeg(img: &DynamicImage) -> Cow<'_, DynamicImage> {
    match img {
        DynamicImage::ImageRgb8(_) | DynamicImage::ImageLuma8(_) => Cow::Borrowed(img),
        other if !other.color().has_alpha() => Cow::Owned(if other.color().has_color() {
            DynamicImage::ImageRgb8(other.to_rgb8())
        } else {
            DynamicImage::ImageLuma8(other.to_luma8())
        }),
        other => {
            let rgba = other.to_rgba8();
            let blend = |c: u8, a: u32| ((u32::from(c) * a + 255 * (255 - a)) / 255) as u8;
            let rgb = RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
                let [r, g, b, a] = rgba.get_pixel(x, y).0;
                let a = u32::from(a);
                Rgb([blend(r, a), blend(g, a), blend(b, a)])
            });
            Cow::Owned(DynamicImage::ImageRgb8(rgb))
        }
    }
}

/// PNG için kayıpsız (veya neredeyse kayıpsız) küçültmeler:
/// - kalite < 100 ise 16 bit kanalları 8 bite indir,
/// - tamamen opak görsellerde gereksiz alfa kanalını at.
fn reduce_for_png(img: &DynamicImage, quality: u8) -> Cow<'_, DynamicImage> {
    let mut out = Cow::Borrowed(img);

    if quality < 100 && is_high_depth(img) {
        let color = img.color();
        out = Cow::Owned(match (color.has_color(), color.has_alpha()) {
            (true, true) => DynamicImage::ImageRgba8(img.to_rgba8()),
            (true, false) => DynamicImage::ImageRgb8(img.to_rgb8()),
            (false, true) => DynamicImage::ImageLumaA8(img.to_luma_alpha8()),
            (false, false) => DynamicImage::ImageLuma8(img.to_luma8()),
        });
    }

    let opaque_with_alpha = out.color().has_alpha() && is_opaque(&out);
    if opaque_with_alpha {
        out = Cow::Owned(if out.color().has_color() {
            DynamicImage::ImageRgb8(out.to_rgb8())
        } else {
            DynamicImage::ImageLuma8(out.to_luma8())
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    #[test]
    fn downscale_keeps_aspect_ratio_and_never_upscales() {
        let img = DynamicImage::ImageRgb8(RgbImage::new(4000, 2000));
        let small = downscale(img, Some(1000), Some(1000));
        assert_eq!((small.width(), small.height()), (1000, 500));

        let tiny = DynamicImage::ImageRgb8(RgbImage::new(100, 50));
        let same = downscale(tiny, Some(1000), Some(1000));
        assert_eq!((same.width(), same.height()), (100, 50));
    }

    #[test]
    fn jpeg_flattens_transparency() {
        let img = DynamicImage::ImageRgba8(RgbaImage::from_pixel(8, 8, Rgba([0, 0, 0, 0])));
        let bytes = encode(&img, ImageFormatKind::Jpeg, 80).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap().to_rgb8();
        // Tamamen saydam siyah piksel beyaz zemine dönüşmeli.
        assert!(decoded.get_pixel(0, 0).0.iter().all(|&c| c > 240));
    }

    #[test]
    fn compress_file_converts_and_resizes() {
        let dir = std::env::temp_dir().join(format!("mandalina-img-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let input = dir.join("foto.png");
        let output = dir.join("Sıkıştırılmış").join("foto.jpg");
        RgbImage::from_fn(800, 400, |x, y| Rgb([(x % 256) as u8, (y % 256) as u8, 128]))
            .save(&input)
            .unwrap();

        let opts = ImageOptions {
            quality: 70,
            max_width: Some(200),
            max_height: Some(200),
            convert_to: Some(ImageFormatKind::Jpeg),
        };
        let outcome = compress_file(&input, &output, &opts).unwrap();

        assert!(outcome.compressed_bytes < outcome.original_bytes);
        let result = image::open(&output).unwrap();
        assert_eq!((result.width(), result.height()), (200, 100));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn encodes_every_target_format() {
        let img = DynamicImage::ImageRgba8(RgbaImage::from_fn(300, 300, |x, y| {
            Rgba([(x % 256) as u8, (y % 256) as u8, 90, if x < 10 { 0 } else { 255 }])
        }));
        for format in [
            ImageFormatKind::Png,
            ImageFormatKind::Jpeg,
            ImageFormatKind::WebP,
            ImageFormatKind::Avif,
            ImageFormatKind::Tiff,
            ImageFormatKind::Bmp,
            ImageFormatKind::Ico,
        ] {
            let bytes = encode(&img, format, 70).unwrap_or_else(|e| panic!("{format:?}: {e}"));
            assert!(!bytes.is_empty(), "{format:?}");
            if format.is_decodable() {
                let decoded = image::load_from_memory(&bytes).unwrap_or_else(|e| panic!("{format:?}: {e}"));
                let expected = if format == ImageFormatKind::Ico { 256 } else { 300 };
                assert_eq!(decoded.width(), expected, "{format:?}");
            }
        }
    }

    #[test]
    fn png_drops_unused_alpha() {
        let img = DynamicImage::ImageRgba8(RgbaImage::from_pixel(8, 8, Rgba([10, 20, 30, 255])));
        let bytes = encode(&img, ImageFormatKind::Png, 80).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert!(!decoded.color().has_alpha());
    }
}
