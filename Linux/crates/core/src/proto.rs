//! Messages exchanged over the encrypted control channel and data channels.

use crate::config::{Removal, SharedSettings};
use crate::layout::{Desktop, Layout};
use crate::library::{ConfigStamp, SavedConfig};
use crate::types::{ClipFormat, DeviceId, Point};
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 2;
/// Maximum number of top-level entries described in offers / drag messages.
pub const MAX_SUMMARY_ITEMS: usize = 64;
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Sent inside the Noise handshake (encrypted) so each side learns who the
/// other claims to be before any application traffic flows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeviceIntro {
    pub id: DeviceId,
    pub name: String,
    pub os: String,
    pub app_version: String,
    pub protocol: u32,
    /// TCP port this device listens on (so the peer can open data channels).
    pub port: u16,
}

/// What a device can do on this machine.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Capabilities {
    pub capture: bool,
    pub emulation: bool,
    pub clipboard: bool,
    pub drag_and_drop: bool,
    /// KVM Bridge is switched on there.
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub desktop: Option<Desktop>,
    pub layout: Layout,
    pub shared: SharedSettings,
    pub caps: Capabilities,
    /// Sender's heartbeat interval so the receiver can size its timeout.
    pub heartbeat_ms: u32,
}

/// Metadata of one file or directory in a transfer. `rel_path` always uses `/`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemMeta {
    pub rel_path: String,
    pub size: u64,
    pub is_dir: bool,
    /// Modification time (seconds since the Unix epoch), if known.
    pub mtime: Option<i64>,
}

/// Why a transfer is happening; decides where the receiver puts the data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferPurpose {
    /// Files sent explicitly from the UI.
    Manual,
    /// Files dragged across screens.
    Drag { drag: u64 },
    /// Files pasted from a remote clipboard.
    ClipboardFiles { req: u64 },
    /// A large clipboard payload (e.g. an image) delivered as a blob.
    ClipboardBlob { req: u64 },
}

/// A computer of the sender's setup, which the receiver should pair with
/// too (it is in the same arrangement).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Introduction {
    pub id: DeviceId,
    pub name: String,
    /// Hex encoded X25519 public key.
    pub public_key: String,
    /// Last address the sender reached it at.
    pub addr: Option<String>,
    pub paired_at_ms: u64,
}

/// New messages go at the end: older versions skip messages they cannot
/// decode, so these stay compatible without a new protocol version.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Msg {
    Hello(Box<Hello>),
    Ping { seq: u32, sent_us: u64 },
    Pong { seq: u32, sent_us: u64 },

    // ---- pairing / trust
    PairAccept,
    PairReject { reason: String },
    /// The sender removed us from its trusted devices.
    Unpaired,

    // ---- shared state
    Layout(Layout),
    Shared(SharedSettings),
    Geometry(Option<Desktop>),
    Identify { number: u32 },
    Rename { name: String },

    // ---- input (controller -> target)
    /// The controller's cursor enters this device at `pos`.
    Enter { session: u32, pos: Point },
    /// The controller's cursor left this device.
    Leave { session: u32 },
    MoveAbs { x: f32, y: f32 },
    MoveRel { dx: f32, dy: f32 },
    Button { button: u32, pressed: bool },
    Key { code: u32, pressed: bool },
    Scroll { dx: f32, dy: f32 },
    ScrollDiscrete { dx: i32, dy: i32 },

    // ---- input (target -> controller)
    /// This device is no longer controlled by the receiver (another device
    /// took over, or it was released locally).
    ControlEnded { session: u32 },

    // ---- clipboard
    ClipOffer { seq: u64, formats: Vec<ClipFormat> },
    ClipRequest { req: u64, seq: u64, format: ClipFormat },
    /// Inline clipboard payload (`None` when it is no longer available).
    ClipData {
        req: u64,
        #[serde(with = "serde_bytes")]
        data: Option<Vec<u8>>,
    },

    // ---- transfers
    /// `items` lists (up to [`MAX_SUMMARY_ITEMS`]) top-level entries for display;
    /// the full file list streams over the data channel.
    TransferOffer { id: u64, purpose: TransferPurpose, items: Vec<ItemMeta>, file_count: u32, total_bytes: u64 },
    TransferAccept { id: u64 },
    TransferReject { id: u64, reason: String },
    TransferCancel { id: u64 },

    // ---- drag and drop
    /// A file drag entered the receiver's screen; the files arrive via `transfer`.
    DragBegin { drag: u64, transfer: u64, items: Vec<ItemMeta> },
    DragCancel { drag: u64 },
    DragFinished { drag: u64, accepted: bool },

    // ---- setups (0.1.15)
    /// Sent by the computer that started a pairing: after pairing, both
    /// keep the settings of `from`.
    PairSettings { from: DeviceId },
    /// The other computers of the sender's setup (see [`Introduction`]).
    Introduce(Vec<Introduction>),
    /// Computers unpaired from the setup; the receiver unpairs them too.
    Forget(Vec<Removal>),
    /// A saved config (stored if newer than the receiver's copy).
    ConfigSync(Box<SavedConfig>),
    /// A saved config was deleted.
    ConfigDelete(ConfigStamp),
}

impl Msg {
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("serialisable message")
    }
    pub fn decode(b: &[u8]) -> Result<Msg, postcard::Error> {
        postcard::from_bytes(b)
    }
    /// Input messages are latency critical and never logged.
    pub fn is_input(&self) -> bool {
        matches!(
            self,
            Msg::MoveAbs { .. }
                | Msg::MoveRel { .. }
                | Msg::Button { .. }
                | Msg::Key { .. }
                | Msg::Scroll { .. }
                | Msg::ScrollDiscrete { .. }
        )
    }
}

/// Header of every frame on a data channel. Chunk frames carry raw bytes after
/// the one byte tag to avoid serialisation overhead.
pub mod data {
    use super::*;

    pub const TAG_CTRL: u8 = 0;
    pub const TAG_CHUNK: u8 = 1;
    /// Raw bytes per chunk frame (fits in one Noise frame together with the tag).
    pub const CHUNK: usize = 60 * 1024;

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    pub enum DataCtrl {
        /// First frame from the sender: which transfer this channel carries.
        Open { transfer: u64 },
        /// Receiver is ready (or refuses).
        Ready { ok: bool, error: Option<String> },
        /// A directory (relative path, `/` separated).
        Dir { rel_path: String },
        /// A file starts; exactly `size` bytes follow as chunk frames.
        FileStart { rel_path: String, size: u64, mtime: Option<i64> },
        FileEnd,
        /// Everything was sent.
        End,
        /// Receiver's final verdict.
        Done { ok: bool, error: Option<String> },
    }

    pub fn encode_ctrl(c: &DataCtrl) -> Vec<u8> {
        let mut v = vec![TAG_CTRL];
        v.extend(postcard::to_stdvec(c).expect("serialisable"));
        v
    }
}
