//! Setups and saved configs.
//!
//! The computers in one arrangement form a setup: a computer that pairs with
//! one of them is paired with all of them (each tells the others about it),
//! and a computer unpaired from one of them is unpaired from all of them
//! (also from those offline at the time, when they reconnect).
//!
//! Saved configs live in the configs folder of every paired computer. Saving,
//! renaming or deleting one reaches the others right away (those offline get
//! it when they reconnect). Loading one makes it the current setup, which
//! then reaches the other computers like any other change.

use super::*;
use crate::config::{Removal, SharedSettings, TrustedPeer, MAX_REMOVALS};
use crate::library::{ConfigId, ConfigStamp, SavedConfig, StoredConfig, MAX_CONFIGS, MAX_TOMBSTONES};
use crate::proto::Introduction;
use crate::types::now_ms;

impl Engine {
    // ------------------------------------------------------------ members

    /// Paired computers in this computer's arrangement.
    pub(crate) fn setup_members(&self) -> Vec<DeviceId> {
        let me = self.me();
        let mut v: Vec<DeviceId> =
            self.st.layout.slots.iter().map(|s| s.device).filter(|d| *d != me && self.is_trusted(*d)).collect();
        v.sort();
        v
    }

    /// Call after the arrangement or the paired computers changed. When the
    /// setup gained or lost computers, every connected member hears about
    /// the others.
    pub(crate) fn setup_changed(&mut self) {
        let members = self.setup_members();
        if members == self.members_sent {
            return;
        }
        self.members_sent = members.clone();
        for p in members {
            self.sync_setup_with(p);
        }
    }

    /// Bring a connected computer up to date: the other computers of the
    /// setup (if it is part of it), unpaired computers and saved configs.
    pub(crate) fn sync_setup_with(&mut self, peer: DeviceId) {
        if self.conn_of(peer).is_none() {
            return;
        }
        if self.st.layout.contains(peer) {
            let intro: Vec<Introduction> = self
                .setup_members()
                .into_iter()
                .filter(|d| *d != peer)
                .filter_map(|d| self.st.trusted(d))
                .map(|t| Introduction {
                    id: t.id,
                    name: t.name.clone(),
                    public_key: t.public_key.clone(),
                    addr: self.peers.get(&t.id).and_then(|p| p.addr).map(|a| a.to_string()).or(t.last_addr.clone()),
                    paired_at_ms: t.paired_at_ms,
                })
                .collect();
            if !intro.is_empty() {
                self.send_to(peer, Msg::Introduce(intro));
            }
        }
        if !self.st.removed.is_empty() {
            self.send_to(peer, Msg::Forget(self.st.removed.clone()));
        }
        for c in &self.configs {
            self.send_to(peer, Msg::ConfigSync(Box::new(c.config.clone())));
        }
        for t in &self.st.deleted_configs {
            self.send_to(peer, Msg::ConfigDelete(*t));
        }
    }

    /// `from` (a member of our setup) told us about the other computers of
    /// its setup: pair with those in our arrangement.
    pub(crate) fn on_introduce(&mut self, from: DeviceId, list: Vec<Introduction>) {
        let me = self.me();
        if !self.st.layout.contains(from) {
            return;
        }
        let mut added = false;
        for i in list {
            if i.id == me || self.is_trusted(i.id) || !self.st.layout.contains(i.id) {
                continue;
            }
            if hex::decode(&i.public_key).map(|k| k.len()) != Ok(32) {
                continue;
            }
            if self.st.removed.iter().any(|r| r.id == i.id && r.at_ms >= i.paired_at_ms) {
                continue;
            }
            if self.group_full() {
                self.notice(Notice::Info {
                    message: format!(
                        "{} is part of this setup, but the limit of {} computers is reached",
                        i.name,
                        crate::config::MAX_DEVICES
                    ),
                });
                break;
            }
            tracing::info!("paired with {} through {}", i.name, self.peer_label(from));
            // A pairing request it may have sent meanwhile is not needed.
            if let Some(p) = self.peers.get_mut(&i.id) {
                if let Some(pairing) = p.pairing.take() {
                    if let Some(c) = self.conns.remove(&pairing.conn) {
                        c.handle.close();
                    }
                }
            }
            let t = TrustedPeer {
                id: i.id,
                name: i.name.clone(),
                public_key: i.public_key,
                last_addr: i.addr,
                paired_at_ms: i.paired_at_ms,
            };
            self.st.removed.retain(|r| r.id != i.id);
            self.st.trusted.push(t.clone());
            self.seed_peer(&t);
            self.notice(Notice::Paired { device: i.id, name: i.name });
            added = true;
        }
        if added {
            self.save_now();
            self.setup_changed();
            self.mark_dirty();
        }
    }

