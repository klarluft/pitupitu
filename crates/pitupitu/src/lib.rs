//! The pitupitu box agent as a library: what `main.rs` wires into commands,
//! and what the tests under `tests/` drive.

pub mod calls;
pub mod config;
pub mod device;
pub mod home;
pub mod inventory;
pub mod link;
pub mod service;
pub mod settings;
pub mod skill;
#[cfg(test)]
pub mod testhost;
pub mod update;
