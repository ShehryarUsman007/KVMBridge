//! File and blob transfers (manual sends, drag-and-drop, clipboard files).

use super::*;
use crate::net::DataChannel;
use crate::proto::{ItemMeta, TransferPurpose, MAX_SUMMARY_ITEMS};
use crate::transfer::{self, Destination, Manifest, Progress, Received};
use tokio::task::AbortHandle;

pub(crate) struct Transfer {
    pub id: u64,
    /// Id used on the wire (equal to `id` for outgoing transfers).
    pub wire_id: u64,
    pub peer: DeviceId,
    pub peer_name: String,
    pub outgoing: bool,
    pub purpose: TransferPurpose,
    pub state: TransferState,
    pub summary: Vec<ItemMeta>,
    pub file_count: u32,
    pub progress: Arc<Progress>,
    pub manifest: Option<Manifest>,
    pub task: Option<AbortHandle>,
    pub error: Option<String>,
    pub paths: Vec<PathBuf>,
    pub finished_at: Option<Instant>,
    pub rate: f64,
    pub sample: (Instant, u64),
    pub dest: Option<Destination>,
}

impl Transfer {
    pub fn view(&self) -> TransferView {
        let (done, total) = self.progress.get();
        TransferView {
            id: self.id,
            peer: self.peer,
            outgoing: self.outgoing,
            kind: match self.purpose {
                TransferPurpose::Manual => TransferKind::Manual,
                TransferPurpose::Drag { .. } => TransferKind::Drag,
                _ => TransferKind::Clipboard,
            },
            state: self.state,
            items: self.summary.clone(),
            file_count: self.file_count,
            bytes_done: done,
            bytes_total: total,
            rate_bps: self.rate,
            error: self.error.clone(),
            paths: self.paths.iter().map(|p| p.display().to_string()).collect(),
        }
    }

    fn active(&self) -> bool {
        matches!(self.state, TransferState::Preparing | TransferState::Waiting | TransferState::Running)
    }
}

fn next_transfer_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    if NEXT.load(Ordering::Relaxed) == 0 {
        let _ = NEXT.compare_exchange(0, (crate::crypto::random_u64() >> 16) | 1, Ordering::Relaxed, Ordering::Relaxed);
    }
    NEXT.fetch_add(1, Ordering::Relaxed)
}

impl Engine {
    fn receive_dir(&self) -> PathBuf {
        self.st.settings.transfers.receive_dir.clone().unwrap_or_else(|| self.paths.default_receive_dir.clone())
    }

    fn new_transfer(&mut self, peer: DeviceId, outgoing: bool, purpose: TransferPurpose, wire_id: Option<u64>) -> u64 {
        let id = next_transfer_id();
        let peer_name = self.peers.get(&peer).map(|p| p.name.clone()).unwrap_or_default();
        self.transfers.insert(
            id,
            Transfer {
                id,
                wire_id: wire_id.unwrap_or(id),
                peer,
                peer_name,
                outgoing,
                purpose,
                state: TransferState::Preparing,
                summary: vec![],
                file_count: 0,
                progress: Progress::new(0),
                manifest: None,
                task: None,
                error: None,
                paths: vec![],
                finished_at: None,
                rate: 0.0,
                sample: (Instant::now(), 0),
                dest: None,
            },
        );
        // Forget old finished transfers.
        if self.transfers.len() > 60 {
            let mut done: Vec<(u64, Instant)> =
                self.transfers.values().filter_map(|t| t.finished_at.map(|f| (t.id, f))).collect();
            done.sort_by_key(|x| x.1);
            for (id, _) in done.iter().take(self.transfers.len() - 50) {
                self.transfers.remove(id);
            }
        }
        self.mark_dirty();
        id
    }

    pub(crate) fn send_files(&mut self, to: DeviceId, paths: Vec<PathBuf>) -> Result<(), String> {
        if self.conn_of(to).is_none() {
            return Err("That device is not connected".into());
        }
        if paths.is_empty() {
            return Err("Nothing to send".into());
        }
        self.start_outgoing(to, paths, TransferPurpose::Manual);
        Ok(())
    }

