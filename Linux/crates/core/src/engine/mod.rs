//! The engine: a single task that owns all state and reacts to events from
//! the network, the platform backends and the user interface.

mod backup;
mod clipboard;
mod dnd;
mod input;
mod peers;
mod power;
mod transfers;
pub mod view;

use crate::config::{self, Paths, Settings, State, Timing};
use crate::crypto::{has_hw_aes, CipherPreference, Identity};
use crate::discovery::{self, Beacon, DiscoveryConfig, Seen};
use crate::layout::{Desktop, Layout};
use crate::net::{self, CipherPolicy, NetCtx, NetEvent};
use crate::platform::*;
use crate::proto::{DeviceIntro, Msg, APP_VERSION, PROTOCOL_VERSION};
use crate::types::{os_tag, DeviceId, Edge};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::time::Instant;
pub use backup::ImportPlan;
pub use view::*;

pub(crate) use clipboard::ClipState;
pub(crate) use input::{Controlled, Input};
pub(crate) use peers::{ConnState, Peer};
pub(crate) use transfers::Transfer;

/// Commands from the user interface.
#[derive(Debug)]
pub enum Command {
    SetSettings(Box<Settings>),
    SetCipher(CipherPreference),
    SetLayout(Layout),
    /// Start pairing with a discovered device.
    Pair(DeviceId),
    AcceptPair(DeviceId),
    RejectPair(DeviceId),
    Unpair(DeviceId),
    /// Connect to `host` or `host:port` directly (no broadcast needed).
    ConnectAddress(String),
    /// Show screen numbers on every connected device.
    Identify,
    /// Look for devices on the local network (nothing is searched otherwise).
    Search,
    SendFiles { to: DeviceId, paths: Vec<PathBuf> },
    CancelTransfer(u64),
    /// Bring the cursor back to this device.
    ReleaseControl,
    SetLocked(bool),
    /// Ask the platform backends to start over (e.g. to show a permission
    /// prompt again after it was dismissed).
    RestartBackends,
    /// Copy of the persisted state (for exporting a configuration file).
    GetState(oneshot::Sender<State>),
    /// Choose the computer that controls the screens (`None`: any).
    SetController(Option<DeviceId>),
    /// Switch KVM Bridge on or off on this computer.
    SetEnabled(bool),
    /// Try pausing this computer's keyboard and mouse again (after setup).
    RetryInputPause,
    /// Diagnostics: drag a test file from here; it drops after `seconds`.
    TestDrag { seconds: u64 },
    /// Apply parts of an imported configuration file.
    ImportConfig(Box<ImportPlan>),
    /// Adopt a restored identity; the app must restart afterwards.
    ReplaceIdentity { device_id: DeviceId, private_key: [u8; 32] },
    Shutdown,
}

pub struct Request {
    pub cmd: Command,
    pub reply: Option<oneshot::Sender<Result<(), String>>>,
}

/// Cheap, cloneable handle used by the UI layer.
#[derive(Clone)]
pub struct EngineHandle {
    cmd: mpsc::UnboundedSender<Request>,
    snapshot: watch::Receiver<Snapshot>,
    notices: broadcast::Sender<Notice>,
}

impl EngineHandle {
    /// Send a command and wait until the engine processed it.
    pub async fn request(&self, cmd: Command) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.cmd.send(Request { cmd, reply: Some(tx) }).map_err(|_| "engine stopped".to_string())?;
        rx.await.map_err(|_| "engine stopped".to_string())?
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.cmd.send(Request { cmd, reply: None });
    }

    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.borrow().clone()
    }

    pub fn watch(&self) -> watch::Receiver<Snapshot> {
        self.snapshot.clone()
    }

    pub fn notices(&self) -> broadcast::Receiver<Notice> {
        self.notices.subscribe()
    }

    /// The persisted state (settings, layout, paired devices).
    pub async fn state(&self) -> Result<State, String> {
        let (tx, rx) = oneshot::channel();
        self.request(Command::GetState(tx)).await?;
        rx.await.map_err(|_| "engine stopped".to_string())
    }
}

