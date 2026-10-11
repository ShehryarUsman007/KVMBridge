//! Peers, connections, pairing and synchronisation of shared state.

use super::*;
use crate::config::{SharedSettings, TrustedPeer};
use crate::crypto::{fingerprint, Cipher};
use crate::net::ConnHandle;
use crate::proto::{Capabilities, Hello};
use crate::types::now_ms;

pub(crate) const PAIRING_TIMEOUT: Duration = Duration::from_secs(180);

pub(crate) struct Peer {
    pub id: DeviceId,
    pub name: String,
    pub os: String,
    pub fingerprint: String,
    /// Address of the control listener.
    pub addr: Option<SocketAddr>,
    pub last_seen: Option<Instant>,
    pub instance: u64,
    pub conn: Option<u64>,
    pub dialing: Option<Instant>,
    pub next_dial: Instant,
    pub backoff: Duration,
    pub pairing: Option<Pairing>,
    /// User asked to pair: dial even though the device is not trusted yet.
    pub want_pair: bool,
    pub desktop: Option<Desktop>,
    pub caps: Capabilities,
    pub rtt_ms: Option<f32>,
    pub heartbeat_ms: u32,
    pub error: Option<String>,
    pub rejected_at: Option<Instant>,
}

impl Peer {
    pub(crate) fn new(id: DeviceId, name: String) -> Peer {
        Peer {
            id,
            name,
            os: String::new(),
            fingerprint: String::new(),
            addr: None,
            last_seen: None,
            instance: 0,
            conn: None,
            dialing: None,
            next_dial: Instant::now(),
            backoff: Duration::from_secs(1),
            pairing: None,
            want_pair: false,
            desktop: None,
            caps: Capabilities::default(),
            rtt_ms: None,
            heartbeat_ms: 1000,
            error: None,
            rejected_at: None,
        }
    }
}

pub(crate) struct Pairing {
    pub conn: u64,
    pub sas: String,
    pub incoming: bool,
    pub local_ok: bool,
    pub remote_ok: bool,
    pub started: Instant,
    /// Both computers keep this computer's settings. The computer that
    /// started the pairing chooses (see [`Msg::PairSettings`]).
    pub settings_from: Option<DeviceId>,
}

pub(crate) struct ConnState {
    pub handle: ConnHandle,
    pub peer: DeviceId,
    pub last_rx: Instant,
    pub next_ping: Instant,
    pub ping_seq: u32,
    /// Trusted and active (not just pairing).
    pub active: bool,
}

impl Engine {
    pub(crate) fn seed_trusted_peers(&mut self) {
        for t in self.st.trusted.clone() {
            self.seed_peer(&t);
        }
    }

    /// No room for another paired device.
    pub(crate) fn group_full(&self) -> bool {
        self.st.trusted.len() + 1 >= crate::config::MAX_DEVICES
    }

    fn limit_message() -> String {
        format!(
            "KVM Bridge connects up to {} computers (this one plus {}). Unpair one first.",
            crate::config::MAX_DEVICES,
            crate::config::MAX_DEVICES - 1
        )
    }

    pub(crate) fn is_trusted(&self, id: DeviceId) -> bool {
        self.st.trusted(id).is_some()
    }

    /// Trusted peers with an active control connection.
    pub(crate) fn connected_peers(&self) -> Vec<DeviceId> {
        self.conns.values().filter(|c| c.active).map(|c| c.peer).collect()
    }

    pub(crate) fn conn_of(&self, peer: DeviceId) -> Option<&ConnState> {
        let id = self.peers.get(&peer)?.conn?;
        self.conns.get(&id).filter(|c| c.active)
    }

    pub(crate) fn send_to(&self, peer: DeviceId, m: Msg) -> bool {
        match self.conn_of(peer) {
            Some(c) => c.handle.send(m),
            None => false,
        }
    }

    pub(crate) fn broadcast(&self, m: Msg) {
        for c in self.conns.values().filter(|c| c.active) {
            c.handle.send(m.clone());
        }
    }

    fn broadcast_except(&self, except: Option<DeviceId>, m: Msg) {
        for c in self.conns.values().filter(|c| c.active && Some(c.peer) != except) {
            c.handle.send(m.clone());
        }
    }

    pub(crate) fn broadcast_caps(&mut self) {
        // Capabilities travel in Hello; resend it so peers learn about changes.
        let peers = self.connected_peers();
        for p in peers {
            let h = self.hello();
            self.send_to(p, h);
        }
        self.update_edges();
    }

