//! Keyboard/mouse sharing: crossing screen edges, forwarding captured input
//! to the device under the (virtual) cursor and replaying input we receive.

use super::*;
use crate::layout::{map_across, step, Step};
use crate::types::{buttons, keys, Point, Rect};
use std::collections::HashSet;

pub(crate) enum Input {
    /// The local keyboard/mouse act on this device.
    Local,
    /// Local input is captured and forwarded to `target`.
    Controlling(Box<Controlling>),
}

pub(crate) struct Controlling {
    pub target: DeviceId,
    pub session: u32,
    /// Virtual cursor position on the target's desktop.
    pub pos: Point,
    pub desktop: Desktop,
    pub pending_motion: bool,
    /// Keys currently held (normalised) for hotkey detection.
    pub held: HashSet<u32>,
    /// Keys whose press was consumed by a hotkey (their release is dropped too).
    pub swallowed: HashSet<u32>,
    pub buttons: HashSet<u32>,
    /// Buttons pressed at any time since the cursor left this device.
    pub seen_buttons: HashSet<u32>,
    /// Where the cursor left this device (to put it back there on release).
    pub exit_edge: Edge,
    pub exit_pos: Point,
}

/// This device is being controlled by `peer`.
pub(crate) struct Controlled {
    pub peer: DeviceId,
    pub session: u32,
    pub pos: Point,
}

fn default_desktop() -> Desktop {
    Desktop::new(vec![Rect::new(0.0, 0.0, 1920.0, 1080.0)])
}

impl Engine {
    pub(crate) fn local_desktop(&self) -> Desktop {
        self.desktop.clone().unwrap_or_else(default_desktop)
    }

    fn peer_desktop(&self, p: DeviceId) -> Desktop {
        self.peers.get(&p).and_then(|p| p.desktop.clone()).filter(|d| !d.is_empty()).unwrap_or_else(default_desktop)
    }

    /// Can the cursor move onto this peer right now?
    pub(crate) fn reachable(&self, p: DeviceId) -> bool {
        self.conn_of(p).is_some() && self.peers.get(&p).is_some_and(|x| x.caps.emulation && x.caps.enabled)
    }

    /// The next screen from `from` towards `edge`, skipping screens that are
    /// switched off or unreachable (this computer always counts).
    fn next_screen(&self, from: DeviceId, edge: Edge) -> Option<DeviceId> {
        let me = self.me();
        let s = self.st.layout.slot(from)?;
        let (dc, dr) = edge.grid_delta();
        let (mut col, mut row) = (s.col, s.row);
        loop {
            col += dc;
            row += dr;
            let n = self.st.layout.at(col, row)?.device;
            if n == me || self.reachable(n) {
                return Some(n);
            }
        }
    }

    /// The screen under the cursor was switched off or went away: continue
    /// on the nearest screen that is still on.
    pub(crate) fn relocate_cursor(&mut self) {
        let Input::Controlling(c) = &self.input else { return };
        let (from, at) = (c.target, c.pos);
        let me = self.me();
        let Some(src) = self.st.layout.slot(from).cloned() else {
            self.release_control_local();
            return;
        };
        let best = self
            .st
            .layout
            .slots
            .iter()
            .filter(|s| s.device != from && (s.device == me || self.reachable(s.device)))
            .min_by_key(|s| ((s.col - src.col).abs() + (s.row - src.row).abs(), s.device != me))
            .cloned();
        let Some(t) = best else {
            self.release_control_local();
            return;
        };
        let (dc, dr) = (t.col - src.col, t.row - src.row);
        let edge = if dc.abs() >= dr.abs() {
            if dc > 0 {
                Edge::Right
            } else {
                Edge::Left
            }
        } else if dr > 0 {
            Edge::Bottom
        } else {
            Edge::Top
        };
        tracing::info!("the screen under the cursor is gone; continuing on {:?}", t.device);
        if t.device == me {
            self.return_home(edge, at);
        } else {
            self.switch_target(t.device, edge, at);
        }
    }

