//! Channel based interface between the engine and OS specific backends.
//!
//! Each backend runs as its own task/thread, receives commands on one channel
//! and reports events on another. Backends that are not available on a
//! platform simply report [`BackendStatus::Unavailable`].

use crate::layout::Desktop;
use crate::types::{ClipFormat, Edge, Point};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::sync::{mpsc, oneshot};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "detail", rename_all = "lowercase")]
pub enum BackendStatus {
    /// Not started yet.
    Idle,
    /// Starting (e.g. waiting for the user to approve a permission dialog).
    Starting(String),
    /// Working; the string names the implementation in use.
    Ready(String),
    /// Not usable; the string explains why.
    Unavailable(String),
}

impl BackendStatus {
    pub fn is_ready(&self) -> bool {
        matches!(self, BackendStatus::Ready(_))
    }
}

// ---------------------------------------------------------------- capture

/// Events from the backend that captures the local keyboard and mouse.
#[derive(Debug)]
pub enum CaptureEvent {
    Status(BackendStatus),
    /// Monitor layout of this machine (logical coordinates).
    Geometry(Desktop),
    /// The cursor hit a screen edge that has a neighbour; input is now
    /// captured (the local cursor stays put / is hidden).
    Activated { edge: Edge, pos: Point },
    Motion { dx: f64, dy: f64 },
    Button { button: u32, pressed: bool },
    /// Smooth scrolling in logical pixels.
    Scroll { dx: f64, dy: f64 },
    /// Wheel clicks in 1/120 of a detent.
    ScrollDiscrete { dx: i32, dy: i32 },
    Key { code: u32, pressed: bool },
    /// Capture ended without us asking (compositor revoked it, etc.).
    Deactivated,
    /// State of pausing this computer's own keyboards and mice.
    Guard(GuardStatus),
}

