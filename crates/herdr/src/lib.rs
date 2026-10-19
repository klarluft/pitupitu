//! The one place pitupitu talks to herdr.
//!
//! Everything goes through the `herdr` CLI, which speaks JSON over its Unix
//! socket API. Payload shapes below were taken from herdr 0.9.3 responses;
//! `herdr api schema --json` is the authority if they drift.
//!
//! Lessons encoded here:
//! - `workspace create` / `tab create` return a fresh shell pane; that is
//!   where an agent gets started. pitupitu only ever closes what it created.
//! - `agent start` in a directory Claude Code has not seen can return
//!   `agent_not_ready` because of its "trust this folder?" dialog;
//!   [`HerdrCli::start_agent`] can answer it.
//! - `agent prompt` returns before the agent is observed working, so a wait
//!   right after can see the stale `idle`. Callers must key on
//!   `state_change_seq` advancing, not on status alone.

mod cli;
pub mod detect;
mod types;

pub use cli::HerdrCli;
pub use types::*;

use async_trait::async_trait;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// herdr answered with `{"error":{"code","message"}}`.
    #[error("herdr: {code}: {message}")]
    Api { code: String, message: String },
    #[error("herdr exited with status {status}: {stderr}")]
    Exit { status: i32, stderr: String },
    #[error("could not run `herdr`: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("unexpected herdr response: {0}")]
    Shape(String),
    #[error("agent {name} did not become ready: {why}")]
    NotReady { name: String, why: String },
}

impl Error {
    pub fn code(&self) -> Option<&str> {
        match self {
            Error::Api { code, .. } => Some(code),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// `unsupported_agent_kind` when `kinds` is known and lacks `kind`.
pub fn refuse_unsupported(kinds: &[String], kind: &AgentKind) -> Result<()> {
    if kinds.is_empty() || kinds.iter().any(|k| k == kind.as_str()) {
        return Ok(());
    }
    Err(Error::Api {
        code: "unsupported_agent_kind".into(),
        message: format!(
            "this box cannot start {kind}; it can start {}",
            kinds.join(", ")
        ),
    })
}

/// What pitupitu cloud can ask of the box, one box link call per method
/// (see `docs/box-link.md`). herdr is the only implementation today; the
/// trait exists so another agent host could be added behind the same calls.
#[async_trait]
pub trait AgentHost: Send + Sync {
    async fn list_workspaces(&self) -> Result<Vec<Workspace>>;

    /// Open a new workspace for `cwd`, with one fresh shell pane.
    async fn create_workspace(&self, cwd: &Path, label: &str) -> Result<Created>;

    /// [`AgentHost::create_workspace`] with the cloud's hint where the
    /// session should run (`terminal` or `herdr`) and the agent kind it
    /// means to start there, refused as `unsupported_agent_kind` before
    /// anything is opened (`docs/sessions.md`, "Kinds"). A host with one
    /// place to put it ignores the hint.
    async fn create_workspace_hinted(
        &self,
        cwd: &Path,
        label: &str,
        driver: Option<&str>,
        kind: Option<&AgentKind>,
    ) -> Result<Created> {
        let _ = driver;
        if let Some(k) = kind {
            refuse_unsupported(&self.kinds().await, k)?;
        }
        self.create_workspace(cwd, label).await
    }

    /// The agent kinds this host can start right now. Empty means the
    /// host cannot say, and nothing is refused on that account.
    async fn kinds(&self) -> Vec<String>;

    /// Open a new tab in an existing workspace, with one fresh shell pane.
    /// `cwd` defaults to the workspace's directory.
    async fn create_tab(
        &self,
        workspace: &WorkspaceId,
        cwd: Option<&Path>,
        label: &str,
    ) -> Result<Created>;

    async fn close_workspace(&self, workspace: &WorkspaceId) -> Result<()>;

    async fn close_tab(&self, tab: &TabId) -> Result<()>;

    /// Start `kind` in an idle shell pane under a unique `name`, and get it to
    /// the point where it accepts prompts.
    async fn start_agent(
        &self,
        name: &str,
        kind: AgentKind,
        pane: &PaneId,
        opts: StartOpts,
    ) -> Result<AgentInfo>;

    /// Submit text as a prompt and return immediately. The returned info is
    /// the state *before* the agent reacted; compare `state_change_seq`.
    async fn prompt(&self, agent: &str, text: &str) -> Result<AgentInfo>;

    async fn agent(&self, agent: &str) -> Result<AgentInfo>;

    async fn list_agents(&self) -> Result<Vec<AgentInfo>>;

    /// Block until the agent reaches one of `until` (or any settled state
    /// when `until` is empty), or `timeout` elapses.
    async fn wait(
        &self,
        agent: &str,
        until: &[AgentStatus],
        timeout: Duration,
    ) -> Result<AgentInfo>;

    /// Recent unwrapped transcript of the agent's pane.
    async fn read(&self, agent: &str, lines: u32) -> Result<String>;

    /// The same transcript with its colours and attributes as SGR codes
    /// (`ESC [ … m`) and no other escapes. Hosts that cannot tell
    /// colours give the plain text.
    async fn read_colored(&self, agent: &str, lines: u32) -> Result<String> {
        self.read(agent, lines).await
    }

    async fn send_keys(&self, agent: &str, keys: &[&str]) -> Result<()>;
}
