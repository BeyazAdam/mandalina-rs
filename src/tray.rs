//! Sistem tepsisi simgesi, dinamik menü ve menü olaylarının eyleme çevrilmesi.
//!
//! Bu türdeki her şey ana (olay döngüsü) iş parçacığında yaşamalıdır:
//! `MenuItem` / `TrayIcon` `!Send` türlerdir. Arka plan işleri menüyü
//! `AppEvent` göndererek dolaylı yoldan günceller.

use anyhow::{anyhow, Result};
use tiny_skia::{
    Color, FillRule, GradientStop, LineCap, LinearGradient, Paint, PathBuilder, Pixmap, Point, SpreadMode, Stroke,
    Transform,
};
use tray_icon::menu::{CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::config::Settings;
use crate::image_compressor::ImageFormatKind;
use crate::jobs::SessionStats;
use crate::theme;
use crate::util;

/// Menü tıklamalarının uygulama düzeyindeki karşılığı.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    CompressFiles,
    CompressFolder,
    CompressClipboard,
    ToggleWatchFolder(bool),
    OpenWatchFolder,
    ToggleClipboardWatch(bool),
    ToggleWheel(bool),
    OpenLastOutput,
    OpenRepository,
    SetQuality(u8),
    SetMaxDimension(Option<u32>),
    SetFormat(Option<ImageFormatKind>),
    Quit,
}

/// Tek seçimli (radyo düğmesi gibi davranan) onay kutusu grubu.
struct RadioGroup<T> {
    items: Vec<(T, CheckMenuItem)>,
}

impl<T: Copy + PartialEq> RadioGroup<T> {
    fn new(options: &[(T, &str)], selected: T) -> Self {
        let items = options
            .iter()
            .map(|&(value, label)| (value, CheckMenuItem::new(label, true, value == selected, None)))
            .collect();
        Self { items }
    }

    fn submenu(&self, title: &str) -> Result<Submenu> {
        let refs: Vec<&dyn IsMenuItem> = self.items.iter().map(|(_, i)| i as &dyn IsMenuItem).collect();
        Ok(Submenu::with_items(title, true, &refs)?)
    }

    fn find(&self, id: &MenuId) -> Option<T> {
        self.items.iter().find(|(_, i)| i.id() == id).map(|(v, _)| *v)
    }

