//! Drag-and-drop of files between screens.
//!
//! Controller side: a mouse button held while the cursor left this screen
//! and let go on another one may have dragged files along. Only then the
//! platform is asked to *probe* for a file drag in progress (normal crossings
//! show nothing). The compositor only hands a drag to the probe window when
//! the pointer moves, so input capture is let go for a moment ("handoff"),
//! the local drag is read and cancelled, and the pointer is pushed against
//! the edge again to capture input on the same screen. If files were found,
//! they stream to the target, which begins a native drag under its cursor and
//! drops it right away into whatever application is there; that application
//! receives the files once the transfer has completed. Drops are always
//! copies: the files arrive in a holding folder the application copies them
//! from, and the originals stay where they are. Like any drop on that
//! computer, if nothing under the cursor takes the files, nothing happens.

use super::*;
use crate::platform::ProbeOutcome;
use crate::proto::{ItemMeta, TransferPurpose, MAX_SUMMARY_ITEMS};
use crate::transfer::Received;
use crate::types::{Edge, Point};

/// Longest time input capture stays let go for a probe. If the pointer has
/// not been captured again by then, the cursor stays on this screen.
const HANDOFF_TIMEOUT: Duration = Duration::from_secs(3);
/// When to wiggle the pointer after letting go of capture (the release has
/// to have reached the compositor, or the motion would be captured too).
const NUDGE_AFTER: [u64; 2] = [80, 250];
/// How far towards the middle of its monitor the pointer goes for a probe:
/// the probe window only covers the work area, without the docks and panels
/// along the edges.
const PROBE_INSET: f64 = 0.25;

#[derive(Default)]
pub(crate) struct DndState {
    /// Probe issued when the cursor left this screen.
    probe: Option<u64>,
    /// Outcome of the probe, held back until the cursor is home again (the
    /// backend ends the local drag then, which needs uncaptured input).
    held_outcome: Option<(u64, ProbeOutcome)>,
    outgoing: Option<OutDrag>,
    incoming: HashMap<u64, InDrag>,
    /// Input capture is let go for the probe.
    handoff: Option<Handoff>,
}

struct Handoff {
    probe: u64,
    edge: Edge,
    /// Just inside the edge where the cursor left (where it goes back to).
    at: Point,
    deadline: Instant,
    /// This computer's own emulated pointer was used (to be reset after).
    moved: bool,
}

struct OutDrag {
    drag: u64,
    target: DeviceId,
    transfer: u64,
}

struct InDrag {
    peer: DeviceId,
    transfer: Option<u64>,
    result: Option<Result<Vec<PathBuf>, String>>,
    reply: Option<oneshot::Sender<Option<Vec<PathBuf>>>>,
    /// The native drag finished (the entry only lives on to deliver data).
    ended: bool,
}

impl Engine {
    fn dnd_enabled(&self) -> bool {
        self.st.enabled && self.st.settings.transfers.drag_and_drop && self.backends.dnd.is_ready()
    }

    /// The cursor just left this screen: an earlier probe is over. Its
    /// outcome waits until the cursor is home again, because ending the local
    /// drag needs input that is not captured.
    pub(crate) fn drag_crossed_out(&mut self) {
        if let Some(old) = self.dnd.probe.take() {
            self.dnd.held_outcome = Some((old, ProbeOutcome::Abandoned));
        }
    }

    /// `button` was held while the cursor left this screen and has just been
    /// let go on another one. Returns true if a probe for a file drag takes
    /// the release over (the target never saw the press anyway).
    pub(crate) fn probe_drag(&mut self, button: u32) -> bool {
        if button != crate::types::buttons::BTN_LEFT || !self.dnd_enabled() || self.dnd.probe.is_some() {
            return false;
        }
        let Input::Controlling(c) = &self.input else { return false };
        // The local pointer is still where it left the screen.
        let pos = c.exit_pos;
        tracing::info!("a button held across the edge was let go on {}; checking for a file drag", self.peer_label(c.target));
        let probe = crate::crypto::random_u64();
        self.dnd.probe = Some(probe);
        let _ = self.dndcmd.send(DndCommand::Probe { probe, pos });
        true
    }

