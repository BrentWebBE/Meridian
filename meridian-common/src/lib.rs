//! Code shared by every Meridian program (compositor, bar, dock, ...).

pub mod config;
pub mod ipc;
#[cfg(feature = "gtk")]
pub mod shell;
pub mod theme;
