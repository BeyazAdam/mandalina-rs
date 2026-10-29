//! Klasör izleme (Watch Folder) ve pano (clipboard) sıkıştırma.

use std::borrow::Cow;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use arboard::{Clipboard, ImageData};
use image::{DynamicImage, RgbaImage};
use notify_debouncer_full::notify::event::ModifyKind;
use notify_debouncer_full::notify::{EventKind, RecursiveMode};
use notify_debouncer_full::{new_debouncer, DebounceEventResult, DebouncedEvent};

use crate::config::{self, Settings};
use crate::events::{AppEvent, EventSink};
use crate::image_compressor::{self, CompressOutcome, ImageFormatKind, ImageOptions};
use crate::jobs;

/// Dosya kopyalama sırasında oluşan olay fırtınasını tek bir partiye toplar.
const DEBOUNCE: Duration = Duration::from_secs(2);
const CLIPBOARD_POLL: Duration = Duration::from_millis(1200);

// ---------------------------------------------------------------------------
// Klasör izleme
// ---------------------------------------------------------------------------

/// Etkin bir klasör izleyicisi. Bırakıldığında (drop) izleme durur.
pub struct FolderWatcher {
    dir: PathBuf,
    // Debouncer'ın somut türü crate sürümleri arasında değişebildiği için
    // sahipliği tür silinmiş olarak tutulur; tek görevi `Drop` ile izlemeyi durdurmak.
    _debouncer: Box<dyn std::any::Any>,
}

