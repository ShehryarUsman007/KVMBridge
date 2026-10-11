//! Windows backends for KVM Bridge.
//!
//! [`start`] wires the engine's backend channels to the Windows
//! implementation. The engine is the Linux folder's core crate.

mod clipboard;
pub mod clipfmt;
pub mod dnd;
pub mod edges;
mod emulation;
mod input;
pub mod keymap;
mod screens;

use kvmbridge_core::platform::*;
use std::path::PathBuf;

/// Pausing this computer's keyboard and mouse needs no one-time setup on
/// Windows.
pub fn setup_input_pause() -> Result<(), String> {
    Err("Not needed on this system".into())
}

/// Start the input, clipboard and drag-and-drop backends. `state_dir` is
/// used for small state. Must run before any window is created (it makes the
/// process per-monitor DPI aware), inside a tokio runtime.
pub fn start(sides: PlatformSides, state_dir: PathBuf) {
    let _ = state_dir;
    let PlatformSides { capture, emulation, clipboard, dnd } = sides;
    // Debug aid: run without touching input, the clipboard or drags (e.g. a
    // second instance on the same machine for testing).
    if std::env::var_os("KVMBRIDGE_NO_INPUT").is_some() {
        let why = "Disabled (KVMBRIDGE_NO_INPUT is set)";
        capture.unavailable(why, CaptureEvent::Status);
        emulation.unavailable(why, EmulationEvent::Status);
        clipboard.unavailable(why, ClipboardEvent::Status);
        dnd.unavailable(why, DndEvent::Status);
        return;
    }
    screens::init_dpi();
    let _ = emulation.events.send(EmulationEvent::Geometry(screens::desktop()));
    match input::InputThread::spawn(capture.events.clone(), emulation.events.clone()) {
        Ok(thread) => {
            tokio::spawn(capture_commands(capture, thread));
        }
        Err(e) => {
            tracing::warn!("input capture unavailable: {e}");
            capture.unavailable(format!("Input capture is not available: {e}"), CaptureEvent::Status);
        }
    }
    tokio::spawn(emulation::run(emulation));
    match clipboard::ClipThread::spawn(clipboard.events.clone(), tokio::runtime::Handle::current()) {
        Ok(thread) => {
            tokio::spawn(clipboard_commands(clipboard, thread));
        }
        Err(e) => {
            tracing::warn!("clipboard unavailable: {e}");
            clipboard.unavailable(format!("The clipboard is not available: {e}"), ClipboardEvent::Status);
        }
    }
    dnd::start(dnd);
}

/// Hands the engine's clipboard commands to the clipboard thread.
async fn clipboard_commands(mut side: BackendSide<ClipboardCommand, ClipboardEvent>, clip: clipboard::ClipThread) {
    while let Some(c) = side.commands.recv().await {
        clip.send(clipboard::Cmd::Engine(c));
    }
    clip.send(clipboard::Cmd::Stop);
}

/// Hands the engine's capture commands to the input thread.
async fn capture_commands(mut side: BackendSide<CaptureCommand, CaptureEvent>, input: input::InputThread) {
    while let Some(c) = side.commands.recv().await {
        input.send(match c {
            CaptureCommand::SetEdges(e) => input::Cmd::Edges(e),
            CaptureCommand::Release { pos } => input::Cmd::Release(pos),
            CaptureCommand::Enable(on) => input::Cmd::Enable(on),
            CaptureCommand::Restart => input::Cmd::Restart,
            CaptureCommand::BlockLocalInput(on) => input::Cmd::Block(on),
        });
    }
    // The engine is gone: input must be free.
    input.send(input::Cmd::Stop);
}