    /// Another computer was chosen to control the screens.
    pub(crate) fn controlled_from_elsewhere(&self) -> bool {
        self.st.layout.controller.is_some_and(|c| c != self.me())
    }

    pub(crate) fn set_controller(&mut self, computer: Option<DeviceId>) -> Result<(), String> {
        if let Some(c) = computer {
            if !self.st.layout.contains(c) {
                return Err("That computer is not part of the arrangement".into());
            }
        }
        if computer == self.st.layout.controller {
            return Ok(());
        }
        let mut l = self.st.layout.clone();
        l.controller = computer;
        l.rev = self.st.layout.rev + 1;
        l.author = self.me();
        self.adopt_layout(l, None);
        Ok(())
    }

    /// Apply the arrangement's choice of controlling computer here.
    pub(crate) fn apply_controls(&mut self) {
        if self.controlled_from_elsewhere() && matches!(self.input, Input::Controlling(_)) {
            self.release_control_local();
        }
        self.update_edges();
    }

    /// Tell the capture backend which edges lead somewhere.
    pub(crate) fn update_edges(&mut self) {
        self.update_guard();
        let want_enabled = self.controlled.is_none() && self.st.enabled;
        if want_enabled != self.capture_enabled {
            self.capture_enabled = want_enabled;
            let _ = self.cap.send(CaptureCommand::Enable(want_enabled));
        }
        // Barriers follow the layout, not connectivity: on some desktops
        // (GNOME) changing barriers is expensive, and hitting an edge whose
        // neighbour is offline is simply released again in `on_activated`.
        let edges: Vec<Edge> = if self.locked || !self.st.enabled || self.controlled_from_elsewhere() {
            vec![]
        } else {
            self.st.layout.neighbors(self.me()).into_iter().map(|(e, _)| e).collect()
        };
        if self.edges_sent.as_ref() != Some(&edges) {
            tracing::debug!("capture edges: {edges:?}");
            self.edges_sent = Some(edges.clone());
            let _ = self.cap.send(CaptureCommand::SetEdges(edges));
        }
    }

    pub(crate) fn set_local_desktop(&mut self, d: Desktop) {
        if self.desktop.as_ref() != Some(&d) {
            tracing::info!("local desktop: {:?}", d.monitors);
            self.desktop = Some(d.clone());
            self.broadcast(Msg::Geometry(Some(d)));
            self.mark_dirty();
        }
    }

    pub(crate) fn on_capture(&mut self, ev: CaptureEvent) {
        match ev {
            CaptureEvent::Status(s) => {
                tracing::info!("capture backend: {s:?}");
                let ready = s.is_ready();
                self.backends.capture = s;
                if ready {
                    // (Re)send configuration to a freshly started backend.
                    self.edges_sent = None;
                    self.guard_sent = None;
                    self.capture_enabled = !self.capture_enabled;
                    self.apply_controls();
                }
                self.broadcast_caps();
                self.mark_dirty();
            }
            CaptureEvent::Geometry(d) => {
                if !self.desktop_from_emulation {
                    self.set_local_desktop(d);
                }
            }
            CaptureEvent::Guard(g) => self.on_guard(g),
            CaptureEvent::Activated { edge, pos } => self.on_activated(edge, pos),
            CaptureEvent::Motion { dx, dy } => self.on_motion(dx, dy),
            CaptureEvent::Button { button, pressed } => {
                if let Input::Controlling(c) = &mut self.input {
                    // Released without a press since the cursor left: it was
                    // held while crossing, maybe dragging files along.
                    let held_across = !pressed && !c.seen_buttons.contains(&button);
                    if pressed {
                        c.buttons.insert(button);
                        c.seen_buttons.insert(button);
                    } else {
                        c.buttons.remove(&button);
                    }
                    let t = c.target;
                    self.flush_motion();
                    if held_across && self.probe_drag(button) {
                        return;
                    }
                    self.send_to(t, Msg::Button { button, pressed });
                }
            }
            CaptureEvent::Scroll { dx, dy } => {
                if let Input::Controlling(c) = &self.input {
                    let t = c.target;
                    self.flush_motion();
                    self.send_to(t, Msg::Scroll { dx: dx as f32, dy: dy as f32 });
                }
            }
            CaptureEvent::ScrollDiscrete { dx, dy } => {
                if let Input::Controlling(c) = &self.input {
                    let t = c.target;
                    self.flush_motion();
                    self.send_to(t, Msg::ScrollDiscrete { dx, dy });
                }
            }
            CaptureEvent::Key { code, pressed } => self.on_captured_key(code, pressed),
            CaptureEvent::Deactivated => {
                if let Input::Controlling(c) = &self.input {
                    tracing::info!("capture ended by the system");
                    let (t, s) = (c.target, c.session);
                    self.end_drag_for_target(t, crate::platform::ProbeOutcome::Abandoned);
                    self.send_to(t, Msg::Leave { session: s });
                    self.input = Input::Local;
                    self.motion_deadline = None;
                    self.flush_drag_outcome();
                    self.update_edges();
                    self.mark_dirty();
                }
            }
        }
    }

