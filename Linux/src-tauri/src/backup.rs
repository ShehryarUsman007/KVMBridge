//! Export / import of `.kvmb` configuration files.

use crate::AppState;
use kvmbridge_core::config;
use kvmbridge_core::configfile::{self, BundleError, BundleInfo, ConfigBundle, IdentityBackup};
use kvmbridge_core::engine::ImportPlan;
use kvmbridge_core::types::now_ms;
use kvmbridge_core::Command;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::{AppHandle, State};

#[derive(Debug, Deserialize)]
pub struct ExportOptions {
    pub settings: bool,
    pub layout: bool,
    pub devices: bool,
    pub identity: bool,
    pub password: Option<String>,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Deserialize)]
pub struct ImportOptions {
    pub settings: bool,
    pub layout: bool,
    pub devices: bool,
    pub identity: bool,
}

#[derive(Debug, Serialize)]
pub struct DeviceSummary {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct Inspection {
    pub encrypted: bool,
    pub needs_password: bool,
    pub has_identity: bool,
    pub info: Option<BundleInfo>,
    pub has_settings: bool,
    pub has_shared: bool,
    pub layout_screens: Option<usize>,
    pub devices: Vec<DeviceSummary>,
    /// The file was made on this very computer.
    pub from_this_device: bool,
}

fn read(path: &str) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("Cannot read {path}: {e}"))
}

#[tauri::command]
pub async fn config_export(state: State<'_, AppState>, path: String, options: ExportOptions) -> Result<String, String> {
    let st = state.engine.state().await?;
    let identity = if options.identity {
        let id = config::load_identity(&state.paths).map_err(|e| e.to_string())?;
        Some(IdentityBackup { device_id: st.device_id, private_key: id.private })
    } else {
        None
    };
    let bundle = ConfigBundle {
        info: BundleInfo {
            created_ms: now_ms(),
            app_version: env!("CARGO_PKG_VERSION").into(),
            source_id: st.device_id,
            source_name: st.settings.device_name.clone(),
            source_os: std::env::consts::OS.into(),
            note: options.note.chars().take(500).collect(),
        },
        settings: options.settings.then(|| st.settings.clone()),
        shared: options.settings.then(|| st.shared.clone()),
        layout: options.layout.then(|| st.layout.clone()),
        devices: options.devices.then(|| st.trusted.clone()),
        identity,
    };
    let bytes = configfile::encode(&bundle, options.password.as_deref(), st.shared.cipher.resolve()).map_err(|e| e.to_string())?;
    let mut path = PathBuf::from(path);
    if path.extension().and_then(|e| e.to_str()) != Some(configfile::EXTENSION) {
        path.set_extension(configfile::EXTENSION);
    }
    let tmp = path.with_extension("kvmb.tmp");
    std::fs::write(&tmp, &bytes).map_err(|e| format!("Cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("Cannot write {}: {e}", path.display()))?;
    tracing::info!("exported configuration to {}", path.display());
    Ok(path.display().to_string())
}

#[tauri::command]
pub async fn config_inspect(state: State<'_, AppState>, path: String, password: Option<String>) -> Result<Inspection, String> {
    let bytes = read(&path)?;
    let hdr = configfile::peek(&bytes).map_err(|e| e.to_string())?;
    let me = state.engine.snapshot().me.id;
    match configfile::decode(&bytes, password.as_deref()) {
        Ok(b) => Ok(Inspection {
            encrypted: hdr.encrypted,
            needs_password: false,
            has_identity: b.identity.is_some(),
            from_this_device: b.info.source_id == me,
            has_settings: b.settings.is_some(),
            has_shared: b.shared.is_some(),
            layout_screens: b.layout.as_ref().map(|l| l.slots.len()),
            devices: b
                .devices
                .clone()
                .unwrap_or_default()
                .into_iter()
                .map(|d| DeviceSummary { id: d.id.to_string(), name: d.name })
                .collect(),
            info: Some(b.info),
        }),
        Err(BundleError::PasswordRequired) => Ok(Inspection {
            encrypted: true,
            needs_password: true,
            has_identity: hdr.has_identity,
            info: None,
            has_settings: false,
            has_shared: false,
            layout_screens: None,
            devices: vec![],
            from_this_device: false,
        }),
        Err(e) => Err(e.to_string()),
    }
}

#[derive(Debug, Serialize)]
pub struct ImportResult {
    pub restarting: bool,
}

#[tauri::command]
pub async fn config_import(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
    password: Option<String>,
    options: ImportOptions,
) -> Result<ImportResult, String> {
    let bytes = read(&path)?;
    let b = configfile::decode(&bytes, password.as_deref()).map_err(|e| e.to_string())?;
    let identity = if options.identity { b.identity.clone() } else { None };
    let plan = ImportPlan {
        source_device: b.info.source_id,
        restoring_identity: identity.is_some(),
        settings: if options.settings { b.settings.clone() } else { None },
        shared: if options.settings { b.shared.clone() } else { None },
        layout: if options.layout { b.layout.clone() } else { None },
        devices: if options.devices { b.devices.clone() } else { None },
    };
    state.engine.request(Command::ImportConfig(Box::new(plan))).await?;
    tracing::info!("imported configuration from {path}");
    if let Some(id) = identity {
        if id.device_id != state.engine.snapshot().me.id || options.identity {
            state
                .engine
                .request(Command::ReplaceIdentity { device_id: id.device_id, private_key: id.private_key })
                .await?;
            state.engine.send(Command::Shutdown);
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(600));
                app.restart();
            });
            return Ok(ImportResult { restarting: true });
        }
    }
    Ok(ImportResult { restarting: false })
}

/// A `.kvmb` file passed on the command line (double-clicked in a file manager).
#[tauri::command]
pub fn pending_config_file(state: State<'_, AppState>) -> Option<String> {
    state.pending_file.lock().take()
}

pub fn config_file_arg(args: &[String]) -> Option<String> {
    args.iter()
        .skip(1)
        .find(|a| a.to_ascii_lowercase().ends_with(".kvmb") && std::path::Path::new(a).is_file())
        .cloned()
}