impl FolderWatcher {
    /// `dir` klasörünü (özyinelemesiz) izler. Yeni/değişen desteklenen dosyalar
    /// toplu halde `on_files` geri çağrısına verilir. Çıktılar `Sıkıştırılmış`
    /// alt klasörüne yazıldığı için sonsuz döngü oluşmaz.
    pub fn start<F>(dir: PathBuf, on_files: F) -> Result<Self>
    where
        F: Fn(Vec<PathBuf>) + Send + 'static,
    {
        fs::create_dir_all(&dir).with_context(|| format!("izleme klasörü oluşturulamadı: {}", dir.display()))?;

        let mut debouncer = new_debouncer(DEBOUNCE, None, move |result: DebounceEventResult| match result {
            Ok(events) => {
                let files = new_files(&events);
                if !files.is_empty() {
                    on_files(files);
                }
            }
            Err(errors) => errors.iter().for_each(|e| log::warn!("klasör izleme hatası: {e}")),
        })
        .context("dosya sistemi izleyicisi başlatılamadı")?;

        debouncer
            .watch(&dir, RecursiveMode::NonRecursive)
            .with_context(|| format!("klasör izlenemiyor: {}", dir.display()))?;

        log::info!("klasör izleniyor: {}", dir.display());
        Ok(Self {
            dir,
            _debouncer: Box::new(debouncer),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

fn new_files(events: &[DebouncedEvent]) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = events
        .iter()
        .filter(|ev| {
            matches!(
                ev.event.kind,
                EventKind::Create(_) | EventKind::Modify(ModifyKind::Name(_) | ModifyKind::Data(_) | ModifyKind::Any)
            )
        })
        .flat_map(|ev| ev.event.paths.iter().cloned())
        .filter(|p| p.is_file() && jobs::is_supported(p))
        .collect();
    files.sort();
    files.dedup();
    files
}

// ---------------------------------------------------------------------------
// Pano
// ---------------------------------------------------------------------------

/// Panoyu arka planda yoklayan iş parçacığının denetleyicisi.
///
/// Panoya erişimi tek bir iş parçacığı yapar: hem otomatik izleme hem de menüden
/// tetiklenen "şimdi sıkıştır" isteği buradan geçer. Böylece kendi yazdığımız
/// görseli "yeni görsel" sanıp tekrar tekrar sıkıştırma riski ortadan kalkar.
pub struct ClipboardWatcher {
    enabled: Arc<AtomicBool>,
    request: Arc<AtomicBool>,
}

impl ClipboardWatcher {
    pub fn spawn(settings: Arc<RwLock<Settings>>, sink: EventSink) -> Result<Self> {
        let enabled = Arc::new(AtomicBool::new(false));
        let request = Arc::new(AtomicBool::new(false));
        let (e, r) = (Arc::clone(&enabled), Arc::clone(&request));
        thread::Builder::new()
            .name("pano-izleyici".into())
            .spawn(move || clipboard_loop(&e, &r, &settings, &sink))
            .context("pano iş parçacığı başlatılamadı")?;
        Ok(Self { enabled, request })
    }

    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::SeqCst);
    }

    /// Panodaki görseli bir sonraki yoklamada sıkıştır.
    pub fn request_compress(&self) {
        self.request.store(true, Ordering::SeqCst);
    }
}

fn clipboard_loop(enabled: &AtomicBool, request: &AtomicBool, settings: &RwLock<Settings>, sink: &EventSink) {
    let mut clipboard = match Clipboard::new() {
        Ok(c) => c,
        Err(e) => {
            sink.send(AppEvent::Error(format!("Panoya erişilemiyor: {e}")));
            return;
        }
    };
    let mut last_seen: Option<u64> = None;
    let mut was_enabled = false;

    loop {
        thread::sleep(CLIPBOARD_POLL);
        let manual = request.swap(false, Ordering::SeqCst);
        let auto = enabled.load(Ordering::SeqCst);
        if !manual && !auto {
            was_enabled = false;
            continue;
        }

        let current = clipboard.get_image().ok().map(|img| fingerprint(&img));
        // Otomatik izleme yeni açıldıysa panoda zaten duran görsel işlenmez.
        if auto && !was_enabled && !manual {
            was_enabled = true;
            last_seen = current;
            continue;
        }
        was_enabled = auto;

        let changed = current.is_some() && current != last_seen;
        if manual || changed {
            jobs::run_clipboard_job(&mut clipboard, &config::snapshot(settings), sink);
            // Yazdığımız görselin parmak izini al ki bir sonraki turda "yeni" sayılmasın.
            last_seen = clipboard.get_image().ok().map(|img| fingerprint(&img));
        }
    }
}

/// Görselin tamamını hash'lemek büyük ekran görüntülerinde pahalıdır;
/// boyut + seyrek örnekleme değişikliği yakalamak için yeterlidir.
fn fingerprint(img: &ImageData<'_>) -> u64 {
    let mut hasher = DefaultHasher::new();
    img.width.hash(&mut hasher);
    img.height.hash(&mut hasher);
    img.bytes.iter().step_by(509).for_each(|b| b.hash(&mut hasher));
    hasher.finish()
}

/// Panodaki görseli küçültür/yeniden kodlar, sonucu panoya geri yazar ve bir
/// kopyasını `out_dir` içine kaydeder.
///
/// Not: Pano ham RGBA piksel tutar; yani panoda kazanılan şey çözünürlük
/// küçültmesi ve kayıplı kodlamanın etkisidir. Asıl bayt tasarrufu diske
/// kaydedilen dosyada (ve onu yapıştırdığınız uygulamada) görülür.
pub fn compress_clipboard_image(
    clipboard: &mut Clipboard,
    opts: &ImageOptions,
    out_dir: &Path,
) -> Result<CompressOutcome> {
    let data = clipboard.get_image().context("panoda görsel yok")?;
    let (width, height) = (u32::try_from(data.width)?, u32::try_from(data.height)?);
    let rgba = RgbaImage::from_raw(width, height, data.bytes.into_owned()).context("pano görüntü verisi geçersiz")?;
    let original_bytes = rgba.as_raw().len() as u64;

    let img = image_compressor::downscale(DynamicImage::ImageRgba8(rgba), opts.max_width, opts.max_height);
    let format = opts.convert_to.unwrap_or(if image_compressor::is_opaque(&img) {
        ImageFormatKind::Jpeg
    } else {
        ImageFormatKind::Png
    });
    let encoded = image_compressor::encode(&img, format, opts.quality)?;
    drop(img);

    // Kayıplı sonucu panoya geri koy.
    let decoded = image::load_from_memory(&encoded)?.to_rgba8();
    let (w, h) = decoded.dimensions();
    clipboard
        .set_image(ImageData {
            width: w as usize,
            height: h as usize,
            bytes: Cow::Owned(decoded.into_raw()),
        })
        .context("görsel panoya yazılamadı")?;

    fs::create_dir_all(out_dir)?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let output = out_dir.join(format!("pano-{stamp}.{}", format.extension()));
    fs::write(&output, &encoded)?;

    Ok(CompressOutcome {
        original_bytes,
        compressed_bytes: encoded.len() as u64,
        output,
        kept_original: false,
    })
}