    /// Tıklama onay işaretini kendiliğinden değiştirir; grubu tutarlı hale getirir.
    fn select(&self, value: T) {
        for (v, item) in &self.items {
            item.set_checked(*v == value);
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum IconState {
    Idle,
    Busy,
}

pub struct TrayMenu {
    tray: TrayIcon,
    // Salt okunur bilgi satırları (devre dışı öğeler, metni dinamik değişir)
    status: MenuItem,
    stats: MenuItem,
    // Eylemler
    compress_files: MenuItem,
    compress_folder: MenuItem,
    compress_clipboard: MenuItem,
    watch_toggle: CheckMenuItem,
    open_watch: MenuItem,
    clipboard_toggle: CheckMenuItem,
    wheel_toggle: CheckMenuItem,
    open_last: MenuItem,
    quality: RadioGroup<u8>,
    resolution: RadioGroup<Option<u32>>,
    format: RadioGroup<Option<ImageFormatKind>>,
    repository: MenuItem,
    quit: MenuItem,
}

impl TrayMenu {
    pub fn new(settings: &Settings) -> Result<Self> {
        let status = MenuItem::new("Hazır", false, None);
        let stats = MenuItem::new("Henüz dosya işlenmedi", false, None);

        let compress_files = MenuItem::new("Dosya seç ve sıkıştır…", true, None);
        let compress_folder = MenuItem::new("Klasör seç ve sıkıştır…", true, None);
        let compress_clipboard = MenuItem::new("Panodaki görseli sıkıştır", true, None);

        let watch_toggle = CheckMenuItem::new("Klasörü izle", true, settings.watch_on_start, None);
        let open_watch = MenuItem::new("İzlenen klasörü aç", true, None);
        let clipboard_toggle = CheckMenuItem::new("Panoyu otomatik sıkıştır", true, false, None);
        let wheel_toggle = CheckMenuItem::new("Shift + sürükle çarkı", cfg!(windows), cfg!(windows), None);
        let open_last = MenuItem::new("Son çıktı klasörünü aç", false, None);

        let quality = RadioGroup::new(
            &[
                (95, "%95 – En yüksek kalite"),
                (85, "%85 – Yüksek"),
                (80, "%80 – Önerilen"),
                (70, "%70 – Dengeli"),
                (60, "%60 – En küçük boyut"),
            ],
            settings.quality,
        );
        let resolution = RadioGroup::new(
            &[
                (None, "Orijinal çözünürlük"),
                (Some(3840), "En fazla 3840 px (4K)"),
                (Some(2560), "En fazla 2560 px"),
                (Some(1920), "En fazla 1920 px (Full HD)"),
                (Some(1280), "En fazla 1280 px"),
            ],
            settings.max_dimension,
        );
        let format = RadioGroup::new(
            &[
                (None, "Orijinal formatı koru"),
                (Some(ImageFormatKind::Jpeg), "JPEG'e dönüştür"),
                (Some(ImageFormatKind::WebP), "WebP'ye dönüştür (kayıpsız)"),
                (Some(ImageFormatKind::Png), "PNG'ye dönüştür"),
            ],
            settings.convert_to,
        );
        // Yapımcı imzası: menünün en üstünde, tıklanamayan başlık satırı.
        let signature = MenuItem::new(
            format!("{} v{} · {}", crate::APP_NAME, crate::VERSION, crate::SIGNATURE),
            false,
            None,
        );
        let repository = MenuItem::new(format!("GitHub'da aç · {}", crate::AUTHOR), true, None);
        let quit = MenuItem::new("Çıkış", true, None);

        let menu = Menu::new();
        menu.append_items(&[
            &signature,
            &PredefinedMenuItem::separator(),
            &status,
            &stats,
            &PredefinedMenuItem::separator(),
            &compress_files,
            &compress_folder,
            &compress_clipboard,
            &PredefinedMenuItem::separator(),
            &watch_toggle,
            &open_watch,
            &clipboard_toggle,
            &wheel_toggle,
            &open_last,
            &PredefinedMenuItem::separator(),
            &quality.submenu("Kalite")?,
            &resolution.submenu("Maksimum çözünürlük")?,
            &format.submenu("Çıktı formatı")?,
            &PredefinedMenuItem::separator(),
            &repository,
            &quit,
        ])?;

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip(format!("{} {} – hazır", crate::APP_NAME, crate::SIGNATURE))
            .with_icon(render_icon(IconState::Idle)?)
            .build()?;

        Ok(Self {
            tray,
            status,
            stats,
            compress_files,
            compress_folder,
            compress_clipboard,
            watch_toggle,
            open_watch,
            clipboard_toggle,
            wheel_toggle,
            open_last,
            quality,
            resolution,
            format,
            repository,
            quit,
        })
    }

    /// Ham menü olayını uygulama eylemine çevirir. Radyo gruplarının tutarlılığı
    /// burada sağlanır ki çağıranın ayrıca uğraşması gerekmesin.
    pub fn action_for(&self, event: &MenuEvent) -> Option<MenuAction> {
        let id = &event.id;
        let action = if id == self.compress_files.id() {
            MenuAction::CompressFiles
        } else if id == self.compress_folder.id() {
            MenuAction::CompressFolder
        } else if id == self.compress_clipboard.id() {
            MenuAction::CompressClipboard
        } else if id == self.watch_toggle.id() {
            // CheckMenuItem tıklamada kendi durumunu zaten değiştirmiştir.
            MenuAction::ToggleWatchFolder(self.watch_toggle.is_checked())
        } else if id == self.open_watch.id() {
            MenuAction::OpenWatchFolder
        } else if id == self.clipboard_toggle.id() {
            MenuAction::ToggleClipboardWatch(self.clipboard_toggle.is_checked())
        } else if id == self.wheel_toggle.id() {
            MenuAction::ToggleWheel(self.wheel_toggle.is_checked())
        } else if id == self.open_last.id() {
            MenuAction::OpenLastOutput
        } else if id == self.repository.id() {
            MenuAction::OpenRepository
        } else if id == self.quit.id() {
            MenuAction::Quit
        } else if let Some(q) = self.quality.find(id) {
            self.quality.select(q);
            MenuAction::SetQuality(q)
        } else if let Some(r) = self.resolution.find(id) {
            self.resolution.select(r);
            MenuAction::SetMaxDimension(r)
        } else {
            let f = self.format.find(id)?;
            self.format.select(f);
            MenuAction::SetFormat(f)
        };
        Some(action)
    }

    // -----------------------------------------------------------------------
    // Dinamik güncellemeler
    // -----------------------------------------------------------------------

    /// İş sürerken: "⏳ Fotoğraflar: işleniyor… 3/10 (+1 iş)"
    pub fn show_progress(&self, label: &str, done: usize, total: usize, other_jobs: usize) {
        let mut text = format!("⏳ {label}: işleniyor… {done}/{total}");
        if other_jobs > 0 {
            text.push_str(&format!(" (+{other_jobs} iş)"));
        }
        self.status.set_text(&text);
        self.set_tooltip(&format!("{} – {text}", crate::APP_NAME));
        self.set_icon(IconState::Busy);
    }

    /// Tüm işler bitince: son işin özeti ve oturum toplamı.
    pub fn show_idle(&self, stats: &SessionStats) {
        let status = match &stats.last {
            Some(last) => format!("✓ {}: {}", last.label, last.summary()),
            None => "Hazır".to_owned(),
        };
        let total = format!(
            "Toplam: {} dosya · {} tasarruf",
            stats.files,
            util::format_bytes(stats.saved_bytes())
        );
        self.status.set_text(&status);
        self.stats.set_text(&total);
        self.open_last
            .set_enabled(stats.last.as_ref().is_some_and(|r| r.output_dir.is_some()));
        self.set_tooltip(&format!("{} {} – {total}", crate::APP_NAME, crate::SIGNATURE));
        self.set_icon(IconState::Idle);
    }

    /// Klasör izleme açılamadıysa onay işaretini geri al.
    pub fn set_watch_checked(&self, on: bool) {
        self.watch_toggle.set_checked(on);
    }

    fn set_tooltip(&self, text: &str) {
        if let Err(e) = self.tray.set_tooltip(Some(text)) {
            log::debug!("tooltip güncellenemedi: {e}");
        }
    }

    fn set_icon(&self, state: IconState) {
        match render_icon(state) {
            Ok(icon) => {
                if let Err(e) = self.tray.set_icon(Some(icon)) {
                    log::debug!("simge güncellenemedi: {e}");
                }
            }
            Err(e) => log::debug!("{e}"),
        }
    }
}

/// Dış dosyaya ihtiyaç duymamak için simge çalışma anında çizilir: referans
/// sayfanın logosundaki gibi mavi → indigo gradyanlı yuvarlak kare ve içinde
/// beyaz, sekiz kollu bir çark. Meşgulken sağ altta yeşil bir durum noktası belirir.
fn render_icon(state: IconState) -> Result<Icon> {
    let pixmap = icon_pixmap(state, 1.0).ok_or_else(|| anyhow!("tepsi simgesi çizilemedi"))?;
    // Pixmap ön-çarpılmış; tray-icon düz (straight) RGBA bekler.
    let rgba = pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    Icon::from_rgba(rgba, pixmap.width(), pixmap.height()).map_err(|e| anyhow!("tepsi simgesi oluşturulamadı: {e:?}"))
}

/// Simge 32x32 tasarım ızgarasında çizilir; `scale` ile büyütülür (README logosu için).
fn icon_pixmap(state: IconState, scale: f32) -> Option<Pixmap> {
    const SIZE: f32 = 32.0;
    let px = (SIZE * scale).round() as u32;
    let mut pixmap = Pixmap::new(px, px)?;
    let identity = Transform::from_scale(scale, scale);

    // `bg-gradient-to-tr from-blue-600 to-indigo-600`, `rounded-xl`
    let shader = LinearGradient::new(
        Point::from_xy(0.0, SIZE),
        Point::from_xy(SIZE, 0.0),
        vec![
            GradientStop::new(0.0, color(theme::BLUE_600)),
            GradientStop::new(1.0, color(theme::INDIGO_600)),
        ],
        SpreadMode::Pad,
        Transform::identity(),
    )?;
    let background = Paint {
        shader,
        anti_alias: true,
        ..Paint::default()
    };
    pixmap.fill_path(
        &rounded_rect(1.0, 1.0, SIZE - 2.0, SIZE - 2.0, 8.0)?,
        &background,
        FillRule::Winding,
        identity,
        None,
    );

    // Sekiz kollu çark.
    let c = SIZE / 2.0;
    let mut spokes = PathBuilder::new();
    for i in 0..8 {
        let a = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::FRAC_PI_4;
        spokes.move_to(c + 6.0 * a.cos(), c + 6.0 * a.sin());
        spokes.line_to(c + 10.5 * a.cos(), c + 10.5 * a.sin());
    }
    let stroke = Stroke {
        width: 3.0,
        line_cap: LineCap::Round,
        ..Stroke::default()
    };
    pixmap.stroke_path(
        &spokes.finish()?,
        &solid(theme::WHITE.with_alpha(240)),
        &stroke,
        identity,
        None,
    );
    pixmap.fill_path(
        &PathBuilder::from_circle(c, c, 2.4)?,
        &solid(theme::WHITE),
        FillRule::Winding,
        identity,
        None,
    );

    if matches!(state, IconState::Busy) {
        let dot = PathBuilder::from_circle(SIZE - 7.0, SIZE - 7.0, 5.5)?;
        pixmap.fill_path(&dot, &solid(theme::BG_BOTTOM), FillRule::Winding, identity, None);
        let dot = PathBuilder::from_circle(SIZE - 7.0, SIZE - 7.0, 4.0)?;
        pixmap.fill_path(&dot, &solid(theme::GREEN_400), FillRule::Winding, identity, None);
    }
    Some(pixmap)
}

fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.quad_to(x + w, y, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.quad_to(x + w, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.quad_to(x, y + h, x, y + h - r);
    pb.line_to(x, y + r);
    pb.quad_to(x, y, x + r, y);
    pb.close();
    pb.finish()
}

fn color(c: theme::Rgba) -> Color {
    Color::from_rgba8(c.0, c.1, c.2, c.3)
}

fn solid(c: theme::Rgba) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color_rgba8(c.0, c.1, c.2, c.3);
    paint.anti_alias = true;
    paint
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `cargo test icon_preview -- --ignored` →
    /// `assets/icon.png` (256 px README logosu) ve `assets/icon.ico` (exe simgesi, build.rs gömer).
    #[test]
    #[ignore = "önizleme PNG'si üretir"]
    fn icon_preview() {
        use image::codecs::ico::{IcoEncoder, IcoFrame};
        use image::ExtendedColorType;

        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        icon_pixmap(IconState::Idle, 8.0)
            .unwrap()
            .save_png(root.join("assets").join("icon.png"))
            .unwrap();

        let frames: Vec<IcoFrame> = [16_u32, 24, 32, 48, 64, 256]
            .iter()
            .map(|&size| {
                let pixmap = icon_pixmap(IconState::Idle, size as f32 / 32.0).unwrap();
                let rgba: Vec<u8> = pixmap
                    .pixels()
                    .iter()
                    .flat_map(|p| {
                        let c = p.demultiply();
                        [c.red(), c.green(), c.blue(), c.alpha()]
                    })
                    .collect();
                IcoFrame::as_png(&rgba, size, size, ExtendedColorType::Rgba8).unwrap()
            })
            .collect();
        let file = std::fs::File::create(root.join("assets").join("icon.ico")).unwrap();
        IcoEncoder::new(file).encode_images(&frames).unwrap();
        icon_pixmap(IconState::Busy, 1.0)
            .unwrap()
            .save_png(root.join("target").join("icon-busy.png"))
            .unwrap();
    }
}
