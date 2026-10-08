//! Input emulation and clipboard access on Wayland through the
//! `org.freedesktop.portal.RemoteDesktop` portal (input via libei) and the
//! `org.freedesktop.portal.Clipboard` portal attached to the same session.
//!
//! The permission is persisted with a restore token so the dialog is only
//! shown the first time.

use super::{clipboard_mimes, mime_to_format, now_us, selection};
use ashpd::desktop::clipboard::{Clipboard, RequestClipboardOptions, SetSelectionOptions};
use ashpd::desktop::remote_desktop::{
    ConnectToEISOptions, DeviceType, RemoteDesktop, SelectDevicesOptions, StartOptions,
};
use ashpd::desktop::{CreateSessionOptions, PersistMode, Session};
use futures::StreamExt;
use kvmbridge_core::platform::{
    BackendSide, BackendStatus, ClipboardCommand, ClipboardEvent, EmulationCommand, EmulationEvent,
};
use kvmbridge_core::types::{ClipFormat, Point};
use reis::ei;
use reis::event::{Device, DeviceCapability, EiEvent};
use std::collections::HashSet;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;

type EmuSide = BackendSide<EmulationCommand, EmulationEvent>;
type ClipSide = BackendSide<ClipboardCommand, ClipboardEvent>;

const MAX_CLIPBOARD: usize = 256 * 1024 * 1024;

/// A session that lasted at least this long counts as a good one when it ends.
const STABLE_SESSION: Duration = Duration::from_secs(60);
/// Reconnect attempts after sessions that ended soon after starting.
const MAX_QUICK_RECONNECTS: u32 = 3;

pub async fn run(mut emu: EmuSide, mut clip: ClipSide, token_file: PathBuf) {
    let mut quick_ends = 0u32;
    loop {
        let _ = emu.events.send(EmulationEvent::Status(BackendStatus::Starting(
            "Waiting for permission to control this computer…".into(),
        )));
        let _ = clip.events.send(ClipboardEvent::Status(BackendStatus::Starting("Waiting for remote desktop session…".into())));
        let started = std::time::Instant::now();
        let exit = match Remote::create(&token_file).await {
            Ok(mut r) => {
                let e = r.serve(&mut emu, &mut clip).await;
                let _ = r.session.close().await;
                e
            }
            Err(e) => Exit::Failed(describe(&e)),
        };
        let retry_in = match exit {
            Exit::ChannelClosed => return,
            Exit::Restart => {
                quick_ends = 0;
                continue;
            }
            Exit::Closed(msg) => {
                // The desktop ended a working session. The saved permission
                // lets us start a new one without asking.
                quick_ends = if started.elapsed() >= STABLE_SESSION { 1 } else { quick_ends + 1 };
                let has_token = std::fs::read_to_string(&token_file).is_ok_and(|t| !t.trim().is_empty());
                if has_token && quick_ends <= MAX_QUICK_RECONNECTS {
                    tracing::warn!("remote desktop backend: {msg}; reconnecting");
                    Some(Duration::from_secs(2u64.pow(quick_ends)))
                } else {
                    tracing::warn!("remote desktop backend: {msg}");
                    let msg = format!("{msg}. Click Retry to reconnect.");
                    let _ = emu.events.send(EmulationEvent::Status(BackendStatus::Unavailable(msg.clone())));
                    let _ = clip.events.send(ClipboardEvent::Status(BackendStatus::Unavailable(msg)));
                    None
                }
            }
            Exit::Failed(msg) => {
                tracing::warn!("remote desktop backend: {msg}");
                let _ = emu.events.send(EmulationEvent::Status(BackendStatus::Unavailable(msg.clone())));
                let _ = clip.events.send(ClipboardEvent::Status(BackendStatus::Unavailable(msg)));
                None
            }
        };
        // Wait for the reconnect delay or an explicit retry; answer
        // clipboard reads with nothing meanwhile.
        let deadline = retry_in.map(|d| tokio::time::Instant::now() + d);
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline.unwrap_or_else(tokio::time::Instant::now)), if deadline.is_some() => break,
                c = emu.commands.recv() => match c {
                    None => return,
                    Some(EmulationCommand::Restart) => {
                        quick_ends = 0;
                        break;
                    }
                    Some(_) => {}
                },
                c = clip.commands.recv() => match c {
                    None => return,
                    Some(ClipboardCommand::Read { reply, .. }) => { let _ = reply.send(None); }
                    Some(_) => {}
                },
            }
        }
    }
}

