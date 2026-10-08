//! Serializable snapshot of the engine state for the user interface.

use crate::config::{Settings, SharedSettings};
use crate::crypto::Cipher;
use crate::layout::{Desktop, Layout};
use crate::platform::BackendStatus;
use crate::proto::{Capabilities, ItemMeta};
use crate::platform::GuardStatus;
use crate::types::DeviceId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Snapshot {
    pub me: MeView,
    pub peers: Vec<PeerView>,
    pub layout: Layout,
    pub settings: Settings,
    pub shared: SharedSettings,
    pub effective_cipher: Cipher,
    pub hw_aes: bool,
    pub input: InputView,
    pub transfers: Vec<TransferView>,
    pub backends: BackendsView,
    pub desktop: Option<Desktop>,
    /// KVM Bridge is switched on here.
    pub enabled: bool,
    /// Pausing of this computer's keyboard and mouse.
    pub guard: GuardStatus,
    /// Most computers (including this one) that can be paired together.
    pub max_devices: usize,
    /// A search for devices on the network is running.
    pub searching: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MeView {
    pub id: DeviceId,
    pub name: String,
    pub os: String,
    pub fingerprint: String,
    pub port: u16,
    pub addresses: Vec<String>,
    pub version: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PeerStatus {
    /// Seen on the network, not paired.
    Discovered,
    Connecting,
    /// Waiting for both users to confirm the pairing code.
    Pairing,
    Connected,
    /// Paired but currently unreachable.
    Offline,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PairingView {
    pub sas: String,
    /// The other device asked to pair with us.
    pub incoming: bool,
    pub local_accepted: bool,
    pub remote_accepted: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PeerView {
    pub id: DeviceId,
    pub name: String,
    pub os: String,
    pub address: Option<String>,
    pub fingerprint: String,
    pub status: PeerStatus,
    pub trusted: bool,
    pub rtt_ms: Option<f32>,
    pub cipher: Option<Cipher>,
    pub pairing: Option<PairingView>,
    pub desktop: Option<Desktop>,
    pub caps: Capabilities,
    pub error: Option<String>,
    pub last_seen_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum InputView {
    Local { locked: bool },
    Controlling { target: DeviceId, locked: bool },
    Controlled { by: DeviceId },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransferState {
    Preparing,
    Waiting,
    Running,
    Done,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransferKind {
    Manual,
    Drag,
    Clipboard,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TransferView {
    pub id: u64,
    pub peer: DeviceId,
    pub outgoing: bool,
    pub kind: TransferKind,
    pub state: TransferState,
    pub items: Vec<ItemMeta>,
    pub file_count: u32,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub rate_bps: f64,
    pub error: Option<String>,
    /// Where received files ended up.
    pub paths: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BackendsView {
    pub capture: BackendStatus,
    pub emulation: BackendStatus,
    pub clipboard: BackendStatus,
    pub dnd: BackendStatus,
}

/// One-off events for the UI (toasts, dialogs, overlays).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Notice {
    /// Show the big "Screen N" overlay on this machine.
    Identify { number: u32, name: String },
    PairingRequest { device: DeviceId, name: String, sas: String },
    Paired { device: DeviceId, name: String },
    PairingFailed { device: DeviceId, name: String, reason: String },
    TransferFinished { id: u64, peer_name: String, outgoing: bool, ok: bool, error: Option<String>, paths: Vec<String> },
    Error { message: String },
    Info { message: String },
}
