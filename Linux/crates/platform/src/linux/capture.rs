//! Input capture on Wayland through the `org.freedesktop.portal.InputCapture`
//! portal (GNOME 45+, KDE Plasma 6.1+). Captured events arrive over libei.
//!
//! GNOME quirks handled here:
//! * creating a session shows a permission dialog every time, so the session
//!   is kept alive for the lifetime of the app;
//! * barriers can only be changed while the session is disabled, and after a
//!   `Disable` the existing libei connection stops receiving events. We
//!   therefore drop the libei connection, change barriers, re-enable and
//!   connect a fresh libei client.

use ashpd::desktop::input_capture::{
    ActivatedBarrier, Barrier, Capabilities, ConnectToEISOptions, CreateSessionOptions, DisableOptions,
    EnableOptions, GetZonesOptions, InputCapture, ReleaseOptions, SetPointerBarriersOptions,
};
use ashpd::desktop::Session;
use futures::StreamExt;
use super::guard::Guard;
use kvmbridge_core::layout::Desktop;
use kvmbridge_core::platform::{BackendSide, BackendStatus, CaptureCommand, CaptureEvent};
use kvmbridge_core::types::{Edge, Rect};
use reis::ei;
use reis::event::{DeviceCapability, EiEvent};
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::os::unix::net::UnixStream;
use std::time::Duration;

type Side = BackendSide<CaptureCommand, CaptureEvent>;

fn status(side: &Side, s: BackendStatus) {
    let _ = side.events.send(CaptureEvent::Status(s));
}

pub async fn run(mut side: Side, guard: Guard) {
    let mut edges: Vec<Edge> = Vec::new();
    loop {
        status(&side, BackendStatus::Starting("Waiting for permission to capture input…".into()));
        match Capture::create(guard.clone()).await {
            Ok(mut cap) => {
                let reason = cap.serve(&mut side, &mut edges).await;
                let _ = cap.ic.release(&cap.session, ReleaseOptions::default()).await;
                let _ = cap.session.close().await;
                match reason {
                    Exit::ChannelClosed => return,
                    Exit::Restart => continue,
                    Exit::Failed(e) => {
                        tracing::warn!("input capture session ended: {e}");
                        status(&side, BackendStatus::Unavailable(format!("Input capture stopped: {e}")));
                    }
                }
            }
            Err(e) => {
                let msg = describe(&e);
                tracing::warn!("input capture unavailable: {msg}");
                status(&side, BackendStatus::Unavailable(msg));
            }
        }
        // Wait for an explicit restart (avoid spamming permission dialogs),
        // but keep tracking the edges we are asked for.
        loop {
            match side.commands.recv().await {
                None => return,
                Some(CaptureCommand::Restart) => break,
                Some(CaptureCommand::SetEdges(e)) => edges = e,
                Some(CaptureCommand::BlockLocalInput(b)) => guard.set(b),
                Some(_) => {}
            }
        }
    }
}

fn describe(e: &anyhow::Error) -> String {
    let s = format!("{e:#}");
    if s.contains("Cancelled") || s.contains("cancelled") {
        "Permission to capture input was denied. Click Retry to ask again.".into()
    } else if s.contains("No such interface") || s.contains("org.freedesktop.portal.InputCapture") || s.contains("ServiceUnknown") {
        "This desktop does not offer the InputCapture portal (needs GNOME 45+ or KDE Plasma 6.1+).".into()
    } else {
        format!("Input capture is not available: {s}")
    }
}

enum Exit {
    ChannelClosed,
    Restart,
    Failed(String),
}

struct Capture {
    ic: InputCapture,
    session: Session<InputCapture>,
    zones: Vec<Rect>,
    zone_set: u32,
    barriers: HashMap<u32, Edge>,
    enabled: bool,
    ei: Option<EiConn>,
    activation: Option<u32>,
    /// Pauses this computer's own keyboards and mice.
    guard: Guard,
}

struct EiConn {
    context: ei::Context,
    events: reis::tokio::EiConvertEventStream,
}

impl Capture {
    async fn create(guard: Guard) -> anyhow::Result<Capture> {
        let ic = InputCapture::new().await?;
        let (session, caps) = ic
            .create_session(
                None,
                CreateSessionOptions::default().set_capabilities(Capabilities::Keyboard | Capabilities::Pointer),
            )
            .await?;
        tracing::info!("input capture session created (capabilities: {caps:?})");
        let mut c = Capture {
            ic,
            session,
            zones: vec![],
            zone_set: 0,
            barriers: HashMap::new(),
            enabled: false,
            ei: None,
            activation: None,
            guard,
        };
        c.refresh_zones().await?;
        Ok(c)
    }

