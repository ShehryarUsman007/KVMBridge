//! Applying an imported configuration file to the running engine.

use super::*;
use crate::config::{SharedSettings, TrustedPeer, MAX_DEVICES};

/// What to apply from a `.kvmb` file.
#[derive(Clone, Debug)]
pub struct ImportPlan {
    /// Device that created the file (its "this computer" slot in the layout).
    pub source_device: DeviceId,
    /// The file's identity is being restored onto this computer as well
    /// (followed by [`Command::ReplaceIdentity`] and a restart).
    pub restoring_identity: bool,
    pub settings: Option<Settings>,
    pub shared: Option<SharedSettings>,
    pub layout: Option<Layout>,
    pub devices: Option<Vec<TrustedPeer>>,
}

impl Engine {
    pub(crate) fn import_config(&mut self, p: ImportPlan) -> Result<(), String> {
        let me = self.me();
        // The id this computer will have once the import is complete.
        let future_me = if p.restoring_identity { p.source_device } else { me };

        if let Some(devices) = p.devices {
            for d in devices {
                if d.id == me || d.id == future_me || d.id == p.source_device {
                    continue;
                }
                if hex::decode(&d.public_key).map(|k| k.len()) != Ok(32) {
                    continue;
                }
                let exists = self.st.trusted.iter().any(|t| t.id == d.id);
                if !exists && self.st.trusted.len() + 1 >= MAX_DEVICES {
                    self.notice(Notice::Info {
                        message: format!("Skipped {} — the limit of {MAX_DEVICES} computers is reached", d.name),
                    });
                    continue;
                }
                self.st.trusted.retain(|t| t.id != d.id);
                self.st.trusted.push(d.clone());
                // Reconnect with the (possibly different) key.
                if let Some(c) = self.peers.get(&d.id).and_then(|p| p.conn).and_then(|c| self.conns.get(&c)) {
                    c.handle.close();
                }
                self.seed_peer(&d);
            }
        }

        if let Some(mut s) = p.settings {
            if !p.restoring_identity {
                // Settings may come from another computer: keep our own name.
                s.device_name = self.st.settings.device_name.clone();
            }
            self.set_settings(s)?;
        }

        if let Some(sh) = p.shared {
            let rev = self.st.shared.rev.max(sh.rev) + 1;
            self.adopt_shared(SharedSettings { rev, author: me, cipher: sh.cipher }, None);
        }

        if let Some(mut l) = p.layout {
            for s in &mut l.slots {
                if s.device == p.source_device {
                    s.device = future_me;
                }
            }
            if l.controller == Some(p.source_device) {
                l.controller = Some(future_me);
            }
            let trusted: Vec<DeviceId> = self.st.trusted.iter().map(|t| t.id).collect();
            l.slots.retain(|s| s.device == future_me || trusted.contains(&s.device));
            if !l.contains(future_me) {
                let (c, r) = l.nearest_free(0, 0);
                let name = self.st.settings.device_name.clone();
                l.slots.push(crate::layout::Slot { device: future_me, col: c, row: r, number: 0, name });
            }
            l.slots.truncate(MAX_DEVICES);
            self.set_layout_from_ui(l)?;
        }
        self.save_now();
        self.update_edges();
        self.mark_dirty();
        Ok(())
    }

    /// Take over a restored identity. The caller restarts the app afterwards.
    pub(crate) fn replace_identity(&mut self, device_id: DeviceId, private_key: [u8; 32]) -> Result<(), String> {
        let old = self.me();
        let key_file = self.paths.identity_file();
        std::fs::create_dir_all(&self.paths.config_dir).map_err(|e| e.to_string())?;
        crate::config::write_identity(&key_file, &private_key).map_err(|e| e.to_string())?;
        self.st.device_id = device_id;
        self.st.trusted.retain(|t| t.id != device_id);
        if old != device_id {
            if self.st.layout.contains(device_id) {
                self.st.layout.remove(old);
            } else {
                for s in &mut self.st.layout.slots {
                    if s.device == old {
                        s.device = device_id;
                    }
                }
            }
            self.st.layout.rev += 1;
            self.st.layout.author = device_id;
            if self.st.shared.author == old {
                self.st.shared.author = device_id;
            }
        }
        self.save_now();
        Ok(())
    }

    pub(crate) fn seed_peer(&mut self, t: &TrustedPeer) {
        let p = self.peers.entry(t.id).or_insert_with(|| peers::Peer::new(t.id, t.name.clone()));
        p.name = t.name.clone();
        p.fingerprint = hex::decode(&t.public_key).map(|k| crate::crypto::fingerprint(&k)).unwrap_or_default();
        if p.addr.is_none() {
            p.addr = t.last_addr.as_deref().and_then(|a| a.parse().ok());
        }
        p.next_dial = Instant::now();
        p.error = None;
    }
}