    fn hello(&self) -> Msg {
        Msg::Hello(Box::new(Hello {
            desktop: self.desktop.clone(),
            layout: self.st.layout.clone(),
            shared: self.st.shared.clone(),
            caps: self.caps(),
            heartbeat_ms: self.timing().heartbeat_ms,
        }))
    }

    // ------------------------------------------------------------ discovery

    pub(crate) fn on_seen(&mut self, seen: Seen) {
        let b = seen.beacon;
        if b.id == self.me() || b.protocol != PROTOCOL_VERSION {
            return;
        }
        let addr = SocketAddr::new(seen.from.ip(), b.port);
        let me = self.me();
        let p = self.peers.entry(b.id).or_insert_with(|| {
            let mut p = Peer::new(b.id, b.name.clone());
            // The lower id dials first; the other side waits a bit to avoid
            // both connecting at the same time.
            if me > b.id {
                p.next_dial = Instant::now() + Duration::from_millis(1500);
            }
            p
        });
        let changed = p.name != b.name || p.addr != Some(addr) || p.last_seen.is_none() || p.instance != b.instance;
        if p.instance != b.instance && p.instance != 0 {
            // Peer restarted: retry right away.
            p.next_dial = Instant::now();
            p.backoff = Duration::from_secs(1);
        }
        p.name = b.name;
        p.os = b.os;
        p.fingerprint = b.fingerprint;
        p.addr = Some(addr);
        p.last_seen = Some(Instant::now());
        p.instance = b.instance;
        if changed {
            self.mark_dirty();
        }
        self.maybe_dial(b.id);
    }

    /// Look for devices on the network. Earlier results that are not paired
    /// or in use are dropped first, so the list shows who answers now.
    pub(crate) fn search(&mut self) {
        const SEARCH_TIME: Duration = Duration::from_secs(4);
        let stale: Vec<DeviceId> = self
            .peers
            .values()
            .filter(|p| p.conn.is_none() && p.pairing.is_none() && p.dialing.is_none() && !p.want_pair)
            .map(|p| p.id)
            .filter(|id| !self.is_trusted(*id))
            .collect();
        for id in stale {
            self.peers.remove(&id);
        }
        tracing::info!("searching for devices on the network");
        let _ = self.searches.send(());
        self.searching_until = Some(Instant::now() + SEARCH_TIME);
        self.mark_dirty();
    }

    fn maybe_dial(&mut self, id: DeviceId) {
        let trusted = self.is_trusted(id);
        let Some(p) = self.peers.get_mut(&id) else { return };
        if p.conn.is_some() || p.dialing.is_some() || !(trusted || p.want_pair) {
            return;
        }
        let Some(addr) = p.addr else { return };
        if Instant::now() < p.next_dial {
            return;
        }
        p.dialing = Some(Instant::now());
        let user = p.want_pair;
        tracing::debug!("dialing {} at {addr}", p.name);
        net::dial_control(self.net.clone(), addr, user);
        self.mark_dirty();
    }

    pub(crate) fn connect_address(&mut self, a: &str) -> Result<(), String> {
        let addr = resolve_host(a, crate::config::DEFAULT_LISTEN_PORT).ok_or_else(|| format!("Cannot resolve {a:?}"))?;
        // Remember it so discovery beacons go there too.
        if !self.st.settings.manual_peers.iter().any(|m| m == a.trim()) {
            self.st.settings.manual_peers.push(a.trim().to_string());
            let disc = discovery_config(&self.st.settings, &self.overrides);
            self.disc_cfg.send_replace(disc);
            self.schedule_save();
        }
        net::dial_control(self.net.clone(), addr, true);
        Ok(())
    }

    pub(crate) fn tick_connections(&mut self, now: Instant) {
        // Heartbeats and timeouts.
        let my_hb = self.timing().heartbeat_ms;
        let misses = self.timing().heartbeat_misses;
        let mut dead = Vec::new();
        for (id, c) in self.conns.iter_mut() {
            let peer_hb = self.peers.get(&c.peer).map(|p| p.heartbeat_ms).unwrap_or(my_hb);
            let timeout = Duration::from_millis((my_hb.max(peer_hb) * misses) as u64 + 500);
            if now.duration_since(c.last_rx) > timeout {
                dead.push(*id);
                continue;
            }
            if now >= c.next_ping {
                c.ping_seq = c.ping_seq.wrapping_add(1);
                c.handle.send(Msg::Ping { seq: c.ping_seq, sent_us: crate::types::mono_us() });
                c.next_ping = now + Duration::from_millis(my_hb as u64);
            }
        }
        for id in dead {
            tracing::info!("connection {id} timed out");
            if let Some(c) = self.conns.get(&id) {
                c.handle.close();
            }
            self.on_closed(id, "timed out".into());
        }
        // Pairing timeouts.
        let expired: Vec<DeviceId> = self
            .peers
            .values()
            .filter(|p| p.pairing.as_ref().is_some_and(|x| now.duration_since(x.started) > PAIRING_TIMEOUT))
            .map(|p| p.id)
            .collect();
        for id in expired {
            self.fail_pairing(id, "Pairing timed out");
        }
        // Stuck dials.
        for p in self.peers.values_mut() {
            if p.dialing.is_some_and(|t| now.duration_since(t) > Duration::from_secs(15)) {
                p.dialing = None;
            }
        }
        // Reconnect trusted peers.
        let ids: Vec<DeviceId> = self.peers.keys().copied().collect();
        for id in ids {
            self.maybe_dial(id);
        }
    }

