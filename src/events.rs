//! Arka plan iş parçacıklarından ana olay döngüsüne giden mesajlar.
//!
//! Tepsi menü öğeleri `!Send` olduğundan (Windows'ta `Rc` tabanlı) yalnızca
//! ana iş parçacığında güncellenebilir. Bu nedenle işçiler menüye asla doğrudan
//! dokunmaz; `EventSink` üzerinden olay gönderir, ana döngü menüyü günceller.

use std::sync::{Arc, Mutex, PoisonError};

use tao::event_loop::EventLoopProxy;
use tray_icon::menu::MenuEvent;

use crate::jobs::JobReport;
use crate::wheel::WheelEvent;

#[derive(Debug)]
pub enum AppEvent {
    /// Tepsi menüsünde bir öğeye tıklandı.
    Menu(MenuEvent),
    JobStarted {
        id: u64,
        label: String,
        total: usize,
    },
    JobProgress {
        id: u64,
        done: usize,
        total: usize,
    },
    JobFinished {
        id: u64,
        report: JobReport,
    },
    /// Shift + sürükle çarkından gelen olaylar (yalnızca Windows'ta üretilir).
    #[cfg_attr(not(windows), allow(dead_code))]
    Wheel(WheelEvent),
    Error(String),
}

/// `EventLoopProxy` için iş parçacıkları arası paylaşılabilir sarmalayıcı.
/// `Mutex` sayesinde `Sync` olur; böylece `rayon` kapanışlarında `&EventSink`
/// olarak kullanılabilir.
#[derive(Clone)]
pub struct EventSink(Arc<Mutex<EventLoopProxy<AppEvent>>>);

impl EventSink {
    pub fn new(proxy: EventLoopProxy<AppEvent>) -> Self {
        Self(Arc::new(Mutex::new(proxy)))
    }

    pub fn send(&self, event: AppEvent) {
        let proxy = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        // Olay döngüsü kapandıysa (uygulama çıkıyor) olay sessizce düşer.
        let _ = proxy.send_event(event);
    }
}
