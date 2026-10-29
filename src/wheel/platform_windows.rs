//! Windows: katmanlı çark penceresi, OLE bırakma hedefi ve Shift tetikleyicisi.
//!
//! - Pencere `WS_EX_LAYERED` + `UpdateLayeredWindow` ile piksel başına alfa
//!   kullanır. Tamamen saydam pikseller fare ve sürükleme için "yoktur";
//!   böylece yalnızca diskin kendisi bırakma kabul eder.
//! - Pencere etkinleşmez (`WS_EX_NOACTIVATE`), görev çubuğunda görünmez
//!   (`WS_EX_TOOLWINDOW`) ve sürüklenen uygulamadan odak çalmaz.
//! - Bırakma hedefi her zaman `DROPEFFECT_COPY` döndürür. Shift basılıyken
//!   Explorer sürüklemeyi "taşı" olarak başlatır; hedef "taşı"yı kabul etseydi
//!   kaynak dosya silinebilirdi.

use std::cell::Cell;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use std::{mem, ptr, thread};

use anyhow::{Context, Result};
use tiny_skia::Pixmap;
use windows::core::{implement, w, Ref, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, POINTL, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, GetMonitorInfoW, MonitorFromPoint, ReleaseDC,
    SelectObject, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS,
    MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Com::{IDataObject, DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::{
    IDropTarget, IDropTarget_Impl, OleInitialize, RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop, CF_HDROP,
    DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VIRTUAL_KEY, VK_LBUTTON, VK_RBUTTON, VK_SHIFT};
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetSystemMetrics, LoadCursorW, RegisterClassW,
    SetWindowPos, ShowWindow, UpdateLayeredWindow, HWND_TOPMOST, IDC_ARROW, MA_NOACTIVATE, SM_SWAPBUTTON,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_HIDE, SW_SHOWNOACTIVATE, ULW_ALPHA, WM_MOUSEACTIVATE, WNDCLASSW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use super::{Geometry, WheelEvent};
use crate::events::{AppEvent, EventSink};

const CLASS_NAME: PCWSTR = w!("MandalinaWheel");

pub struct Overlay {
    hwnd: HWND,
    size: i32,
    scale: f32,
    origin: (i32, i32),
    // COM nesnesi pencere yaşadığı sürece canlı kalmalı.
    _target: IDropTarget,
}

impl Overlay {
    pub fn new(sink: EventSink) -> Result<Self> {
        // SAFETY: Tüm çağrılar ana (STA) iş parçacığında, geçerli argümanlarla yapılır.
        unsafe {
            // tao OLE'yi zaten başlattıysa S_FALSE döner; bu bir hata değildir.
            if let Err(e) = OleInitialize(None) {
                log::debug!("OleInitialize: {e}");
            }
            let instance = GetModuleHandleW(None).context("modül tanıtıcısı alınamadı")?;
            let class = WNDCLASSW {
                lpfnWndProc: Some(wnd_proc),
                hInstance: instance.into(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                lpszClassName: CLASS_NAME,
                ..Default::default()
            };
            // Uygulama yeniden başlatılmadan ikinci kez çağrılırsa 0 döner; sorun değil.
            RegisterClassW(&class);

            let scale = GetDpiForSystem() as f32 / 96.0;
            let size = Geometry { scale }.size_px() as i32;
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                CLASS_NAME,
                w!("Mandalina"),
                WS_POPUP,
                0,
                0,
                size,
                size,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .context("çark penceresi oluşturulamadı")?;

            let target: IDropTarget = WheelDropTarget {
                sink,
                accepts: Cell::new(false),
            }
            .into();
            RegisterDragDrop(hwnd, &target).context("bırakma hedefi kaydedilemedi")?;

            Ok(Self {
                hwnd,
                size,
                scale,
                origin: (0, 0),
                _target: target,
            })
        }
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Çarkı (x, y) etrafında, bulunduğu monitörün çalışma alanına sığacak şekilde açar.
    pub fn show_centered(&mut self, x: i32, y: i32, pixmap: &Pixmap) {
        let half = self.size / 2;
        let (mut left, mut top) = (x - half, y - half);
        // SAFETY: MONITORINFO doğru boyutla başlatılıyor.
        unsafe {
            let monitor = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO {
                cbSize: mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(monitor, &mut info).as_bool() {
                let work = info.rcWork;
                left = left.clamp(work.left, (work.right - self.size).max(work.left));
                top = top.clamp(work.top, (work.bottom - self.size).max(work.top));
            }
        }
        self.origin = (left, top);
        self.update(pixmap);
        // SAFETY: `hwnd` bu nesneye ait geçerli bir pencere.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }

    pub fn hide(&self) {
        // SAFETY: `hwnd` geçerli.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    pub fn to_local(&self, x: i32, y: i32) -> (f32, f32) {
        ((x - self.origin.0) as f32, (y - self.origin.1) as f32)
    }

    /// Pixmap'i (ön-çarpılmış RGBA) BGRA DIB'e kopyalayıp katmanlı pencereye basar.
    pub fn update(&self, pixmap: &Pixmap) {
        if let Err(e) = self.try_update(pixmap) {
            log::warn!("çark çizilemedi: {e}");
        }
    }

    fn try_update(&self, pixmap: &Pixmap) -> Result<()> {
        let (w, h) = (pixmap.width() as i32, pixmap.height() as i32);
        // SAFETY: DIB tamponu `w * h * 4` bayttır ve yalnızca bu blok içinde,
        // seçili olduğu bellek DC'si yaşarken yazılır. Tüm GDI nesneleri serbest bırakılır.
        unsafe {
            let screen_dc = GetDC(None);
            let mem_dc = CreateCompatibleDC(Some(screen_dc));
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h, // yukarıdan aşağı
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = ptr::null_mut();
            let result = CreateDIBSection(Some(mem_dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)
                .context("DIB oluşturulamadı")
                .and_then(|dib| {
                    let dst = std::slice::from_raw_parts_mut(bits.cast::<u8>(), (w * h * 4) as usize);
                    let (dst, _) = dst.as_chunks_mut::<4>();
                    let (src, _) = pixmap.data().as_chunks::<4>();
                    for (d, &[r, g, b, a]) in dst.iter_mut().zip(src) {
                        *d = [b, g, r, a];
                    }
                    let old = SelectObject(mem_dc, dib.into());
                    let blend = BLENDFUNCTION {
                        BlendOp: AC_SRC_OVER as u8,
                        BlendFlags: 0,
                        SourceConstantAlpha: 255,
                        AlphaFormat: AC_SRC_ALPHA as u8,
                    };
                    let updated = UpdateLayeredWindow(
                        self.hwnd,
                        Some(screen_dc),
                        Some(&POINT {
                            x: self.origin.0,
                            y: self.origin.1,
                        }),
                        Some(&SIZE { cx: w, cy: h }),
                        Some(mem_dc),
                        Some(&POINT { x: 0, y: 0 }),
                        COLORREF(0),
                        Some(&blend),
                        ULW_ALPHA,
                    );
                    SelectObject(mem_dc, old);
                    let _ = DeleteObject(dib.into());
                    updated.context("UpdateLayeredWindow başarısız")
                });
            let _ = DeleteDC(mem_dc);
            ReleaseDC(None, screen_dc);
            result
        }
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        // SAFETY: Pencere ve kayıt bu nesneye aittir.
        unsafe {
            let _ = RevokeDragDrop(self.hwnd);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_MOUSEACTIVATE {
        return LRESULT(MA_NOACTIVATE as isize);
    }
    // SAFETY: İşlenmeyen mesajlar varsayılan pencere yordamına aktarılır.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

// ---------------------------------------------------------------------------
// OLE bırakma hedefi
// ---------------------------------------------------------------------------

#[implement(IDropTarget)]
struct WheelDropTarget {
    sink: EventSink,
    /// Sürüklenen veride dosya var mı (DragOver aynı efekti döndürmeli).
    accepts: Cell<bool>,
}

impl WheelDropTarget {
    fn effect(&self) -> DROPEFFECT {
        if self.accepts.get() {
            DROPEFFECT_COPY
        } else {
            DROPEFFECT_NONE
        }
    }
}

fn set_effect(out: *mut DROPEFFECT, effect: DROPEFFECT) {
    if !out.is_null() {
        // SAFETY: OLE geçerli bir çıktı işaretçisi verir; null olup olmadığı denetlendi.
        unsafe { *out = effect };
    }
}

impl IDropTarget_Impl for WheelDropTarget_Impl {
    fn DragEnter(
        &self,
        data: Ref<'_, IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let files = data.as_ref().map(dropped_files).unwrap_or_default();
        self.accepts.set(!files.is_empty());
        set_effect(effect, self.effect());
        self.sink.send(AppEvent::Wheel(WheelEvent::DragEnter {
            files,
            x: pt.x,
            y: pt.y,
        }));
        Ok(())
    }

    fn DragOver(&self, _keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> windows::core::Result<()> {
        set_effect(effect, self.effect());
        self.sink
            .send(AppEvent::Wheel(WheelEvent::DragOver { x: pt.x, y: pt.y }));
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        self.accepts.set(false);
        self.sink.send(AppEvent::Wheel(WheelEvent::DragLeave));
        Ok(())
    }

    fn Drop(
        &self,
        data: Ref<'_, IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let files = data.as_ref().map(dropped_files).unwrap_or_default();
        // Her zaman "kopyala": kaynak uygulama orijinal dosyayı asla silmemeli.
        set_effect(
            effect,
            if files.is_empty() {
                DROPEFFECT_NONE
            } else {
                DROPEFFECT_COPY
            },
        );
        self.accepts.set(false);
        self.sink.send(AppEvent::Wheel(WheelEvent::Drop {
            files,
            x: pt.x,
            y: pt.y,
        }));
        Ok(())
    }
}

/// `IDataObject` içindeki CF_HDROP dosya listesini okur.
fn dropped_files(data: &IDataObject) -> Vec<PathBuf> {
    let format = FORMATETC {
        cfFormat: CF_HDROP.0,
        ptd: ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    };
    // SAFETY: Ortam (medium) okunduktan sonra ReleaseStgMedium ile serbest bırakılır;
    // DragQueryFileW tampon uzunluğuna göre yazar.
    unsafe {
        let Ok(mut medium) = data.GetData(&format) else {
            return Vec::new();
        };
        let hdrop = HDROP(medium.u.hGlobal.0);
        let count = DragQueryFileW(hdrop, u32::MAX, None);
        let mut files = Vec::with_capacity(count as usize);
        for i in 0..count {
            let len = DragQueryFileW(hdrop, i, None) as usize;
            let mut buf = vec![0u16; len + 1];
            let written = DragQueryFileW(hdrop, i, Some(&mut buf)) as usize;
            files.push(PathBuf::from(OsString::from_wide(&buf[..written])));
        }
        ReleaseStgMedium(&mut medium);
        files
    }
}

// ---------------------------------------------------------------------------
// Shift + sürükle tetikleyicisi
// ---------------------------------------------------------------------------

/// Fare tuşunu ve Shift'i yoklar. Tuş basılıyken imleç eşik kadar hareket
/// ettiyse ve Shift basılıysa çarkı açar. Genel klavye/fare kancası (hook)
/// kurmaz; yalnızca anlık tuş durumunu okur.
pub fn spawn_trigger(sink: EventSink, enabled: Arc<AtomicBool>) -> Result<()> {
    thread::Builder::new()
        .name("cark-tetikleyici".into())
        .spawn(move || trigger_loop(&sink, &enabled))
        .context("çark tetikleyicisi başlatılamadı")?;
    Ok(())
}

fn trigger_loop(sink: &EventSink, enabled: &AtomicBool) {
    const IDLE_POLL: Duration = Duration::from_millis(40);
    const DRAG_POLL: Duration = Duration::from_millis(16);
    const THRESHOLD: i32 = 10;

    let mut press: Option<POINT> = None;
    let mut triggered = false;
    loop {
        thread::sleep(if press.is_some() { DRAG_POLL } else { IDLE_POLL });

        if !key_down(primary_button()) {
            if triggered {
                sink.send(AppEvent::Wheel(WheelEvent::Released));
            }
            press = None;
            triggered = false;
            continue;
        }

        let pos = cursor_pos();
        let origin = *press.get_or_insert(pos);
        let moved = (pos.x - origin.x).abs().max((pos.y - origin.y).abs()) > THRESHOLD;
        if !triggered && moved && key_down(VK_SHIFT) && enabled.load(Ordering::Relaxed) {
            triggered = true;
            sink.send(AppEvent::Wheel(WheelEvent::Trigger { x: pos.x, y: pos.y }));
        }
    }
}

fn key_down(key: VIRTUAL_KEY) -> bool {
    // En üst bit (negatif değer) tuşun şu an basılı olduğunu gösterir.
    // SAFETY: Yan etkisiz durum sorgusu.
    unsafe { GetAsyncKeyState(i32::from(key.0)) < 0 }
}

/// Sol elini kullananlar için tuşları değiştirilmiş fareyi hesaba katar.
fn primary_button() -> VIRTUAL_KEY {
    // SAFETY: Yan etkisiz sistem sorgusu.
    if unsafe { GetSystemMetrics(SM_SWAPBUTTON) } != 0 {
        VK_RBUTTON
    } else {
        VK_LBUTTON
    }
}

fn cursor_pos() -> POINT {
    let mut pos = POINT::default();
    // SAFETY: Geçerli bir POINT işaretçisi verilir.
    unsafe {
        let _ = GetCursorPos(&mut pos);
    }
    pos
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wheel::{Hit, Phase, Renderer, WheelModel};
    use tao::event_loop::EventLoopBuilder;
    use tao::platform::windows::EventLoopBuilderExtWindows;
    use windows::Win32::UI::WindowsAndMessaging::{SM_CXSCREEN, SM_CYSCREEN};

    /// Gerçek katmanlı pencereyi ekranın ortasında 4 sn gösterir:
    /// `cargo test overlay_on_screen -- --ignored`
    #[test]
    #[ignore = "ekranda pencere açar"]
    fn overlay_on_screen() {
        let event_loop = EventLoopBuilder::<AppEvent>::with_user_event()
            .with_any_thread(true)
            .build();
        let sink = EventSink::new(event_loop.create_proxy());
        let mut overlay = Overlay::new(sink).unwrap();
        let model = WheelModel {
            phase: Phase::Open,
            files: vec![PathBuf::from("mandalina.png")],
            total_bytes: 2_300_000,
            hover: Some(Hit::Segment(1)),
        };
        let pixmap = Renderer::new().render(Geometry { scale: overlay.scale() }, &model);
        // SAFETY: Yan etkisiz sistem sorgusu.
        let (w, h) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
        overlay.show_centered(w / 2, h / 2, &pixmap);
        overlay.try_update(&pixmap).unwrap();
        thread::sleep(Duration::from_secs(4));
        overlay.hide();
    }
}