    async fn refresh_zones(&mut self) -> anyhow::Result<()> {
        let zones = self.ic.zones(&self.session, GetZonesOptions::default()).await?.response()?;
        self.zone_set = zones.zone_set();
        self.zones = zones
            .regions()
            .iter()
            .map(|r| Rect::new(r.x_offset() as f64, r.y_offset() as f64, r.width() as f64, r.height() as f64))
            .collect();
        Ok(())
    }

    fn desktop(&self) -> Desktop {
        Desktop::new(self.zones.clone())
    }

    /// Disable (if needed), set new barriers, enable and reconnect libei.
    async fn configure(&mut self, edges: &[Edge]) -> anyhow::Result<()> {
        if self.enabled {
            self.ic.disable(&self.session, DisableOptions::default()).await?;
            self.enabled = false;
            self.activation = None;
        }
        if self.ei.take().is_some() {
            // Let the compositor notice the old libei client went away before
            // a new one connects (it only accepts one client per session).
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        let desk = self.desktop();
        let mut barriers = Vec::new();
        self.barriers.clear();
        let mut next_id = 1u32;
        for &edge in edges {
            for (fixed, a, z) in desk.outer_segments(edge) {
                let (fixed, a, z) = (fixed.round() as i32, a.round() as i32, z.round() as i32);
                let pos = match edge {
                    Edge::Left | Edge::Right => (fixed, a, fixed, z - 1),
                    Edge::Top | Edge::Bottom => (a, fixed, z - 1, fixed),
                };
                let id = NonZeroU32::new(next_id).unwrap();
                self.barriers.insert(next_id, edge);
                barriers.push(Barrier::new(id, pos));
                next_id += 1;
            }
        }
        let resp = self
            .ic
            .set_pointer_barriers(&self.session, &barriers, self.zone_set, SetPointerBarriersOptions::default())
            .await?
            .response()?;
        for failed in resp.failed_barriers() {
            tracing::warn!("compositor rejected barrier {failed} ({:?})", self.barriers.get(&failed.get()));
            self.barriers.remove(&failed.get());
        }
        if self.barriers.is_empty() {
            tracing::info!("no screen edges lead anywhere; capture idle");
            return Ok(());
        }
        let fd = self.ic.connect_to_eis(&self.session, ConnectToEISOptions::default()).await?;
        let context = ei::Context::new(UnixStream::from(fd))?;
        let (_conn, events) = context.handshake_tokio("kvmbridge-capture", ei::handshake::ContextType::Receiver).await?;
        self.ei = Some(EiConn { context, events });
        self.ic.enable(&self.session, EnableOptions::default()).await?;
        self.enabled = true;
        tracing::info!("input capture armed on {:?}", self.barriers.values().collect::<Vec<_>>());
        Ok(())
    }

    async fn release(&mut self, pos: Option<(f64, f64)>) {
        let Some(act) = self.activation.take() else { return };
        let mut opts = ReleaseOptions::default().set_activation_id(act);
        if let Some(p) = pos {
            opts = opts.set_cursor_position(p);
        }
        if let Err(e) = self.ic.release(&self.session, opts).await {
            tracing::debug!("release failed: {e}");
        }
    }

    async fn serve(&mut self, side: &mut Side, edges: &mut Vec<Edge>) -> Exit {
        // A second proxy for signal streams so they do not borrow `self`.
        let sig = match InputCapture::new().await {
            Ok(s) => s,
            Err(e) => return Exit::Failed(e.to_string()),
        };
        let (mut activated, mut deactivated, mut disabled, mut zones_changed) = match futures::try_join!(
            sig.receive_activated(),
            sig.receive_deactivated(),
            sig.receive_disabled(),
            sig.receive_zones_changed()
        ) {
            Ok(s) => s,
            Err(e) => return Exit::Failed(e.to_string()),
        };
        let _ = side.events.send(CaptureEvent::Geometry(self.desktop()));
        status(side, BackendStatus::Ready("InputCapture portal".into()));
        if let Err(e) = self.configure(edges).await {
            return Exit::Failed(format!("{e:#}"));
        }

        loop {
            tokio::select! {
                cmd = side.commands.recv() => match cmd {
                    None => return Exit::ChannelClosed,
                    Some(CaptureCommand::SetEdges(e)) => {
                        if &e != edges {
                            *edges = e;
                            if let Err(e) = self.configure(edges).await {
                                return Exit::Failed(format!("{e:#}"));
                            }
                        }
                    }
                    Some(CaptureCommand::Release { pos }) => self.release(pos.map(|p| (p.x, p.y))).await,
                    Some(CaptureCommand::Enable(_)) => {}
                    Some(CaptureCommand::Restart) => return Exit::Restart,
                    Some(CaptureCommand::BlockLocalInput(b)) => self.guard.set(b),
                },
                Some(a) = activated.next() => {
                    let Some(id) = a.activation_id() else { continue };
                    let edge = match a.barrier_id() {
                        Some(ActivatedBarrier::Barrier(b)) => self.barriers.get(&b.get()).copied(),
                        _ => None,
                    };
                    let pos = a.cursor_position().map(|(x, y)| kvmbridge_core::types::Point::new(x as f64, y as f64));
                    self.activation = Some(id);
                    match (edge, pos) {
                        (Some(edge), Some(pos)) => {
                            let _ = side.events.send(CaptureEvent::Activated { edge, pos });
                        }
                        _ => {
                            tracing::debug!("activation without usable barrier/position; releasing");
                            self.release(None).await;
                        }
                    }
                }
                Some(_d) = deactivated.next() => {
                    if self.activation.take().is_some() {
                        let _ = side.events.send(CaptureEvent::Deactivated);
                    }
                }
                Some(_d) = disabled.next() => {
                    // The compositor disabled us (e.g. the user pressed the
                    // "cancel input capture" shortcut). Re-arm.
                    tracing::info!("input capture disabled by the compositor; re-arming");
                    self.enabled = false;
                    if self.activation.take().is_some() {
                        let _ = side.events.send(CaptureEvent::Deactivated);
                    }
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    if let Err(e) = self.configure(edges).await {
                        return Exit::Failed(format!("{e:#}"));
                    }
                }
                Some(_z) = zones_changed.next() => {
                    if let Err(e) = self.refresh_zones().await {
                        return Exit::Failed(format!("{e:#}"));
                    }
                    let _ = side.events.send(CaptureEvent::Geometry(self.desktop()));
                    if let Err(e) = self.configure(edges).await {
                        return Exit::Failed(format!("{e:#}"));
                    }
                }
                ev = next_ei(&mut self.ei) => match ev {
                    Some(Ok(ev)) => self.on_ei(side, ev),
                    Some(Err(e)) => {
                        tracing::warn!("libei error: {e}");
                        self.ei = None;
                    }
                    None => {
                        tracing::info!("libei connection closed");
                        self.ei = None;
                    }
                },
            }
        }
    }

    fn on_ei(&mut self, side: &Side, ev: EiEvent) {
        let send = |e| {
            let _ = side.events.send(e);
        };
        match ev {
            EiEvent::SeatAdded(s) => {
                s.seat.bind_capabilities(
                    DeviceCapability::Pointer
                        | DeviceCapability::Keyboard
                        | DeviceCapability::Button
                        | DeviceCapability::Scroll,
                );
                if let Some(c) = &self.ei {
                    let _ = c.context.flush();
                }
            }
            // Only forward input while we hold an activation.
            _ if self.activation.is_none() => {}
            EiEvent::PointerMotion(m) => send(CaptureEvent::Motion { dx: m.dx as f64, dy: m.dy as f64 }),
            EiEvent::Button(b) => send(CaptureEvent::Button {
                button: b.button,
                pressed: b.state == ei::button::ButtonState::Press,
            }),
            EiEvent::ScrollDelta(s) => send(CaptureEvent::Scroll { dx: s.dx as f64, dy: s.dy as f64 }),
            EiEvent::ScrollDiscrete(s) => send(CaptureEvent::ScrollDiscrete { dx: s.discrete_dx, dy: s.discrete_dy }),
            EiEvent::KeyboardKey(k) => send(CaptureEvent::Key {
                code: k.key,
                pressed: k.state == ei::keyboard::KeyState::Press,
            }),
            EiEvent::Disconnected(d) => tracing::info!("libei disconnected: {:?} {:?}", d.reason, d.explanation),
            _ => {}
        }
    }
}

async fn next_ei(ei: &mut Option<EiConn>) -> Option<Result<EiEvent, reis::Error>> {
    match ei {
        Some(c) => c.events.next().await,
        None => std::future::pending().await,
    }
}