    pub(crate) fn on_dnd(&mut self, ev: DndEvent) {
        match ev {
            DndEvent::Status(s) => {
                tracing::info!("drag-and-drop backend: {s:?}");
                self.backends.dnd = s;
                self.broadcast_caps();
                self.mark_dirty();
            }
            DndEvent::ProbeReady { probe } => self.release_for_probe(probe),
            DndEvent::Probed { probe, paths, dragging } => {
                if self.dnd.probe != Some(probe) {
                    return;
                }
                if self.dnd.handoff.as_ref().is_some_and(|h| h.probe == probe) {
                    self.end_handoff_drag(probe, dragging);
                }
                if paths.is_empty() {
                    tracing::info!("no files were being dragged");
                    return;
                }
                let Input::Controlling(c) = &self.input else { return };
                let target = c.target;
                tracing::info!("dragging {} item(s) to {:?}", paths.len(), target);
                self.begin_out_drag(target, paths);
            }
            DndEvent::DropRequested { drag, reply } => {
                let Some(d) = self.dnd.incoming.get_mut(&drag) else {
                    let _ = reply.send(None);
                    return;
                };
                match &d.result {
                    Some(Ok(paths)) => {
                        let _ = reply.send(Some(paths.clone()));
                    }
                    Some(Err(_)) => {
                        let _ = reply.send(None);
                    }
                    None => d.reply = Some(reply),
                }
            }
            DndEvent::InjectButton { pressed } => {
                let _ = self.emu.send(EmulationCommand::Button { button: crate::types::buttons::BTN_LEFT, pressed });
            }
            DndEvent::InjectMove { dx, dy } => {
                let _ = self.emu.send(EmulationCommand::MoveRel { dx, dy });
            }
            DndEvent::InjectEscape => {
                // Never while input is captured: the key would go to another screen.
                if !matches!(self.input, Input::Controlling(_)) {
                    let esc = crate::types::keys::KEY_ESC;
                    let _ = self.emu.send(EmulationCommand::Key { code: esc, pressed: true });
                    let _ = self.emu.send(EmulationCommand::Key { code: esc, pressed: false });
                }
            }
            DndEvent::DragEnded { drag, accepted } => {
                let Some(d) = self.dnd.incoming.get_mut(&drag) else { return };
                if d.ended {
                    return;
                }
                d.ended = true;
                let (peer, transfer) = (d.peer, d.transfer);
                tracing::info!("remote drag {drag:x} ended (accepted: {accepted})");
                if peer == self.me() {
                    // The diagnostics test drag.
                    let msg = if accepted { "Test file dropped" } else { "The test drag was not accepted anywhere" };
                    self.notice(Notice::Info { message: msg.into() });
                    self.dnd.incoming.remove(&drag);
                    return;
                }
                self.send_to(peer, Msg::DragFinished { drag, accepted });
                if !accepted {
                    // Nothing under the cursor took the files.
                    if let Some(t) = transfer {
                        let _ = self.cancel_transfer(t, true);
                    }
                }
                self.settle_in_drag(drag);
            }
        }
    }

    fn begin_out_drag(&mut self, target: DeviceId, paths: Vec<PathBuf>) {
        let drag = crate::crypto::random_u64();
        let items: Vec<ItemMeta> = paths
            .iter()
            .take(MAX_SUMMARY_ITEMS)
            .map(|p| ItemMeta {
                rel_path: p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                size: 0,
                is_dir: p.is_dir(),
                mtime: None,
            })
            .collect();
        // Announce the drag first so the target can start its native drag
        // immediately; the offer follows once the files are enumerated.
        let transfer_id = self.start_outgoing(target, paths, TransferPurpose::Drag { drag });
        self.send_to(target, Msg::DragBegin { drag, transfer: transfer_id, items });
        self.dnd.outgoing = Some(OutDrag { drag, target, transfer: transfer_id });
    }

    /// The controller's cursor moved on (`None` = back home). A drag that
    /// went out was already dropped there (it is only sent once the button
    /// is let go), so it is left to finish wherever the cursor goes.
    pub(crate) fn move_drag_to(&mut self, next: Option<DeviceId>) {
        if next.is_none() {
            self.finish_probe(ProbeOutcome::Returned);
        }
    }