fn describe(e: &anyhow::Error) -> String {
    let s = format!("{e:#}");
    if s.contains("Cancelled") || s.contains("cancelled") {
        "Permission to control this computer was denied. Click Retry to ask again.".into()
    } else if s.contains("ServiceUnknown") || s.contains("No such interface") {
        "This desktop does not offer the RemoteDesktop portal.".into()
    } else {
        format!("Remote control is not available: {s}")
    }
}

enum Exit {
    ChannelClosed,
    Restart,
    /// A running session ended.
    Closed(String),
    Failed(String),
}

#[derive(Default)]
struct Devices {
    abs: Option<Device>,
    rel: Option<Device>,
    kbd: Option<Device>,
    resumed: HashSet<u64>,
    emulating: HashSet<u64>,
    sequence: u32,
}

fn dev_id(d: &Device) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    d.device().hash(&mut h);
    h.finish()
}

struct Remote {
    rd: RemoteDesktop,
    session: Arc<Session<RemoteDesktop>>,
    clipboard: Option<Arc<ClipboardCtx>>,
    context: ei::Context,
    connection: reis::event::Connection,
    events: reis::tokio::EiConvertEventStream,
    dev: Devices,
    keys: HashSet<u32>,
    buttons: HashSet<u32>,
    active: bool,
    last: Option<Point>,
}

struct ClipboardCtx {
    cb: Clipboard,
    session: Arc<Session<RemoteDesktop>>,
}

impl Remote {
    async fn create(token_file: &PathBuf) -> anyhow::Result<Remote> {
        let rd = RemoteDesktop::new().await?;
        let session = rd.create_session(CreateSessionOptions::default()).await?;
        let token = std::fs::read_to_string(token_file).ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        rd.select_devices(
            &session,
            SelectDevicesOptions::default()
                .set_devices(DeviceType::Keyboard | DeviceType::Pointer)
                .set_persist_mode(PersistMode::ExplicitlyRevoked)
                .set_restore_token(token.as_deref()),
        )
        .await?
        .response()?;
        // The clipboard must be requested before the session starts.
        let clipboard = match Clipboard::new().await {
            Ok(cb) => match cb.request(&session, RequestClipboardOptions::default()).await {
                Ok(()) => Some(cb),
                Err(e) => {
                    tracing::info!("clipboard portal unavailable: {e}");
                    None
                }
            },
            Err(e) => {
                tracing::info!("clipboard portal unavailable: {e}");
                None
            }
        };
        let started = rd.start(&session, None, StartOptions::default()).await?.response()?;
        if let Some(t) = started.restore_token() {
            if let Some(dir) = token_file.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(token_file, t);
        }
        let clipboard_ok = started.is_clipboard_enabled();
        tracing::info!("remote desktop session started: devices {:?}, clipboard {clipboard_ok}", started.devices());
        let session = Arc::new(session);
        let clipboard = match (clipboard, clipboard_ok) {
            (Some(cb), true) => Some(Arc::new(ClipboardCtx { cb, session: session.clone() })),
            _ => None,
        };
        let fd = rd.connect_to_eis(&session, ConnectToEISOptions::default()).await?;
        let context = ei::Context::new(UnixStream::from(fd))?;
        let (connection, events) = context.handshake_tokio("kvmbridge", ei::handshake::ContextType::Sender).await?;
        Ok(Remote {
            rd,
            session,
            clipboard,
            context,
            connection,
            events,
            dev: Devices::default(),
            keys: HashSet::new(),
            buttons: HashSet::new(),
            active: false,
            last: None,
        })
    }

