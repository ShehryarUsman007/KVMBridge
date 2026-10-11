//! User settings and persisted state (config file + identity key).

use crate::crypto::{CipherPreference, Identity};
use crate::layout::Layout;
use crate::library::{ConfigId, ConfigStamp};
use crate::types::{keys, DeviceId};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const DEFAULT_DISCOVERY_PORT: u16 = 47800;
/// Most computers that can share one keyboard and mouse (this one included).
pub const MAX_DEVICES: usize = 5;
pub const DEFAULT_LISTEN_PORT: u16 = 47801;

/// Timing knobs: the trade-off between responsiveness and resource usage.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Timing {
    /// Mouse motion is coalesced and flushed at most once per this many
    /// microseconds. 0 sends every motion event immediately.
    pub motion_interval_us: u32,
    /// How often connected devices exchange heartbeats.
    pub heartbeat_ms: u32,
    /// Heartbeats that may be missed before a device is considered gone.
    pub heartbeat_misses: u32,
    /// No longer used: devices are only searched for on request. Kept so
    /// settings stay readable by older versions.
    pub discovery_ms: u32,
    /// Clipboard polling interval for platforms without change notifications.
    pub clipboard_poll_ms: u32,
}

impl Default for Timing {
    fn default() -> Self {
        Timing::preset(TimingPreset::Balanced)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TimingPreset {
    LowLatency,
    Balanced,
    PowerSaver,
}

impl Timing {
    pub fn preset(p: TimingPreset) -> Timing {
        match p {
            TimingPreset::LowLatency => Timing {
                motion_interval_us: 0,
                heartbeat_ms: 500,
                heartbeat_misses: 6,
                discovery_ms: 1000,
                clipboard_poll_ms: 250,
            },
            TimingPreset::Balanced => Timing {
                motion_interval_us: 4_000,
                heartbeat_ms: 1000,
                heartbeat_misses: 5,
                discovery_ms: 2000,
                clipboard_poll_ms: 500,
            },
            TimingPreset::PowerSaver => Timing {
                motion_interval_us: 16_000,
                heartbeat_ms: 3000,
                heartbeat_misses: 4,
                discovery_ms: 5000,
                clipboard_poll_ms: 1500,
            },
        }
    }

    /// Clamp values into sane ranges.
    pub fn sanitized(mut self) -> Timing {
        self.motion_interval_us = self.motion_interval_us.min(100_000);
        self.heartbeat_ms = self.heartbeat_ms.clamp(100, 30_000);
        self.heartbeat_misses = self.heartbeat_misses.clamp(2, 30);
        self.discovery_ms = self.discovery_ms.clamp(250, 60_000);
        self.clipboard_poll_ms = self.clipboard_poll_ms.clamp(100, 10_000);
        self
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipboardSettings {
    pub enabled: bool,
    /// Share copied files (they are transferred when pasted).
    pub files: bool,
    /// Largest clipboard payload (text/image) that is synchronised.
    pub max_bytes: u64,
}

impl Default for ClipboardSettings {
    fn default() -> Self {
        ClipboardSettings { enabled: true, files: true, max_bytes: 64 * 1024 * 1024 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TransferSettings {
    /// Where received files are stored. `None` = `<Downloads>/KVMBridge`.
    pub receive_dir: Option<PathBuf>,
    /// Enable dragging files between screens.
    pub drag_and_drop: bool,
}

impl Default for TransferSettings {
    fn default() -> Self {
        TransferSettings { receive_dir: None, drag_and_drop: true }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hotkeys {
    /// Pressing all of these (evdev codes, left/right variants are treated
    /// alike) returns the cursor to this device.
    pub release: Vec<u32>,
    /// Toggle that keeps the cursor on the current screen.
    pub lock: Vec<u32>,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Hotkeys {
            release: vec![keys::KEY_LEFTCTRL, keys::KEY_LEFTALT, keys::KEY_LEFTSHIFT, keys::KEY_ESC],
            lock: vec![keys::KEY_SCROLLLOCK],
        }
    }
}

/// Colour scheme of the user interface.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    /// Follow the operating system.
    #[default]
    System,
    Light,
    Dark,
}

/// This device's settings. The parts in [`CommonSettings`] are the same on
/// every paired device: changing them here changes them everywhere.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub device_name: String,
    pub listen_port: u16,
    pub discovery_port: u16,
    pub discovery_enabled: bool,
    /// Extra peers to contact directly (host or host:port), for networks that
    /// block broadcast.
    pub manual_peers: Vec<String>,
    pub timing: Timing,
    pub clipboard: ClipboardSettings,
    pub transfers: TransferSettings,
    pub hotkeys: Hotkeys,
    /// Only accept connections that use the configured cipher.
    pub strict_cipher: bool,
    pub start_minimized: bool,
    /// Keep running in the background when the window is closed.
    pub close_to_background: bool,
    pub appearance: Appearance,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            device_name: default_device_name(),
            listen_port: DEFAULT_LISTEN_PORT,
            discovery_port: DEFAULT_DISCOVERY_PORT,
            discovery_enabled: true,
            manual_peers: vec![],
            timing: Timing::default(),
            clipboard: ClipboardSettings::default(),
            transfers: TransferSettings::default(),
            hotkeys: Hotkeys::default(),
            strict_cipher: false,
            start_minimized: false,
            close_to_background: true,
            appearance: Appearance::System,
        }
    }
}

pub fn default_device_name() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "My computer".to_string())
}

/// The parts of [`Settings`] that all paired devices share.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CommonSettings {
    pub strict_cipher: bool,
    pub timing: Timing,
    pub clipboard: ClipboardSettings,
    pub drag_and_drop: bool,
    pub hotkeys: Hotkeys,
}

impl Default for CommonSettings {
    fn default() -> Self {
        CommonSettings::of(&Settings::default())
    }
}

impl CommonSettings {
    pub fn of(s: &Settings) -> CommonSettings {
        CommonSettings {
            strict_cipher: s.strict_cipher,
            timing: s.timing.clone(),
            clipboard: s.clipboard.clone(),
            drag_and_drop: s.transfers.drag_and_drop,
            hotkeys: s.hotkeys.clone(),
        }
    }