    /// Control over a screen ended for another reason. As above, files
    /// already dropped there keep coming (the transfer can be cancelled on
    /// the Transfers page).
    pub(crate) fn end_drag_for_target(&mut self, outcome: ProbeOutcome) {
        self.finish_probe(outcome);
    }

    fn finish_probe(&mut self, outcome: ProbeOutcome) {
        if let Some(probe) = self.dnd.probe.take() {
            self.dnd.held_outcome = Some((probe, outcome));
            if !matches!(self.input, Input::Controlling(_)) {
                self.flush_drag_outcome();
            }
        }
    }

    /// The probe window is on screen: let go of input capture with the
    /// pointer moved off the edge into it, which moves the local drag onto it.
    fn release_for_probe(&mut self, probe: u64) {
        if self.dnd.probe != Some(probe) || self.dnd.handoff.is_some() {
            return;
        }
        let Input::Controlling(c) = &self.input else { return };
        let (dx, dy) = edge_normal(c.exit_edge);
        let mut at = Point::new(c.exit_pos.x - dx, c.exit_pos.y - dy);
        let local = self.local_desktop();
        if !local.monitors.iter().any(|m| m.contains(at)) {
            at = local.entry_point(c.exit_edge, local.fraction_along(c.exit_edge, c.exit_pos), 1.0);
        }
        let inside = local
            .monitors
            .iter()
            .find(|m| m.contains(at))
            .map(|m| {
                let (cx, cy) = (m.x + m.w / 2.0, m.y + m.h / 2.0);
                Point::new(at.x + (cx - at.x) * PROBE_INSET, at.y + (cy - at.y) * PROBE_INSET)
            })
            .unwrap_or(at);
        tracing::info!("letting go of input for a moment so the drag here can be read");
        let _ = self.cap.send(CaptureCommand::Release { pos: Some(inside) });
        self.dnd.handoff =
            Some(Handoff { probe, edge: c.exit_edge, at, deadline: Instant::now() + HANDOFF_TIMEOUT, moved: false });
        let internal = self.internal.clone();
        tokio::spawn(async move {
            for (i, ms) in NUDGE_AFTER.iter().enumerate() {
                let since = if i == 0 { *ms } else { ms - NUDGE_AFTER[i - 1] };
                tokio::time::sleep(Duration::from_millis(since)).await;
                if internal.send(Internal::HandoffNudge { probe }).is_err() {
                    return;
                }
            }
        });
    }

    /// The warp alone only brings the drag *into* the probe window; GTK needs
    /// a motion to report it. Move one pixel along the edge and back (never
    /// towards it, which would capture input again).
    pub(crate) fn nudge_handoff(&mut self, probe: u64) {
        let Some(h) = self.dnd.handoff.as_mut().filter(|h| h.probe == probe) else { return };
        if !matches!(self.input, Input::Controlling(_)) || self.dnd.probe != Some(probe) {
            return;
        }
        h.moved = true;
        let (nx, ny) = edge_normal(h.edge);
        let (dx, dy) = (ny.abs(), nx.abs());
        let _ = self.emu.send(EmulationCommand::MoveRel { dx, dy });
        let _ = self.emu.send(EmulationCommand::MoveRel { dx: -dx, dy: -dy });
    }

    /// The probe is done while input is let go: cancel the local drag (if
    /// there is one) and push the pointer against the edge, so the barrier
    /// captures input again and the cursor carries on where it was.
    fn end_handoff_drag(&mut self, probe: u64, dragging: bool) {
        let Some(h) = self.dnd.handoff.as_mut() else { return };
        h.moved = true;
        let esc = crate::types::keys::KEY_ESC;
        if dragging {
            let _ = self.emu.send(EmulationCommand::Key { code: esc, pressed: true });
            let _ = self.emu.send(EmulationCommand::Key { code: esc, pressed: false });
        }
        let (dx, dy) = edge_normal(h.edge);
        let _ = self.emu.send(EmulationCommand::MoveAbs { pos: h.at });
        let _ = self.emu.send(EmulationCommand::MoveRel { dx: dx * 4.0, dy: dy * 4.0 });
        // Nothing is left to clean up when the cursor comes back.
        self.dnd.probe = None;
        let _ = self.dndcmd.send(DndCommand::ProbeDone { probe, outcome: ProbeOutcome::Cancelled });
    }