    async fn serve(&mut self, emu: &mut EmuSide, clip: &mut ClipSide) -> Exit {
        let _ = &self.rd;
        let mut clip_state = ClipState::default();
        let cbctx = self.clipboard.clone();
        // Signal streams (separate proxy so they do not borrow `self`).
        let sig = if cbctx.is_some() { Clipboard::new().await.ok() } else { None };
        let (mut owner_changed, mut transfers) = match &sig {
            Some(sig) => {
                // Owner changes are decoded by `selection`, not ashpd (see there).
                let owner = sig.receive_signal("SelectionOwnerChanged").await.map_err(|e| e.to_string());
                let transfer = sig.receive_selection_transfer::<RemoteDesktop>().await.map_err(|e| e.to_string());
                match (owner, transfer) {
                    (Ok(a), Ok(b)) => (
                        Some(a.filter_map(|m| std::future::ready(selection::owner_change(&m))).boxed()),
                        Some(b.boxed()),
                    ),
                    (Err(e), _) | (_, Err(e)) => {
                        tracing::warn!("clipboard signals unavailable: {e}");
                        (None, None)
                    }
                }
            }
            None => (None, None),
        };
        match &cbctx {
            Some(_) => {
                let _ = clip.events.send(ClipboardEvent::Status(BackendStatus::Ready("Clipboard portal".into())));
            }
            None => {
                let _ = clip.events.send(ClipboardEvent::Status(BackendStatus::Unavailable(
                    "The desktop did not grant clipboard access".into(),
                )));
            }
        }
        let mut ready_sent = false;

        loop {
            tokio::select! {
                biased;
                cmd = emu.commands.recv() => match cmd {
                    None => return Exit::ChannelClosed,
                    Some(EmulationCommand::Restart) => return Exit::Restart,
                    Some(c) => self.emulate(c),
                },
                ev = self.events.next() => match ev {
                    Some(Ok(ev)) => {
                        if self.on_ei(ev) && !ready_sent {
                            ready_sent = true;
                            let kind = if self.dev.abs.is_some() { "RemoteDesktop portal (absolute)" } else { "RemoteDesktop portal (relative)" };
                            let _ = emu.events.send(EmulationEvent::Status(BackendStatus::Ready(kind.into())));
                        }
                    }
                    Some(Err(e)) => return Exit::Closed(format!("libei error: {e}")),
                    None => return Exit::Closed("the remote desktop session was closed".into()),
                },
                cmd = clip.commands.recv() => match cmd {
                    None => return Exit::ChannelClosed,
                    Some(c) => clip_state.command(c, &cbctx),
                },
                // Portals only deliver session signals to the session owner,
                // and we own exactly one remote desktop session.
                Some(change) = next_opt(&mut owner_changed) => {
                    if change.session_is_owner { continue; }
                    let mimes = change.mime_types;
                    let mut formats: Vec<ClipFormat> = Vec::new();
                    for m in &mimes {
                        if let Some(f) = mime_to_format(m) {
                            if !formats.contains(&f) { formats.push(f); }
                        }
                    }
                    clip_state.local_mimes = mimes;
                    if !formats.is_empty() {
                        let _ = clip.events.send(ClipboardEvent::LocalChanged { formats });
                    }
                }
                Some((_session, mime, serial)) = next_opt(&mut transfers) => {
                    clip_state.transfer(mime, serial, &cbctx, clip);
                }
            }
        }
    }

