//! Shift + sürükle çarkı.
//!
//! Kullanıcı Shift'e basılı tutarak bir dosya sürüklediğinde imlecin
//! etrafında hedef formatların dizili olduğu yarı saydam bir çark açılır.
//! Dosya bir dilimin üzerine bırakılırsa o formata dönüştürülür, ortadaki
//! hapın üzerine bırakılırsa mevcut ayarlarla sıkıştırılır.
//!
//! Bu modül platformdan bağımsızdır (geometri, durum, çizim). Pencere, OLE
//! bırakma hedefi ve Shift tetikleyicisi `platform` alt modülündedir.

mod render;

#[cfg(windows)]
#[path = "platform_windows.rs"]
mod platform;

#[cfg(not(windows))]
#[path = "platform_stub.rs"]
mod platform;

use std::f32::consts::{FRAC_PI_2, PI, TAU};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::events::EventSink;
use crate::image_compressor::ImageFormatKind;
use crate::jobs::{self, JobReport};
use crate::util;

pub use render::Renderer;

/// Çark ile ilgili, platform katmanından gelen olaylar. Koordinatlar ekran
/// pikselidir.
#[derive(Debug)]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum WheelEvent {
    /// Shift basılıyken sol tuşla sürükleme başladı.
    Trigger {
        x: i32,
        y: i32,
    },
    /// Sol tuş bırakıldı.
    Released,
    DragEnter {
        files: Vec<PathBuf>,
        x: i32,
        y: i32,
    },
    DragOver {
        x: i32,
        y: i32,
    },
    DragLeave,
    Drop {
        files: Vec<PathBuf>,
        x: i32,
        y: i32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Image(ImageFormatKind),
    Pdf,
}

impl Target {
    pub fn label(self) -> &'static str {
        match self {
            Self::Image(f) => f.label(),
            Self::Pdf => "PDF",
        }
    }
}

/// Saat yönünde, en üstten başlayarak dilimler.
pub const SEGMENTS: [Target; 8] = [
    Target::Image(ImageFormatKind::Jpeg),
    Target::Image(ImageFormatKind::WebP),
    Target::Image(ImageFormatKind::Avif),
    Target::Image(ImageFormatKind::Tiff),
    Target::Image(ImageFormatKind::Png),
    Target::Image(ImageFormatKind::Bmp),
    Target::Pdf,
    Target::Image(ImageFormatKind::Ico),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// Ortadaki hap: mevcut ayarlarla sıkıştır.
    Center,
    Segment(usize),
}

/// Çarkın ölçüleri (mantıksal piksel, `scale` ile fiziksel piksele çevrilir).
#[derive(Debug, Clone, Copy)]
pub struct Geometry {
    pub scale: f32,
}

impl Geometry {
    pub const SIZE: f32 = 460.0;
    /// Yarı saydam diskin yarıçapı.
    pub const DISC: f32 = 206.0;
    pub const RING_OUT: f32 = 194.0;
    pub const RING_IN: f32 = 98.0;
    /// Merkezdeki dairenin yarıçapı.
    pub const HUB: f32 = 84.0;
    /// Dilimler arası boşluk ve köşe yuvarlaklığı.
    pub const GAP: f32 = 9.0;
    pub const ROUNDING: f32 = 14.0;

    pub fn size_px(self) -> u32 {
        (Self::SIZE * self.scale).round() as u32
    }

    pub fn center(self) -> f32 {
        self.size_px() as f32 / 2.0
    }

    /// `i` numaralı dilimin orta açısı (radyan, 0 = sağ, saat yönünde artar).
    pub fn segment_angle(i: usize) -> f32 {
        -FRAC_PI_2 + i as f32 * TAU / SEGMENTS.len() as f32
    }

    pub fn segment_half_width() -> f32 {
        PI / SEGMENTS.len() as f32
    }