/// Overrides mainly used by tests and for running several instances.
#[derive(Clone, Debug, Default)]
pub struct Overrides {
    pub listen_port: Option<u16>,
    pub discovery_port: Option<u16>,
    pub discovery_broadcast: Option<bool>,
    pub discovery_targets: Vec<SocketAddr>,
}

/// Internal events produced by helper tasks spawned from the engine.
#[derive(Debug)]
pub(crate) enum Internal {
    ManifestReady { id: u64, result: Result<crate::transfer::Manifest, String> },
    TransferFinished { id: u64, result: Result<crate::transfer::Received, String> },
    ClipReadDone { peer: DeviceId, req: u64, format: crate::types::ClipFormat, data: Option<Vec<u8>> },
    DataOpened { peer: DeviceId, wire_id: u64, channel: Box<crate::net::DataChannel> },
}

pub(crate) struct Engine {
    paths: Paths,
    st: State,
    identity: Arc<Identity>,
    net: Arc<NetCtx>,
    port: u16,
    overrides: Overrides,

    cap: mpsc::UnboundedSender<CaptureCommand>,
    emu: mpsc::UnboundedSender<EmulationCommand>,
    clipcmd: mpsc::UnboundedSender<ClipboardCommand>,
    dndcmd: mpsc::UnboundedSender<DndCommand>,
    backends: BackendsView,

    peers: HashMap<DeviceId, Peer>,
    conns: HashMap<u64, ConnState>,
    desktop: Option<Desktop>,
    /// `desktop` came from the emulation backend (capture's is then ignored).
    desktop_from_emulation: bool,
    input: Input,
    controlled: Option<Controlled>,
    locked: bool,
    edges_sent: Option<Vec<Edge>>,
    capture_enabled: bool,
    /// Pausing of this computer's own keyboard and mouse.
    guard: GuardStatus,
    guard_sent: Option<bool>,
    /// Peers we just paired with (merged into the arrangement on Hello).
    just_paired: std::collections::HashSet<DeviceId>,
    motion_deadline: Option<Instant>,

    clip: ClipState,
    transfers: HashMap<u64, Transfer>,
    /// (peer, remote transfer id) -> local transfer id, for offers we accepted.
    incoming: HashMap<(DeviceId, u64), u64>,
    dnd: dnd::DndState,

    internal: mpsc::UnboundedSender<Internal>,
    snapshot_tx: watch::Sender<Snapshot>,
    notices: broadcast::Sender<Notice>,
    dirty: bool,
    last_publish: Instant,
    save_at: Option<Instant>,
    disc_cfg: watch::Sender<DiscoveryConfig>,
    beacon: watch::Sender<Beacon>,
    searches: mpsc::UnboundedSender<()>,
    /// A search for devices is running until then.
    searching_until: Option<Instant>,
    shutting_down: bool,
}

