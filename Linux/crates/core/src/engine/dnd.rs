//! Drag-and-drop of files between screens.
//!
//! Controller side: a mouse button held while the cursor left this screen
//! and let go on another one may have dragged files along. Only then the
//! platform is asked to *probe* for a file drag in progress (normal crossings
//! show nothing). If one is found, the files start streaming to the target
//! and the target begins a native drag under its cursor and drops it right
//! away, into whatever application is there; that application receives the
//! files once the transfer has completed.

use super::*;
use crate::platform::ProbeOutcome;
use crate::proto::{ItemMeta, TransferPurpose, MAX_SUMMARY_ITEMS};
use crate::transfer::Received;

#[derive(Default)]
pub(crate) struct DndState {
    /// Probe issued when the cursor left this screen.
    probe: Option<u64>,
    /// Outcome of the probe, held back until the cursor is home again (the
    /// backend ends the local drag then, which needs uncaptured input).
    held_outcome: Option<(u64, ProbeOutcome)>,
    outgoing: Option<OutDrag>,
    incoming: HashMap<u64, InDrag>,
}

struct OutDrag {
    drag: u64,
    target: DeviceId,
    transfer: u64,
    paths: Vec<PathBuf>,
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
        if button != crate::types::buttons::BTN_LEFT
            || !self.dnd_enabled()
            || self.dnd.probe.is_some()
            || self.dnd.outgoing.is_some()
        {
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
            DndEvent::Probed { probe, paths } => {
                if self.dnd.probe != Some(probe) || self.dnd.outgoing.is_some() {
                    return;
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
                let peer = d.peer;
                let transfer = d.transfer;
                let waiting_for_data = d.reply.is_some();
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
                    if let Some(t) = transfer {
                        let _ = self.cancel_transfer(t, true);
                    }
                }
                if !waiting_for_data {
                    self.dnd.incoming.remove(&drag);
                }
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
        let transfer_id = self.start_outgoing(target, paths.clone(), TransferPurpose::Drag { drag });
        self.send_to(target, Msg::DragBegin { drag, transfer: transfer_id, items });
        self.dnd.outgoing = Some(OutDrag { drag, target, transfer: transfer_id, paths });
    }

    /// The controller's cursor moved from `old` to `next` (`None` = back home).
    pub(crate) fn move_drag_to(&mut self, old: DeviceId, next: Option<DeviceId>) {
        match self.dnd.outgoing.take() {
            Some(d) if d.target == old => {
                self.send_to(old, Msg::DragCancel { drag: d.drag });
                let _ = self.cancel_transfer(d.transfer, true);
                match next {
                    Some(n) => self.begin_out_drag(n, d.paths),
                    None => self.finish_probe(ProbeOutcome::Returned),
                }
            }
            other => {
                self.dnd.outgoing = other;
                if next.is_none() {
                    self.finish_probe(ProbeOutcome::Returned);
                }
            }
        }
    }

    /// Control over `target` ended for another reason.
    pub(crate) fn end_drag_for_target(&mut self, target: DeviceId, outcome: ProbeOutcome) {
        if let Some(d) = self.dnd.outgoing.take() {
            if d.target == target {
                self.send_to(target, Msg::DragCancel { drag: d.drag });
                let _ = self.cancel_transfer(d.transfer, true);
            } else {
                self.dnd.outgoing = Some(d);
            }
        }
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

    /// The cursor is back on this screen: tell the backend how the drag
    /// that left it ended.
    pub(crate) fn flush_drag_outcome(&mut self) {
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

    pub(crate) fn dnd_transfer_offered(&mut self, peer: DeviceId, drag: u64, local_transfer: u64) {
        if let Some(d) = self.dnd.incoming.get_mut(&drag) {
            if d.peer == peer {
                d.transfer = Some(local_transfer);
            }
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
        if d.ended {
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
