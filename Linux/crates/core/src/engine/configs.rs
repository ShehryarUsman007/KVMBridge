//! Saved configs, switching KVM Bridge on and off, and pausing this
//! computer's own keyboard and mouse while another computer controls it.
//!
//! Configs live in the configs folder of every computer that is part of
//! them. Saving one sends it to its other members (those offline get it when
//! they reconnect). Loading one, or switching on, asks the other members to
//! load it too ("activation"); when computers disagree the newest activation
//! wins.

use super::*;
use crate::config::{SharedSettings, TrustedPeer};
use crate::library::{Activation, ConfigId, ConfigStamp, Member, SharedConfig, StoredConfig, MAX_CONFIGS, MAX_TOMBSTONES};
use crate::types::now_ms;

impl Engine {
    // ------------------------------------------------------------ library

    /// Re-read the configs folder (it may have been changed by hand).
    pub(crate) fn reload_configs(&mut self) {
        let list = self.library.list();
        if list != self.configs {
            self.configs = list;
            self.mark_dirty();
        }
    }

    pub(crate) fn config_stamps(&self) -> Vec<ConfigStamp> {
        self.configs.iter().map(|c| c.config.stamp()).chain(self.st.deleted_configs.iter().copied()).collect()
    }

    pub(crate) fn config_view(&self, c: &StoredConfig) -> view::ConfigView {
        let name_of = |id: DeviceId| {
            if id == self.me() {
                self.st.settings.device_name.clone()
            } else {
                self.peers
                    .get(&id)
                    .map(|p| p.name.clone())
                    .or_else(|| c.config.members.iter().find(|m| m.id == id).map(|m| m.name.clone()))
                    .unwrap_or_default()
            }
        };
        view::ConfigView {
            id: c.config.id,
            name: c.config.name.clone(),
            saved_ms: c.config.saved_ms,
            saved_by: name_of(c.config.saved_by),
            members: c.config.members.iter().map(|m| view::ConfigMemberView { id: m.id, name: name_of(m.id) }).collect(),
            controller: c.config.layout.controller,
            file: c.path.display().to_string(),
            includes_me: c.config.has_member(self.me()),
        }
    }

    pub(crate) fn stored_config(&self, id: ConfigId) -> Option<&StoredConfig> {
        self.configs.iter().find(|c| c.config.id == id)
    }

    /// The current setup as a config.
    fn current_as_config(&self, id: ConfigId, name: String, rev: u64) -> SharedConfig {
        let me = self.me();
        let mut members = vec![Member {
            id: me,
            name: self.st.settings.device_name.clone(),
            public_key: hex::encode(self.identity.public),
        }];
        for s in &self.st.layout.slots {
            if let Some(t) = self.st.trusted(s.device) {
                members.push(Member { id: t.id, name: t.name.clone(), public_key: t.public_key.clone() });
            }
        }
        let mut layout = self.st.layout.clone();
        layout.slots.retain(|s| members.iter().any(|m| m.id == s.device));
        let layout = layout.normalized();
        SharedConfig {
            id,
            name,
            rev,
            saved_ms: now_ms(),
            saved_by: me,
            layout,
            cipher: self.st.shared.cipher,
            members,
        }
    }

    /// Does the current setup differ from the active config?
    pub(crate) fn active_config_modified(&self) -> bool {
        let Some(a) = &self.st.active_config else { return false };
        let Some(sc) = self.stored_config(a.id) else { return false };
        let cur = self.current_as_config(a.id, a.name.clone(), 0);
        let cells = |l: &Layout| {
            let n = l.clone().normalized();
            let mut v: Vec<(DeviceId, i32, i32)> = n.slots.iter().map(|s| (s.device, s.col, s.row)).collect();
            v.sort();
            (v, n.controller)
        };
        cells(&cur.layout) != cells(&sc.config.layout) || cur.cipher != sc.config.cipher
    }