/// Start the engine. Must be called inside a tokio runtime.
pub async fn start(paths: Paths, platform: Platform, overrides: Overrides) -> anyhow::Result<EngineHandle> {
    let mut st = config::load_state(&paths)?;
    let identity = Arc::new(config::load_identity(&paths)?);
    if !st.layout.contains(st.device_id) && st.layout.slots.is_empty() {
        st.layout = Layout::single(st.device_id, &st.settings.device_name);
    }

    let (net_tx, net_rx) = mpsc::unbounded_channel();
    let intro = DeviceIntro {
        id: st.device_id,
        name: st.settings.device_name.clone(),
        os: os_tag().into(),
        app_version: APP_VERSION.into(),
        protocol: PROTOCOL_VERSION,
        port: 0,
    };
    let policy = CipherPolicy { preferred: st.shared.cipher.resolve(), strict: st.settings.strict_cipher };
    let netctx = NetCtx::new(identity.clone(), intro, policy, net_tx.clone());
    let want_port = overrides.listen_port.unwrap_or(st.settings.listen_port);
    let (port, _listener) = net::listen(netctx.clone(), want_port).await?;
    netctx.intro.write().port = port;

    let instance = crate::crypto::random_u64();
    let beacon = Beacon {
        protocol: PROTOCOL_VERSION,
        id: st.device_id,
        name: st.settings.device_name.clone(),
        os: os_tag().into(),
        port,
        fingerprint: identity.fingerprint(),
        instance,
        probe: false,
    };
    let disc = discovery_config(&st.settings, &overrides);
    let (disc_tx, disc_rx) = watch::channel(disc);
    let (beacon_tx, beacon_rx) = watch::channel(beacon);
    let (seen_tx, seen_rx) = mpsc::channel(256);
    let (search_tx, search_rx) = mpsc::unbounded_channel();
    tokio::spawn(discovery::run(disc_rx, beacon_rx, search_rx, seen_tx));

    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (internal_tx, internal_rx) = mpsc::unbounded_channel();
    let (notices, _) = broadcast::channel(64);

    let Platform { capture, emulation, clipboard, dnd } = platform;
    let mut engine = Engine {
        paths,
        st,
        identity,
        net: netctx,
        port,
        overrides,
        cap: capture.cmd,
        emu: emulation.cmd,
        clipcmd: clipboard.cmd,
        dndcmd: dnd.cmd,
        backends: BackendsView {
            capture: BackendStatus::Idle,
            emulation: BackendStatus::Idle,
            clipboard: BackendStatus::Idle,
            dnd: BackendStatus::Idle,
        },
        peers: HashMap::new(),
        conns: HashMap::new(),
        desktop: None,
        desktop_from_emulation: false,
        input: Input::Local,
        controlled: None,
        locked: false,
        edges_sent: None,
        capture_enabled: true,
        guard: GuardStatus::Off,
        guard_sent: None,
        just_paired: Default::default(),
        motion_deadline: None,
        clip: ClipState::default(),
        transfers: HashMap::new(),
        incoming: HashMap::new(),
        dnd: dnd::DndState::default(),
        internal: internal_tx,
        snapshot_tx: watch::channel(dummy_snapshot()).0,
        notices: notices.clone(),
        dirty: true,
        last_publish: Instant::now(),
        save_at: None,
        disc_cfg: disc_tx,
        beacon: beacon_tx,
        searches: search_tx,
        searching_until: None,
        shutting_down: false,
    };
    let snap = engine.build_snapshot();
    let (snapshot_tx, snapshot_rx) = watch::channel(snap);
    engine.snapshot_tx = snapshot_tx;
    engine.seed_trusted_peers();

    let rx = Receivers {
        cmd: cmd_rx,
        net: net_rx,
        seen: seen_rx,
        internal: internal_rx,
        capture: capture.events,
        emulation: emulation.events,
        clipboard: clipboard.events,
        dnd: dnd.events,
    };
    tokio::spawn(engine.run(rx));
    Ok(EngineHandle { cmd: cmd_tx, snapshot: snapshot_rx, notices })
}

fn discovery_config(s: &Settings, o: &Overrides) -> DiscoveryConfig {
    let mut extra = o.discovery_targets.clone();
    let port = o.discovery_port.unwrap_or(s.discovery_port);
    for m in &s.manual_peers {
        // Beacons to manually configured hosts make discovery work across
        // networks that filter broadcast.
        if let Some(a) = resolve_host(m, port) {
            extra.push(a);
        }
    }
    DiscoveryConfig {
        port,
        enabled: s.discovery_enabled,
        broadcast: o.discovery_broadcast.unwrap_or(true),
        extra_targets: extra,
    }
}

/// Parse `host`, `host:port`, `ip` or `ip:port`.
pub(crate) fn resolve_host(s: &str, default_port: u16) -> Option<SocketAddr> {
    use std::net::ToSocketAddrs;
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(a) = s.parse::<SocketAddr>() {
        return Some(a);
    }
    if let Ok(ip) = s.parse::<std::net::IpAddr>() {
        return Some(SocketAddr::new(ip, default_port));
    }
    let with_port = if s.contains(':') { s.to_string() } else { format!("{s}:{default_port}") };
    with_port.to_socket_addrs().ok()?.find(|a| a.is_ipv4())
}