/// Pausing this computer's own keyboards and mice while another computer
/// controls it (so they cannot interfere).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "detail", rename_all = "snake_case")]
pub enum GuardStatus {
    /// Not paused.
    Off,
    /// Paused; the number of devices held.
    Paused(u32),
    /// A one-time setup (administrator password) is needed first.
    NeedsSetup,
    /// Pausing is not possible; the string explains why.
    Unavailable(String),
    /// The user pressed the emergency shortcut on this computer's keyboard.
    Escaped,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CaptureCommand {
    /// Edges that lead to another screen (barriers are placed on these).
    SetEdges(Vec<Edge>),
    /// Stop capturing and put the local cursor at `pos` (if given).
    Release { pos: Option<Point> },
    /// Temporarily disable/enable capture (e.g. while this device is
    /// controlled remotely). Backends may ignore this if disabling is costly;
    /// the engine releases any activation that happens while disabled.
    Enable(bool),
    /// Tear down and set up again (e.g. after the user denied a permission).
    Restart,
    /// Pause (or resume) this computer's own keyboards and mice. Injected
    /// input from the controlling computer keeps working.
    BlockLocalInput(bool),
}

// ---------------------------------------------------------------- emulation

#[derive(Debug)]
pub enum EmulationEvent {
    Status(BackendStatus),
    /// Monitor layout as the emulated pointer sees it (logical coordinates).
    /// Takes precedence over [`CaptureEvent::Geometry`], and lets a computer
    /// that is only controlled report its screens without capturing input.
    Geometry(Desktop),
}

#[derive(Debug, Clone, PartialEq)]
pub enum EmulationCommand {
    /// A remote cursor arrives at `pos`.
    Enter { pos: Point },
    MoveAbs { pos: Point },
    MoveRel { dx: f64, dy: f64 },
    Button { button: u32, pressed: bool },
    Key { code: u32, pressed: bool },
    Scroll { dx: f64, dy: f64 },
    ScrollDiscrete { dx: i32, dy: i32 },
    /// The remote cursor left: release anything still held down, then move
    /// the pointer to `park` (out of sight) if given.
    Leave { park: Option<Point> },
    /// Tear down and set up again (e.g. after the user denied a permission).
    Restart,
}

// ---------------------------------------------------------------- clipboard

#[derive(Debug)]
pub enum ClipboardEvent {
    Status(BackendStatus),
    /// Something on this machine copied new content.
    LocalChanged { formats: Vec<ClipFormat> },
    /// A local application pastes content advertised with
    /// [`ClipboardCommand::AdvertiseRemote`]. For [`ClipFormat::Files`] the
    /// reply is a `text/uri-list` of local paths.
    Paste { offer: u64, format: ClipFormat, reply: oneshot::Sender<Option<Vec<u8>>> },
}

#[derive(Debug)]
pub enum ClipboardCommand {
    /// Take ownership of the local clipboard on behalf of a remote device.
    /// Data is fetched lazily through [`ClipboardEvent::Paste`].
    AdvertiseRemote { offer: u64, formats: Vec<ClipFormat> },
    /// Read local clipboard content (for a remote paste). `Files` returns a
    /// `text/uri-list`.
    Read { format: ClipFormat, reply: oneshot::Sender<Option<Vec<u8>>> },
}

// ---------------------------------------------------------------- drag & drop

#[derive(Debug)]
pub enum DndEvent {
    Status(BackendStatus),
    /// The probe window of a [`DndCommand::Probe`] is on screen. The
    /// compositor only hands it a drag in progress once the pointer moves,
    /// which a captured pointer cannot: capture is let go for a moment.
    ProbeReady { probe: u64 },
    /// Result of a [`DndCommand::Probe`]: files being dragged off this screen.
    /// `dragging`: a drag (of anything) was found, so it is still going on here.
    Probed { probe: u64, paths: Vec<PathBuf>, dragging: bool },
    /// A local application accepted a drop of a remote drag; reply with the
    /// local paths once the files have arrived (`None` = failed).
    DropRequested { drag: u64, reply: oneshot::Sender<Option<Vec<PathBuf>>> },
    /// A remote drag that we started locally ended.
    DragEnded { drag: u64, accepted: bool },
    /// Press or release the left button through the input emulation (to
    /// start a native drag under the cursor, or to end one).
    InjectButton { pressed: bool },
    /// Move the pointer a little through the input emulation (so the
    /// compositor notices windows that appeared under it).
    InjectMove { dx: f64, dy: f64 },
    /// Tap Escape through the input emulation (cancels a drag in progress).
    InjectEscape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// The drag was dropped on another device.
    Dropped { accepted: bool },
    /// The cursor came back.
    Returned,
    /// Nothing happened with it (control was released, etc.).
    Abandoned,
    /// The local drag was already ended (or there was none): nothing to clean up.
    Cancelled,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DndCommand {
    /// A mouse button held while the cursor left this screen at `pos` was let
    /// go on another screen: put a probe window under the pointer, report
    /// [`DndEvent::ProbeReady`], and then whether a file drag came over it
    /// with [`DndEvent::Probed`].
    Probe { probe: u64, pos: Point },
    ProbeDone { probe: u64, outcome: ProbeOutcome },
    /// Start a native drag of not-yet-received files under the (remote
    /// controlled) cursor at `pos`, and drop it wherever the cursor is
    /// `drop_after_ms` later.
    ///
    /// `after_release`: the cursor came back to this screen with the button
    /// still held (the files come from the screen it left), so the drag
    /// starts only once that button is let go here, where it was let go. The
    /// files are not known yet (`names` empty): the drag is dropped only once
    /// [`DndCommand::DragFiles`] names them, and [`DndCommand::CancelDrag`]
    /// follows if nothing was being dragged.
    StartDrag { drag: u64, names: Vec<String>, pos: Point, drop_after_ms: u64, after_release: bool },
    /// The files of a carried drag are known: it may be dropped.
    DragFiles { drag: u64, names: Vec<String> },
    CancelDrag { drag: u64 },
}

// ---------------------------------------------------------------- plumbing

/// Engine side of a backend.
pub struct Backend<C, E> {
    pub cmd: mpsc::UnboundedSender<C>,
    pub events: mpsc::UnboundedReceiver<E>,
}

/// Backend side of a backend.
pub struct BackendSide<C, E> {
    pub commands: mpsc::UnboundedReceiver<C>,
    pub events: mpsc::UnboundedSender<E>,
}

pub fn backend_pair<C, E>() -> (Backend<C, E>, BackendSide<C, E>) {
    let (ctx, crx) = mpsc::unbounded_channel();
    let (etx, erx) = mpsc::unbounded_channel();
    (Backend { cmd: ctx, events: erx }, BackendSide { commands: crx, events: etx })
}

/// All backends the engine talks to.
pub struct Platform {
    pub capture: Backend<CaptureCommand, CaptureEvent>,
    pub emulation: Backend<EmulationCommand, EmulationEvent>,
    pub clipboard: Backend<ClipboardCommand, ClipboardEvent>,
    pub dnd: Backend<DndCommand, DndEvent>,
}

/// Backend sides, handed to the platform implementation.
pub struct PlatformSides {
    pub capture: BackendSide<CaptureCommand, CaptureEvent>,
    pub emulation: BackendSide<EmulationCommand, EmulationEvent>,
    pub clipboard: BackendSide<ClipboardCommand, ClipboardEvent>,
    pub dnd: BackendSide<DndCommand, DndEvent>,
}

pub fn platform_channels() -> (Platform, PlatformSides) {
    let (c, cs) = backend_pair();
    let (e, es) = backend_pair();
    let (k, ks) = backend_pair();
    let (d, ds) = backend_pair();
    (
        Platform { capture: c, emulation: e, clipboard: k, dnd: d },
        PlatformSides { capture: cs, emulation: es, clipboard: ks, dnd: ds },
    )
}

impl<C: Send + 'static, E: Send + 'static> BackendSide<C, E> {
    /// Report the backend as unavailable and drain commands forever.
    pub fn unavailable(mut self, reason: impl Into<String>, status: impl Fn(BackendStatus) -> E + Send + 'static)
    where
        E: std::fmt::Debug,
    {
        let _ = self.events.send(status(BackendStatus::Unavailable(reason.into())));
        tokio::spawn(async move { while self.commands.recv().await.is_some() {} });
    }
}