    fn on_activated(&mut self, edge: Edge, pos: Point) {
        if self.st.enabled && self.resume_after_handoff(edge, pos) {
            return;
        }
        if self.controlled.is_some() || self.locked || !self.st.enabled || self.controlled_from_elsewhere() {
            let _ = self.cap.send(CaptureCommand::Release { pos: None });
            return;
        }
        if matches!(self.input, Input::Controlling(_)) {
            return;
        }
        let me = self.me();
        let target = match self.next_screen(me, edge) {
            Some(t) if t != me => t,
            _ => {
                let _ = self.cap.send(CaptureCommand::Release { pos: None });
                return;
            }
        };
        let local = self.local_desktop();
        let desk = self.peer_desktop(target);
        let entry = map_across(&local, pos, edge, &desk);
        let session = rand::random::<u32>();
        tracing::debug!("cursor -> {:?} at {:?}", target, entry);
        self.send_to(target, Msg::Enter { session, pos: entry });
        self.input = Input::Controlling(Box::new(Controlling {
            target,
            session,
            pos: entry,
            desktop: desk,
            pending_motion: false,
            held: HashSet::new(),
            swallowed: HashSet::new(),
            buttons: HashSet::new(),
            seen_buttons: HashSet::new(),
            exit_edge: edge,
            exit_pos: pos,
        }));
        self.drag_crossed_out();
        self.mark_dirty();
    }

    fn on_motion(&mut self, dx: f64, dy: f64) {
        let locked = self.locked;
        let Input::Controlling(c) = &mut self.input else { return };
        match step(&c.desktop, c.pos, dx, dy) {
            Step::Inside(p) => {
                c.pos = p;
                self.queue_motion();
            }
            Step::Exit { edge, at } => {
                let from = c.target;
                let next = if locked { None } else { self.next_screen(from, edge) };
                match next {
                    Some(n) if n == self.me() => self.return_home(edge, at),
                    Some(n) => self.switch_target(n, edge, at),
                    _ => {
                        if let Input::Controlling(c) = &mut self.input {
                            c.pos = at;
                        }
                        self.queue_motion();
                    }
                }
            }
        }
    }

    fn queue_motion(&mut self) {
        let interval = self.timing().motion_interval_us;
        let Input::Controlling(c) = &mut self.input else { return };
        if interval == 0 {
            let (t, p) = (c.target, c.pos);
            self.send_to(t, Msg::MoveAbs { x: p.x as f32, y: p.y as f32 });
            return;
        }
        c.pending_motion = true;
        if self.motion_deadline.is_none() {
            self.motion_deadline = Some(Instant::now() + Duration::from_micros(interval as u64));
        }
    }

    pub(crate) fn flush_motion(&mut self) {
        self.motion_deadline = None;
        if let Input::Controlling(c) = &mut self.input {
            if c.pending_motion {
                c.pending_motion = false;
                let (t, p) = (c.target, c.pos);
                self.send_to(t, Msg::MoveAbs { x: p.x as f32, y: p.y as f32 });
            }
        }
    }