struct Receivers {
    cmd: mpsc::UnboundedReceiver<Request>,
    net: mpsc::UnboundedReceiver<NetEvent>,
    seen: mpsc::Receiver<Seen>,
    internal: mpsc::UnboundedReceiver<Internal>,
    capture: mpsc::UnboundedReceiver<CaptureEvent>,
    emulation: mpsc::UnboundedReceiver<EmulationEvent>,
    clipboard: mpsc::UnboundedReceiver<ClipboardEvent>,
    dnd: mpsc::UnboundedReceiver<DndEvent>,
}

fn dummy_snapshot() -> Snapshot {
    let st = State::fresh();
    Snapshot {
        me: MeView {
            id: st.device_id,
            name: String::new(),
            os: String::new(),
            fingerprint: String::new(),
            port: 0,
            addresses: vec![],
            version: String::new(),
        },
        peers: vec![],
        layout: st.layout,
        settings: st.settings,
        shared: st.shared,
        effective_cipher: crate::crypto::Cipher::Aes256Gcm,
        hw_aes: false,
        input: InputView::Local { locked: false },
        transfers: vec![],
        backends: BackendsView {
            capture: BackendStatus::Idle,
            emulation: BackendStatus::Idle,
            clipboard: BackendStatus::Idle,
            dnd: BackendStatus::Idle,
        },
        desktop: None,
        enabled: true,
        guard: GuardStatus::Off,
        max_devices: config::MAX_DEVICES,
        searching: false,
    }
}

impl Engine {
    pub(crate) fn me(&self) -> DeviceId {
        self.st.device_id
    }

    fn timing(&self) -> &Timing {
        &self.st.settings.timing
    }

    pub(crate) fn notice(&self, n: Notice) {
        let _ = self.notices.send(n);
    }

    pub(crate) fn error(&self, msg: impl Into<String>) {
        let m = msg.into();
        tracing::warn!("{m}");
        self.notice(Notice::Error { message: m });
    }

    pub(crate) fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub(crate) fn schedule_save(&mut self) {
        if self.save_at.is_none() {
            self.save_at = Some(Instant::now() + Duration::from_millis(300));
        }
        self.dirty = true;
    }

    fn save_now(&mut self) {
        self.save_at = None;
        if let Err(e) = config::save_state(&self.paths, &self.st) {
            self.error(format!("Could not save settings: {e}"));
        }
    }

    async fn run(mut self, mut rx: Receivers) {
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let deadline = self.motion_deadline;
            tokio::select! {
                biased;
                // Input first: it is the latency critical path.
                Some(ev) = rx.capture.recv() => self.on_capture(ev),
                _ = async { tokio::time::sleep_until(deadline.unwrap()).await }, if deadline.is_some() => {
                    self.flush_motion();
                }
                Some(ev) = rx.net.recv() => self.on_net(ev),
                Some(req) = rx.cmd.recv() => {
                    let r = self.on_command(req.cmd);
                    // Publish before replying so the caller sees the result.
                    if self.dirty { self.publish(); }
                    if let Some(tx) = req.reply { let _ = tx.send(r); }
                    if self.shutting_down { break; }
                }
                Some(ev) = rx.emulation.recv() => self.on_emulation(ev),
                Some(ev) = rx.clipboard.recv() => self.on_clipboard(ev),
                Some(ev) = rx.dnd.recv() => self.on_dnd(ev),
                Some(ev) = rx.internal.recv() => self.on_internal(ev),
                Some(seen) = rx.seen.recv() => self.on_seen(seen),
                _ = tick.tick() => self.on_tick(),
            }
            if self.dirty && self.last_publish.elapsed() >= Duration::from_millis(80) {
                self.publish();
            }
        }
        // Graceful shutdown: give control back, say goodbye.
        self.release_control_local();
        for c in self.conns.values() {
            c.handle.close();
        }
        self.save_now();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    fn publish(&mut self) {
        self.dirty = false;
        self.last_publish = Instant::now();
        let s = self.build_snapshot();
        self.snapshot_tx.send_if_modified(|old| {
            if *old != s {
                *old = s;
                true
            } else {
                false
            }
        });
    }

