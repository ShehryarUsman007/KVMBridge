//! Shared clipboard: offers are broadcast when something is copied, the data
//! itself is only fetched when a remote application pastes.

use super::*;
use crate::proto::TransferPurpose;
use crate::transfer::Received;
use crate::types::ClipFormat;

/// Payloads up to this size travel inline on the control channel.
pub(crate) const INLINE_MAX: usize = 48 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Default)]
pub(crate) struct ClipState {
    /// Our own clipboard content as currently offered to peers.
    pub local: Option<(u64, Vec<ClipFormat>)>,
    pub local_seq: u64,
    /// Remote content currently advertised on this machine.
    pub remote: Option<RemoteClip>,
    pub next_offer: u64,
    pub pending: HashMap<u64, PendingPaste>,
    pub next_req: u64,
}

pub(crate) struct RemoteClip {
    pub peer: DeviceId,
    pub seq: u64,
    pub offer: u64,
    pub formats: Vec<ClipFormat>,
    /// Data already fetched for this offer (applications paste the same
    /// content several times, and files should only travel once).
    pub fetched: HashMap<ClipFormat, Vec<u8>>,
}

pub(crate) struct PendingPaste {
    pub peer: DeviceId,
    pub seq: u64,
    pub format: ClipFormat,
    /// Every local request waiting for this data.
    pub replies: Vec<oneshot::Sender<Option<Vec<u8>>>>,
    pub started: Instant,
    /// Set once the data is coming through a transfer (no timeout then).
    pub transfer: Option<u64>,
}

impl Engine {
    /// Clipboard sharing is on (and KVM Bridge is switched on).
    pub(crate) fn clipboard_on(&self) -> bool {
        self.st.enabled && self.st.settings.clipboard.enabled
    }

    /// A peer's name for the log.
    pub(crate) fn peer_label(&self, peer: DeviceId) -> String {
        self.peers.get(&peer).map_or_else(|| format!("{peer:?}"), |p| p.name.clone())
    }

    pub(crate) fn shareable_formats(&self, f: &[ClipFormat]) -> Vec<ClipFormat> {
        f.iter().copied().filter(|x| *x != ClipFormat::Files || self.st.settings.clipboard.files).collect()
    }

    pub(crate) fn on_clipboard(&mut self, ev: ClipboardEvent) {
        match ev {
            ClipboardEvent::Status(s) => {
                tracing::info!("clipboard backend: {s:?}");
                self.backends.clipboard = s;
                self.broadcast_caps();
                self.mark_dirty();
            }
            ClipboardEvent::LocalChanged { formats } => {
                if !self.clipboard_on() {
                    tracing::info!("clipboard changed here; sharing is off");
                    return;
                }
                self.clip.local_seq += 1;
                let seq = self.clip.local_seq;
                self.clip.local = Some((seq, formats.clone()));
                self.clip.remote = None;
                let share = self.shareable_formats(&formats);
                let peers = self.connected_peers().len();
                tracing::info!("clipboard changed here ({formats:?}); offering {share:?} to {peers} computer(s)");
                if !share.is_empty() {
                    self.broadcast(Msg::ClipOffer { seq, formats: share });
                }
            }
            ClipboardEvent::Paste { offer, format, reply } => {
                let Some(r) = self.clip.remote.as_ref().filter(|r| r.offer == offer && r.formats.contains(&format)) else {
                    tracing::info!("paste of {format:?} here: no matching clipboard offer from another computer");
                    let _ = reply.send(None);
                    return;
                };
                if let Some(d) = r.fetched.get(&format) {
                    let _ = reply.send(Some(d.clone()));
                    return;
                }
                let (peer, seq) = (r.peer, r.seq);
                if let Some(p) = self.clip.pending.values_mut().find(|p| p.peer == peer && p.seq == seq && p.format == format) {
                    p.replies.push(reply);
                    return;
                }
                tracing::info!("paste of {format:?} here; asking {}", self.peer_label(peer));
                self.clip.next_req += 1;
                let req = self.clip.next_req;
                if self.send_to(peer, Msg::ClipRequest { req, seq, format }) {
                    self.clip.pending.insert(
                        req,
                        PendingPaste { peer, seq, format, replies: vec![reply], started: Instant::now(), transfer: None },
                    );
                } else {
                    let _ = reply.send(None);
                }
            }
        }
    }

    pub(crate) fn on_clip_offer(&mut self, peer: DeviceId, seq: u64, formats: Vec<ClipFormat>) {
        if !self.clipboard_on() {
            tracing::info!("ignoring clipboard from {}: sharing is off here", self.peer_label(peer));
            return;
        }
        let formats = self.shareable_formats(&formats);
        if formats.is_empty() {
            return;
        }
        tracing::info!("clipboard from {} ({formats:?}) is now available here", self.peer_label(peer));
        self.clip.next_offer += 1;
        let offer = self.clip.next_offer;
        self.clip.remote = Some(RemoteClip { peer, seq, offer, formats: formats.clone(), fetched: HashMap::new() });
        self.clip.local = None;
        let _ = self.clipcmd.send(ClipboardCommand::AdvertiseRemote { offer, formats });
    }

