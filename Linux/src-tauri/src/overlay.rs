//! "Identify screens": a big transparent overlay with the screen number on
//! every monitor of this machine.

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

const SHOW_FOR: std::time::Duration = std::time::Duration::from_millis(3500);

fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn show_identify(app: &AppHandle, number: u32, name: &str) {
    close_identify(app);
    let monitors = app.available_monitors().unwrap_or_default();
    let count = monitors.len().max(1);
    for i in 0..count {
        let label = format!("identify-{i}");
        let url = format!("index.html#/identify?n={number}&name={}", encode(name));
        let mut b = WebviewWindowBuilder::new(app, &label, WebviewUrl::App(url.into()))
            .title("KVM Bridge – Identify")
            .decorations(false)
            .transparent(true)
            .always_on_top(true)
            .skip_taskbar(true)
            .focused(false)
            .resizable(false)
            .shadow(false)
            .visible(false);
        if let Some(m) = monitors.get(i) {
            let scale = m.scale_factor();
            let pos = m.position().to_logical::<f64>(scale);
            let size = m.size().to_logical::<f64>(scale);
            b = b.position(pos.x, pos.y).inner_size(size.width, size.height);
        }
        let win = match b.build() {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!("cannot create identify overlay: {e}");
                continue;
            }
        };
        #[cfg(target_os = "linux")]
        {
            let w = win.clone();
            let _ = app.run_on_main_thread(move || {
                use gtk::prelude::*;
                if let Ok(gw) = w.gtk_window() {
                    // Wayland ignores client positioning, so fullscreen the
                    // overlay on the monitor explicitly.
                    if let Some(screen) = gtk::prelude::GtkWindowExt::screen(&gw) {
                        gw.fullscreen_on_monitor(&screen, i as i32);
                    } else {
                        gw.fullscreen();
                    }
                    // Clicks go through the overlay.
                    gw.set_accept_focus(false);
                }
                let _ = w.show();
            });
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = win.set_fullscreen(true);
            let _ = win.set_ignore_cursor_events(true);
            let _ = win.show();
        }
    }
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(SHOW_FOR);
        close_identify(&app);
    });
}

pub fn close_identify(app: &AppHandle) {
    for (label, w) in app.webview_windows() {
        if label.starts_with("identify-") {
            let _ = w.destroy();
        }
    }
}