    pub fn apply_to(&self, s: &mut Settings) {
        s.strict_cipher = self.strict_cipher;
        s.timing = self.timing.clone().sanitized();
        s.clipboard = self.clipboard.clone();
        s.transfers.drag_and_drop = self.drag_and_drop;
        s.hotkeys = self.hotkeys.clone();
    }
}

/// Settings that are the same on all paired devices. Every change gets a
/// newer version and is sent to the connected devices; a device that was
/// offline adopts the newest version when it connects.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SharedSettings {
    pub rev: u64,
    pub author: DeviceId,
    pub cipher: CipherPreference,
    #[serde(default)]
    pub common: CommonSettings,
}

impl SharedSettings {
    pub fn new(author: DeviceId) -> SharedSettings {
        SharedSettings { rev: 0, author, cipher: CipherPreference::Auto, common: CommonSettings::default() }
    }

    pub fn version(&self) -> (u64, DeviceId) {
        (self.rev, self.author)
    }
}


/// A device the user paired with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedPeer {
    pub id: DeviceId,
    pub name: String,
    /// Hex encoded X25519 public key.
    pub public_key: String,
    /// Last address the device was reached at.
    #[serde(default)]
    pub last_addr: Option<String>,
    #[serde(default)]
    pub paired_at_ms: u64,
}

/// A computer that was unpaired from the setup. Every paired computer
/// forgets it too (also those offline at the time, when they reconnect),
/// unless it was paired again after `at_ms`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Removal {
    pub id: DeviceId,
    pub at_ms: u64,
}

/// Most removals remembered.
pub const MAX_REMOVALS: usize = 50;

/// Everything persisted in `config.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    pub device_id: DeviceId,
    pub settings: Settings,
    pub shared: SharedSettings,
    pub layout: Layout,
    pub trusted: Vec<TrustedPeer>,
    /// KVM Bridge is switched on (sharing input, clipboard and drops).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Saved configs deleted here or elsewhere.
    #[serde(default)]
    pub deleted_configs: Vec<ConfigStamp>,
    /// Computers unpaired from the setup.
    #[serde(default)]
    pub removed: Vec<Removal>,
    /// The saved config last loaded or saved here.
    #[serde(default)]
    pub last_config: Option<ConfigId>,
}

fn default_true() -> bool {
    true
}

impl State {
    pub fn fresh() -> State {
        let id = DeviceId::new_random();
        let settings = Settings::default();
        State {
            version: 1,
            device_id: id,
            shared: SharedSettings::new(id),
            layout: Layout::single(id, &settings.device_name),
            settings,
            trusted: vec![],
            enabled: true,
            deleted_configs: vec![],
            removed: vec![],
            last_config: None,
        }
    }

