//! Saved configs: named setups (screen arrangement, the computer that
//! controls it, encryption and the settings shared by all computers).
//!
//! Every paired computer keeps a copy of every config in its configs folder
//! (`<config dir>/configs/<name>.kvmb`). Loading one makes it the current
//! setup, which then reaches the other computers like any other change.

use crate::config::CommonSettings;
use crate::configfile::{self, BundleInfo, ConfigBundle};
use crate::crypto::{Cipher, CipherPreference};
use crate::layout::Layout;
use crate::types::{now_ms, os_tag, DeviceId};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Most configs kept in the folder.
pub const MAX_CONFIGS: usize = 100;
/// Deletions remembered so that offline computers do not bring configs back.
pub const MAX_TOMBSTONES: usize = 200;

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConfigId(pub Uuid);

impl ConfigId {
    pub fn new_random() -> ConfigId {
        ConfigId(Uuid::new_v4())
    }
}

impl fmt::Debug for ConfigId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "config:{}", &self.0.simple().to_string()[..8])
    }
}

impl fmt::Display for ConfigId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl std::str::FromStr for ConfigId {
    type Err = uuid::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(s).map(ConfigId)
    }
}

/// A saved setup. It is the same on every computer; the computers in it are
/// the screens of its arrangement.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedConfig {
    pub id: ConfigId,
    pub name: String,
    pub rev: u64,
    pub saved_ms: u64,
    pub saved_by: DeviceId,
    pub layout: Layout,
    pub cipher: CipherPreference,
    pub common: CommonSettings,
}

impl SavedConfig {
    pub fn stamp(&self) -> ConfigStamp {
        ConfigStamp { id: self.id, rev: self.rev, saved_ms: self.saved_ms, saved_by: self.saved_by, deleted: false }
    }

    pub fn has_member(&self, id: DeviceId) -> bool {
        self.layout.contains(id)
    }

    /// Same arrangement, controlling computer and shared settings.
    pub fn matches(&self, layout: &Layout, cipher: CipherPreference, common: &CommonSettings) -> bool {
        cells(&self.layout) == cells(layout) && self.cipher == cipher && &self.common == common
    }
}

/// Where each screen is, and which computer controls them.
fn cells(l: &Layout) -> (Vec<(DeviceId, i32, i32)>, Option<DeviceId>) {
    let n = l.clone().normalized();
    let mut v: Vec<(DeviceId, i32, i32)> = n.slots.iter().map(|s| (s.device, s.col, s.row)).collect();
    v.sort();
    (v, n.controller)
}

/// Version of a config, or of its deletion, used to synchronise copies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigStamp {
    pub id: ConfigId,
    pub rev: u64,
    pub saved_ms: u64,
    pub saved_by: DeviceId,
    pub deleted: bool,
}

impl ConfigStamp {
    pub fn version(&self) -> (u64, u64, DeviceId) {
        (self.rev, self.saved_ms, self.saved_by)
    }

    pub fn is_newer_than(&self, other: &ConfigStamp) -> bool {
        self.version() > other.version()
    }
}

/// A config as stored on this computer.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredConfig {
    pub config: SavedConfig,
    pub path: PathBuf,
}

/// The configs folder.
#[derive(Clone, Debug)]
pub struct Library {
    dir: PathBuf,
}

impl Library {
    pub fn new(dir: PathBuf) -> Library {
        Library { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Every config in the folder, sorted by name. Files that are not
    /// configs (e.g. backups) are skipped.
    pub fn list(&self) -> Vec<StoredConfig> {
        let Ok(rd) = std::fs::read_dir(&self.dir) else { return vec![] };
        let mut out: Vec<StoredConfig> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some(configfile::EXTENSION))
            .filter_map(|p| Library::read_file(&p).ok())
            .collect();
        // Two files with the same config: keep the newest copy.
        out.sort_by(|a, b| b.config.stamp().version().cmp(&a.config.stamp().version()));
        let mut seen = std::collections::HashSet::new();
        out.retain(|c| seen.insert(c.config.id));
        out.sort_by(|a, b| a.config.name.to_lowercase().cmp(&b.config.name.to_lowercase()));
        out
    }

    /// Read a config file (from the folder or anywhere else).
    pub fn read_file(path: &Path) -> Result<StoredConfig, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
        let b = configfile::decode(&bytes, None).map_err(|e| e.to_string())?;
        let config = b.config.ok_or_else(|| "This file is a backup, not a saved config".to_string())?;
        Ok(StoredConfig { config, path: path.to_path_buf() })
    }

    /// Write `config`, replacing any older copy (also under another name).
    pub fn store(&self, config: &SavedConfig, me: DeviceId, my_name: &str) -> Result<PathBuf, String> {
        std::fs::create_dir_all(&self.dir).map_err(|e| format!("Cannot create {}: {e}", self.dir.display()))?;
        let old: Vec<PathBuf> = self.list().into_iter().filter(|c| c.config.id == config.id).map(|c| c.path).collect();
        let base = file_stem(&config.name);
        let mut path = self.dir.join(format!("{base}.{}", configfile::EXTENSION));
        let mut n = 2;
        while path.exists() && !old.contains(&path) {
            path = self.dir.join(format!("{base} ({n}).{}", configfile::EXTENSION));
            n += 1;
        }
        let bundle = ConfigBundle {
            info: BundleInfo {
                created_ms: now_ms(),
                app_version: crate::proto::APP_VERSION.into(),
                source_id: me,
                source_name: my_name.to_string(),
                source_os: os_tag().into(),
                note: String::new(),
            },
            settings: None,
            shared: None,
            layout: None,
            devices: None,
            identity: None,
            config: Some(config.clone()),
        };
        let bytes = configfile::encode(&bundle, None, Cipher::Aes256Gcm).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("kvmb.tmp");
        std::fs::write(&tmp, &bytes).map_err(|e| format!("Cannot write {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("Cannot write {}: {e}", path.display()))?;
        for p in old.into_iter().filter(|p| *p != path) {
            let _ = std::fs::remove_file(p);
        }
        Ok(path)
    }

    pub fn remove(&self, id: ConfigId) -> Result<(), String> {
        for c in self.list().into_iter().filter(|c| c.config.id == id) {
            std::fs::remove_file(&c.path).map_err(|e| format!("Cannot delete {}: {e}", c.path.display()))?;
        }
        Ok(())
    }
}

/// A file name for a config name (no path separators or reserved characters).
fn file_stem(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| if c.is_control() || "/\\:*?\"<>|".contains(c) { '_' } else { c })
        .collect();
    s = s.trim().trim_matches('.').trim().chars().take(80).collect();
    if s.is_empty() {
        "Config".into()
    } else {
        s
    }
}