    /// Prepare (walk) files in the background, then offer them to `peer`.
    pub(crate) fn start_outgoing(&mut self, peer: DeviceId, paths: Vec<PathBuf>, purpose: TransferPurpose) -> u64 {
        let id = self.new_transfer(peer, true, purpose, None);
        if let Some(t) = self.transfers.get_mut(&id) {
            t.summary = paths
                .iter()
                .take(MAX_SUMMARY_ITEMS)
                .map(|p| ItemMeta {
                    rel_path: p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                    size: 0,
                    is_dir: p.is_dir(),
                    mtime: None,
                })
                .collect();
        }
        let internal = self.internal.clone();
        tokio::task::spawn_blocking(move || {
            let result = transfer::build_manifest(&paths).map_err(|e| e.to_string());
            let _ = internal.send(Internal::ManifestReady { id, result });
        });
        id
    }

    pub(crate) fn start_outgoing_blob(&mut self, peer: DeviceId, name: &str, data: Vec<u8>, purpose: TransferPurpose) -> u64 {
        let id = self.new_transfer(peer, true, purpose, None);
        self.on_manifest_ready(id, Ok(transfer::blob_manifest(name, data)));
        id
    }

    pub(crate) fn on_manifest_ready(&mut self, id: u64, result: Result<Manifest, String>) {
        let Some(t) = self.transfers.get_mut(&id) else { return };
        if t.state != TransferState::Preparing {
            return;
        }
        match result {
            Ok(m) => {
                t.summary = m.summary.clone();
                t.file_count = m.file_count;
                t.progress.total.store(m.total_bytes, std::sync::atomic::Ordering::Relaxed);
                t.state = TransferState::Waiting;
                let msg = Msg::TransferOffer {
                    id,
                    purpose: t.purpose,
                    items: m.summary.clone(),
                    file_count: m.file_count,
                    total_bytes: m.total_bytes,
                };
                t.manifest = Some(m);
                let peer = t.peer;
                if !self.send_to(peer, msg) {
                    self.finish_transfer(id, Err("device disconnected".into()));
                }
            }
            Err(e) => self.finish_transfer(id, Err(e)),
        }
        self.mark_dirty();
    }

    pub(crate) fn on_transfer_accept(&mut self, peer: DeviceId, id: u64) {
        let Some(t) = self.transfers.get_mut(&id) else { return };
        if !t.outgoing || t.peer != peer || t.state != TransferState::Waiting {
            return;
        }
        let Some(manifest) = t.manifest.take() else { return };
        let key = self.st.trusted(peer).and_then(|tp| hex::decode(&tp.public_key).ok());
        let addr = self.peers.get(&peer).and_then(|p| p.addr);
        let (Some(key), Some(addr)) = (key, addr) else {
            self.finish_transfer(id, Err("device address unknown".into()));
            return;
        };
        let mut k = [0u8; 32];
        k.copy_from_slice(&key);
        let t = self.transfers.get_mut(&id).unwrap();
        t.state = TransferState::Running;
        t.sample = (Instant::now(), 0);
        let progress = t.progress.clone();
        let net = self.net.clone();
        let internal = self.internal.clone();
        let task = tokio::spawn(async move {
            let res = async {
                let ch = net::dial_data(&net, addr, &k).await?;
                transfer::send(ch.reader, ch.writer, id, manifest, progress).await
            }
            .await;
            let _ = internal.send(Internal::TransferFinished { id, result: res.map(|_| Received::Files(vec![])) });
        });
        t.task = Some(task.abort_handle());
        self.mark_dirty();
    }

    pub(crate) fn on_transfer_reject(&mut self, peer: DeviceId, id: u64, reason: String) {
        if self.transfers.get(&id).is_some_and(|t| t.outgoing && t.peer == peer) {
            self.finish_transfer(id, Err(format!("declined: {reason}")));
        }
    }

