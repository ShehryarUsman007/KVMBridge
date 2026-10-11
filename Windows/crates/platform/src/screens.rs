//! Monitors, in the coordinates the input functions use.
//!
//! The process is per-monitor DPI aware, so cursor positions, hook
//! coordinates and monitor rectangles are all physical pixels of the virtual
//! screen: the one coordinate space Windows keeps consistent across monitors
//! with different scaling.

use kvmbridge_core::layout::Desktop;
use kvmbridge_core::types::{Point, Rect};
use windows_sys::Win32::Foundation::{LPARAM, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
use windows_sys::Win32::UI::HiDpi::{
    GetAwarenessFromDpiAwarenessContext, GetThreadDpiAwarenessContext, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_PER_MONITOR_AWARE, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;

/// Make positions physical pixels. Must run before any window exists.
pub fn init_dpi() {
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let aware = unsafe { GetAwarenessFromDpiAwarenessContext(GetThreadDpiAwarenessContext()) };
    if aware != DPI_AWARENESS_PER_MONITOR_AWARE {
        tracing::warn!("not per-monitor DPI aware ({aware}); positions may be off on scaled screens");
    }
}

unsafe extern "system" fn collect(m: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> i32 {
    let list = unsafe { &mut *(data as *mut Vec<(bool, Rect)>) };
    let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    if unsafe { GetMonitorInfoW(m, &mut info) } != 0 {
        let r = info.rcMonitor;
        let rect = Rect::new(r.left as f64, r.top as f64, (r.right - r.left) as f64, (r.bottom - r.top) as f64);
        list.push((info.dwFlags & MONITORINFOF_PRIMARY != 0, rect));
    }
    1
}

/// This computer's monitors, the primary one first.
pub fn desktop() -> Desktop {
    let mut list: Vec<(bool, Rect)> = Vec::new();
    unsafe { EnumDisplayMonitors(std::ptr::null_mut(), std::ptr::null(), Some(collect), &mut list as *mut _ as LPARAM) };
    list.sort_by_key(|(primary, _)| !primary);
    Desktop::new(list.into_iter().map(|(_, r)| r).collect())
}

pub fn to_point(p: POINT) -> Point {
    Point::new(p.x as f64, p.y as f64)
}

pub fn to_win(p: Point) -> POINT {
    POINT { x: p.x.round() as i32, y: p.y.round() as i32 }
}

/// The monitor that contains `p` (or the nearest one).
pub fn monitor_at(desk: &Desktop, p: Point) -> Rect {
    let p = desk.clamp(p);
    desk.monitors.iter().copied().find(|m| m.contains(p)).unwrap_or_else(|| desk.bounds())
}
