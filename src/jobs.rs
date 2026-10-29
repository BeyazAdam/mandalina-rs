//! İş kuyruğu: dosyaları `rayon` ile paralel sıkıştırır, ilerlemeyi ana
//! döngüye bildirir ve iş bitince işletim sistemi bildirimi gösterir.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use arboard::Clipboard;
use rayon::prelude::*;

use crate::config::{Settings, OUTPUT_DIR_NAME};
use crate::error::{CompressError, Result};
use crate::events::{AppEvent, EventSink};
use crate::image_compressor::{self, CompressOutcome, ImageFormatKind};
use crate::{pdf_compressor, util, watcher};

static NEXT_JOB_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Default)]
pub struct JobReport {
    pub label: String,
    pub processed: usize,
    pub failed: usize,
    pub original_bytes: u64,
    pub compressed_bytes: u64,
    pub elapsed: Duration,
    pub output_dir: Option<PathBuf>,
}

impl JobReport {
    fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            ..Self::default()
        }
    }

    fn add(&mut self, outcome: &CompressOutcome) {
        self.processed += 1;
        self.original_bytes += outcome.original_bytes;
        self.compressed_bytes += outcome.compressed_bytes;
        if self.output_dir.is_none() {
            self.output_dir = outcome.output.parent().map(Path::to_path_buf);
        }
    }

    pub fn saved_bytes(&self) -> u64 {
        self.original_bytes.saturating_sub(self.compressed_bytes)
    }

    /// "12 dosya · 34,5 MB tasarruf (%61)"
    pub fn summary(&self) -> String {
        let mut text = format!(
            "{} dosya · {} tasarruf ({})",
            self.processed,
            util::format_bytes(self.saved_bytes()),
            util::format_percent(self.saved_bytes(), self.original_bytes),
        );
        if self.failed > 0 {
            text.push_str(&format!(" · {} hata", self.failed));
        }
        text
    }
}

/// Oturum boyunca biriken istatistikler (menüde gösterilir).
#[derive(Debug, Default)]
pub struct SessionStats {
    pub files: usize,
    pub original_bytes: u64,
    pub compressed_bytes: u64,
    pub last: Option<JobReport>,
}

impl SessionStats {
    pub fn record(&mut self, report: JobReport) {
        self.files += report.processed;
        self.original_bytes += report.original_bytes;
        self.compressed_bytes += report.compressed_bytes;
        self.last = Some(report);
    }

    pub fn saved_bytes(&self) -> u64 {
        self.original_bytes.saturating_sub(self.compressed_bytes)
    }
}

pub fn is_supported(path: &Path) -> bool {
    is_image(path) || is_pdf(path)
}

pub fn is_image(path: &Path) -> bool {
    ImageFormatKind::decodable_from_path(path).is_some()
}