    pub(crate) fn on_clip_request(&mut self, peer: DeviceId, req: u64, seq: u64, format: ClipFormat) {
        let ok = self.clipboard_on()
            && self.clip.local.as_ref().is_some_and(|(s, f)| *s == seq && f.contains(&format))
            && (format != ClipFormat::Files || self.st.settings.clipboard.files);
        if !ok {
            tracing::info!("{} asked for {format:?}, which this clipboard no longer offers", self.peer_label(peer));
            self.send_to(peer, Msg::ClipData { req, data: None });
            return;
        }
        tracing::info!("{} is pasting {format:?} from this clipboard", self.peer_label(peer));
        let (tx, rx) = oneshot::channel();
        let _ = self.clipcmd.send(ClipboardCommand::Read { format, reply: tx });
        let internal = self.internal.clone();
        tokio::spawn(async move {
            let data = tokio::time::timeout(Duration::from_secs(10), rx).await.ok().and_then(|r| r.ok()).flatten();
            let _ = internal.send(Internal::ClipReadDone { peer, req, format, data });
        });
    }

    pub(crate) fn on_clip_read_done(&mut self, peer: DeviceId, req: u64, format: ClipFormat, data: Option<Vec<u8>>) {
        let Some(data) = data else {
            tracing::warn!("could not read {format:?} from the clipboard here");
            self.send_to(peer, Msg::ClipData { req, data: None });
            return;
        };
        if format == ClipFormat::Files {
            let paths = crate::transfer::parse_uri_list(&String::from_utf8_lossy(&data));
            let paths: Vec<PathBuf> = paths.into_iter().filter(|p| p.exists()).collect();
            if paths.is_empty() {
                self.send_to(peer, Msg::ClipData { req, data: None });
            } else {
                self.start_outgoing(peer, paths, TransferPurpose::ClipboardFiles { req });
            }
            return;
        }
        if data.len() as u64 > self.st.settings.clipboard.max_bytes {
            tracing::info!("clipboard content too large to share ({} bytes)", data.len());
            self.send_to(peer, Msg::ClipData { req, data: None });
        } else if data.len() <= INLINE_MAX {
            self.send_to(peer, Msg::ClipData { req, data: Some(data) });
        } else {
            self.start_outgoing_blob(peer, "clipboard", data, TransferPurpose::ClipboardBlob { req });
        }
    }

    /// Answers every request waiting for `p`, keeping the data for later
    /// pastes of the same offer.
    fn finish_paste(&mut self, p: PendingPaste, data: Option<Vec<u8>>) {
        if let (Some(d), Some(r)) = (&data, self.clip.remote.as_mut()) {
            if r.peer == p.peer && r.seq == p.seq {
                r.fetched.insert(p.format, d.clone());
            }
        }
        for reply in p.replies {
            let _ = reply.send(data.clone());
        }
    }

    pub(crate) fn on_clip_data(&mut self, peer: DeviceId, req: u64, data: Option<Vec<u8>>) {
        if self.clip.pending.get(&req).is_some_and(|p| p.peer == peer) {
            let p = self.clip.pending.remove(&req).expect("checked");
            match &data {
                Some(d) => tracing::info!("received {} bytes of {:?} from {}", d.len(), p.format, self.peer_label(peer)),
                None => tracing::info!("{} had no {:?} to paste", self.peer_label(peer), p.format),
            }
            self.finish_paste(p, data);
        }
    }

    /// The remote side is sending the pasted content as a transfer.
    pub(crate) fn clip_transfer_offered(&mut self, peer: DeviceId, req: u64, local_transfer: u64) -> bool {
        match self.clip.pending.get_mut(&req) {
            Some(p) if p.peer == peer => {
                p.transfer = Some(local_transfer);
                true
            }
            _ => false,
        }
    }

    pub(crate) fn clip_transfer_done(&mut self, req: u64, result: Result<&Received, String>) {
        let Some(p) = self.clip.pending.remove(&req) else { return };
        let data = match result {
            Ok(Received::Bytes(b)) => Some(b.clone()),
            Ok(Received::Files(paths)) => Some(crate::transfer::uri_list(paths).into_bytes()),
            Err(e) => {
                tracing::warn!("clipboard transfer failed: {e}");
                None
            }
        };
        self.finish_paste(p, data);
    }

    pub(crate) fn tick_clipboard(&mut self, now: Instant) {
        let expired: Vec<u64> = self
            .clip
            .pending
            .iter()
            .filter(|(_, p)| p.transfer.is_none() && now.duration_since(p.started) > REQUEST_TIMEOUT)
            .map(|(k, _)| *k)
            .collect();
        for k in expired {
            if let Some(p) = self.clip.pending.remove(&k) {
                self.finish_paste(p, None);
            }
        }
    }

    pub(crate) fn clip_peer_gone(&mut self, peer: DeviceId) {
        let gone: Vec<u64> = self.clip.pending.iter().filter(|(_, p)| p.peer == peer && p.transfer.is_none()).map(|(k, _)| *k).collect();
        for k in gone {
            if let Some(p) = self.clip.pending.remove(&k) {
                self.finish_paste(p, None);
            }
        }
    }
}