    // ------------------------------------------------------------ unpairing

    /// Remember that `r.id` left the setup. Returns whether this is news.
    fn remember_removal(&mut self, r: Removal) -> bool {
        if self.st.removed.iter().any(|x| x.id == r.id && x.at_ms >= r.at_ms) {
            return false;
        }
        self.st.removed.retain(|x| x.id != r.id);
        self.st.removed.push(r);
        if self.st.removed.len() > MAX_REMOVALS {
            let extra = self.st.removed.len() - MAX_REMOVALS;
            self.st.removed.drain(..extra);
        }
        self.schedule_save();
        true
    }

    /// Unpair `id` here and on every other paired computer.
    pub(crate) fn unpair_everywhere(&mut self, id: DeviceId) -> Result<(), String> {
        let r = Removal { id, at_ms: now_ms() };
        self.remember_removal(r);
        for p in self.connected_peers().into_iter().filter(|p| *p != id) {
            self.send_to(p, Msg::Forget(vec![r]));
        }
        self.unpair(id, true)
    }

    /// `from` says these computers were unpaired from the setup.
    pub(crate) fn on_forget(&mut self, from: DeviceId, list: Vec<Removal>) {
        let me = self.me();
        for r in list {
            if r.id == me || r.id == from {
                continue;
            }
            self.remember_removal(r);
            // Paired again since then: keep it.
            let Some(t) = self.st.trusted(r.id) else { continue };
            if t.paired_at_ms > r.at_ms {
                continue;
            }
            tracing::info!("{} was unpaired from the setup on {}", t.name, self.peer_label(from));
            self.notice(Notice::Info { message: format!("{} was unpaired from this setup", t.name) });
            let _ = self.unpair(r.id, true);
        }
    }

    /// Pairing again lifts an earlier removal.
    pub(crate) fn forget_removal(&mut self, id: DeviceId) {
        if self.st.removed.iter().any(|r| r.id == id) {
            self.st.removed.retain(|r| r.id != id);
            self.schedule_save();
        }
    }

    // ------------------------------------------------------------ saved configs

    /// Re-read the configs folder (it may have been changed by hand).
    pub(crate) fn reload_configs(&mut self) {
        let list = self.library.list();
        if list != self.configs {
            self.configs = list;
            self.mark_dirty();
        }
    }

    pub(crate) fn stored_config(&self, id: ConfigId) -> Option<&StoredConfig> {
        self.configs.iter().find(|c| c.config.id == id)
    }

    /// The current setup as a config.
    fn current_as_config(&self, id: ConfigId, name: String, rev: u64) -> SavedConfig {
        SavedConfig {
            id,
            name,
            rev,
            saved_ms: now_ms(),
            saved_by: self.me(),
            layout: self.st.layout.clone().normalized(),
            cipher: self.st.shared.cipher,
            common: self.st.shared.common.clone(),
        }
    }

    /// Store a config if it is newer than our copy. Returns whether it was
    /// stored.
    fn accept_config(&mut self, config: &SavedConfig) -> bool {
        let stamp = config.stamp();
        if let Some(local) = self.stored_config(config.id) {
            if !stamp.is_newer_than(&local.config.stamp()) {
                return false;
            }
        } else if self.configs.len() >= MAX_CONFIGS {
            tracing::warn!("ignoring config {:?}: the configs folder is full", config.id);
            return false;
        }
        if self.st.deleted_configs.iter().any(|t| t.id == config.id && !stamp.is_newer_than(t)) {
            return false;
        }
        let name = self.st.settings.device_name.clone();
        if let Err(e) = self.library.store(config, self.me(), &name) {
            self.error(format!("Could not save config \"{}\": {e}", config.name));
            return false;
        }
        tracing::info!("stored config \"{}\" (rev {})", config.name, config.rev);
        self.st.deleted_configs.retain(|t| t.id != config.id);
        self.schedule_save();
        self.reload_configs();
        true
    }