    /// Store a config received from elsewhere if it is newer than ours.
    /// Returns whether it was stored.
    fn accept_config(&mut self, config: &SharedConfig) -> bool {
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
        if let Err(e) = self.library.store(config, &self.st.settings, self.me()) {
            self.error(format!("Could not save config \"{}\": {e}", config.name));
            return false;
        }
        tracing::info!("stored config \"{}\" (rev {})", config.name, config.rev);
        self.st.deleted_configs.retain(|t| t.id != config.id);
        if let Some(a) = &mut self.st.active_config {
            if a.id == config.id {
                a.name = config.name.clone();
            }
        }
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

    // ------------------------------------------------------------ UI commands

    /// Save the current setup as a new config (`id: None`) or over an
    /// existing one.
    pub(crate) fn save_config(&mut self, name: String, id: Option<ConfigId>) -> Result<(), String> {
        let name: String = name.trim().chars().take(64).collect();
        if name.is_empty() {
            return Err("Give the config a name".into());
        }
        let clash = self.configs.iter().any(|c| Some(c.config.id) != id && c.config.name.eq_ignore_ascii_case(&name));
        if clash {
            return Err(format!("A config named \"{name}\" already exists"));
        }
        let (id, rev) = match id {
            Some(id) => (id, self.stored_config(id).ok_or("That config no longer exists")?.config.rev + 1),
            None if self.configs.len() >= MAX_CONFIGS => return Err(format!("At most {MAX_CONFIGS} configs can be saved")),
            None => (ConfigId::new_random(), 1),
        };
        let config = self.current_as_config(id, name, rev);
        self.library.store(&config, &self.st.settings, self.me())?;
        self.reload_configs();
        tracing::info!("saved config \"{}\" (rev {rev})", config.name);
        for m in config.members.iter().filter(|m| m.id != self.me()) {
            self.send_to(m.id, Msg::ConfigSync(Box::new(config.clone())));
        }
        // The current setup is this config now. Saving does not switch
        // anyone on.
        self.st.active_config = Some(Activation { id, name: config.name.clone(), at_ms: now_ms(), by: self.me(), enable: false });
        self.announce_activation();
        self.save_now();
        self.mark_dirty();
        Ok(())
    }

    pub(crate) fn rename_config(&mut self, id: ConfigId, name: String) -> Result<(), String> {
        let name: String = name.trim().chars().take(64).collect();
        if name.is_empty() {
            return Err("Give the config a name".into());
        }
        if self.configs.iter().any(|c| c.config.id != id && c.config.name.eq_ignore_ascii_case(&name)) {
            return Err(format!("A config named \"{name}\" already exists"));
        }
        let mut config = self.stored_config(id).ok_or("That config no longer exists")?.config.clone();
        config.name = name;
        config.rev += 1;
        config.saved_ms = now_ms();
        config.saved_by = self.me();
        if !self.accept_config(&config) {
            return Err("Could not rename the config".into());
        }
        for m in config.members.iter().filter(|m| m.id != self.me()) {
            self.send_to(m.id, Msg::ConfigSync(Box::new(config.clone())));
        }
        Ok(())
    }

    pub(crate) fn delete_config(&mut self, id: ConfigId) -> Result<(), String> {
        let config = self.stored_config(id).ok_or("That config no longer exists")?.config.clone();
        self.library.remove(id)?;
        let tomb = ConfigStamp { id, rev: config.rev + 1, saved_ms: now_ms(), saved_by: self.me(), deleted: true };
        self.remember_tombstone(tomb);
        if self.st.active_config.as_ref().is_some_and(|a| a.id == id) {
            self.st.active_config = None;
        }
        for m in config.members.iter().filter(|m| m.id != self.me()) {
            self.send_to(m.id, Msg::ConfigDelete(tomb));
        }
        self.reload_configs();
        self.save_now();
        self.mark_dirty();
        Ok(())
    }

    /// Add a config file from elsewhere to the configs folder.
    pub(crate) fn import_config_file(&mut self, path: PathBuf) -> Result<(), String> {
        let sc = crate::library::Library::read_file(&path)?;
        if !sc.config.has_member(self.me()) {
            return Err("This config is for other computers; it doesn't include this one".into());
        }
        if self.stored_config(sc.config.id).is_some_and(|c| !sc.config.stamp().is_newer_than(&c.config.stamp())) {
            return Err(format!("\"{}\" is already in your configs", sc.config.name));
        }
        if self.accept_config(&sc.config) {
            Ok(())
        } else {
            Err("Could not add the config".into())
        }
    }

    /// Load a config here and ask its other computers to load it too.
    pub(crate) fn load_config(&mut self, id: ConfigId) -> Result<(), String> {
        let sc = self.stored_config(id).cloned().ok_or("That config no longer exists")?;
        if !sc.config.has_member(self.me()) {
            return Err("This config doesn't include this computer".into());
        }
        let act =
            Activation { id, name: sc.config.name.clone(), at_ms: now_ms(), by: self.me(), enable: self.st.enabled };
        self.apply_config(&sc, act)?;
        self.announce_activation();
        Ok(())
    }

    /// Switch KVM Bridge on or off here (from the UI). Switching on asks the
    /// other computers of the active config to load it and switch on.
    pub(crate) fn set_enabled(&mut self, on: bool) -> Result<(), String> {
        if on == self.st.enabled {
            return Ok(());
        }
        if on {
            if let Some(a) = self.st.active_config.clone() {
                if self.stored_config(a.id).is_some() {
                    self.st.active_config =
                        Some(Activation { at_ms: now_ms(), by: self.me(), enable: true, ..a });
                }
            }
        } else {
            self.st.disabled_at_ms = now_ms();
        }
        self.switch(on);
        if on {
            self.announce_activation();
        }
        Ok(())
    }

    /// Apply the on/off state.
    fn switch(&mut self, on: bool) {
        tracing::info!("KVM Bridge switched {}", if on { "on" } else { "off" });
        self.st.enabled = on;
        if !on {
            // Give the cursor back; whoever controls us moves it to the
            // nearest screen that is still on when it learns we are off.
            self.release_control_local();
            if self.controlled.take().is_some() {
                let _ = self.emu.send(EmulationCommand::Leave);
                self.dnd_controller_changed();
            }
            self.clip = ClipState::default();
        }
        // Peers learn the new state from Hello; this also updates the edges
        // and the keyboard/mouse pause.
        self.broadcast_caps();
        self.save_now();
        self.mark_dirty();
    }

    // ------------------------------------------------------------ applying

    /// Make `sc` the current setup: arrangement, controlling computer,
    /// encryption, its members (trusted) and this computer's saved settings.
    fn apply_config(&mut self, sc: &StoredConfig, act: Activation) -> Result<(), String> {
        let me = self.me();
        let c = &sc.config;
        tracing::info!("loading config \"{}\"", c.name);
        // Members trust each other through the config.
        for m in c.members.iter().filter(|m| m.id != me) {
            if self.is_trusted(m.id) {
                continue;
            }
            if hex::decode(&m.public_key).map(|k| k.len()) != Ok(32) {
                continue;
            }
            if self.st.trusted.len() + 1 >= crate::config::MAX_DEVICES {
                self.notice(Notice::Info { message: format!("Skipped {} — the limit of computers is reached", m.name) });
                continue;
            }
            let t = TrustedPeer {
                id: m.id,
                name: m.name.clone(),
                public_key: m.public_key.clone(),
                last_addr: None,
                paired_at_ms: now_ms(),
            };
            self.st.trusted.push(t.clone());
            self.seed_peer(&t);
        }
        if let Some(mut s) = sc.settings.clone() {
            // Keep this computer's name and look.
            s.device_name = self.st.settings.device_name.clone();
            s.appearance = self.st.settings.appearance;
            if s != self.st.settings {
                self.set_settings(s)?;
            }
        }
        let mut layout = c.layout.clone();
        layout.slots.retain(|s| s.device == me || self.is_trusted(s.device));
        if !layout.contains(me) {
            return Err("This config doesn't include this computer".into());
        }
        if c.cipher != self.st.shared.cipher {
            self.adopt_shared(SharedSettings { rev: self.st.shared.rev + 1, author: me, cipher: c.cipher }, None);
        }
        self.set_layout_from_ui(layout)?;
        self.st.active_config = Some(act);
        self.save_now();
        self.update_edges();
        self.mark_dirty();
        Ok(())
    }

    /// Tell the connected members of the active config which config to run.
    /// Members that are offline hear it when they reconnect (see
    /// [`Engine::reconcile_activation`]).
    fn announce_activation(&mut self) {
        let Some(a) = self.st.active_config.clone() else { return };
        let Some(sc) = self.stored_config(a.id).cloned() else { return };
        for m in sc.config.members.iter().filter(|m| m.id != self.me()) {
            self.send_to(m.id, Msg::ConfigActivate { config: Box::new(sc.config.clone()), activation: a.clone() });
        }
    }

    // ------------------------------------------------------------ messages

    /// Bring a newly connected (or changed) peer up to date.
    pub(crate) fn sync_configs_with(&mut self, peer: DeviceId, theirs: &[ConfigStamp], active: Option<&Activation>) {
        let me = self.me();
        for t in theirs.iter().filter(|t| t.deleted) {
            self.on_config_delete(peer, *t);
        }
        for c in self.configs.clone() {
            if !c.config.has_member(peer) || !c.config.has_member(me) {
                continue;
            }
            let their = theirs.iter().find(|t| t.id == c.config.id);
            if their.is_none_or(|t| c.config.stamp().is_newer_than(t)) {
                self.send_to(peer, Msg::ConfigSync(Box::new(c.config.clone())));
            }
        }
        self.reconcile_activation(peer, active);
    }

    /// If our activation is newer than the peer's, send it.
    fn reconcile_activation(&mut self, peer: DeviceId, theirs: Option<&Activation>) {
        let Some(mine) = self.st.active_config.clone() else { return };
        if theirs.is_some_and(|t| t.order() >= mine.order()) {
            return;
        }
        let Some(sc) = self.stored_config(mine.id) else { return };
        if sc.config.has_member(peer) {
            let config = Box::new(sc.config.clone());
            self.send_to(peer, Msg::ConfigActivate { config, activation: mine });
        }
    }

    pub(crate) fn on_config_sync(&mut self, peer: DeviceId, config: SharedConfig) {
        if config.has_member(peer) && config.has_member(self.me()) {
            self.accept_config(&config);
        }
    }

    pub(crate) fn on_config_activate(&mut self, peer: DeviceId, config: SharedConfig, act: Activation) {
        let me = self.me();
        if !config.has_member(peer) || !config.has_member(me) || act.id != config.id {
            return;
        }
        self.accept_config(&config);
        let mine = self.st.active_config.as_ref().map(|a| a.order());
        if mine.is_some_and(|m| m >= act.order()) {
            if mine.is_some_and(|m| m > act.order()) {
                // We know a newer one: let the peer catch up.
                self.reconcile_activation(peer, Some(&act));
            }
            return;
        }
        let Some(sc) = self.stored_config(config.id).cloned() else { return };
        let peer_name = self.peers.get(&peer).map(|p| p.name.clone()).unwrap_or_default();
        let switch_on = act.enable && !self.st.enabled && act.at_ms > self.st.disabled_at_ms;
        let changed = self.st.active_config.as_ref().is_none_or(|a| a.id != act.id);
        if let Err(e) = self.apply_config(&sc, act) {
            self.error(format!("Could not load config \"{}\": {e}", config.name));
            return;
        }
        if switch_on {
            self.switch(true);
        }
        if changed || switch_on {
            let what = if switch_on { "switched KVM Bridge on with" } else { "loaded" };
            self.notice(Notice::Info { message: format!("{peer_name} {what} the config \"{}\"", config.name) });
        }
    }

    pub(crate) fn on_config_delete(&mut self, peer: DeviceId, tomb: ConfigStamp) {
        if !tomb.deleted {
            return;
        }
        if let Some(local) = self.stored_config(tomb.id).cloned() {
            if !local.config.has_member(peer) || !tomb.is_newer_than(&local.config.stamp()) {
                return;
            }
            if let Err(e) = self.library.remove(tomb.id) {
                self.error(e);
                return;
            }
            tracing::info!("config \"{}\" was deleted on another computer", local.config.name);
            if self.st.active_config.as_ref().is_some_and(|a| a.id == tomb.id) {
                self.st.active_config = None;
            }
            self.remember_tombstone(tomb);
            self.reload_configs();
            self.schedule_save();
        }
    }

    // ------------------------------------------------------------ keyboard/mouse pause

    /// Another computer controls the screens right now, so this computer's
    /// own keyboard and mouse should be paused.
    fn want_local_input_paused(&self) -> bool {
        let me = self.me();
        let Some(c) = self.st.layout.controller else { return false };
        self.st.enabled
            && c != me
            && self.st.layout.contains(me)
            && self.conn_of(c).is_some()
            && self.peers.get(&c).is_some_and(|p| p.caps.enabled && p.caps.capture)
    }

    pub(crate) fn update_guard(&mut self) {
        let want = self.want_local_input_paused();
        if self.guard_sent != Some(want) {
            tracing::info!("{} this computer's keyboard and mouse", if want { "pausing" } else { "resuming" });
            self.guard_sent = Some(want);
            let _ = self.cap.send(CaptureCommand::BlockLocalInput(want));
        }
    }

    pub(crate) fn on_guard(&mut self, s: GuardStatus) {
        tracing::info!("keyboard/mouse pause: {s:?}");
        if s == GuardStatus::Escaped {
            self.guard = GuardStatus::Off;
            self.notice(Notice::Info {
                message: "Your keyboard and mouse are back. KVM Bridge is now off on this computer.".into(),
            });
            let _ = self.set_enabled(false);
        } else {
            self.guard = s;
        }
        self.mark_dirty();
    }
}