    /// Returns true once a usable device exists.
    fn on_ei(&mut self, ev: EiEvent) -> bool {
        match ev {
            EiEvent::SeatAdded(s) => {
                s.seat.bind_capabilities(
                    DeviceCapability::Pointer
                        | DeviceCapability::PointerAbsolute
                        | DeviceCapability::Keyboard
                        | DeviceCapability::Button
                        | DeviceCapability::Scroll,
                );
                let _ = self.context.flush();
            }
            EiEvent::DeviceAdded(d) => {
                let dev = d.device;
                tracing::debug!(
                    "libei device {:?} type {:?} regions {:?}",
                    dev.name(),
                    dev.device_type(),
                    dev.regions()
                );
                if dev.has_capability(DeviceCapability::PointerAbsolute) {
                    self.dev.abs = Some(dev);
                } else if dev.has_capability(DeviceCapability::Pointer) {
                    self.dev.rel = Some(dev);
                } else if dev.has_capability(DeviceCapability::Keyboard) {
                    self.dev.kbd = Some(dev);
                }
            }
            EiEvent::DeviceRemoved(d) => {
                let id = dev_id(&d.device);
                self.dev.resumed.remove(&id);
                self.dev.emulating.remove(&id);
                for slot in [&mut self.dev.abs, &mut self.dev.rel, &mut self.dev.kbd] {
                    if slot.as_ref().is_some_and(|x| dev_id(x) == id) {
                        *slot = None;
                    }
                }
            }
            EiEvent::DeviceResumed(d) => {
                self.dev.resumed.insert(dev_id(&d.device));
                if self.active {
                    self.start_emulating(&d.device);
                }
            }
            EiEvent::DevicePaused(d) => {
                let id = dev_id(&d.device);
                self.dev.resumed.remove(&id);
                self.dev.emulating.remove(&id);
            }
            EiEvent::Disconnected(d) => {
                tracing::warn!("libei disconnected: {:?} {:?}", d.reason, d.explanation);
            }
            _ => {}
        }
        let pointer = self.dev.abs.as_ref().or(self.dev.rel.as_ref());
        pointer.is_some_and(|p| self.dev.resumed.contains(&dev_id(p)))
    }

    fn serial(&self) -> u32 {
        self.connection.serial()
    }

    fn start_emulating(&mut self, d: &Device) {
        let id = dev_id(d);
        if !self.dev.resumed.contains(&id) || self.dev.emulating.contains(&id) {
            return;
        }
        self.dev.sequence = self.dev.sequence.wrapping_add(1);
        d.device().start_emulating(self.serial(), self.dev.sequence);
        self.dev.emulating.insert(id);
    }

    fn stop_emulating_all(&mut self) {
        for d in [self.dev.abs.clone(), self.dev.rel.clone(), self.dev.kbd.clone()].into_iter().flatten() {
            if self.dev.emulating.remove(&dev_id(&d)) {
                d.device().stop_emulating(self.serial());
            }
        }
    }

    fn ensure_emulating(&mut self) {
        for d in [self.dev.abs.clone(), self.dev.rel.clone(), self.dev.kbd.clone()].into_iter().flatten() {
            self.start_emulating(&d);
        }
    }

    fn frame(&self, d: &Device) {
        d.device().frame(self.serial(), now_us());
    }

    fn pointer_dev(&self) -> Option<Device> {
        self.dev.abs.clone().or_else(|| self.dev.rel.clone())
    }

    fn move_to(&mut self, p: Point) {
        if let Some(abs) = self.dev.abs.clone() {
            if let Some(i) = abs.interface::<ei::PointerAbsolute>() {
                i.motion_absolute(p.x as f32, p.y as f32);
                self.frame(&abs);
            }
        } else if let Some(rel) = self.dev.rel.clone() {
            if let Some(i) = rel.interface::<ei::Pointer>() {
                match self.last {
                    Some(l) => i.motion_relative((p.x - l.x) as f32, (p.y - l.y) as f32),
                    None => {
                        // Unknown position: slam into the top-left corner first.
                        i.motion_relative(-100_000.0, -100_000.0);
                        self.frame(&rel);
                        i.motion_relative(p.x as f32, p.y as f32);
                    }
                }
                self.frame(&rel);
            }
        }
        self.last = Some(p);
    }

