#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod backup;
mod commands;
#[cfg(target_os = "linux")]
mod dnd_linux;
mod logging;
mod overlay;
mod tray;

use kvmbridge_core::config::Paths;
use kvmbridge_core::platform::platform_channels;
#[cfg(not(target_os = "linux"))]
use kvmbridge_core::platform::DndEvent;
use kvmbridge_core::{Command, EngineHandle, Notice, Overrides};
use tauri::{AppHandle, Emitter, Manager, RunEvent, WindowEvent};

pub struct AppState {
    pub engine: EngineHandle,
    pub logs: logging::LogRing,
    pub paths: Paths,
    /// A `.kvmb` file to offer for import once the UI is up.
    pub pending_file: parking_lot::Mutex<Option<String>>,
}

pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

pub fn quit(app: &AppHandle) {
    if let Some(s) = app.try_state::<AppState>() {
        s.engine.send(Command::Shutdown);
    }
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(300));
        app.exit(0);
    });
}

fn close_to_background(app: &AppHandle) -> bool {
    app.try_state::<AppState>().is_some_and(|s| s.engine.snapshot().settings.close_to_background)
}

fn forward_events(app: AppHandle, engine: EngineHandle) {
    let mut snap = engine.watch();
    let a = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut enabled = None;
        while snap.changed().await.is_ok() {
            let s = snap.borrow_and_update().clone();
            if enabled != Some(s.enabled) {
                enabled = Some(s.enabled);
                tray::set_enabled(&a, s.enabled);
            }
            let _ = a.emit("snapshot", &s);
        }
    });
    let mut notices = engine.notices();
    tauri::async_runtime::spawn(async move {
        loop {
            match notices.recv().await {
                Ok(n) => {
                    match &n {
                        Notice::Identify { number, name } => overlay::show_identify(&app, *number, name),
                        Notice::PairingRequest { .. } => show_main(&app),
                        _ => {}
                    }
                    let _ = app.emit("notice", &n);
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    });
}

/// Hands the command line to a KVM Bridge that is already running, through
/// the D-Bus object of the single-instance plugin. This happens before
/// anything else starts, so a second launch does not touch the log, the
/// network ports or the desktop permissions. Returns true if one took it.
#[cfg(target_os = "linux")]
fn forward_to_running_instance(args: &[String]) -> bool {
    // Must match the identifier in tauri.conf.json.
    const NAME: &str = "io.kvmbridge.app.SingleInstance";
    const PATH: &str = "/io/kvmbridge/app/SingleInstance";
    let Ok(conn) = zbus::blocking::Connection::session() else { return false };
    let cwd = std::env::current_dir().ok().and_then(|d| d.to_str().map(String::from)).unwrap_or_default();
    conn.call_method(Some(NAME), PATH, Some("org.SingleInstance.DBus"), "ExecuteCallback", &(args, cwd)).is_ok()
}

#[cfg(not(target_os = "linux"))]
fn forward_to_running_instance(_args: &[String]) -> bool {
    false
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--home") {
        if let Some(dir) = args.get(i + 1) {
            std::env::set_var("KVMBRIDGE_HOME", dir);
        }
    }
    // A separate profile (--home / KVMBRIDGE_HOME) may run next to the default one.
    if std::env::var_os("KVMBRIDGE_HOME").is_none() && forward_to_running_instance(&args) {
        return;
    }
    let minimized_flag = args.iter().any(|a| a == "--minimized") && backup::config_file_arg(&args).is_none();
    let paths = Paths::discover();
    let logs = logging::init(&paths.log_dir());
    tracing::info!("KVM Bridge {} starting (config in {})", env!("CARGO_PKG_VERSION"), paths.config_dir.display());

    let (platform, sides) = platform_channels();
    let started = tauri::async_runtime::block_on({
        let paths = paths.clone();
        async move {
            let rest = kvmbridge_platform::start(sides, paths.config_dir.clone());
            let engine = kvmbridge_core::start(paths, platform, Overrides::default()).await;
            engine.map(|e| (e, rest))
        }
    });
    let (engine, rest) = match started {
        Ok(x) => x,
        Err(e) => {
            tracing::error!("cannot start: {e:#}");
            eprintln!("KVM Bridge could not start: {e:#}");
            std::process::exit(1);
        }
    };
    let start_minimized = minimized_flag || engine.snapshot().settings.start_minimized;
    let mut dnd_side = Some(rest.dnd);

    let mut builder = tauri::Builder::default();
    // A separate profile (--home / KVMBRIDGE_HOME) may run next to the default one.
    if std::env::var_os("KVMBRIDGE_HOME").is_none() {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            show_main(app);
            if let Some(f) = backup::config_file_arg(&args) {
                let _ = app.emit("open-config", f);
            }
        }));
    }
    let app = builder
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .manage(AppState {
            engine: engine.clone(),
            logs,
            paths,
            pending_file: parking_lot::Mutex::new(backup::config_file_arg(&args)),
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_snapshot,
            commands::set_settings,
            commands::set_cipher,
            commands::set_controller,
            commands::set_enabled,
            commands::setup_input_pause,
            commands::retry_input_pause,
            commands::test_drag,
            commands::save_config,
            commands::rename_config,
            commands::load_config,
            commands::delete_config,
            commands::import_config_file,
            commands::export_config_file,
            commands::rescan_configs,
            commands::open_configs_dir,
            commands::set_layout,
            commands::pair,
            commands::accept_pair,
            commands::reject_pair,
            commands::unpair,
            commands::connect_address,
            commands::identify,
            commands::search,
            commands::send_files,
            commands::cancel_transfer,
            commands::release_control,
            commands::set_locked,
            commands::restart_backends,
            commands::get_logs,
            commands::receive_dir,
            commands::open_receive_dir,
            commands::reveal_path,
            commands::app_info,
            commands::quit,
            backup::config_export,
            backup::config_inspect,
            backup::config_import,
            backup::pending_config_file,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            if let Err(e) = tray::create(&handle, engine.snapshot().enabled) {
                tracing::info!("no tray icon available: {e}");
            }
            forward_events(handle.clone(), engine.clone());
            let dnd = dnd_side.take().expect("setup runs once");
            #[cfg(target_os = "linux")]
            dnd_linux::start(&handle, dnd);
            #[cfg(not(target_os = "linux"))]
            tauri::async_runtime::spawn(async move {
                dnd.unavailable("Drag and drop between screens is not available on this system yet", DndEvent::Status);
            });
            if !start_minimized {
                show_main(&handle);
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" && close_to_background(window.app_handle()) {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building the application");

    app.run(|app, event| {
        if let RunEvent::ExitRequested { api, code, .. } = event {
            // Closing the last window keeps the app running in the background.
            if code.is_none() && close_to_background(app) {
                api.prevent_exit();
            }
        }
    });
}

