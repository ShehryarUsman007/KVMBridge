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
}

pub(crate) struct PendingPaste {
    pub peer: DeviceId,
    pub format: ClipFormat,
    pub reply: oneshot::Sender<Option<Vec<u8>>>,
    pub started: Instant,
    /// Set once the data is coming through a transfer (no timeout then).
    pub transfer: Option<u64>,
}

impl Engine {
    /// Clipboard sharing is on (and KVM Bridge is switched on).
    pub(crate) fn clipboard_on(&self) -> bool {
        self.st.enabled && self.st.settings.clipboard.enabled
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
                    return;
                }
                self.clip.local_seq += 1;
                let seq = self.clip.local_seq;
                self.clip.local = Some((seq, formats.clone()));
                self.clip.remote = None;
                let share = self.shareable_formats(&formats);
                tracing::debug!("local clipboard changed: {formats:?}");
                if !share.is_empty() {
                    self.broadcast(Msg::ClipOffer { seq, formats: share });
                }
            }
            ClipboardEvent::Paste { offer, format, reply } => {
                let Some(r) = self.clip.remote.as_ref().filter(|r| r.offer == offer && r.formats.contains(&format)) else {
                    let _ = reply.send(None);
                    return;
                };
                let (peer, seq) = (r.peer, r.seq);
                self.clip.next_req += 1;
                let req = self.clip.next_req;
                if self.send_to(peer, Msg::ClipRequest { req, seq, format }) {
                    self.clip.pending.insert(
                        req,
                        PendingPaste { peer, format, reply, started: Instant::now(), transfer: None },
                    );
                } else {
                    let _ = reply.send(None);
                }
            }
        }
    }

    pub(crate) fn on_clip_offer(&mut self, peer: DeviceId, seq: u64, formats: Vec<ClipFormat>) {
        if !self.clipboard_on() || formats.is_empty() {
            return;
        }
        let formats = self.shareable_formats(&formats);
        if formats.is_empty() {
            return;
        }
        self.clip.next_offer += 1;
        let offer = self.clip.next_offer;
        self.clip.remote = Some(RemoteClip { peer, seq, offer, formats: formats.clone() });
        self.clip.local = None;
        let _ = self.clipcmd.send(ClipboardCommand::AdvertiseRemote { offer, formats });
    }

    pub(crate) fn on_clip_request(&mut self, peer: DeviceId, req: u64, seq: u64, format: ClipFormat) {
        let ok = self.clipboard_on()
            && self.clip.local.as_ref().is_some_and(|(s, f)| *s == seq && f.contains(&format))
            && (format != ClipFormat::Files || self.st.settings.clipboard.files);
        if !ok {
            self.send_to(peer, Msg::ClipData { req, data: None });
            return;
        }
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

    pub(crate) fn on_clip_data(&mut self, peer: DeviceId, req: u64, data: Option<Vec<u8>>) {
        if let Some(p) = self.clip.pending.remove(&req) {
            if p.peer == peer {
                let _ = p.reply.send(data);
            }
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
        let _ = p.format;
        let _ = p.reply.send(data);
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
                let _ = p.reply.send(None);
            }
        }
    }

    pub(crate) fn clip_peer_gone(&mut self, peer: DeviceId) {
        let gone: Vec<u64> = self.clip.pending.iter().filter(|(_, p)| p.peer == peer && p.transfer.is_none()).map(|(k, _)| *k).collect();
        for k in gone {
            if let Some(p) = self.clip.pending.remove(&k) {
                let _ = p.reply.send(None);
            }
        }
    }
}