pub fn is_pdf(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

/// Klasördeki desteklenen dosyaları (alt klasörler dahil) toplar.
/// Daha önce üretilmiş `Sıkıştırılmış` klasörleri atlanır.
pub fn collect_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if entry.file_name() != OUTPUT_DIR_NAME {
                    stack.push(path);
                }
            } else if is_supported(&path) {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// Dosyaları ayrı bir iş parçacığında işler; çağıran (ana döngü) beklemez.
/// Dönen kimlik, `JobStarted`/`JobFinished` olaylarındaki `id` ile aynıdır.
pub fn spawn_file_job(
    label: impl Into<String>,
    files: Vec<PathBuf>,
    settings: Settings,
    sink: EventSink,
) -> Option<u64> {
    if files.is_empty() {
        sink.send(AppEvent::Error("İşlenecek desteklenen dosya bulunamadı.".into()));
        return None;
    }
    let label = label.into();
    let id = NEXT_JOB_ID.fetch_add(1, Ordering::Relaxed);
    spawn_worker(move || run_file_job(id, label, &files, &settings, &sink));
    Some(id)
}

/// Görselleri sırayla tek bir PDF'te birleştirir (her görsel bir sayfa).
pub fn spawn_images_to_pdf_job(images: Vec<PathBuf>, settings: Settings, sink: EventSink) -> Option<u64> {
    let first = images.first()?.clone();
    let id = NEXT_JOB_ID.fetch_add(1, Ordering::Relaxed);
    spawn_worker(move || {
        let started = Instant::now();
        sink.send(AppEvent::JobStarted {
            id,
            label: "PDF oluştur".into(),
            total: images.len(),
        });

        let output = merged_pdf_path(&first, images.len());
        let mut report = JobReport::new("PDF oluştur");
        match pdf_compressor::images_to_pdf(&images, &output, &settings.image_options()) {
            Ok(outcome) => {
                report.add(&outcome);
                report.processed = images.len();
            }
            Err(e) => {
                report.failed = images.len();
                sink.send(AppEvent::Error(format!("PDF oluşturulamadı: {e}")));
            }
        }
        sink.send(AppEvent::JobProgress {
            id,
            done: images.len(),
            total: images.len(),
        });
        report.elapsed = started.elapsed();
        finish(id, report, &sink);
    });
    Some(id)
}

/// `foto.png` -> `Sıkıştırılmış/foto.pdf`; birden çok görselde `foto-ve-3-gorsel.pdf`.
fn merged_pdf_path(first: &Path, count: usize) -> PathBuf {
    let dir = first.parent().unwrap_or(Path::new(".")).join(OUTPUT_DIR_NAME);
    let stem = first.file_stem().unwrap_or_default().to_string_lossy();
    if count == 1 {
        dir.join(format!("{stem}.pdf"))
    } else {
        dir.join(format!("{stem}-ve-{}-gorsel.pdf", count - 1))
    }
}

fn spawn_worker(work: impl FnOnce() + Send + 'static) {
    if let Err(e) = thread::Builder::new().name("sikistirma-isi".into()).spawn(work) {
        log::error!("iş parçacığı başlatılamadı: {e}");
    }
}

fn run_file_job(id: u64, label: String, files: &[PathBuf], settings: &Settings, sink: &EventSink) {
    let total = files.len();
    let started = Instant::now();
    sink.send(AppEvent::JobStarted {
        id,
        label: label.clone(),
        total,
    });

    let done = AtomicUsize::new(0);
    let results: Vec<Result<CompressOutcome>> = files
        .par_iter()
        .map(|path| {
            let result = process_file(path, settings);
            match &result {
                Ok(o) if o.kept_original => {
                    log::info!("{}: daha fazla küçültülemedi, orijinal kopyalandı", path.display());
                }
                Ok(o) => log::info!(
                    "{} -> {} ({} tasarruf)",
                    path.display(),
                    o.output.display(),
                    util::format_bytes(o.saved_bytes())
                ),
                Err(e) => log::warn!("{}: {e}", path.display()),
            }
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            sink.send(AppEvent::JobProgress { id, done: n, total });
            result
        })
        .collect();

    let mut report = JobReport::new(label);
    for result in &results {
        match result {
            Ok(outcome) => report.add(outcome),
            Err(_) => report.failed += 1,
        }
    }
    report.elapsed = started.elapsed();
    finish(id, report, sink);
}

fn process_file(path: &Path, settings: &Settings) -> Result<CompressOutcome> {
    wait_until_stable(path)?;
    let output = settings.output_path(path);
    if is_pdf(path) {
        let outcome = pdf_compressor::compress_pdf(path, &output, &settings.pdf_options())?;
        log::info!(
            "{}: {}/{} görsel yeniden kodlandı",
            path.display(),
            outcome.images_recompressed,
            outcome.images_found
        );
        Ok(outcome.file)
    } else if is_image(path) {
        image_compressor::compress_file(path, &output, &settings.image_options())
    } else {
        Err(CompressError::Unsupported(path.to_path_buf()))
    }
}

/// İzlenen klasöre kopyalanmakta olan dosyanın yazımı bitene kadar bekler.
fn wait_until_stable(path: &Path) -> Result<()> {
    const STEP: Duration = Duration::from_millis(250);
    let mut last = fs::metadata(path)?.len();
    for _ in 0..40 {
        thread::sleep(STEP);
        let now = fs::metadata(path)?.len();
        if now == last && now > 0 {
            return Ok(());
        }
        last = now;
    }
    Ok(())
}

/// Panodaki görseli sıkıştırır. Pano izleyici iş parçacığında çalışır; panoya
/// tek bir `Clipboard` örneği üzerinden erişilmesi yarış durumlarını önler.
pub fn run_clipboard_job(clipboard: &mut Clipboard, settings: &Settings, sink: &EventSink) {
    let id = NEXT_JOB_ID.fetch_add(1, Ordering::Relaxed);
    let started = Instant::now();
    sink.send(AppEvent::JobStarted {
        id,
        label: "Pano".into(),
        total: 1,
    });

    let mut report = JobReport::new("Pano");
    match watcher::compress_clipboard_image(clipboard, &settings.image_options(), &settings.clipboard_output_dir()) {
        Ok(outcome) => report.add(&outcome),
        Err(e) => {
            report.failed = 1;
            sink.send(AppEvent::Error(format!("Pano: {e:#}")));
        }
    }
    sink.send(AppEvent::JobProgress { id, done: 1, total: 1 });
    report.elapsed = started.elapsed();
    finish(id, report, sink);
}

fn finish(id: u64, report: JobReport, sink: &EventSink) {
    if report.processed > 0 {
        notify_report(&report);
    }
    sink.send(AppEvent::JobFinished { id, report });
}

/// İşletim sisteminin yerel bildirimini gösterir (Windows Toast, macOS
/// Bildirim Merkezi, Linux D-Bus). Hata durumunda yalnızca loglanır.
pub fn notify_report(report: &JobReport) {
    let body = format!(
        "{}\n{} → {} · {:.1} sn",
        report.summary(),
        util::format_bytes(report.original_bytes),
        util::format_bytes(report.compressed_bytes),
        report.elapsed.as_secs_f32(),
    );
    show_notification(&format!("{} tamamlandı", report.label), &body);
}

pub fn show_notification(summary: &str, body: &str) {
    let result = notify_rust::Notification::new()
        .appname(crate::APP_NAME)
        .summary(summary)
        .body(body)
        .show();
    if let Err(e) = result {
        log::warn!("bildirim gösterilemedi: {e}");
    }
}