    fn switch_target(&mut self, next: DeviceId, edge: Edge, at: Point) {
        let Input::Controlling(c) = &mut self.input else { return };
        let old = c.target;
        let old_session = c.session;
        let from_desk = c.desktop.clone();
        let to_desk = self.peer_desktop(next);
        let entry = map_across(&from_desk, at, edge, &to_desk);
        let session = rand::random::<u32>();
        // Drag-and-drop follows the cursor.
        self.move_drag_to(old, Some(next));
        self.send_to(old, Msg::Leave { session: old_session });
        self.send_to(next, Msg::Enter { session, pos: entry });
        if let Input::Controlling(c) = &mut self.input {
            c.target = next;
            c.session = session;
            c.pos = entry;
            c.desktop = to_desk;
            c.pending_motion = false;
            // Keys/buttons still held were released on the old target.
            c.buttons.clear();
        }
        self.motion_deadline = None;
        self.mark_dirty();
    }

    fn return_home(&mut self, edge: Edge, at: Point) {
        let Input::Controlling(c) = &self.input else { return };
        let (t, s, desk) = (c.target, c.session, c.desktop.clone());
        let local = self.local_desktop();
        let pos = map_across(&desk, at, edge, &local);
        self.move_drag_to(t, None);
        self.send_to(t, Msg::Leave { session: s });
        let _ = self.cap.send(CaptureCommand::Release { pos: Some(pos) });
        self.input = Input::Local;
        self.motion_deadline = None;
        self.flush_drag_outcome();
        self.update_edges();
        self.mark_dirty();
    }

    /// Give the cursor back to this device (hotkey, UI button, errors).
    pub(crate) fn release_control_local(&mut self) {
        let Input::Controlling(c) = &self.input else { return };
        let (t, s) = (c.target, c.session);
        let local = self.local_desktop();
        let t_frac = local.fraction_along(c.exit_edge, c.exit_pos);
        let pos = local.entry_point(c.exit_edge, t_frac, 8.0);
        self.end_drag_for_target(t, crate::platform::ProbeOutcome::Abandoned);
        self.send_to(t, Msg::Leave { session: s });
        let _ = self.cap.send(CaptureCommand::Release { pos: Some(pos) });
        self.input = Input::Local;
        self.motion_deadline = None;
        self.flush_drag_outcome();
        self.update_edges();
        self.mark_dirty();
    }

    fn on_captured_key(&mut self, code: u32, pressed: bool) {
        let release_combo: HashSet<u32> = self.st.settings.hotkeys.release.iter().map(|k| keys::normalize(*k)).collect();
        let lock_combo: HashSet<u32> = self.st.settings.hotkeys.lock.iter().map(|k| keys::normalize(*k)).collect();
        let Input::Controlling(c) = &mut self.input else { return };
        let n = keys::normalize(code);
        if pressed {
            c.held.insert(n);
            if !release_combo.is_empty() && release_combo.is_subset(&c.held) {
                c.swallowed.insert(code);
                tracing::info!("release hotkey pressed");
                self.release_control_local();
                return;
            }
            if !lock_combo.is_empty() && lock_combo.is_subset(&c.held) && lock_combo.contains(&n) {
                c.swallowed.insert(code);
                self.locked = !self.locked;
                tracing::info!("cursor lock {}", if self.locked { "on" } else { "off" });
                self.mark_dirty();
                return;
            }
        } else {
            c.held.remove(&n);
            if c.swallowed.remove(&code) {
                return;
            }
        }
        let t = c.target;
        self.flush_motion();
        self.send_to(t, Msg::Key { code, pressed });
    }

    pub(crate) fn on_peer_geometry(&mut self, peer: DeviceId, d: Option<Desktop>) {
        if let Input::Controlling(c) = &mut self.input {
            if c.target == peer {
                if let Some(d) = d.filter(|d| !d.is_empty()) {
                    c.pos = d.clamp(c.pos);
                    c.desktop = d;
                }
            }
        }
    }

