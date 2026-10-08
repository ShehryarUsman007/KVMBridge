//! Commands invoked from the TypeScript frontend.

use crate::AppState;
use kvmbridge_core::config::Settings;
use kvmbridge_core::library::ConfigId;
use kvmbridge_core::crypto::CipherPreference;
use kvmbridge_core::layout::Layout;
use kvmbridge_core::types::DeviceId;
use kvmbridge_core::{Command, Snapshot};
use std::path::PathBuf;
use tauri::State;

type R<T = ()> = Result<T, String>;

fn id(s: &str) -> R<DeviceId> {
    s.parse().map_err(|_| format!("invalid device id {s:?}"))
}

#[tauri::command]
pub fn get_snapshot(state: State<'_, AppState>) -> Snapshot {
    state.engine.snapshot()
}

#[tauri::command]
pub async fn set_settings(state: State<'_, AppState>, settings: Settings) -> R {
    state.engine.request(Command::SetSettings(Box::new(settings))).await
}

#[tauri::command]
pub async fn set_cipher(state: State<'_, AppState>, cipher: CipherPreference) -> R {
    state.engine.request(Command::SetCipher(cipher)).await
}

fn config_id(s: &str) -> R<ConfigId> {
    s.parse().map_err(|_| format!("invalid config id {s:?}"))
}

/// The computer whose keyboard and mouse control all screens (`None`: any).
#[tauri::command]
pub async fn set_controller(state: State<'_, AppState>, device: Option<String>) -> R {
    let d = device.as_deref().map(id).transpose()?;
    state.engine.request(Command::SetController(d)).await
}

#[tauri::command]
pub async fn set_enabled(state: State<'_, AppState>, enabled: bool) -> R {
    state.engine.request(Command::SetEnabled(enabled)).await
}

/// One-time setup (asks for an administrator password) so this computer's
/// keyboard and mouse can be paused while another computer controls it.
#[tauri::command]
pub async fn setup_input_pause(state: State<'_, AppState>) -> R {
    tauri::async_runtime::spawn_blocking(kvmbridge_platform::setup_input_pause).await.map_err(|e| e.to_string())??;
    // Try pausing again now that the helper is there.
    state.engine.request(Command::RetryInputPause).await
}

/// Drag a test file from this computer; it drops after a few seconds.
#[tauri::command]
pub async fn test_drag(state: State<'_, AppState>) -> R {
    state.engine.request(Command::TestDrag { seconds: 6 }).await
}

#[tauri::command]
pub async fn retry_input_pause(state: State<'_, AppState>) -> R {
    state.engine.request(Command::RetryInputPause).await
}

#[tauri::command]
pub async fn save_config(state: State<'_, AppState>, name: String, id: Option<String>) -> R {
    let id = id.as_deref().map(config_id).transpose()?;
    state.engine.request(Command::SaveConfig { name, id }).await
}

#[tauri::command]
pub async fn rename_config(state: State<'_, AppState>, id: String, name: String) -> R {
    state.engine.request(Command::RenameConfig { id: config_id(&id)?, name }).await
}

#[tauri::command]
pub async fn load_config(state: State<'_, AppState>, id: String) -> R {
    state.engine.request(Command::LoadConfig(config_id(&id)?)).await
}

#[tauri::command]
pub async fn delete_config(state: State<'_, AppState>, id: String) -> R {
    state.engine.request(Command::DeleteConfig(config_id(&id)?)).await
}

#[tauri::command]
pub async fn import_config_file(state: State<'_, AppState>, path: String) -> R {
    state.engine.request(Command::ImportConfigFile(PathBuf::from(path))).await
}

/// Save a copy of a config somewhere else (e.g. to move it by USB stick).
#[tauri::command]
pub fn export_config_file(state: State<'_, AppState>, id: String, path: String) -> R<String> {
    let id = config_id(&id)?;
    let snap = state.engine.snapshot();
    let c = snap.configs.iter().find(|c| c.id == id).ok_or("That config no longer exists")?;
    let mut dest = PathBuf::from(path);
    if dest.extension().and_then(|e| e.to_str()) != Some(kvmbridge_core::configfile::EXTENSION) {
        dest.set_extension(kvmbridge_core::configfile::EXTENSION);
    }
    std::fs::copy(&c.file, &dest).map_err(|e| format!("Cannot write {}: {e}", dest.display()))?;
    Ok(dest.display().to_string())
}

