//! The pitupitu box agent as a library: what `main.rs` wires into commands,
//! and what `tests/conformance.rs` drives through the one `AgentHost`
//! trait, against herdr and the terminal host alike.

pub mod calls;
pub mod config;
pub mod device;
pub mod home;
pub mod inventory;
pub mod link;
pub mod router;
pub mod service;
pub mod settings;
pub mod skill;
pub mod term;
#[cfg(test)]
pub mod testhost;