    /// A peer disconnected: clean up anything that involved it.
    pub(crate) fn peer_gone(&mut self, peer: DeviceId) {
        if matches!(&self.input, Input::Controlling(c) if c.target == peer) {
            self.relocate_cursor();
        }
        if self.controlled.as_ref().is_some_and(|c| c.peer == peer) {
            self.controlled = None;
            let _ = self.emu.send(EmulationCommand::Leave { park: None });
            self.update_edges();
        }
        self.clip_peer_gone(peer);
        self.transfers_peer_gone(peer);
        self.dnd_peer_gone(peer);
    }

    // ------------------------------------------------------------ being controlled

    pub(crate) fn on_remote_enter(&mut self, peer: DeviceId, session: u32, pos: Point) {
        if !self.st.enabled {
            // Switched off: the controller keeps the cursor.
            self.send_to(peer, Msg::ControlEnded { session });
            return;
        }
        // If we were driving another screen ourselves, stop.
        if matches!(self.input, Input::Controlling(_)) {
            self.release_control_local();
        }
        if let Some(prev) = self.controlled.take() {
            if prev.peer != peer {
                self.send_to(prev.peer, Msg::ControlEnded { session: prev.session });
            }
            self.dnd_controller_changed();
        }
        let pos = self.local_desktop().clamp(pos);
        self.controlled = Some(Controlled { peer, session, pos });
        let _ = self.emu.send(EmulationCommand::Enter { pos });
        self.update_edges();
        self.mark_dirty();
    }

    pub(crate) fn on_remote_leave(&mut self, peer: DeviceId, session: u32) {
        if self.controlled.as_ref().is_some_and(|c| c.peer == peer && c.session == session) {
            self.controlled = None;
            // The cursor moved on to another screen: tuck this one away.
            let park = self.local_desktop().park_point();
            let _ = self.emu.send(EmulationCommand::Leave { park: Some(park) });
            self.update_edges();
            self.mark_dirty();
        }
    }

    pub(crate) fn on_control_ended(&mut self, peer: DeviceId, session: u32) {
        let ours = matches!(&self.input, Input::Controlling(c) if c.target == peer && c.session == session);
        if ours {
            tracing::info!("{:?} is now controlled by another device", peer);
            let _ = self.cap.send(CaptureCommand::Release { pos: None });
            self.end_drag_for_target(peer, crate::platform::ProbeOutcome::Abandoned);
            self.input = Input::Local;
            self.motion_deadline = None;
            self.flush_drag_outcome();
            self.update_edges();
            self.mark_dirty();
        }
    }

    pub(crate) fn on_remote_input(&mut self, peer: DeviceId, msg: Msg) {
        let Some(c) = &mut self.controlled else { return };
        if c.peer != peer {
            return;
        }
        let cmd = match msg {
            Msg::MoveAbs { x, y } => {
                c.pos = Point::new(x as f64, y as f64);
                EmulationCommand::MoveAbs { pos: c.pos }
            }
            Msg::MoveRel { dx, dy } => EmulationCommand::MoveRel { dx: dx as f64, dy: dy as f64 },
            Msg::Button { button, pressed } => {
                if button == buttons::BTN_LEFT && !pressed {
                    self.dnd_remote_button_released();
                }
                EmulationCommand::Button { button, pressed }
            }
            Msg::Key { code, pressed } => EmulationCommand::Key { code, pressed },
            Msg::Scroll { dx, dy } => EmulationCommand::Scroll { dx: dx as f64, dy: dy as f64 },
            Msg::ScrollDiscrete { dx, dy } => EmulationCommand::ScrollDiscrete { dx, dy },
            _ => return,
        };
        let _ = self.emu.send(cmd);
    }

    /// Position of the remote-controlled cursor on this device.
    pub(crate) fn controlled_pos(&self) -> Option<Point> {
        self.controlled.as_ref().map(|c| c.pos)
    }
}