#[tauri::command]
pub async fn rescan_configs(state: State<'_, AppState>) -> R {
    state.engine.request(Command::RescanConfigs).await
}

#[tauri::command]
pub fn open_configs_dir(state: State<'_, AppState>) -> R {
    let dir = state.paths.configs_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    tauri_plugin_opener::open_path(dir, None::<&str>).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_layout(state: State<'_, AppState>, layout: Layout) -> R {
    state.engine.request(Command::SetLayout(layout)).await
}

#[tauri::command]
pub async fn pair(state: State<'_, AppState>, device: String) -> R {
    state.engine.request(Command::Pair(id(&device)?)).await
}

#[tauri::command]
pub async fn accept_pair(state: State<'_, AppState>, device: String) -> R {
    state.engine.request(Command::AcceptPair(id(&device)?)).await
}

#[tauri::command]
pub async fn reject_pair(state: State<'_, AppState>, device: String) -> R {
    state.engine.request(Command::RejectPair(id(&device)?)).await
}

#[tauri::command]
pub async fn unpair(state: State<'_, AppState>, device: String) -> R {
    state.engine.request(Command::Unpair(id(&device)?)).await
}

#[tauri::command]
pub async fn connect_address(state: State<'_, AppState>, address: String) -> R {
    state.engine.request(Command::ConnectAddress(address)).await
}

#[tauri::command]
pub async fn identify(state: State<'_, AppState>) -> R {
    state.engine.request(Command::Identify).await
}

#[tauri::command]
pub async fn search(state: State<'_, AppState>) -> R {
    state.engine.request(Command::Search).await
}

#[tauri::command]
pub async fn send_files(state: State<'_, AppState>, device: String, paths: Vec<String>) -> R {
    let paths = paths.into_iter().map(PathBuf::from).collect();
    state.engine.request(Command::SendFiles { to: id(&device)?, paths }).await
}

#[tauri::command]
pub async fn cancel_transfer(state: State<'_, AppState>, id: u64) -> R {
    state.engine.request(Command::CancelTransfer(id)).await
}

#[tauri::command]
pub async fn release_control(state: State<'_, AppState>) -> R {
    state.engine.request(Command::ReleaseControl).await
}

#[tauri::command]
pub async fn set_locked(state: State<'_, AppState>, locked: bool) -> R {
    state.engine.request(Command::SetLocked(locked)).await
}

#[tauri::command]
pub async fn restart_backends(state: State<'_, AppState>) -> R {
    state.engine.request(Command::RestartBackends).await
}

#[tauri::command]
pub fn get_logs(state: State<'_, AppState>) -> Vec<String> {
    state.logs.lines()
}

#[tauri::command]
pub fn receive_dir(state: State<'_, AppState>) -> String {
    let s = state.engine.snapshot();
    s.settings
        .transfers
        .receive_dir
        .unwrap_or_else(|| state.paths.default_receive_dir.clone())
        .display()
        .to_string()
}

#[tauri::command]
pub fn open_receive_dir(state: State<'_, AppState>) -> R {
    let dir = PathBuf::from(receive_dir(state));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    tauri_plugin_opener::open_path(dir, None::<&str>).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn reveal_path(path: String) -> R {
    tauri_plugin_opener::reveal_item_in_dir(PathBuf::from(path)).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn app_info(state: State<'_, AppState>) -> serde_json::Value {
    serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "config_dir": state.paths.config_dir.display().to_string(),
        "log_dir": state.paths.log_dir().display().to_string(),
        "os": std::env::consts::OS,
        "session": session_kind(),
    })
}

fn session_kind() -> String {
    #[cfg(target_os = "linux")]
    {
        format!("{:?}", kvmbridge_platform::linux::session_type())
    }
    #[cfg(not(target_os = "linux"))]
    {
        std::env::consts::OS.to_string()
    }
}

#[tauri::command]
pub fn quit(app: tauri::AppHandle, state: State<'_, AppState>) {
    state.engine.send(Command::Shutdown);
    let app2 = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(300));
        app2.exit(0);
    });
}