    pub(crate) fn on_transfer_offer(
        &mut self,
        peer: DeviceId,
        wire_id: u64,
        purpose: TransferPurpose,
        items: Vec<ItemMeta>,
        file_count: u32,
        total_bytes: u64,
    ) {
        let reject = |s: &Engine, reason: &str| {
            s.send_to(peer, Msg::TransferReject { id: wire_id, reason: reason.into() });
        };
        let dest = match purpose {
            TransferPurpose::Manual | TransferPurpose::Drag { .. } => Destination::Dir(self.receive_dir()),
            TransferPurpose::ClipboardFiles { .. } => {
                let d = self.paths.cache_dir.join("clipboard").join(format!("{:x}", crate::crypto::random_u64()));
                Destination::Dir(d)
            }
            TransferPurpose::ClipboardBlob { .. } => {
                if total_bytes > self.st.settings.clipboard.max_bytes {
                    return reject(self, "too large");
                }
                Destination::Memory
            }
        };
        if let TransferPurpose::Drag { .. } = purpose {
            if !self.st.settings.transfers.drag_and_drop {
                return reject(self, "drag and drop is disabled");
            }
        }
        let id = self.new_transfer(peer, false, purpose, Some(wire_id));
        match purpose {
            TransferPurpose::ClipboardFiles { req } | TransferPurpose::ClipboardBlob { req } => {
                if !self.clip_transfer_offered(peer, req, id) {
                    self.transfers.remove(&id);
                    return reject(self, "not requested");
                }
            }
            TransferPurpose::Drag { drag } => self.dnd_transfer_offered(peer, drag, id),
            TransferPurpose::Manual => {}
        }
        let t = self.transfers.get_mut(&id).unwrap();
        t.summary = items;
        t.file_count = file_count;
        t.progress.total.store(total_bytes, std::sync::atomic::Ordering::Relaxed);
        t.state = TransferState::Waiting;
        t.dest = Some(dest);
        self.incoming.insert((peer, wire_id), id);
        self.send_to(peer, Msg::TransferAccept { id: wire_id });
        self.mark_dirty();
    }

    pub(crate) fn on_incoming_data(&mut self, ch: DataChannel) {
        let peer = ch.intro.id;
        let trusted = self
            .st
            .trusted(peer)
            .is_some_and(|t| hex::decode(&t.public_key).ok().as_deref() == Some(&ch.remote_key[..]));
        if !trusted {
            tracing::warn!("data channel from untrusted device {:?} refused", peer);
            return;
        }
        let internal = self.internal.clone();
        tokio::spawn(async move {
            let mut ch = ch;
            match tokio::time::timeout(Duration::from_secs(10), transfer::read_open(&mut ch.reader)).await {
                Ok(Ok(wire_id)) => {
                    let _ = internal.send(Internal::DataOpened { peer, wire_id, channel: Box::new(ch) });
                }
                _ => tracing::debug!("data channel without a valid Open frame"),
            }
        });
    }

    pub(crate) fn on_data_opened(&mut self, peer: DeviceId, wire_id: u64, ch: DataChannel) {
        let Some(&id) = self.incoming.get(&(peer, wire_id)) else {
            tokio::spawn(transfer::refuse(ch.writer, "unknown transfer"));
            return;
        };
        let Some(t) = self.transfers.get_mut(&id) else { return };
        if t.state != TransferState::Waiting {
            tokio::spawn(transfer::refuse(ch.writer, "transfer not waiting"));
            return;
        }
        let dest = t.dest.clone().unwrap_or(Destination::Memory);
        let max = t.progress.total.load(std::sync::atomic::Ordering::Relaxed);
        let progress = t.progress.clone();
        t.state = TransferState::Running;
        t.sample = (Instant::now(), 0);
        let internal = self.internal.clone();
        let task = tokio::spawn(async move {
            let res = transfer::receive(ch.reader, ch.writer, dest, max, progress).await;
            let _ = internal.send(Internal::TransferFinished { id, result: res });
        });
        t.task = Some(task.abort_handle());
        self.mark_dirty();
    }