    // ------------------------------------------------------------ network events

    pub(crate) fn on_net(&mut self, ev: NetEvent) {
        match ev {
            NetEvent::Connected { conn, user_initiated } => self.on_connected(conn, user_initiated),
            NetEvent::Message { conn, msg } => self.on_message(conn, msg),
            NetEvent::Closed { conn, reason } => self.on_closed(conn, reason),
            NetEvent::DialFailed { addr, error, user_initiated } => {
                let mut found = false;
                for p in self.peers.values_mut() {
                    if p.addr == Some(addr) && p.dialing.is_some() {
                        p.dialing = None;
                        p.next_dial = Instant::now() + p.backoff;
                        p.backoff = (p.backoff * 2).min(Duration::from_secs(30));
                        p.error = Some(error.clone());
                        found = true;
                        if p.want_pair {
                            p.want_pair = false;
                        }
                    }
                }
                tracing::debug!("dial {addr} failed: {error}");
                if user_initiated {
                    self.error(format!("Could not connect to {addr}: {error}"));
                }
                if found {
                    self.mark_dirty();
                }
            }
            NetEvent::IncomingData(ch) => self.on_incoming_data(*ch),
            NetEvent::ListenFailed { port, error } => self.error(format!("Cannot listen on port {port}: {error}")),
        }
    }

    fn on_connected(&mut self, conn: ConnHandle, user_initiated: bool) {
        let intro = conn.intro.clone();
        let me = self.me();
        if intro.id == me {
            conn.close();
            if user_initiated {
                self.error("That address is this device");
            }
            return;
        }
        if intro.protocol != PROTOCOL_VERSION {
            conn.close();
            self.error(format!("{} runs an incompatible version ({})", intro.name, intro.app_version));
            return;
        }
        let key_hex = hex::encode(conn.remote_key);
        let trusted = self.st.trusted(intro.id).cloned();
        let p = self.peers.entry(intro.id).or_insert_with(|| Peer::new(intro.id, intro.name.clone()));
        p.name = intro.name.clone();
        p.os = intro.os.clone();
        p.fingerprint = fingerprint(&conn.remote_key);
        p.addr = Some(SocketAddr::new(conn.addr.ip(), intro.port));
        p.dialing = None;
        p.backoff = Duration::from_secs(1);
        p.error = None;
        if p.last_seen.is_none() {
            p.last_seen = Some(Instant::now());
        }
        if let Some(t) = &trusted {
            if t.public_key != key_hex {
                p.error = Some("Security warning: this device presented a different identity key. If it was reinstalled, unpair it and pair again.".into());
                let msg = format!("{} presented a different identity key; connection refused", intro.name);
                conn.close();
                self.error(msg);
                self.mark_dirty();
                return;
            }
        }
        // Resolve duplicate connections deterministically: keep the one
        // initiated by the device with the lower id.
        if let Some(existing) = p.conn.or(p.pairing.as_ref().map(|x| x.conn)) {
            if let Some(old) = self.conns.get(&existing) {
                let initiator_of = |h: &ConnHandle| if h.initiator { me } else { intro.id };
                let preferred = me.min(intro.id);
                let keep_new = initiator_of(&conn) == preferred && initiator_of(&old.handle) != preferred;
                if !keep_new {
                    conn.close();
                    return;
                }
                old.handle.close();
                let old_id = existing;
                self.conns.remove(&old_id);
                if let Some(p) = self.peers.get_mut(&intro.id) {
                    p.conn = None;
                    if let Some(pair) = &mut p.pairing {
                        pair.conn = conn.id;
                        pair.sas = conn.sas.clone();
                    }
                }
            }
        }
        let now = Instant::now();
        self.conns.insert(
            conn.id,
            ConnState { handle: conn.clone(), peer: intro.id, last_rx: now, next_ping: now, ping_seq: 0, active: false },
        );
        if trusted.is_some() {
            self.activate(conn.id);
            return;
        }
        // Unknown device: pairing.
        let p = self.peers.get_mut(&intro.id).unwrap();
        if p.pairing.is_some() {
            return;
        }
        let incoming = !(user_initiated || p.want_pair);
        if incoming && p.rejected_at.is_some_and(|t| t.elapsed() < Duration::from_secs(60)) {
            conn.close();
            return;
        }
        if incoming && self.st.layout.contains(intro.id) {
            // A computer of our setup that already trusts us, before we
            // heard about it (see `on_introduce`): it connects again later.
            tracing::debug!("{} is not paired yet; waiting to hear about it from the setup", intro.name);
            conn.close();
            self.conns.remove(&conn.id);
            return;
        }
        if self.st.trusted.len() + 1 >= crate::config::MAX_DEVICES {
            let name = p.name.clone();
            p.want_pair = false;
            conn.send(Msg::PairReject { reason: Self::limit_message() });
            conn.close();
            self.conns.remove(&conn.id);
            if incoming {
                self.notice(Notice::Info {
                    message: format!("{name} asked to pair, but the limit of {} computers is reached", crate::config::MAX_DEVICES),
                });
            } else {
                self.error(Self::limit_message());
            }
            self.mark_dirty();
            return;
        }
        let p = self.peers.get_mut(&intro.id).unwrap();
        p.pairing = Some(Pairing {
            conn: conn.id,
            sas: conn.sas.clone(),
            incoming,
            local_ok: false,
            remote_ok: false,
            started: now,
            settings_from: if incoming { None } else { Some(me) },
        });
        p.want_pair = false;
        let name = p.name.clone();
        if incoming {
            self.notice(Notice::PairingRequest { device: intro.id, name, sas: conn.sas.clone() });
        } else {
            conn.send(Msg::PairSettings { from: me });
        }
        self.mark_dirty();
    }

