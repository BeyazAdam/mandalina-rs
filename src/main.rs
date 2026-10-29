//! Mandalina — tamamen yerel çalışan, sistem tepsisi tabanlı görsel/PDF sıkıştırıcı.
//!
//! İş parçacığı modeli:
//! - Ana iş parçacığı: `tao` olay döngüsü + tepsi simgesi + menü (tek sahip).
//! - `rayon` havuzu: dosya başına paralel sıkıştırma.
//! - Klasör izleyici (`notify`) ve pano yoklayıcı: kendi iş parçacıkları.
//!
//! Arka plan tarafı ana döngüyle yalnızca `AppEvent` mesajlarıyla konuşur.

// Sürüm derlemesinde Windows'ta konsol penceresi açılmasın.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod error;
mod events;
mod image_compressor;
mod jobs;
mod pdf_compressor;
mod theme;
mod tray;
mod util;
mod watcher;
mod wheel;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Instant;

use anyhow::{Context, Result};
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::MenuEvent;

use crate::config::Settings;
use crate::events::{AppEvent, EventSink};
use crate::jobs::SessionStats;
use crate::tray::{MenuAction, TrayMenu};
use crate::watcher::{ClipboardWatcher, FolderWatcher};
use crate::wheel::{DropAction, Target, Wheel};

/// Uygulamanın görünen adı.
pub const APP_NAME: &str = "Mandalina";
/// Yapımcı. `Cargo.toml` içindeki `authors` alanından gelir.
pub const AUTHOR: &str = env!("CARGO_PKG_AUTHORS");
/// Arayüzde gösterilen yapımcı imzası.
pub const SIGNATURE: &str = concat!("by ", env!("CARGO_PKG_AUTHORS"));
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

/// Sürmekte olan bir işin menüde gösterilecek durumu.
struct ActiveJob {
    label: String,
    done: usize,
    total: usize,
}

/// Ana iş parçacığına ait uygulama durumu.
struct App {
    settings: Arc<RwLock<Settings>>,
    sink: EventSink,
    tray: Option<TrayMenu>,
    folder_watcher: Option<FolderWatcher>,
    clipboard: ClipboardWatcher,
    /// Shift + sürükle çarkı. Platform desteklemiyorsa `None`.
    wheel: Option<Wheel>,
    wheel_enabled: Arc<AtomicBool>,
    active: HashMap<u64, ActiveJob>,
    stats: SessionStats,
}

impl App {
    /// Olay döngüsü çalışmaya başladığında çağrılır (macOS tepsi simgesinin
    /// döngü başladıktan sonra oluşturulmasını şart koşar).
    fn init(&mut self) -> Result<()> {
        let settings = config::snapshot(&self.settings);
        self.tray = Some(TrayMenu::new(&settings).context("sistem tepsisi oluşturulamadı")?);
        if settings.watch_on_start {
            self.set_folder_watch(true);
        }
        match Wheel::new(self.sink.clone(), Arc::clone(&self.wheel_enabled)) {
            Ok(wheel) => self.wheel = Some(wheel),
            // Çark isteğe bağlı bir özelliktir; açılamaması uygulamayı durdurmaz.
            Err(e) => log::warn!("çark devre dışı: {e:#}"),
        }
        Ok(())
    }

    /// Olay döngüsünün zamanlayıcıyla uyanması gereken an (çarkı kapatmak için).
    fn deadline(&self) -> Option<Instant> {
        self.wheel.as_ref().and_then(Wheel::deadline)
    }

    fn tick(&mut self) {
        if let Some(wheel) = &mut self.wheel {
            wheel.tick(Instant::now());
        }
    }

    fn handle_wheel_drop(&mut self, action: DropAction) {
        let mut settings = config::snapshot(&self.settings);
        let job = match action.target {
            // Merkez: mevcut ayarlarla, format değiştirmeden sıkıştır.
            None => {
                settings.convert_to = None;
                jobs::spawn_file_job("Çark: sıkıştır", action.files, settings, self.sink.clone())
            }
            Some(Target::Image(format)) => {
                settings.convert_to = Some(format);
                let images = action.files.into_iter().filter(|p| jobs::is_image(p)).collect();
                jobs::spawn_file_job(format!("Çark: {}", format.label()), images, settings, self.sink.clone())
            }
            Some(Target::Pdf) => {
                let (pdfs, images): (Vec<_>, Vec<_>) = action.files.into_iter().partition(|p| jobs::is_pdf(p));
                // Görseller tek PDF'te birleşir, PDF'ler sıkıştırılır. Çark ilk işin sonucunu gösterir.
                let merged = jobs::spawn_images_to_pdf_job(images, settings.clone(), self.sink.clone());
                let compressed = (!pdfs.is_empty())
                    .then(|| jobs::spawn_file_job("Çark: PDF", pdfs, settings, self.sink.clone()))
                    .flatten();
                merged.or(compressed)
            }
        };
        if let Some(wheel) = &mut self.wheel {
            wheel.set_job(job);
        }
    }