    fn remember_tombstone(&mut self, t: ConfigStamp) {
        self.st.deleted_configs.retain(|x| x.id != t.id);
        self.st.deleted_configs.push(t);
        if self.st.deleted_configs.len() > MAX_TOMBSTONES {
            let extra = self.st.deleted_configs.len() - MAX_TOMBSTONES;
            self.st.deleted_configs.drain(..extra);
        }
    }

    fn config_name(&self, name: &str, id: Option<ConfigId>) -> Result<String, String> {
        let name: String = name.trim().chars().take(64).collect();
        if name.is_empty() {
            return Err("Give the config a name".into());
        }
        if self.configs.iter().any(|c| Some(c.config.id) != id && c.config.name.eq_ignore_ascii_case(&name)) {
            return Err(format!("A config named \"{name}\" already exists"));
        }
        Ok(name)
    }

    /// Save the current setup as a new config (`id: None`) or over an
    /// existing one.
    pub(crate) fn save_config(&mut self, name: String, id: Option<ConfigId>) -> Result<(), String> {
        let name = self.config_name(&name, id)?;
        let (id, rev) = match id {
            Some(id) => (id, self.stored_config(id).ok_or("That config no longer exists")?.config.rev + 1),
            None if self.configs.len() >= MAX_CONFIGS => return Err(format!("At most {MAX_CONFIGS} configs can be saved")),
            None => (ConfigId::new_random(), 1),
        };
        let config = self.current_as_config(id, name, rev);
        if !self.accept_config(&config) {
            return Err("Could not save the config".into());
        }
        tracing::info!("saved config \"{}\"", config.name);
        self.broadcast(Msg::ConfigSync(Box::new(config)));
        self.st.last_config = Some(id);
        self.save_now();
        self.mark_dirty();
        Ok(())
    }

    pub(crate) fn rename_config(&mut self, id: ConfigId, name: String) -> Result<(), String> {
        let name = self.config_name(&name, Some(id))?;
        let mut config = self.stored_config(id).ok_or("That config no longer exists")?.config.clone();
        config.name = name;
        config.rev += 1;
        config.saved_ms = now_ms();
        config.saved_by = self.me();
        if !self.accept_config(&config) {
            return Err("Could not rename the config".into());
        }
        self.broadcast(Msg::ConfigSync(Box::new(config)));
        Ok(())
    }

    pub(crate) fn delete_config(&mut self, id: ConfigId) -> Result<(), String> {
        let config = self.stored_config(id).ok_or("That config no longer exists")?.config.clone();
        self.library.remove(id)?;
        let tomb = ConfigStamp { id, rev: config.rev + 1, saved_ms: now_ms(), saved_by: self.me(), deleted: true };
        self.remember_tombstone(tomb);
        if self.st.last_config == Some(id) {
            self.st.last_config = None;
        }
        self.broadcast(Msg::ConfigDelete(tomb));
        self.reload_configs();
        self.save_now();
        self.mark_dirty();
        Ok(())
    }

    /// Add a config file from elsewhere to the configs folder (and to the
    /// other computers).
    pub(crate) fn add_config_file(&mut self, path: PathBuf) -> Result<(), String> {
        let sc = crate::library::Library::read_file(&path)?;
        if self.stored_config(sc.config.id).is_some_and(|c| !sc.config.stamp().is_newer_than(&c.config.stamp())) {
            return Err(format!("\"{}\" is already in your configs", sc.config.name));
        }
        // An earlier deletion does not stop a file added on purpose.
        self.st.deleted_configs.retain(|t| t.id != sc.config.id);
        if !self.accept_config(&sc.config) {
            return Err("Could not add the config".into());
        }
        self.broadcast(Msg::ConfigSync(Box::new(sc.config)));
        Ok(())
    }