    fn activate(&mut self, conn_id: u64) {
        let Some(c) = self.conns.get_mut(&conn_id) else { return };
        c.active = true;
        let peer = c.peer;
        let addr = c.handle.addr;
        let cipher = c.handle.cipher;
        if let Some(p) = self.peers.get_mut(&peer) {
            p.conn = Some(conn_id);
            p.pairing = None;
            tracing::info!("connected to {} ({}) using {}", p.name, addr, cipher.label());
            let paddr = p.addr.map(|a| a.to_string());
            let pname = p.name.clone();
            if let Some(t) = self.st.trusted.iter_mut().find(|t| t.id == peer) {
                if t.last_addr != paddr || t.name != pname {
                    t.last_addr = paddr;
                    t.name = pname;
                    self.save_at.get_or_insert(Instant::now() + Duration::from_millis(300));
                }
            }
        }
        let h = self.hello();
        self.send_to(peer, h);
        // Let the peer know our current clipboard.
        if let Some((seq, formats)) = self.clip.local.clone().filter(|_| self.clipboard_on()) {
            self.send_to(peer, Msg::ClipOffer { seq, formats: self.shareable_formats(&formats) });
        }
        self.sync_setup_with(peer);
        self.update_edges();
        self.mark_dirty();
    }

    pub(crate) fn on_closed(&mut self, conn_id: u64, reason: String) {
        let Some(c) = self.conns.remove(&conn_id) else { return };
        let peer = c.peer;
        tracing::info!("connection to {:?} closed: {reason}", peer);
        let mut pairing_failed = false;
        if let Some(p) = self.peers.get_mut(&peer) {
            if p.conn == Some(conn_id) {
                p.conn = None;
                p.rtt_ms = None;
                p.next_dial = Instant::now() + Duration::from_millis(500);
            }
            if p.pairing.as_ref().is_some_and(|x| x.conn == conn_id) {
                p.pairing = None;
                pairing_failed = true;
            }
        }
        if pairing_failed {
            let name = self.peers.get(&peer).map(|p| p.name.clone()).unwrap_or_default();
            self.notice(Notice::PairingFailed { device: peer, name, reason: "Connection closed".into() });
        }
        if c.active {
            self.peer_gone(peer);
        }
        self.update_edges();
        self.mark_dirty();
    }