    /// `false` dönerse uygulama kapanır.
    fn handle(&mut self, event: AppEvent) -> bool {
        match event {
            AppEvent::Menu(menu_event) => {
                let action = self.tray.as_ref().and_then(|t| t.action_for(&menu_event));
                if let Some(action) = action {
                    return self.handle_action(action);
                }
            }
            AppEvent::JobStarted { id, label, total } => {
                self.active.insert(id, ActiveJob { label, done: 0, total });
                self.refresh_tray();
            }
            AppEvent::JobProgress { id, done, total } => {
                if let Some(job) = self.active.get_mut(&id) {
                    job.done = done;
                    job.total = total;
                }
                self.refresh_tray();
            }
            AppEvent::JobFinished { id, report } => {
                self.active.remove(&id);
                log::info!("{}: {}", report.label, report.summary());
                if let Some(wheel) = &mut self.wheel {
                    wheel.job_finished(id, &report);
                }
                self.stats.record(report);
                self.refresh_tray();
            }
            AppEvent::Wheel(wheel_event) => {
                let action = self.wheel.as_mut().and_then(|w| w.handle(wheel_event));
                if let Some(action) = action {
                    self.handle_wheel_drop(action);
                }
            }
            AppEvent::Error(message) => {
                log::error!("{message}");
                jobs::show_notification(APP_NAME, &message);
            }
        }
        true
    }

    fn handle_action(&mut self, action: MenuAction) -> bool {
        match action {
            MenuAction::CompressFiles => {
                // Not: rfd pencereleri bloklayıcıdır; açıkken menü yanıt vermez.
                // İşleme ise ayrı iş parçacığında yapılır, döngü hemen serbest kalır.
                if let Some(files) = rfd::FileDialog::new()
                    .set_title("Sıkıştırılacak dosyaları seçin")
                    .add_filter(
                        "Görseller ve PDF",
                        &["png", "jpg", "jpeg", "webp", "tif", "tiff", "bmp", "ico", "pdf"],
                    )
                    .pick_files()
                {
                    let files = files.into_iter().filter(|p| jobs::is_supported(p)).collect();
                    self.spawn_job("Seçili dosyalar", files);
                }
            }
            MenuAction::CompressFolder => {
                if let Some(dir) = rfd::FileDialog::new()
                    .set_title("Sıkıştırılacak klasörü seçin")
                    .pick_folder()
                {
                    let label = dir
                        .file_name()
                        .map_or_else(|| "Klasör".to_owned(), |n| n.to_string_lossy().into_owned());
                    self.spawn_job(label, jobs::collect_files(&dir));
                }
            }
            MenuAction::CompressClipboard => self.clipboard.request_compress(),
            MenuAction::ToggleWatchFolder(on) => self.set_folder_watch(on),
            MenuAction::OpenWatchFolder => {
                let dir = config::snapshot(&self.settings).watch_dir;
                self.open_dir(&dir);
            }
            MenuAction::ToggleClipboardWatch(on) => self.clipboard.set_enabled(on),
            MenuAction::ToggleWheel(on) => self.wheel_enabled.store(on, Ordering::Relaxed),
            MenuAction::OpenRepository => {
                // Yalnızca kullanıcı tıkladığında varsayılan tarayıcıda açılır; uygulama ağa bağlanmaz.
                if let Err(e) = util::open_url(REPOSITORY) {
                    self.sink.send(AppEvent::Error(format!("Bağlantı açılamadı: {e}")));
                }
            }
            MenuAction::OpenLastOutput => {
                if let Some(dir) = self.stats.last.as_ref().and_then(|r| r.output_dir.as_deref()) {
                    self.open_dir(dir);
                }
            }
            MenuAction::SetQuality(q) => self.update_settings(|s| s.quality = q),
            MenuAction::SetMaxDimension(d) => self.update_settings(|s| s.max_dimension = d),
            MenuAction::SetFormat(f) => self.update_settings(|s| s.convert_to = f),
            MenuAction::Quit => return false,
        }
        true
    }

    fn spawn_job(&self, label: impl Into<String>, files: Vec<PathBuf>) {
        jobs::spawn_file_job(label, files, config::snapshot(&self.settings), self.sink.clone());
    }

    fn update_settings(&self, change: impl FnOnce(&mut Settings)) {
        let mut guard = self.settings.write().unwrap_or_else(PoisonError::into_inner);
        change(&mut guard);
        log::info!("ayarlar güncellendi: {:?}", *guard);
    }