    fn on_tick(&mut self) {
        let now = Instant::now();
        self.tick_connections(now);
        if self.searching_until.is_some_and(|t| now >= t) {
            self.searching_until = None;
            self.mark_dirty();
        }
        self.tick_transfers();
        self.tick_clipboard(now);
        self.tick_dnd(now);
        if let Some(t) = self.save_at {
            if now >= t {
                self.save_now();
            }
        }
    }

    fn on_emulation(&mut self, ev: EmulationEvent) {
        match ev {
            EmulationEvent::Status(s) => {
                tracing::info!("emulation backend: {s:?}");
                self.backends.emulation = s;
                self.broadcast_caps();
                self.mark_dirty();
            }
            EmulationEvent::Geometry(d) => {
                self.desktop_from_emulation = true;
                self.set_local_desktop(d);
            }
        }
    }

    pub(crate) fn caps(&self) -> crate::proto::Capabilities {
        crate::proto::Capabilities {
            capture: self.backends.capture.is_ready(),
            emulation: self.backends.emulation.is_ready(),
            clipboard: self.backends.clipboard.is_ready(),
            drag_and_drop: self.backends.dnd.is_ready(),
            enabled: self.st.enabled,
        }
    }

    fn on_command(&mut self, cmd: Command) -> Result<(), String> {
        match cmd {
            Command::SetSettings(s) => self.set_settings(*s),
            Command::SetCipher(c) => {
                let shared = config::SharedSettings { cipher: c, ..self.next_shared() };
                self.adopt_shared(shared, None);
                Ok(())
            }
            Command::SetController(c) => self.set_controller(c),
            Command::SetEnabled(on) => self.set_enabled(on),
            Command::TestDrag { seconds } => self.test_drag(seconds.clamp(2, 30)),
            Command::RetryInputPause => {
                self.guard_sent = None;
                self.guard = GuardStatus::Off;
                self.update_guard();
                Ok(())
            }
            Command::SetLayout(l) => self.set_layout_from_ui(l),
            Command::Pair(id) => self.start_pairing(id),
            Command::AcceptPair(id) => self.accept_pairing(id),
            Command::RejectPair(id) => self.reject_pairing(id),
            Command::Unpair(id) => self.unpair(id, true),
            Command::ConnectAddress(a) => self.connect_address(&a),
            Command::Identify => {
                self.identify();
                Ok(())
            }
            Command::Search => {
                self.search();
                Ok(())
            }
            Command::SendFiles { to, paths } => self.send_files(to, paths),
            Command::CancelTransfer(id) => self.cancel_transfer(id, true),
            Command::ReleaseControl => {
                self.release_control_local();
                Ok(())
            }
            Command::SetLocked(l) => {
                self.locked = l;
                self.update_edges();
                self.mark_dirty();
                Ok(())
            }
            Command::GetState(tx) => {
                let _ = tx.send(self.st.clone());
                Ok(())
            }
            Command::ImportConfig(plan) => self.import_config(*plan),
            Command::ReplaceIdentity { device_id, private_key } => self.replace_identity(device_id, private_key),
            Command::RestartBackends => {
                let _ = self.cap.send(CaptureCommand::Restart);
                let _ = self.emu.send(EmulationCommand::Restart);
                self.edges_sent = None;
                self.guard_sent = None;
                self.update_edges();
                Ok(())
            }
            Command::Shutdown => {
                self.shutting_down = true;
                Ok(())
            }
        }
    }