    /// Make a saved config the current setup. The other computers follow
    /// through the usual arrangement and settings updates.
    pub(crate) fn load_config(&mut self, id: ConfigId) -> Result<(), String> {
        let sc = self.stored_config(id).cloned().ok_or("That config no longer exists")?;
        let me = self.me();
        let c = &sc.config;
        if !c.has_member(me) {
            return Err("This config doesn't include this computer".into());
        }
        tracing::info!("loading config \"{}\"", c.name);
        let mut layout = c.layout.clone();
        let missing: Vec<String> =
            layout.slots.iter().filter(|s| s.device != me && !self.is_trusted(s.device)).map(|s| s.name.clone()).collect();
        layout.slots.retain(|s| s.device == me || self.is_trusted(s.device));
        if c.cipher != self.st.shared.cipher || c.common != self.st.shared.common {
            let shared = SharedSettings { cipher: c.cipher, common: c.common.clone(), ..self.next_shared() };
            self.adopt_shared(shared, None);
        }
        self.set_layout_from_ui(layout)?;
        self.st.last_config = Some(id);
        self.save_now();
        if !missing.is_empty() {
            self.notice(Notice::Info {
                message: format!("Left out {}: not paired with this computer any more", missing.join(", ")),
            });
        }
        self.update_edges();
        self.mark_dirty();
        Ok(())
    }

    pub(crate) fn on_config_sync(&mut self, config: SavedConfig) {
        self.accept_config(&config);
    }

    pub(crate) fn on_config_delete(&mut self, tomb: ConfigStamp) {
        if !tomb.deleted || self.st.deleted_configs.iter().any(|t| t.id == tomb.id && !tomb.is_newer_than(t)) {
            return;
        }
        if let Some(local) = self.stored_config(tomb.id).cloned() {
            if !tomb.is_newer_than(&local.config.stamp()) {
                return;
            }
            if let Err(e) = self.library.remove(tomb.id) {
                self.error(e);
                return;
            }
            tracing::info!("config \"{}\" was deleted on another computer", local.config.name);
            if self.st.last_config == Some(tomb.id) {
                self.st.last_config = None;
            }
        }
        self.remember_tombstone(tomb);
        self.reload_configs();
        self.schedule_save();
    }

    // ------------------------------------------------------------ views

    pub(crate) fn config_views(&self) -> Vec<view::ConfigView> {
        let me = self.me();
        let name_of = |id: DeviceId, cached: &str| {
            if id == me {
                self.st.settings.device_name.clone()
            } else {
                self.peers.get(&id).map(|p| p.name.clone()).unwrap_or_else(|| cached.to_string())
            }
        };
        self.configs
            .iter()
            .map(|c| {
                let saved_by = c
                    .config
                    .layout
                    .slot(c.config.saved_by)
                    .map(|s| name_of(s.device, &s.name))
                    .or_else(|| self.peers.get(&c.config.saved_by).map(|p| p.name.clone()))
                    .unwrap_or_default();
                view::ConfigView {
                    id: c.config.id,
                    name: c.config.name.clone(),
                    saved_ms: c.config.saved_ms,
                    saved_by,
                    members: c
                        .config
                        .layout
                        .slots
                        .iter()
                        .map(|s| view::ConfigMemberView {
                            id: s.device,
                            name: name_of(s.device, &s.name),
                            paired: s.device == me || self.is_trusted(s.device),
                        })
                        .collect(),
                    controller: c.config.layout.controller,
                    file: c.path.display().to_string(),
                    includes_me: c.config.has_member(me),
                }
            })
            .collect()
    }

    /// The saved config the current setup matches, or the one last loaded
    /// or saved here (then changed since).
    pub(crate) fn current_config_view(&self) -> Option<view::CurrentConfigView> {
        let (layout, cipher, common) = (&self.st.layout, self.st.shared.cipher, &self.st.shared.common);
        let matching = self
            .st
            .last_config
            .and_then(|id| self.stored_config(id))
            .filter(|c| c.config.matches(layout, cipher, common))
            .or_else(|| self.configs.iter().find(|c| c.config.matches(layout, cipher, common)));
        if let Some(c) = matching {
            return Some(view::CurrentConfigView { id: c.config.id, name: c.config.name.clone(), modified: false });
        }
        let last = self.st.last_config.and_then(|id| self.stored_config(id))?;
        Some(view::CurrentConfigView { id: last.config.id, name: last.config.name.clone(), modified: true })
    }
}