    fn set_folder_watch(&mut self, on: bool) {
        if !on {
            // `FolderWatcher` bırakılınca izleme durur.
            self.folder_watcher = None;
            return;
        }
        let dir = config::snapshot(&self.settings).watch_dir;
        let settings = Arc::clone(&self.settings);
        let sink = self.sink.clone();
        let result = FolderWatcher::start(dir, move |files| {
            jobs::spawn_file_job("İzlenen klasör", files, config::snapshot(&settings), sink.clone());
        });
        match result {
            Ok(w) => {
                log::info!("klasör izleme açık: {}", w.dir().display());
                self.folder_watcher = Some(w);
            }
            Err(e) => {
                self.sink.send(AppEvent::Error(format!("{e:#}")));
                if let Some(tray) = &self.tray {
                    tray.set_watch_checked(false);
                }
            }
        }
    }

    fn open_dir(&self, dir: &Path) {
        let result = std::fs::create_dir_all(dir).and_then(|()| util::open_in_file_manager(dir));
        if let Err(e) = result {
            self.sink
                .send(AppEvent::Error(format!("{} açılamadı: {e}", dir.display())));
        }
    }

    /// Menüyü mevcut duruma göre yeniden çizer (bkz. README: dinamik menü).
    fn refresh_tray(&self) {
        let Some(tray) = &self.tray else {
            return;
        };
        // En eski işi (en küçük id) öne çıkar, diğerlerini "+N iş" olarak göster.
        match self.active.iter().min_by_key(|(id, _)| **id) {
            Some((_, job)) => tray.show_progress(&job.label, job.done, job.total, self.active.len() - 1),
            None => tray.show_idle(&self.stats),
        }
    }
}

fn configure_thread_pool() {
    // Düşük bellek/CPU etkisi için bir çekirdek boşta bırakılır ve üst sınır konur:
    // her iş parçacığı aynı anda tam çözülmüş bir görseli bellekte tutar.
    let threads = std::thread::available_parallelism()
        .map_or(2, |n| n.get().saturating_sub(1))
        .clamp(1, 6);
    if let Err(e) = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|i| format!("sikistirici-{i}"))
        .build_global()
    {
        log::warn!("rayon havuzu yapılandırılamadı: {e}");
    }
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    log::info!("{APP_NAME} v{VERSION} — {AUTHOR} · {REPOSITORY}");
    configure_thread_pool();

    let settings = Arc::new(RwLock::new(Settings::default()));
    let event_loop = EventLoopBuilder::<AppEvent>::with_user_event().build();
    let sink = EventSink::new(event_loop.create_proxy());

    // Menü olaylarını olay döngüsüne yönlendir; böylece döngü `ControlFlow::Wait`
    // ile tamamen uykuda kalabilir (boşta CPU kullanımı ~0).
    {
        let sink = sink.clone();
        MenuEvent::set_event_handler(Some(move |event| sink.send(AppEvent::Menu(event))));
    }

    let clipboard = ClipboardWatcher::spawn(Arc::clone(&settings), sink.clone())?;

    let mut app = App {
        settings,
        sink,
        tray: None,
        folder_watcher: None,
        clipboard,
        wheel: None,
        wheel_enabled: Arc::new(AtomicBool::new(true)),
        active: HashMap::new(),
        stats: SessionStats::default(),
    };

    event_loop.run(move |event, _target, control_flow| {
        if *control_flow == ControlFlow::Exit {
            return;
        }
        match event {
            Event::NewEvents(StartCause::ResumeTimeReached { .. }) => app.tick(),
            Event::NewEvents(StartCause::Init) => {
                if let Err(e) = app.init() {
                    log::error!("{e:#}");
                    jobs::show_notification(&format!("{APP_NAME} başlatılamadı"), &format!("{e:#}"));
                    *control_flow = ControlFlow::Exit;
                    return;
                }
            }
            Event::UserEvent(app_event) => {
                if !app.handle(app_event) {
                    // Tepsi simgesini döngü bitmeden kaldır; aksi halde Windows'ta
                    // fare üzerine gelene kadar "hayalet" simge kalabilir.
                    app.tray = None;
                    app.folder_watcher = None;
                    app.wheel = None;
                    *control_flow = ControlFlow::Exit;
                    return;
                }
            }
            _ => {}
        }
        // Çark açıkken kapanma zamanı için uyan; aksi halde tamamen uyu.
        *control_flow = match app.deadline() {
            Some(at) => ControlFlow::WaitUntil(at),
            None => ControlFlow::Wait,
        };
    })
}
