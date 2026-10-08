//! Switching KVM Bridge on and off, and pausing this computer's own
//! keyboard and mouse while another computer controls it.

use super::*;

impl Engine {
    /// Switch KVM Bridge on or off here (from the UI). The other computers
    /// learn it from this computer's capabilities.
    pub(crate) fn set_enabled(&mut self, on: bool) -> Result<(), String> {
        if on == self.st.enabled {
            return Ok(());
        }
        self.switch(on);
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
                let _ = self.emu.send(EmulationCommand::Leave { park: None });
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
