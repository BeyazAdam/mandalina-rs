//! Windows dışı platformlar: çark henüz desteklenmiyor.
//!
//! macOS için `NSPanel` + `NSDraggingDestination`, Linux için X11/Wayland
//! sürükle-bırak hedefi gerekir. Arayüz `platform_windows.rs` ile aynıdır.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use anyhow::{bail, Result};
use tiny_skia::Pixmap;

use crate::events::EventSink;

pub struct Overlay;

impl Overlay {
    pub fn new(_sink: EventSink) -> Result<Self> {
        bail!("Shift + sürükle çarkı şimdilik yalnızca Windows'ta destekleniyor")
    }

    pub fn scale(&self) -> f32 {
        1.0
    }

    pub fn show_centered(&mut self, _x: i32, _y: i32, _pixmap: &Pixmap) {}

    pub fn hide(&self) {}

    pub fn to_local(&self, x: i32, y: i32) -> (f32, f32) {
        (x as f32, y as f32)
    }

    pub fn update(&self, _pixmap: &Pixmap) {}
}

pub fn spawn_trigger(_sink: EventSink, _enabled: Arc<AtomicBool>) -> Result<()> {
    Ok(())
}
