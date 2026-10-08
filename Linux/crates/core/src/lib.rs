//! Core of KVM Bridge: protocol, encryption, discovery, layout and the engine
//! that ties them together. Platform specific input/clipboard/drag-and-drop
//! code lives in `kvmbridge-platform` and talks to the engine through the
//! channel based interfaces in [`platform`].

pub mod crypto;
pub mod layout;
pub mod types;
pub mod config;
pub mod configfile;
pub mod discovery;
pub mod proto;
pub mod engine;
pub mod net;
pub mod platform;
pub mod transfer;

pub use engine::{start, Command, EngineHandle, Notice, Overrides, Snapshot};