    fn emulate(&mut self, c: EmulationCommand) {
        match c {
            EmulationCommand::Enter { pos } => {
                self.active = true;
                self.ensure_emulating();
                self.move_to(pos);
            }
            EmulationCommand::MoveAbs { pos } => {
                if !self.active {
                    self.active = true;
                    self.ensure_emulating();
                }
                self.move_to(pos);
            }
            EmulationCommand::MoveRel { dx, dy } => {
                self.ensure_emulating();
                if let Some(rel) = self.dev.rel.clone() {
                    if let Some(i) = rel.interface::<ei::Pointer>() {
                        i.motion_relative(dx as f32, dy as f32);
                        self.frame(&rel);
                    }
                }
                if let Some(l) = &mut self.last {
                    l.x += dx;
                    l.y += dy;
                }
            }
            EmulationCommand::Button { button, pressed } => {
                // Also used outside a remote session (drag and drop).
                self.ensure_emulating();
                if let Some(d) = self.pointer_dev() {
                    if let Some(i) = d.interface::<ei::Button>() {
                        i.button(button, if pressed { ei::button::ButtonState::Press } else { ei::button::ButtonState::Released });
                        self.frame(&d);
                        if pressed {
                            self.buttons.insert(button);
                        } else {
                            self.buttons.remove(&button);
                        }
                    }
                }
            }
            EmulationCommand::Key { code, pressed } => {
                if let Some(d) = self.dev.kbd.clone() {
                    if let Some(i) = d.interface::<ei::Keyboard>() {
                        i.key(code, if pressed { ei::keyboard::KeyState::Press } else { ei::keyboard::KeyState::Released });
                        self.frame(&d);
                        if pressed {
                            self.keys.insert(code);
                        } else {
                            self.keys.remove(&code);
                        }
                    }
                }
            }
            EmulationCommand::Scroll { dx, dy } => {
                if let Some(d) = self.pointer_dev() {
                    if let Some(i) = d.interface::<ei::Scroll>() {
                        i.scroll(dx as f32, dy as f32);
                        self.frame(&d);
                    }
                }
            }
            EmulationCommand::ScrollDiscrete { dx, dy } => {
                if let Some(d) = self.pointer_dev() {
                    if let Some(i) = d.interface::<ei::Scroll>() {
                        i.scroll_discrete(dx, dy);
                        self.frame(&d);
                    }
                }
            }
            EmulationCommand::Leave { park } => {
                self.release_all();
                if let Some(p) = park {
                    self.move_to(p);
                }
                self.stop_emulating_all();
                self.active = false;
            }
            EmulationCommand::Restart => {}
        }
        let _ = self.context.flush();
    }

    fn release_all(&mut self) {
        if let Some(d) = self.dev.kbd.clone() {
            if let Some(i) = d.interface::<ei::Keyboard>() {
                for k in self.keys.drain() {
                    i.key(k, ei::keyboard::KeyState::Released);
                }
                self.frame(&d);
            }
        }
        if let Some(d) = self.pointer_dev() {
            if let Some(i) = d.interface::<ei::Button>() {
                for b in self.buttons.drain() {
                    i.button(b, ei::button::ButtonState::Released);
                }
                self.frame(&d);
            }
        }
        self.keys.clear();
        self.buttons.clear();
    }
}

async fn next_opt<S: futures::Stream + Unpin>(s: &mut Option<S>) -> Option<S::Item> {
    match s {
        Some(s) => s.next().await,
        None => std::future::pending().await,
    }
}

/// Clipboard bookkeeping.
#[derive(Default)]
struct ClipState {
    /// Mime types offered by the local owner of the clipboard.
    local_mimes: Vec<String>,
    /// Offer currently advertised on behalf of a remote device.
    remote_offer: Option<u64>,
}

impl ClipState {
    fn command(&mut self, c: ClipboardCommand, ctx: &Option<Arc<ClipboardCtx>>) {
        let Some(ctx) = ctx.clone() else {
            if let ClipboardCommand::Read { reply, .. } = c {
                let _ = reply.send(None);
            }
            return;
        };
        match c {
            ClipboardCommand::AdvertiseRemote { offer, formats } => {
                self.remote_offer = Some(offer);
                let mimes: Vec<&'static str> = formats.iter().flat_map(|f| clipboard_mimes(*f).iter().copied()).collect();
                tokio::spawn(async move {
                    if let Err(e) = ctx.cb.set_selection(&ctx.session, SetSelectionOptions::default().set_mime_types(&mimes)).await {
                        tracing::warn!("cannot set clipboard: {e}");
                    }
                });
            }
            ClipboardCommand::Read { format, reply } => {
                // Pick the best mime type the owner offers for this format.
                let mime = clipboard_mimes(format)
                    .iter()
                    .find(|m| self.local_mimes.iter().any(|l| l == *m))
                    .map(|s| s.to_string())
                    .or_else(|| self.local_mimes.iter().find(|m| mime_to_format(m) == Some(format)).cloned());
                let Some(mime) = mime else {
                    let _ = reply.send(None);
                    return;
                };
                tokio::spawn(async move {
                    let data = read_selection(&ctx, &mime).await;
                    let data = data.map(|d| if format == ClipFormat::Files { normalize_file_list(&mime, &d) } else { d });
                    let _ = reply.send(data);
                });
            }
        }
    }