    fn on_message(&mut self, conn_id: u64, msg: Msg) {
        let Some(c) = self.conns.get_mut(&conn_id) else { return };
        c.last_rx = Instant::now();
        let peer = c.peer;
        let active = c.active;
        match msg {
            Msg::Ping { seq, sent_us } => {
                c.handle.send(Msg::Pong { seq, sent_us });
                return;
            }
            Msg::Pong { sent_us, .. } => {
                let rtt = (crate::types::mono_us().saturating_sub(sent_us)) as f32 / 1000.0;
                if let Some(p) = self.peers.get_mut(&peer) {
                    let old = p.rtt_ms;
                    p.rtt_ms = Some(match old {
                        Some(o) => o * 0.7 + rtt * 0.3,
                        None => rtt,
                    });
                    if old.is_none_or(|o| (o - rtt).abs() > 0.5) {
                        self.dirty = true;
                    }
                }
                return;
            }
            Msg::PairAccept if !active => {
                if let Some(p) = self.peers.get_mut(&peer) {
                    if let Some(x) = &mut p.pairing {
                        x.remote_ok = true;
                    }
                }
                self.maybe_finish_pairing(peer);
                self.mark_dirty();
                return;
            }
            Msg::PairSettings { from } if !active => {
                let me = self.me();
                if let Some(x) = self.peers.get_mut(&peer).and_then(|p| p.pairing.as_mut()) {
                    if x.incoming && (from == peer || from == me) {
                        x.settings_from = Some(from);
                    }
                }
                self.mark_dirty();
                return;
            }
            Msg::PairReject { reason } if !active => {
                let reason = if reason.is_empty() { "The other device declined".to_string() } else { reason };
                self.fail_pairing(peer, &reason);
                return;
            }
            Msg::PairAccept => {
                // We already trust this device (e.g. restored from a config
                // file) while it is still pairing with us: confirm.
                c.handle.send(Msg::PairAccept);
                return;
            }
            _ => {}
        }
        if !active {
            // Nothing else is allowed before the device is trusted.
            return;
        }
        if !msg.is_input() {
            tracing::trace!("<- {:?}: {:?}", peer, msg);
        }
        match msg {
            Msg::Hello(h) => self.on_hello(peer, *h),
            Msg::Layout(l) => self.on_layout(peer, l),
            Msg::Shared(s) => {
                if s.version() > self.st.shared.version() {
                    self.adopt_shared(s, Some(peer));
                }
            }
            Msg::Geometry(d) => {
                if let Some(p) = self.peers.get_mut(&peer) {
                    p.desktop = d.clone();
                }
                self.on_peer_geometry(peer, d);
                self.update_edges();
                self.mark_dirty();
            }
            Msg::Identify { .. } => {
                let number = self.st.layout.number_of(self.me()).unwrap_or(0);
                self.notice(Notice::Identify { number, name: self.st.settings.device_name.clone() });
            }
            Msg::Rename { name } => {
                if let Some(p) = self.peers.get_mut(&peer) {
                    p.name = name.clone();
                }
                if let Some(t) = self.st.trusted.iter_mut().find(|t| t.id == peer) {
                    t.name = name;
                    self.schedule_save();
                }
                self.mark_dirty();
            }
            Msg::Unpaired => {
                let name = self.peers.get(&peer).map(|p| p.name.clone()).unwrap_or_default();
                self.notice(Notice::Info { message: format!("{name} removed this device from its paired devices") });
                let _ = self.unpair(peer, false);
            }
            Msg::Enter { session, pos } => self.on_remote_enter(peer, session, pos),
            Msg::Leave { session } => self.on_remote_leave(peer, session),
            Msg::ControlEnded { session } => self.on_control_ended(peer, session),
            Msg::MoveAbs { .. }
            | Msg::MoveRel { .. }
            | Msg::Button { .. }
            | Msg::Key { .. }
            | Msg::Scroll { .. }
            | Msg::ScrollDiscrete { .. } => self.on_remote_input(peer, msg),
            Msg::ClipOffer { seq, formats } => self.on_clip_offer(peer, seq, formats),
            Msg::ClipRequest { req, seq, format } => self.on_clip_request(peer, req, seq, format),
            Msg::ClipData { req, data } => self.on_clip_data(peer, req, data),
            Msg::TransferOffer { id, purpose, items, file_count, total_bytes } => {
                self.on_transfer_offer(peer, id, purpose, items, file_count, total_bytes)
            }
            Msg::TransferAccept { id } => self.on_transfer_accept(peer, id),
            Msg::TransferReject { id, reason } => self.on_transfer_reject(peer, id, reason),
            Msg::TransferCancel { id } => self.on_transfer_cancel_remote(peer, id),
            Msg::DragBegin { drag, transfer, items } => self.on_drag_begin(peer, drag, transfer, items),
            Msg::DragCancel { drag } => self.on_drag_cancel(peer, drag),
            Msg::DragFinished { drag, accepted } => self.on_drag_finished(peer, drag, accepted),
            Msg::Introduce(list) => self.on_introduce(peer, list),
            Msg::Forget(list) => self.on_forget(peer, list),
            Msg::ConfigSync(c) => self.on_config_sync(*c),
            Msg::ConfigDelete(t) => self.on_config_delete(t),
            Msg::PairSettings { .. } => {}
            Msg::Ping { .. } | Msg::Pong { .. } | Msg::PairAccept | Msg::PairReject { .. } => {}
        }
    }