    pub fn trusted(&self, id: DeviceId) -> Option<&TrustedPeer> {
        self.trusted.iter().find(|t| t.id == id)
    }
}

/// Where config, identity and caches live.
#[derive(Clone, Debug)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub default_receive_dir: PathBuf,
}

impl Paths {
    /// Platform default locations; `KVMBRIDGE_HOME` overrides everything
    /// (handy for running several instances on one machine).
    pub fn discover() -> Paths {
        if let Some(home) = std::env::var_os("KVMBRIDGE_HOME") {
            return Paths::rooted(Path::new(&home));
        }
        let pd = directories::ProjectDirs::from("io", "kvmbridge", "KVMBridge");
        let ud = directories::UserDirs::new();
        let (config_dir, cache_dir) = match &pd {
            Some(pd) => (pd.config_dir().to_path_buf(), pd.cache_dir().to_path_buf()),
            None => (PathBuf::from(".kvmbridge"), PathBuf::from(".kvmbridge/cache")),
        };
        let downloads = ud
            .as_ref()
            .and_then(|u| u.download_dir().map(Path::to_path_buf))
            .or_else(|| ud.as_ref().map(|u| u.home_dir().join("Downloads")))
            .unwrap_or_else(|| PathBuf::from("."));
        Paths { config_dir, cache_dir, default_receive_dir: downloads.join("KVMBridge") }
    }

    pub fn rooted(root: &Path) -> Paths {
        Paths {
            config_dir: root.join("config"),
            cache_dir: root.join("cache"),
            default_receive_dir: root.join("received"),
        }
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.json")
    }
    pub fn identity_file(&self) -> PathBuf {
        self.config_dir.join("identity.key")
    }
    pub fn log_dir(&self) -> PathBuf {
        self.cache_dir.join("logs")
    }
    /// The default location of saved configs.
    pub fn configs_dir(&self) -> PathBuf {
        self.config_dir.join("configs")
    }
}

/// Load the persisted state, creating a fresh one on first run.
pub fn load_state(paths: &Paths) -> anyhow::Result<State> {
    let f = paths.config_file();
    match std::fs::read(&f) {
        Ok(bytes) => match serde_json::from_slice::<State>(&bytes) {
            Ok(mut st) => {
                st.settings.timing = st.settings.timing.clone().sanitized();
                // The settings are what is in use (and files from before
                // 0.1.4 have no copy in `shared`).
                st.shared.common = CommonSettings::of(&st.settings);
                Ok(st)
            }
            Err(e) => {
                // Keep the broken file for inspection and start over.
                let bak = f.with_extension("json.broken");
                let _ = std::fs::rename(&f, &bak);
                tracing::warn!("config file was unreadable ({e}); moved to {}", bak.display());
                let st = State::fresh();
                save_state(paths, &st)?;
                Ok(st)
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let st = State::fresh();
            save_state(paths, &st)?;
            Ok(st)
        }
        Err(e) => Err(e.into()),
    }
}

/// Atomically write the state file.
pub fn save_state(paths: &Paths, st: &State) -> anyhow::Result<()> {
    std::fs::create_dir_all(&paths.config_dir)?;
    let f = paths.config_file();
    let tmp = f.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(st)?)?;
    std::fs::rename(&tmp, &f)?;
    Ok(())
}

/// Load or create the device's long-term key.
pub fn load_identity(paths: &Paths) -> anyhow::Result<Identity> {
    let f = paths.identity_file();
    if let Ok(s) = std::fs::read_to_string(&f) {
        let bytes = hex::decode(s.trim())?;
        if bytes.len() == 32 {
            let mut k = [0u8; 32];
            k.copy_from_slice(&bytes);
            return Ok(Identity::from_private(k));
        }
        tracing::warn!("identity key file is malformed; generating a new identity");
    }
    let id = Identity::generate();
    std::fs::create_dir_all(&paths.config_dir)?;
    write_identity(&f, &id.private)?;
    Ok(id)
}

/// Store a private key (owner-only permissions on Unix).
pub fn write_identity(path: &Path, private: &[u8; 32]) -> std::io::Result<()> {
    write_private(path, hex::encode(private).as_bytes())
}

fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
        f.write_all(data)
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, data)
    }
}