    pub(crate) fn finish_transfer(&mut self, id: u64, result: Result<Received, String>) {
        let Some(t) = self.transfers.get_mut(&id) else { return };
        if !t.active() {
            return;
        }
        t.finished_at = Some(Instant::now());
        t.task = None;
        t.manifest = None;
        match &result {
            Ok(r) => {
                t.state = TransferState::Done;
                let (_, total) = t.progress.get();
                t.progress.done.store(total, std::sync::atomic::Ordering::Relaxed);
                if let Received::Files(p) = r {
                    t.paths = p.clone();
                }
            }
            Err(e) => {
                t.state = TransferState::Failed;
                t.error = Some(e.clone());
            }
        }
        let (purpose, outgoing, peer, wire_id) = (t.purpose, t.outgoing, t.peer, t.wire_id);
        let notice = Notice::TransferFinished {
            id,
            peer_name: t.peer_name.clone(),
            outgoing,
            ok: result.is_ok(),
            error: result.as_ref().err().cloned(),
            paths: t.paths.iter().map(|p| p.display().to_string()).collect(),
        };
        if !outgoing {
            self.incoming.remove(&(peer, wire_id));
        }
        match purpose {
            TransferPurpose::Manual => self.notice(notice),
            TransferPurpose::Drag { drag } => {
                if !outgoing {
                    self.dnd_transfer_done(drag, result.as_ref().map_err(Clone::clone));
                }
                if result.is_err() {
                    self.notice(notice);
                }
            }
            TransferPurpose::ClipboardFiles { req } | TransferPurpose::ClipboardBlob { req } => {
                if !outgoing {
                    self.clip_transfer_done(req, result.as_ref().map_err(Clone::clone));
                } else if result.is_err() {
                    // Tell the requester it will not get anything.
                    self.send_to(peer, Msg::ClipData { req, data: None });
                }
            }
        }
        if let Err(e) = &result {
            tracing::warn!("transfer {id} failed: {e}");
        }
        self.mark_dirty();
    }

    pub(crate) fn cancel_transfer(&mut self, id: u64, tell_peer: bool) -> Result<(), String> {
        let t = self.transfers.get_mut(&id).ok_or("Unknown transfer")?;
        if !t.active() {
            return Ok(());
        }
        if let Some(h) = t.task.take() {
            h.abort();
        }
        let (peer, wire) = (t.peer, t.wire_id);
        self.finish_transfer(id, Err("cancelled".into()));
        if let Some(t) = self.transfers.get_mut(&id) {
            t.state = TransferState::Cancelled;
        }
        if tell_peer {
            self.send_to(peer, Msg::TransferCancel { id: wire });
        }
        Ok(())
    }

    pub(crate) fn on_transfer_cancel_remote(&mut self, peer: DeviceId, wire_id: u64) {
        let local = self
            .transfers
            .values()
            .find(|t| t.peer == peer && t.wire_id == wire_id && t.active())
            .map(|t| t.id);
        if let Some(id) = local {
            let _ = self.cancel_transfer(id, false);
        }
    }

    pub(crate) fn transfers_peer_gone(&mut self, peer: DeviceId) {
        let ids: Vec<u64> = self
            .transfers
            .values()
            .filter(|t| t.peer == peer && matches!(t.state, TransferState::Preparing | TransferState::Waiting))
            .map(|t| t.id)
            .collect();
        for id in ids {
            self.finish_transfer(id, Err("device disconnected".into()));
        }
    }

    pub(crate) fn tick_transfers(&mut self) {
        let now = Instant::now();
        let mut any = false;
        for t in self.transfers.values_mut().filter(|t| t.state == TransferState::Running) {
            any = true;
            let (done, _) = t.progress.get();
            let dt = now.duration_since(t.sample.0).as_secs_f64();
            if dt >= 0.5 {
                let r = (done.saturating_sub(t.sample.1)) as f64 / dt;
                t.rate = if t.rate == 0.0 { r } else { t.rate * 0.5 + r * 0.5 };
                t.sample = (now, done);
            }
        }
        if any {
            self.mark_dirty();
        }
    }

    pub(crate) fn on_internal(&mut self, ev: Internal) {
        match ev {
            Internal::ManifestReady { id, result } => self.on_manifest_ready(id, result),
            Internal::TransferFinished { id, result } => self.finish_transfer(id, result),
            Internal::ClipReadDone { peer, req, format, data } => self.on_clip_read_done(peer, req, format, data),
            Internal::DataOpened { peer, wire_id, channel } => self.on_data_opened(peer, wire_id, *channel),
            Internal::HandoffNudge { probe } => self.nudge_handoff(probe),
        }
    }
}
