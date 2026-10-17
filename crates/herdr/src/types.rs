use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;

/// Opaque herdr handle, e.g. `w7`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkspaceId(pub String);

/// Opaque herdr handle, e.g. `w7:t2`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TabId(pub String);

/// Opaque herdr handle, e.g. `w7:p1`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PaneId(pub String);

impl fmt::Display for WorkspaceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Display for TabId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Display for PaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An agent kind as herdr names it: `claude`, `codex`, `gemini`,
/// `opencode`, … pitupitu does not keep its own list; `herdr agent` prints
/// the kinds a given herdr knows, and `agent start` rejects the rest. Only
/// the spelling is checked here, so a misheard kind fails at herdr with its
/// message rather than deep in a shell argument.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentKind(String);

impl AgentKind {
    pub const CLAUDE: &str = "claude";

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for AgentKind {
    fn default() -> Self {
        AgentKind(Self::CLAUDE.into())
    }
}

impl fmt::Display for AgentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for AgentKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim().to_ascii_lowercase();
        let well_formed = s.len() <= 32
            && s.starts_with(|c: char| c.is_ascii_lowercase())
            && s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
        if well_formed {
            Ok(AgentKind(s))
        } else {
            Err(format!(
                "`{s}` is not an agent kind name (claude, codex, gemini, opencode, …)"
            ))
        }
    }
}

/// herdr's agent lifecycle state. `Idle` and `Done` both mean "ready for
/// input"; `Done` is a completed turn nobody has looked at yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Idle,
    Working,
    Blocked,
    Done,
    Unknown,
}

impl AgentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentStatus::Idle => "idle",
            AgentStatus::Working => "working",
            AgentStatus::Blocked => "blocked",
            AgentStatus::Done => "done",
            AgentStatus::Unknown => "unknown",
        }
    }

    /// Ready to accept a prompt.
    pub fn is_ready(self) -> bool {
        matches!(self, AgentStatus::Idle | AgentStatus::Done)
    }
}

impl fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `result.agent` of `agent get|wait|prompt`, and each entry of `agent list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    /// Agent kind label as herdr reports it (`claude`, `codex`, …). Absent
    /// for a moment while an agent is being launched; one such entry must
    /// not fail the whole `agent list`.
    #[serde(default)]
    pub agent: String,
    /// Live name; absent for agents nobody named.
    #[serde(default)]
    pub name: Option<String>,
    pub agent_status: AgentStatus,
    pub pane_id: PaneId,
    pub workspace_id: WorkspaceId,
    #[serde(default)]
    pub tab_id: Option<TabId>,
    pub cwd: PathBuf,
    /// Advances on every lifecycle transition. The only reliable way to tell
    /// a fresh state from one you already saw.
    #[serde(default)]
    pub state_change_seq: u64,
    /// Advances when a turn completes.
    #[serde(default)]
    pub completion_seq: Option<u64>,
    #[serde(default)]
    pub launch_pending: bool,
    #[serde(default)]
    pub interactive_ready: bool,
    /// The agent's own window title, e.g. Claude Code's session summary.
    #[serde(default)]
    pub terminal_title_stripped: Option<String>,
}

impl AgentInfo {
    /// What `agent` commands accept: the live name when there is one, else
    /// the pane id.
    pub fn target(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.pane_id.0)
    }
}

/// Each entry of `workspace list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub workspace_id: WorkspaceId,
    pub label: String,
    #[serde(default)]
    pub agent_status: Option<AgentStatus>,
    #[serde(default)]
    pub focused: bool,
}

/// What `workspace create` / `tab create` give back: the place where a
/// new shell pane is waiting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Created {
    pub workspace_id: WorkspaceId,
    pub tab_id: TabId,
    pub root_pane: PaneId,
    pub cwd: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct StartOpts {
    /// Answer Claude Code's "do you trust this folder?" dialog with yes.
    /// Only set this for places the user has named explicitly.
    pub accept_trust_prompt: bool,
    /// Startup timeout; herdr's default is 30 s.
    pub timeout: Option<std::time::Duration>,
    /// Native arguments passed to the agent after `--`.
    pub agent_args: Vec<String>,
}

#[cfg(test)]
mod agent_kind_tests {
    use super::AgentKind;

    #[test]
    fn any_well_formed_kind_name_parses() {
        for k in ["claude", "codex", "gemini", "opencode", " Codex "] {
            let kind: AgentKind = k.parse().unwrap();
            assert_eq!(kind.as_str(), k.trim().to_ascii_lowercase());
        }
        assert_eq!(AgentKind::default().as_str(), "claude");
    }

    #[test]
    fn malformed_kind_names_are_rejected() {
        for k in ["", "9lives", "open code", "claude;rm", "ĉlaude"] {
            assert!(k.parse::<AgentKind>().is_err(), "{k:?}");
        }
    }

    #[test]
    fn serialises_as_the_bare_name() {
        let kind: AgentKind = "codex".parse().unwrap();
        assert_eq!(serde_json::to_string(&kind).unwrap(), "\"codex\"");
        let back: AgentKind = serde_json::from_str("\"claude\"").unwrap();
        assert_eq!(back, AgentKind::default());
    }
}
