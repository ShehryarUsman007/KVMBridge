//! System tray / status area icon.

use crate::AppState;
use kvmbridge_core::Command;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};

/// The on/off menu entry (its text follows the state).
pub struct Toggle(pub MenuItem<Wry>);

fn toggle_text(enabled: bool) -> &'static str {
    if enabled {
        "Turn KVM Bridge off"
    } else {
        "Turn KVM Bridge on"
    }
}

pub fn set_enabled(app: &AppHandle, enabled: bool) {
    if let Some(t) = app.try_state::<Toggle>() {
        let _ = t.0.set_text(toggle_text(enabled));
    }
}

pub fn create(app: &AppHandle, enabled: bool) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Open KVM Bridge", true, None::<&str>)?;
    let toggle = MenuItem::with_id(app, "toggle", toggle_text(enabled), true, None::<&str>)?;
    let identify = MenuItem::with_id(app, "identify", "Identify screens", true, None::<&str>)?;
    let release = MenuItem::with_id(app, "release", "Bring the cursor back here", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit KVM Bridge", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&show, &toggle, &identify, &release, &sep, &quit])?;
    app.manage(Toggle(toggle));
    let mut b = TrayIconBuilder::with_id("main")
        .tooltip("KVM Bridge")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, ev| match ev.id.as_ref() {
            "show" => crate::show_main(app),
            "toggle" => {
                if let Some(s) = app.try_state::<AppState>() {
                    let on = s.engine.snapshot().enabled;
                    s.engine.send(Command::SetEnabled(!on));
                }
            }
            "identify" => {
                if let Some(s) = app.try_state::<AppState>() {
                    s.engine.send(Command::Identify);
                }
            }
            "release" => {
                if let Some(s) = app.try_state::<AppState>() {
                    s.engine.send(Command::ReleaseControl);
                }
            }
            "quit" => crate::quit(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, ev| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
                crate::show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        b = b.icon(icon.clone());
    }
    b.build(app)?;
    Ok(())
}
