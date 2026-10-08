//! Platform backends for KVM Bridge.
//!
//! [`start`] wires the engine's backend channels to the implementation for
//! the current operating system / desktop session.

#[cfg(target_os = "linux")]
pub mod linux;

use kvmbridge_core::platform::*;
use std::path::PathBuf;

/// What the GUI layer still has to drive itself.
pub struct Leftovers {
    /// Drag-and-drop backend side (needs the GUI main thread).
    pub dnd: BackendSide<DndCommand, DndEvent>,
}

/// One-time setup that allows pausing this computer's keyboard and mouse
/// while another computer controls it (Linux builds that were not installed
/// from a package: installs a small helper after asking for an
/// administrator password). Blocks until done.
pub fn setup_input_pause() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        linux::guard::install_helper()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err("Not needed on this system".into())
    }
}

/// Start the input/clipboard backends for this OS. `state_dir` is used for
/// small state such as permission restore tokens.
pub fn start(sides: PlatformSides, state_dir: PathBuf) -> Leftovers {
    // Debug aid: run without touching input or the clipboard (e.g. a second
    // instance on the same machine for testing).
    if std::env::var_os("KVMBRIDGE_NO_INPUT").is_some() {
        let PlatformSides { capture, emulation, clipboard, dnd } = sides;
        let why = "Disabled (KVMBRIDGE_NO_INPUT is set)";
        capture.unavailable(why, CaptureEvent::Status);
        emulation.unavailable(why, EmulationEvent::Status);
        clipboard.unavailable(why, ClipboardEvent::Status);
        return Leftovers { dnd };
    }
    #[cfg(target_os = "linux")]
    {
        let rest = linux::start(sides, state_dir);
        Leftovers { dnd: rest.dnd }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = state_dir;
        let PlatformSides { capture, emulation, clipboard, dnd } = sides;
        let why = "This operating system is not supported yet";
        capture.unavailable(why, CaptureEvent::Status);
        emulation.unavailable(why, EmulationEvent::Status);
        clipboard.unavailable(why, ClipboardEvent::Status);
        Leftovers { dnd }
    }
}