    fn transfer(&mut self, mime: String, serial: u32, ctx: &Option<Arc<ClipboardCtx>>, clip: &ClipSide) {
        let Some(ctx) = ctx.clone() else { return };
        let (Some(offer), Some(format)) = (self.remote_offer, mime_to_format(&mime)) else {
            tracing::info!("an application asked for clipboard type {mime}, which is not shared");
            tokio::spawn(async move {
                let _ = ctx.cb.selection_write_done(&ctx.session, serial, false).await;
            });
            return;
        };
        let (tx, rx) = oneshot::channel();
        let _ = clip.events.send(ClipboardEvent::Paste { offer, format, reply: tx });
        tokio::spawn(async move {
            let wait = if format == ClipFormat::Files { Duration::from_secs(3600) } else { Duration::from_secs(30) };
            let data = tokio::time::timeout(wait, rx).await.ok().and_then(|r| r.ok()).flatten();
            let ok = match data {
                Some(d) => {
                    let d = if format == ClipFormat::Files { encode_file_list(&mime, &d) } else { d };
                    match write_selection(&ctx, serial, &d).await {
                        Ok(()) => true,
                        Err(e) => {
                            tracing::warn!("cannot hand the pasted data to the application: {e:#}");
                            false
                        }
                    }
                }
                None => false,
            };
            let _ = ctx.cb.selection_write_done(&ctx.session, serial, ok).await;
        });
    }
}

async fn read_selection(ctx: &ClipboardCtx, mime: &str) -> Option<Vec<u8>> {
    let fd = match ctx.cb.selection_read(&ctx.session, mime).await {
        Ok(fd) => fd,
        Err(e) => {
            tracing::warn!("cannot read the clipboard ({mime}): {e}");
            return None;
        }
    };
    let fd: OwnedFd = fd.into();
    let file = tokio::fs::File::from_std(std::fs::File::from(fd));
    let mut out = Vec::new();
    let res = tokio::time::timeout(Duration::from_secs(15), file.take(MAX_CLIPBOARD as u64).read_to_end(&mut out)).await;
    match res {
        Ok(Ok(_)) => Some(out),
        _ => None,
    }
}

async fn write_selection(ctx: &ClipboardCtx, serial: u32, data: &[u8]) -> anyhow::Result<()> {
    let fd = ctx.cb.selection_write(&ctx.session, serial).await?;
    let fd: OwnedFd = fd.into();
    let mut file = tokio::fs::File::from_std(std::fs::File::from(fd));
    file.write_all(data).await?;
    file.flush().await?;
    Ok(())
}

/// Turn whatever file list format the owner offered into a `text/uri-list`.
fn normalize_file_list(mime: &str, data: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(data);
    let paths = kvmbridge_core::transfer::parse_uri_list(&text);
    let _ = mime;
    kvmbridge_core::transfer::uri_list(&paths).into_bytes()
}

/// Encode a `text/uri-list` for the requested mime type.
fn encode_file_list(mime: &str, uri_list: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(uri_list);
    let uris: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if mime == "x-special/gnome-copied-files" {
        format!("copy\n{}", uris.join("\n")).into_bytes()
    } else if mime.starts_with("text/plain") || mime == "UTF8_STRING" {
        let paths = kvmbridge_core::transfer::parse_uri_list(&text);
        paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n").into_bytes()
    } else {
        uri_list.to_vec()
    }
}