    fn set_settings(&mut self, mut s: Settings) -> Result<(), String> {
        s.device_name = s.device_name.trim().chars().take(64).collect();
        if s.device_name.is_empty() {
            return Err("Device name cannot be empty".into());
        }
        s.timing = s.timing.sanitized();
        if s.hotkeys.release.is_empty() {
            return Err("The release hotkey needs at least one key".into());
        }
        let old = std::mem::replace(&mut self.st.settings, s);
        let common = config::CommonSettings::of(&self.st.settings);
        if common != self.st.shared.common {
            // Shared with every paired computer, right away.
            let shared = config::SharedSettings { common, ..self.next_shared() };
            self.adopt_shared(shared, None);
        }
        self.settings_changed(old);
        Ok(())
    }

    /// A newer version of the shared settings, made here.
    pub(crate) fn next_shared(&self) -> config::SharedSettings {
        config::SharedSettings { rev: self.st.shared.rev + 1, author: self.me(), ..self.st.shared.clone() }
    }

    /// Put changed settings into effect (`old` were in use before).
    pub(crate) fn settings_changed(&mut self, old: Settings) {
        let new = &self.st.settings;
        if old.device_name != new.device_name {
            let name = new.device_name.clone();
            self.net.intro.write().name = name.clone();
            self.beacon.send_modify(|b| b.name = name.clone());
            if let Some(slot) = self.st.layout.slots.iter_mut().find(|s| s.device == self.st.device_id) {
                slot.name = name.clone();
            }
            self.broadcast(Msg::Rename { name });
        }
        if old.strict_cipher != new.strict_cipher {
            self.net.policy.write().strict = new.strict_cipher;
        }
        let disc = discovery_config(&self.st.settings, &self.overrides);
        self.disc_cfg.send_replace(disc);
        if old.clipboard.enabled && !self.st.settings.clipboard.enabled {
            self.clip = ClipState::default();
        }
        if old.timing.motion_interval_us != self.st.settings.timing.motion_interval_us {
            self.flush_motion();
        }
        self.save_now();
        self.mark_dirty();
    }

    fn identify(&mut self) {
        let number = self.st.layout.number_of(self.me()).unwrap_or(0);
        self.notice(Notice::Identify { number, name: self.st.settings.device_name.clone() });
        let targets: Vec<(DeviceId, u32)> = self
            .connected_peers()
            .into_iter()
            .map(|p| (p, self.st.layout.number_of(p).unwrap_or(0)))
            .collect();
        for (p, n) in targets {
            self.send_to(p, Msg::Identify { number: n });
        }
    }

    fn build_snapshot(&self) -> Snapshot {
        let me = MeView {
            id: self.me(),
            name: self.st.settings.device_name.clone(),
            os: os_tag().into(),
            fingerprint: self.identity.fingerprint(),
            port: self.port,
            addresses: discovery::local_addresses().into_iter().map(|a| a.to_string()).collect(),
            version: APP_VERSION.into(),
        };
        let mut peers: Vec<PeerView> = self.peers.values().map(|p| self.peer_view(p)).collect();
        peers.sort_by(|a, b| b.trusted.cmp(&a.trusted).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        let input = match (&self.input, &self.controlled) {
            (Input::Controlling(c), _) => InputView::Controlling { target: c.target, locked: self.locked },
            (Input::Local, Some(c)) => InputView::Controlled { by: c.peer },
            (Input::Local, None) => InputView::Local { locked: self.locked },
        };
        let mut transfers: Vec<TransferView> = self.transfers.values().map(|t| t.view()).collect();
        transfers.sort_by_key(|t| std::cmp::Reverse(t.id));
        Snapshot {
            me,
            peers,
            layout: self.st.layout.clone(),
            settings: self.st.settings.clone(),
            shared: self.st.shared.clone(),
            effective_cipher: self.st.shared.cipher.resolve(),
            hw_aes: has_hw_aes(),
            input,
            transfers,
            backends: self.backends.clone(),
            desktop: self.desktop.clone(),
            enabled: self.st.enabled,
            guard: self.guard.clone(),
            max_devices: config::MAX_DEVICES,
            searching: self.searching_until.is_some(),
        }
    }
}
