//! The terminal host: for a box without herdr (or set to `terminal`), the
//! box agent runs the person's own agent CLIs in terminals it hosts itself
//! and serves the same box link calls over them (`docs/box-link.md`,
//! "Terminal host"). The terminals live in a holder process of their own
//! (`pitupitu terminals`), so restarting the box agent never ends a session.
//!
//! What it must keep (pitupitu cloud's `docs/plan.md`): the agents are the
//! person's own, unmodified binaries from `PATH`, run as the person; their
//! credentials are never read, copied or passed on; no permission flags
//! are added.

pub mod attach;
pub mod client;
pub mod detect;
pub mod holder;

use std::path::PathBuf;

/// Every id the terminal host hands out starts with this, so its ids and
/// herdr's never collide: `tw3`, `tw3:t1`, `tw3:p1`.
pub const PREFIX: &str = "tw";

pub fn is_ours(id: &str) -> bool {
    id.starts_with(PREFIX)
}

/// The holder's socket: `$PITUPITU_TERMS_SOCKET`, else under
/// `$XDG_RUNTIME_DIR/pitupitu/`, else `~/.local/state/pitupitu/`.
pub fn socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("PITUPITU_TERMS_SOCKET") {
        return PathBuf::from(p);
    }
    let dir = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) => PathBuf::from(d).join("pitupitu"),
        None => crate::config::home_dir().join(".local/state/pitupitu"),
    };
    dir.join("terms.sock")
}