    /// Input was captured again after a handoff: carry on on the same screen.
    /// Returns false if there was no handoff.
    pub(crate) fn resume_after_handoff(&mut self, edge: Edge, pos: Point) -> bool {
        let Some(h) = self.dnd.handoff.take() else { return false };
        let Input::Controlling(c) = &mut self.input else { return false };
        tracing::info!("input is captured again");
        c.exit_edge = edge;
        c.exit_pos = pos;
        if h.moved {
            // Stop this computer's own emulated pointer again.
            let _ = self.emu.send(EmulationCommand::Leave { park: None });
        }
        true
    }

    /// Input was not captured again in time (the pointer moved away on this
    /// screen meanwhile): the cursor stays here.
    pub(crate) fn tick_dnd(&mut self, now: Instant) {
        if self.dnd.handoff.as_ref().is_none_or(|h| now < h.deadline) {
            return;
        }
        if let Input::Controlling(c) = &self.input {
            tracing::info!("input was not captured again; the cursor stays on this screen");
            let (t, s) = (c.target, c.session);
            self.send_to(t, Msg::Leave { session: s });
            self.input = Input::Local;
            self.motion_deadline = None;
            self.flush_drag_outcome();
            self.update_edges();
            self.mark_dirty();
        }
        self.dnd.handoff = None;
    }

    /// The cursor is back on this screen: tell the backend how the drag
    /// that left it ended.
    pub(crate) fn flush_drag_outcome(&mut self) {
        if self.dnd.handoff.take().is_some_and(|h| h.moved) {
            let _ = self.emu.send(EmulationCommand::Leave { park: None });
        }
        if let Some((probe, outcome)) = self.dnd.held_outcome.take() {
            let _ = self.dndcmd.send(DndCommand::ProbeDone { probe, outcome });
        }
    }

    /// Diagnostics: start a native drag of a test file here, as if it came
    /// from another computer. It is dropped wherever the pointer is after
    /// `seconds`.
    pub(crate) fn test_drag(&mut self, seconds: u64) -> Result<(), String> {
        if !self.backends.dnd.is_ready() {
            return Err("Drag and drop is not available on this computer".into());
        }
        if self.controlled.is_some() || matches!(self.input, Input::Controlling(_)) {
            return Err("Bring the cursor back to this computer first".into());
        }
        let dir = self.paths.cache_dir.join("dnd-test");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let file = dir.join("KVM Bridge test.txt");
        std::fs::write(&file, "This file was dropped by the KVM Bridge drag-and-drop test.\n").map_err(|e| e.to_string())?;
        let drag = crate::crypto::random_u64();
        self.dnd.incoming.insert(
            drag,
            InDrag { peer: self.me(), transfer: None, result: Some(Ok(vec![file])), reply: None, ended: false },
        );
        let desk = self.local_desktop();
        let b = desk.bounds();
        let pos = crate::types::Point::new(b.x + b.w / 2.0, b.y + b.h / 2.0);
        let _ = self.dndcmd.send(DndCommand::StartDrag {
            drag,
            names: vec!["KVM Bridge test.txt".into()],
            pos,
            drop_after_ms: seconds * 1000,
        });
        Ok(())
    }

    /// Target reports the outcome of a drop (we are the controller).
    pub(crate) fn on_drag_finished(&mut self, peer: DeviceId, drag: u64, accepted: bool) {
        let Some(d) = self.dnd.outgoing.take() else { return };
        if d.drag != drag || d.target != peer {
            self.dnd.outgoing = Some(d);
            return;
        }
        if !accepted {
            let _ = self.cancel_transfer(d.transfer, true);
        }
        self.finish_probe(ProbeOutcome::Dropped { accepted });
    }

    // ------------------------------------------------------------ target side