    /// Pencere içi koordinatın neye denk geldiği.
    pub fn hit(self, x: f32, y: f32) -> Option<Hit> {
        let c = self.center();
        let (dx, dy) = (x - c, y - c);
        let dist = dx.hypot(dy) / self.scale;
        if dist <= Self::RING_IN - Self::GAP / 2.0 {
            return Some(Hit::Center);
        }
        if dist > Self::DISC {
            return None;
        }
        // Açıyı "en üst dilimin sol kenarı = 0" olacak şekilde kaydır.
        let angle = (dy.atan2(dx) + FRAC_PI_2 + Self::segment_half_width()).rem_euclid(TAU);
        let index = (angle / (TAU / SEGMENTS.len() as f32)) as usize % SEGMENTS.len();
        Some(Hit::Segment(index))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Hidden,
    Open,
    /// Dosya bırakıldı; iş sürüyor.
    Busy {
        job: Option<u64>,
    },
    /// İş bitti; sonuç kısa süre gösterilip çark kapanır.
    Done {
        text: String,
        ok: bool,
    },
}

/// Çizim için gereken her şey; platform katmanından bağımsızdır ve test edilebilir.
#[derive(Debug, Clone)]
pub struct WheelModel {
    pub phase: Phase,
    pub files: Vec<PathBuf>,
    pub total_bytes: u64,
    pub hover: Option<Hit>,
}

impl Default for WheelModel {
    fn default() -> Self {
        Self {
            phase: Phase::Hidden,
            files: Vec::new(),
            total_bytes: 0,
            hover: None,
        }
    }
}

impl WheelModel {
    fn set_files(&mut self, files: Vec<PathBuf>) {
        self.files = files.into_iter().filter(|p| jobs::is_supported(p)).collect();
        self.total_bytes = self
            .files
            .iter()
            .filter_map(|p| fs::metadata(p).ok())
            .map(|m| m.len())
            .sum();
    }

    fn has_images(&self) -> bool {
        self.files.iter().any(|p| jobs::is_image(p))
    }

    /// Dosya bilinmiyorsa (henüz `DragEnter` gelmediyse) her şey etkin görünür.
    pub fn is_enabled(&self, hit: Hit) -> bool {
        match hit {
            Hit::Center => true,
            Hit::Segment(i) => match SEGMENTS[i] {
                Target::Image(_) => self.files.is_empty() || self.has_images(),
                Target::Pdf => true,
            },
        }
    }

    /// Ortadaki hapta gösterilecek metin.
    pub fn hub_text(&self) -> String {
        match &self.phase {
            Phase::Busy { .. } => "İşleniyor…".into(),
            Phase::Done { text, .. } => text.clone(),
            Phase::Hidden | Phase::Open => match self.hover {
                Some(Hit::Center) => "Sıkıştır".into(),
                _ if self.total_bytes > 0 => util::format_bytes(self.total_bytes),
                _ => "Sıkıştır".into(),
            },
        }
    }
}

/// Bırakma sonucunda başlatılacak iş.
#[derive(Debug)]
pub struct DropAction {
    pub target: Option<Target>,
    pub files: Vec<PathBuf>,
}

/// Bırakmadan sonra çarkın açık kalma süreleri.
const RELEASE_GRACE: Duration = Duration::from_millis(300);
const RESULT_LINGER: Duration = Duration::from_millis(1400);

/// Çark denetleyicisi: model + çizici + platform penceresi. Ana iş parçacığında yaşar.
pub struct Wheel {
    overlay: platform::Overlay,
    renderer: Renderer,
    geometry: Geometry,
    model: WheelModel,
    hide_at: Option<Instant>,
}

impl Wheel {
    /// Pencereyi ve bırakma hedefini oluşturur, Shift tetikleyicisini başlatır.
    pub fn new(sink: EventSink, enabled: Arc<AtomicBool>) -> Result<Self> {
        let overlay = platform::Overlay::new(sink.clone())?;
        let geometry = Geometry { scale: overlay.scale() };
        platform::spawn_trigger(sink, enabled)?;
        Ok(Self {
            overlay,
            renderer: Renderer::new(),
            geometry,
            model: WheelModel::default(),
            hide_at: None,
        })
    }

    /// Bırakma bir iş başlatmayı gerektiriyorsa `DropAction` döner.
    pub fn handle(&mut self, event: WheelEvent) -> Option<DropAction> {
        match event {
            WheelEvent::Trigger { x, y } => {
                if matches!(self.model.phase, Phase::Hidden | Phase::Done { .. }) {
                    self.model = WheelModel {
                        phase: Phase::Open,
                        ..WheelModel::default()
                    };
                    self.hide_at = None;
                    self.overlay.show_centered(x, y, &self.draw());
                }
            }
            WheelEvent::Released => {
                if self.model.phase == Phase::Open {
                    // Bırakma (Drop) olayı tuş bırakmadan biraz sonra gelebilir.
                    self.hide_at = Some(Instant::now() + RELEASE_GRACE);
                }
            }
            WheelEvent::DragEnter { files, x, y } => {
                if self.model.phase == Phase::Open {
                    self.model.set_files(files);
                    self.set_hover_at(x, y);
                    self.redraw();
                }
            }
            WheelEvent::DragOver { x, y } => {
                if self.model.phase == Phase::Open && self.set_hover_at(x, y) {
                    self.redraw();
                }
            }
            WheelEvent::DragLeave => {
                if self.model.phase == Phase::Open && self.model.hover.take().is_some() {
                    self.redraw();
                }
            }
            WheelEvent::Drop { files, x, y } => return self.on_drop(files, x, y),
        }
        None
    }

