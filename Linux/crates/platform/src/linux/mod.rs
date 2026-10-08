//! Linux backends. On Wayland everything goes through xdg-desktop-portal
//! (InputCapture, RemoteDesktop + libei, Clipboard), which works on GNOME and
//! KDE Plasma without extra permissions or installed dependencies.

mod capture;
pub mod guard;
mod remote;
pub mod selection;

use kvmbridge_core::platform::*;
use kvmbridge_core::types::ClipFormat;
use std::path::PathBuf;

/// CLOCK_MONOTONIC in microseconds (the libei time base).
pub(crate) fn now_us() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: valid pointer to a timespec.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000 + ts.tv_nsec as u64 / 1000
}

/// Mime types we offer for each clipboard format, best first.
pub(crate) fn clipboard_mimes(f: ClipFormat) -> &'static [&'static str] {
    match f {
        ClipFormat::Text => &["text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "TEXT", "STRING"],
        ClipFormat::Html => &["text/html"],
        ClipFormat::Png => &["image/png"],
        ClipFormat::Files => &["x-special/gnome-copied-files", "text/uri-list"],
    }
}

pub(crate) fn mime_to_format(m: &str) -> Option<ClipFormat> {
    let m = m.trim();
    match m {
        "text/plain;charset=utf-8" | "UTF8_STRING" | "text/plain" | "TEXT" | "STRING" | "text/plain;charset=UTF-8" => {
            Some(ClipFormat::Text)
        }
        "text/html" => Some(ClipFormat::Html),
        "image/png" => Some(ClipFormat::Png),
        "x-special/gnome-copied-files" | "text/uri-list" | "x-special/KDE-copied-files" => Some(ClipFormat::Files),
        _ => None,
    }
}

fn spawn_thread<F, Fut>(name: &str, f: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()>,
{
    let name = name.to_string();
    std::thread::Builder::new()
        .name(name.clone())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime");
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, f());
            tracing::debug!("{name} backend thread finished");
        })
        .expect("spawn backend thread");
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Session {
    Wayland,
    X11,
    Unknown,
}

pub fn session_type() -> Session {
    match std::env::var("XDG_SESSION_TYPE").ok().as_deref() {
        Some("wayland") => Session::Wayland,
        Some("x11") => Session::X11,
        _ if std::env::var_os("WAYLAND_DISPLAY").is_some() => Session::Wayland,
        _ if std::env::var_os("DISPLAY").is_some() => Session::X11,
        _ => Session::Unknown,
    }
}

pub(crate) fn start(sides: PlatformSides, state_dir: PathBuf) -> PlatformSides2 {
    let PlatformSides { capture, emulation, clipboard, dnd } = sides;
    match session_type() {
        Session::Wayland => {
            // libei streams are not `Send`; give each backend its own
            // single-threaded runtime (also keeps input latency independent
            // of the rest of the app).
            let token = state_dir.join("remote-desktop.token");
            let guard = guard::Guard::spawn(capture.events.clone());
            spawn_thread("kvmb-capture", move || capture::run(capture, guard));
            spawn_thread("kvmb-remote", move || remote::run(emulation, clipboard, token));
        }
        other => {
            let why = match other {
                Session::X11 => "X11 sessions are not supported yet; please log in with a Wayland session",
                _ => "No graphical session detected",
            };
            capture.unavailable(why, CaptureEvent::Status);
            emulation.unavailable(why, EmulationEvent::Status);
            clipboard.unavailable(why, ClipboardEvent::Status);
        }
    }
    PlatformSides2 { dnd }
}

/// Sides that the caller may still want to wire up (drag-and-drop needs the
/// GUI toolkit's main thread).
pub struct PlatformSides2 {
    pub dnd: BackendSide<DndCommand, DndEvent>,
}