    pub(crate) fn on_drag_begin(&mut self, peer: DeviceId, drag: u64, _transfer: u64, items: Vec<ItemMeta>) {
        let controlled_by_peer = self.controlled.as_ref().is_some_and(|c| c.peer == peer);
        if !controlled_by_peer || !self.dnd_enabled() {
            self.send_to(peer, Msg::DragFinished { drag, accepted: false });
            return;
        }
        let pos = self.controlled_pos().unwrap_or_default();
        let names = items.into_iter().map(|i| i.rel_path).collect();
        self.dnd.incoming.insert(drag, InDrag { peer, transfer: None, result: None, reply: None, ended: false });
        // The user already let go on the controller: drop as soon as the
        // application under the cursor has seen the drag.
        let _ = self.dndcmd.send(DndCommand::StartDrag { drag, names, pos, drop_after_ms: 250 });
    }

    /// The files of `drag` are offered. Returns false if that drag is not
    /// going on here (it was refused or is over), so they are declined.
    pub(crate) fn dnd_transfer_offered(&mut self, peer: DeviceId, drag: u64, local_transfer: u64) -> bool {
        match self.dnd.incoming.get_mut(&drag) {
            Some(d) if d.peer == peer && d.transfer.is_none() => {
                d.transfer = Some(local_transfer);
                true
            }
            _ => false,
        }
    }

    pub(crate) fn dnd_transfer_done(&mut self, drag: u64, result: Result<&Received, String>) {
        let Some(d) = self.dnd.incoming.get_mut(&drag) else { return };
        let r = match result {
            Ok(Received::Files(p)) => Ok(p.clone()),
            Ok(Received::Bytes(_)) => Err("unexpected payload".to_string()),
            Err(e) => Err(e),
        };
        if let Some(reply) = d.reply.take() {
            let _ = reply.send(r.as_ref().ok().cloned());
        }
        d.result = Some(r);
        self.settle_in_drag(drag);
    }

    /// Forget an incoming drag once its native drag is over and nothing
    /// waits for its files any more.
    fn settle_in_drag(&mut self, drag: u64) {
        if self.dnd.incoming.get(&drag).is_some_and(|d| d.ended && d.reply.is_none()) {
            self.dnd.incoming.remove(&drag);
        }
    }

    pub(crate) fn on_drag_cancel(&mut self, peer: DeviceId, drag: u64) {
        if let Some(d) = self.dnd.incoming.remove(&drag) {
            if d.peer != peer {
                self.dnd.incoming.insert(drag, d);
                return;
            }
            if let Some(r) = d.reply {
                let _ = r.send(None);
            }
            let _ = self.dndcmd.send(DndCommand::CancelDrag { drag });
        }
    }

    pub(crate) fn dnd_remote_button_released(&mut self) {
        if !self.dnd.incoming.is_empty() {
            tracing::debug!("controller released the button during a remote drag");
        }
    }

    pub(crate) fn dnd_controller_changed(&mut self) {
        let drags: Vec<u64> = self.dnd.incoming.keys().copied().collect();
        for drag in drags {
            if let Some(d) = self.dnd.incoming.remove(&drag) {
                if let Some(r) = d.reply {
                    let _ = r.send(None);
                }
                let _ = self.dndcmd.send(DndCommand::CancelDrag { drag });
            }
        }
    }

    pub(crate) fn dnd_peer_gone(&mut self, peer: DeviceId) {
        let drags: Vec<u64> = self.dnd.incoming.iter().filter(|(_, d)| d.peer == peer).map(|(k, _)| *k).collect();
        for drag in drags {
            if let Some(d) = self.dnd.incoming.remove(&drag) {
                if let Some(r) = d.reply {
                    let _ = r.send(None);
                }
                let _ = self.dndcmd.send(DndCommand::CancelDrag { drag });
            }
        }
        if self.dnd.outgoing.as_ref().is_some_and(|d| d.target == peer) {
            self.dnd.outgoing = None;
            self.finish_probe(ProbeOutcome::Abandoned);
        }
    }
}

/// Unit vector pointing out of the screen through `edge`.
fn edge_normal(edge: Edge) -> (f64, f64) {
    match edge {
        Edge::Left => (-1.0, 0.0),
        Edge::Right => (1.0, 0.0),
        Edge::Top => (0.0, -1.0),
        Edge::Bottom => (0.0, 1.0),
    }
}