    fn on_drop(&mut self, files: Vec<PathBuf>, x: i32, y: i32) -> Option<DropAction> {
        if self.model.phase != Phase::Open {
            return None;
        }
        self.model.set_files(files);
        self.set_hover_at(x, y);
        let hit = self
            .model
            .hover
            .filter(|&h| self.model.is_enabled(h) && !self.model.files.is_empty());
        let Some(hit) = hit else {
            self.hide();
            return None;
        };
        self.model.phase = Phase::Busy { job: None };
        self.hide_at = None;
        self.redraw();
        Some(DropAction {
            target: match hit {
                Hit::Center => None,
                Hit::Segment(i) => Some(SEGMENTS[i]),
            },
            files: std::mem::take(&mut self.model.files),
        })
    }

    /// `on_drop` sonrası başlatılan işin kimliği (başlatılamadıysa `None`).
    pub fn set_job(&mut self, job: Option<u64>) {
        match job {
            Some(_) => self.model.phase = Phase::Busy { job },
            None => self.finish("Hata".into(), false),
        }
    }

    pub fn job_finished(&mut self, id: u64, report: &JobReport) {
        if self.model.phase == (Phase::Busy { job: Some(id) }) {
            if report.failed == 0 && report.processed > 0 {
                // Yeşil hap başarıyı zaten belirtir; "✓" Segoe UI Bold'da bulunmuyor.
                self.finish(util::format_bytes(report.compressed_bytes), true);
            } else {
                self.finish("Hata".into(), false);
            }
        }
    }

    fn finish(&mut self, text: String, ok: bool) {
        self.model.phase = Phase::Done { text, ok };
        self.hide_at = Some(Instant::now() + RESULT_LINGER);
        self.redraw();
    }

    /// Olay döngüsünün en geç ne zaman uyanması gerektiği.
    pub fn deadline(&self) -> Option<Instant> {
        self.hide_at
    }

    pub fn tick(&mut self, now: Instant) {
        if self.hide_at.is_some_and(|t| t <= now) {
            self.hide();
        }
    }

    fn hide(&mut self) {
        self.hide_at = None;
        self.model = WheelModel::default();
        self.overlay.hide();
    }

    /// Hover değiştiyse `true`.
    fn set_hover_at(&mut self, x: i32, y: i32) -> bool {
        let (lx, ly) = self.overlay.to_local(x, y);
        let hover = self.geometry.hit(lx, ly);
        let changed = hover != self.model.hover;
        self.model.hover = hover;
        changed
    }

    fn draw(&self) -> tiny_skia::Pixmap {
        self.renderer.render(self.geometry, &self.model)
    }

    fn redraw(&mut self) {
        let pixmap = self.draw();
        self.overlay.update(&pixmap);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_test_maps_angles_to_segments() {
        let geo = Geometry { scale: 1.0 };
        let c = geo.center();
        let r = (Geometry::RING_IN + Geometry::RING_OUT) / 2.0;
        assert_eq!(geo.hit(c, c), Some(Hit::Center));
        for i in 0..SEGMENTS.len() {
            let a = Geometry::segment_angle(i);
            assert_eq!(
                geo.hit(c + r * a.cos(), c + r * a.sin()),
                Some(Hit::Segment(i)),
                "dilim {i}"
            );
        }
        assert_eq!(geo.hit(0.0, 0.0), None);
    }

    #[test]
    fn image_targets_disabled_for_pdf_only_drop() {
        let model = WheelModel {
            phase: Phase::Open,
            files: vec![PathBuf::from("belge.pdf")],
            total_bytes: 1,
            hover: None,
        };
        let pdf = SEGMENTS.iter().position(|t| *t == Target::Pdf).unwrap();
        assert!(model.is_enabled(Hit::Segment(pdf)));
        assert!(!model.is_enabled(Hit::Segment(0)));
        assert!(model.is_enabled(Hit::Center));
    }
}