    // ------------------------------------------------------------ pairing

    pub(crate) fn start_pairing(&mut self, id: DeviceId) -> Result<(), String> {
        if self.is_trusted(id) {
            return Err("Already paired".into());
        }
        if self.group_full() {
            return Err(Self::limit_message());
        }
        let p = self.peers.get_mut(&id).ok_or("Unknown device")?;
        if p.pairing.is_some() {
            return Ok(());
        }
        if p.addr.is_none() {
            return Err("Device address unknown".into());
        }
        p.want_pair = true;
        p.next_dial = Instant::now();
        p.error = None;
        self.maybe_dial(id);
        Ok(())
    }

    pub(crate) fn accept_pairing(&mut self, id: DeviceId) -> Result<(), String> {
        if self.group_full() {
            let _ = self.reject_pairing_with(id, &Self::limit_message());
            return Err(Self::limit_message());
        }
        let p = self.peers.get_mut(&id).ok_or("Unknown device")?;
        let pairing = p.pairing.as_mut().ok_or("No pairing in progress")?;
        pairing.local_ok = true;
        let conn = pairing.conn;
        if let Some(c) = self.conns.get(&conn) {
            c.handle.send(Msg::PairAccept);
        }
        self.maybe_finish_pairing(id);
        self.mark_dirty();
        Ok(())
    }

    /// On the computer that started the pairing: both keep the settings of
    /// `from` (this computer or the other one).
    pub(crate) fn set_pair_settings(&mut self, id: DeviceId, from: DeviceId) -> Result<(), String> {
        let me = self.me();
        if from != me && from != id {
            return Err("Choose one of the two computers".into());
        }
        let p = self.peers.get_mut(&id).ok_or("Unknown device")?;
        let pairing = p.pairing.as_mut().ok_or("No pairing in progress")?;
        if pairing.incoming {
            return Err("The computer that started the pairing chooses".into());
        }
        if pairing.local_ok {
            return Err("The pairing is already confirmed".into());
        }
        pairing.settings_from = Some(from);
        if let Some(c) = self.conns.get(&pairing.conn) {
            c.handle.send(Msg::PairSettings { from });
        }
        self.mark_dirty();
        Ok(())
    }

    pub(crate) fn reject_pairing(&mut self, id: DeviceId) -> Result<(), String> {
        self.reject_pairing_with(id, "")
    }

    fn reject_pairing_with(&mut self, id: DeviceId, reason: &str) -> Result<(), String> {
        let p = self.peers.get_mut(&id).ok_or("Unknown device")?;
        p.rejected_at = Some(Instant::now());
        if let Some(pairing) = p.pairing.take() {
            if let Some(c) = self.conns.remove(&pairing.conn) {
                c.handle.send(Msg::PairReject { reason: reason.to_string() });
                c.handle.close();
            }
        }
        self.mark_dirty();
        Ok(())
    }

    fn fail_pairing(&mut self, id: DeviceId, reason: &str) {
        let Some(p) = self.peers.get_mut(&id) else { return };
        let Some(pairing) = p.pairing.take() else { return };
        let name = p.name.clone();
        if let Some(c) = self.conns.remove(&pairing.conn) {
            c.handle.close();
        }
        self.notice(Notice::PairingFailed { device: id, name, reason: reason.into() });
        self.mark_dirty();
    }

    fn maybe_finish_pairing(&mut self, id: DeviceId) {
        let full = self.group_full();
        let Some(p) = self.peers.get_mut(&id) else { return };
        let Some(pairing) = &p.pairing else { return };
        if !(pairing.local_ok && pairing.remote_ok) {
            return;
        }
        if full {
            self.fail_pairing(id, &Self::limit_message());
            return;
        }
        let conn = pairing.conn;
        let Some(c) = self.conns.get(&conn) else { return };
        let key = hex::encode(c.handle.remote_key);
        let name = p.name.clone();
        let settings_from = pairing.settings_from;
        p.pairing = None;
        self.st.trusted.retain(|t| t.id != id);
        self.st.trusted.push(TrustedPeer {
            id,
            name: name.clone(),
            public_key: key,
            last_addr: p.addr.map(|a| a.to_string()),
            paired_at_ms: now_ms(),
        });
        self.save_now();
        tracing::info!("paired with {name}");
        self.notice(Notice::Paired { device: id, name });
        // Put the new computer into the arrangement (see `on_hello`).
        self.just_paired.insert(id, settings_from);
        self.forget_removal(id);
        self.activate(conn);
    }

    pub(crate) fn unpair(&mut self, id: DeviceId, tell_peer: bool) -> Result<(), String> {
        if tell_peer {
            self.send_to(id, Msg::Unpaired);
        }
        let conn = self.peers.get(&id).and_then(|p| p.conn);
        self.st.trusted.retain(|t| t.id != id);
        if let Some(cid) = conn {
            if let Some(c) = self.conns.get(&cid) {
                let h = c.handle.clone();
                // Give the Unpaired message a moment to go out.
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    h.close();
                });
            }
            self.on_closed(cid, "unpaired".into());
        }
        if self.st.layout.contains(id) {
            let mut l = self.st.layout.clone();
            l.remove(id);
            l.rev = self.st.layout.rev + 1;
            l.author = self.me();
            self.adopt_layout(l.normalized(), None);
        }
        let forget = match self.peers.get_mut(&id) {
            Some(p) => {
                p.want_pair = false;
                p.error = None;
                p.last_seen.is_none()
            }
            None => false,
        };
        if forget {
            self.peers.remove(&id);
        }
        self.save_now();
        self.mark_dirty();
        Ok(())
    }

    // ------------------------------------------------------------ shared state

    fn on_hello(&mut self, peer: DeviceId, h: Hello) {
        let me = self.me();
        if let Some(p) = self.peers.get_mut(&peer) {
            p.desktop = h.desktop.clone();
            p.caps = h.caps.clone();
            p.heartbeat_ms = h.heartbeat_ms.max(100);
        }
        self.on_peer_geometry(peer, h.desktop.clone());
        let fresh = self.just_paired.remove(&peer);
        let keep = fresh.flatten();
        match keep {
            // Just paired, and the settings to keep were chosen: both
            // computers make the same, newer version of them, which then
            // reaches the rest of both setups.
            Some(from) => {
                let base = if from == me { self.st.shared.clone() } else { h.shared.clone() };
                let s = SharedSettings { rev: self.st.shared.rev.max(h.shared.rev) + 1, author: from, ..base };
                tracing::info!("keeping the settings of {}", if from == me { "this computer".into() } else { self.peer_label(from) });
                self.adopt_shared(s, None);
            }
            None if h.shared.version() > self.st.shared.version() => self.adopt_shared(h.shared.clone(), Some(peer)),
            None => {}
        }
        let both_present = self.st.layout.contains(peer) && h.layout.contains(me);
        if fresh.is_none() || both_present {
            self.consider_layout(peer, h.layout);
        } else if me < peer {
            // Just paired: we lead the merge for this pair. The arrangement
            // of the computer whose settings are kept keeps its places.
            let mut merged = match keep {
                Some(from) if from == me => Layout::merge(&self.st.layout, &h.layout),
                Some(_) => Layout::merge(&h.layout, &self.st.layout),
                None => self.st.layout.merged_with(&h.layout),
            };
            if !merged.contains(me) {
                let n = self.st.settings.device_name.clone();
                merged.place_next_to(me, &n, peer);
            }
            if !merged.contains(peer) {
                let name = self.peers.get(&peer).map(|p| p.name.clone()).unwrap_or_default();
                merged.place_next_to(peer, &name, me);
            }
            merged.rev = self.st.layout.rev.max(h.layout.rev) + 1;
            merged.author = me;
            self.adopt_layout(merged.normalized(), None);
        }
        // Its screen may just have been switched off under our cursor.
        if matches!(&self.input, Input::Controlling(c) if c.target == peer) && !self.reachable(peer) {
            self.relocate_cursor();
        }
        self.update_edges();
        self.mark_dirty();
    }

    fn on_layout(&mut self, from: DeviceId, l: Layout) {
        self.consider_layout(from, l);
    }

    /// An arrangement from another computer. Computers only follow
    /// arrangements that include them, so separate setups (other configs)
    /// do not interfere.
    fn consider_layout(&mut self, from: DeviceId, l: Layout) {
        let me = self.me();
        if !l.is_newer_than(&self.st.layout) {
            return;
        }
        if l.contains(me) {
            self.adopt_layout(l, Some(from));
        } else if self.st.layout.contains(from) && l.contains(from) {
            // `from` moved to a setup without us: drop it from ours.
            tracing::info!("{from:?} left this arrangement");
            let mut mine = self.st.layout.clone();
            mine.remove(from);
            mine.rev = l.rev.max(self.st.layout.rev) + 1;
            mine.author = me;
            self.adopt_layout(mine.normalized(), None);
        }
    }

    pub(crate) fn adopt_layout(&mut self, l: Layout, from: Option<DeviceId>) {
        self.st.layout = l;
        self.broadcast_except(from, Msg::Layout(self.st.layout.clone()));
        self.schedule_save();
        self.apply_controls();
        self.setup_changed();
        self.mark_dirty();
    }

    pub(crate) fn set_layout_from_ui(&mut self, mut l: Layout) -> Result<(), String> {
        if l.slots.len() > crate::config::MAX_DEVICES {
            return Err(format!("At most {} screens can be arranged", crate::config::MAX_DEVICES));
        }
        l.rev = self.st.layout.rev + 1;
        l.author = self.me();
        let l = l.normalized();
        // Keep cached names fresh.
        let mut l = l;
        for s in &mut l.slots {
            if s.device == self.st.device_id {
                s.name = self.st.settings.device_name.clone();
            } else if let Some(p) = self.peers.get(&s.device) {
                s.name = p.name.clone();
            }
        }
        self.adopt_layout(l, None);
        Ok(())
    }

    /// Take `s` as the shared settings (made here, or newer ones from `from`)
    /// and pass them on to the other connected devices.
    pub(crate) fn adopt_shared(&mut self, s: SharedSettings, from: Option<DeviceId>) {
        let old_cipher = self.st.shared.cipher.resolve();
        let old_settings = self.st.settings.clone();
        s.common.apply_to(&mut self.st.settings);
        self.st.shared = s;
        self.st.shared.common = crate::config::CommonSettings::of(&self.st.settings);
        if old_settings != self.st.settings {
            tracing::info!("shared settings changed{}", from.map(|f| format!(" on {}", self.peer_label(f))).unwrap_or_default());
            self.settings_changed(old_settings);
        }
        let new_cipher = self.st.shared.cipher.resolve();
        self.net.policy.write().preferred = new_cipher;
        self.broadcast_except(from, Msg::Shared(self.st.shared.clone()));
        self.schedule_save();
        if old_cipher != new_cipher {
            self.reconnect_with_cipher(new_cipher);
        }
        self.mark_dirty();
    }

    /// Re-establish connections that use another cipher than the configured one.
    fn reconnect_with_cipher(&mut self, cipher: Cipher) {
        let stale: Vec<ConnHandle> =
            self.conns.values().filter(|c| c.active && c.handle.cipher != cipher).map(|c| c.handle.clone()).collect();
        if stale.is_empty() {
            return;
        }
        tracing::info!("cipher changed to {}; reconnecting {} device(s)", cipher.label(), stale.len());
        tokio::spawn(async move {
            // Let the settings update reach the peers first.
            tokio::time::sleep(Duration::from_millis(700)).await;
            for h in stale {
                h.close();
            }
        });
    }

    pub(crate) fn peer_view(&self, p: &Peer) -> PeerView {
        let trusted = self.is_trusted(p.id);
        let conn = p.conn.and_then(|c| self.conns.get(&c));
        let status = if p.pairing.is_some() {
            PeerStatus::Pairing
        } else if conn.is_some_and(|c| c.active) {
            PeerStatus::Connected
        } else if p.dialing.is_some() && (trusted || p.want_pair) {
            PeerStatus::Connecting
        } else if trusted {
            PeerStatus::Offline
        } else {
            PeerStatus::Discovered
        };
        let pairing = p.pairing.as_ref().map(|x| PairingView {
            sas: x.sas.clone(),
            incoming: x.incoming,
            local_accepted: x.local_ok,
            remote_accepted: x.remote_ok,
            settings_from: x.settings_from,
        });
        let cipher = p
            .pairing
            .as_ref()
            .and_then(|x| self.conns.get(&x.conn))
            .or(conn)
            .map(|c| c.handle.cipher);
        PeerView {
            id: p.id,
            name: p.name.clone(),
            os: p.os.clone(),
            address: p.addr.map(|a| a.ip().to_string()),
            fingerprint: p.fingerprint.clone(),
            status,
            trusted,
            rtt_ms: p.rtt_ms,
            cipher,
            pairing,
            desktop: p.desktop.clone(),
            caps: p.caps.clone(),
            error: p.error.clone(),
            last_seen_ms: p.last_seen.map(|t| t.elapsed().as_millis() as u64),
        }
    }
}
