//! One box link call → one `AgentHost` method. The cloud can ask for these
//! calls and nothing else: no shell, no arbitrary herdr command.

use herdr::{AgentHost, AgentKind, AgentStatus, Created, PaneId, StartOpts, TabId, WorkspaceId};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Longest `wait` the cloud may ask for.
pub const MAX_WAIT: Duration = Duration::from_secs(120);
/// Most transcript lines one `read` returns.
const MAX_LINES: u32 = 2000;

/// The `err` of a reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CallError {
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub message: String,
}

impl CallError {
    pub fn bad_call(message: impl Into<String>) -> Self {
        CallError {
            kind: "bad_call",
            code: None,
            message: message.into(),
        }
    }

    /// Refused by the box itself, with a code the cloud keys on.
    pub fn api(code: &str, message: impl Into<String>) -> Self {
        CallError {
            kind: "api",
            code: Some(code.to_string()),
            message: message.into(),
        }
    }

    pub fn timeout(message: impl Into<String>) -> Self {
        CallError {
            kind: "timeout",
            code: None,
            message: message.into(),
        }
    }
}

impl From<herdr::Error> for CallError {
    fn from(e: herdr::Error) -> Self {
        let message = e.to_string();
        match e {
            herdr::Error::Api { code, .. } => CallError {
                kind: "api",
                code: Some(code),
                message,
            },
            herdr::Error::NotReady { .. } => CallError {
                kind: "not_ready",
                code: None,
                message,
            },
            herdr::Error::Exit { .. } | herdr::Error::Spawn(_) | herdr::Error::Shape(_) => {
                CallError {
                    kind: "io",
                    code: None,
                    message,
                }
            }
        }
    }
}

fn args<T: DeserializeOwned>(call: &str, args: Value) -> Result<T, CallError> {
    let args = if args.is_null() {
        Value::Object(Default::default())
    } else {
        args
    };
    serde_json::from_value(args).map_err(|e| CallError::bad_call(format!("{call}: {e}")))
}

fn ok<T: Serialize>(value: T) -> Result<Value, CallError> {
    serde_json::to_value(value).map_err(|e| CallError {
        kind: "io",
        code: None,
        message: e.to_string(),
    })
}

#[derive(Deserialize)]
struct CreateWorkspace {
    cwd: PathBuf,
    label: String,
    /// The agent the cloud means to start there, refused before the
    /// workspace is opened when this box cannot start it.
    #[serde(default)]
    kind: Option<String>,
    /// Work in the pitupitu home folder (a lookup, a question): a tab in
    /// the box's one `pitupitu` workspace instead of a workspace of its
    /// own.
    #[serde(default)]
    scratch: bool,
}

#[derive(Deserialize)]
struct CreateTab {
    workspace: WorkspaceId,
    cwd: Option<PathBuf>,
    label: String,
}

#[derive(Deserialize)]
struct Workspace {
    workspace: WorkspaceId,
}

#[derive(Deserialize)]
struct Tab {
    tab: TabId,
}

#[derive(Deserialize)]
struct StartAgent {
    name: String,
    kind: String,
    pane: PaneId,
    #[serde(default)]
    accept_trust_prompt: bool,
    timeout_ms: Option<u64>,
    #[serde(default)]
    agent_args: Vec<String>,
}

#[derive(Deserialize)]
struct Agent {
    agent: String,
}

#[derive(Deserialize)]
struct Prompt {
    agent: String,
    text: String,
}

#[derive(Deserialize)]
struct Wait {
    agent: String,
    #[serde(default)]
    until: Vec<AgentStatus>,
    timeout_ms: u64,
}

#[derive(Deserialize)]
struct Read {
    agent: String,
    lines: u32,
    /// Colours as SGR codes, for a page showing the screen.
    #[serde(default)]
    color: bool,
}

#[derive(Deserialize)]
struct SendKeys {
    agent: String,
    keys: Vec<String>,
}

/// The label of the box's workspace for scratch work.
pub const SCRATCH_LABEL: &str = "pitupitu";

/// One scratch tab is opened at a time, so two at once do not make two
/// `pitupitu` workspaces.
static SCRATCH: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A tab for scratch work in the box's `pitupitu` workspace, which is
/// found by its label (so again after a restart) or opened in `cwd`, the
/// home folder, when there is none; its first tab is then the task's.
/// What comes back names the `pitupitu` workspace, the task's tab and its
/// pane: the cloud closes the task with `close_tab`, which leaves the
/// other scratch tasks alone (herdr closes the workspace with its last
/// tab).
async fn scratch_tab(
    host: &dyn AgentHost,
    cwd: &Path,
    label: &str,
    kind: Option<&AgentKind>,
) -> herdr::Result<Created> {
    if let Some(k) = kind {
        herdr::refuse_unsupported(&host.kinds().await, k)?;
    }
    let _one = SCRATCH.lock().await;
    let existing = host
        .list_workspaces()
        .await?
        .into_iter()
        .find(|w| w.label == SCRATCH_LABEL);
    match existing {
        Some(w) => host.create_tab(&w.workspace_id, Some(cwd), label).await,
        None => host.create_workspace(cwd, SCRATCH_LABEL).await,
    }
}

/// How long the box lets a call run before answering `timeout` itself:
/// the call's own timeout plus a margin, or a generous default.
pub fn deadline(call: &str, args: &Value) -> Duration {
    let own = args
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .map(Duration::from_millis);
    match (call, own) {
        // start_agent waits for the start and again for the agent to settle.
        ("start_agent", Some(t)) => t * 2 + Duration::from_secs(30),
        ("wait", Some(t)) => t.min(MAX_WAIT) + Duration::from_secs(10),
        _ => Duration::from_secs(150),
    }
}

/// Run one call against the host.
pub async fn dispatch(host: &dyn AgentHost, call: &str, a: Value) -> Result<Value, CallError> {
    match call {
        "list_workspaces" => ok(host.list_workspaces().await?),
        "create_workspace" => {
            let a: CreateWorkspace = args(call, a)?;
            let kind = a
                .kind
                .map(|k| k.parse::<herdr::AgentKind>().map_err(CallError::bad_call))
                .transpose()?;
            if a.scratch {
                ok(scratch_tab(host, &a.cwd, &a.label, kind.as_ref()).await?)
            } else {
                ok(host
                    .create_workspace_for(&a.cwd, &a.label, kind.as_ref())
                    .await?)
            }
        }
        "create_tab" => {
            let a: CreateTab = args(call, a)?;
            ok(host
                .create_tab(&a.workspace, a.cwd.as_deref(), &a.label)
                .await?)
        }
        "close_workspace" => {
            let a: Workspace = args(call, a)?;
            ok(host.close_workspace(&a.workspace).await?)
        }
        "close_tab" => {
            let a: Tab = args(call, a)?;
            ok(host.close_tab(&a.tab).await?)
        }
        "start_agent" => {
            let a: StartAgent = args(call, a)?;
            let kind = a.kind.parse().map_err(CallError::bad_call)?;
            let opts = StartOpts {
                accept_trust_prompt: a.accept_trust_prompt,
                timeout: a.timeout_ms.map(Duration::from_millis),
                agent_args: a.agent_args,
            };
            ok(host.start_agent(&a.name, kind, &a.pane, opts).await?)
        }
        "prompt" => {
            let a: Prompt = args(call, a)?;
            ok(host.prompt(&a.agent, &a.text).await?)
        }
        "agent" => {
            let a: Agent = args(call, a)?;
            ok(host.agent(&a.agent).await?)
        }
        "list_agents" => ok(host.list_agents().await?),
        "wait" => {
            let a: Wait = args(call, a)?;
            let timeout = Duration::from_millis(a.timeout_ms).min(MAX_WAIT);
            ok(host.wait(&a.agent, &a.until, timeout).await?)
        }
        "read" => {
            let a: Read = args(call, a)?;
            let lines = a.lines.min(MAX_LINES);
            if a.color {
                ok(host.read_colored(&a.agent, lines).await?)
            } else {
                ok(host.read(&a.agent, lines).await?)
            }
        }
        "send_keys" => {
            let a: SendKeys = args(call, a)?;
            let keys: Vec<&str> = a.keys.iter().map(String::as_str).collect();
            ok(host.send_keys(&a.agent, &keys).await?)
        }
        other => Err(CallError::bad_call(format!("unknown call `{other}`"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testhost::FakeHost;
    use serde_json::json;

    #[tokio::test]
    async fn calls_reach_the_host_with_their_arguments() {
        let host = FakeHost::default();
        let out = dispatch(&host, "read", json!({"agent": "notes", "lines": 80}))
            .await
            .unwrap();
        assert_eq!(out, json!("screen of notes, 80 lines"));

        let out = dispatch(
            &host,
            "wait",
            json!({"agent": "notes", "until": ["idle", "done"], "timeout_ms": 999_999}),
        )
        .await
        .unwrap();
        assert_eq!(out["agent_status"], "idle");
        assert_eq!(
            host.log(),
            [
                "read notes 80",
                "wait notes [Idle, Done] 120000ms" // capped
            ]
        );

        let out = dispatch(
            &host,
            "start_agent",
            json!({"name": "t1", "kind": "codex", "pane": "w1:p1",
                   "accept_trust_prompt": true, "agent_args": ["--model", "gpt-5"]}),
        )
        .await
        .unwrap();
        assert_eq!(out["name"], "t1");
        assert_eq!(
            host.log().last().unwrap(),
            "start t1 codex w1:p1 trust=true args=[\"--model\", \"gpt-5\"]"
        );
        assert_eq!(
            dispatch(&host, "close_tab", json!({"tab": "w1:t2"})).await,
            Ok(Value::Null)
        );
    }

    #[tokio::test]
    async fn an_unsupported_kind_is_refused_before_the_workspace_opens() {
        let host = FakeHost::default();
        let e = dispatch(
            &host,
            "create_workspace",
            json!({"cwd": "/x", "label": "x", "kind": "gemini"}),
        )
        .await
        .unwrap_err();
        assert_eq!(e.kind, "api");
        assert_eq!(e.code.as_deref(), Some("unsupported_agent_kind"));
        assert!(host.log().is_empty());
        dispatch(
            &host,
            "create_workspace",
            json!({"cwd": "/x", "label": "x", "kind": "codex"}),
        )
        .await
        .unwrap();
        assert_eq!(host.log(), ["create_workspace /x x"]);
    }

    #[tokio::test]
    async fn scratch_work_shares_one_pitupitu_workspace_a_tab_each() {
        let host = FakeHost::default();
        let scratch = |label: &str| json!({"cwd": "/home/me/pitupitu", "label": label, "scratch": true, "kind": "claude"});
        let first = dispatch(&host, "create_workspace", scratch("weather"))
            .await
            .unwrap();
        let second = dispatch(&host, "create_workspace", scratch("trains"))
            .await
            .unwrap();
        assert_eq!(
            host.log(),
            [
                "create_workspace /home/me/pitupitu pitupitu",
                "create_tab w9 trains"
            ]
        );
        // Both name the one pitupitu workspace, each its own tab and pane.
        assert_eq!(first["workspace_id"], "w9");
        assert_eq!(second["workspace_id"], "w9");
        assert_ne!(first["tab_id"], second["tab_id"]);
        assert_ne!(first["root_pane"], second["root_pane"]);
        // Closing one task is closing its tab; the workspace is closed as such.
        dispatch(&host, "close_tab", json!({"tab": first["tab_id"]}))
            .await
            .unwrap();
        dispatch(&host, "close_workspace", json!({"workspace": "w9"}))
            .await
            .unwrap();
        assert_eq!(host.log()[2..], ["close_tab w9:t1", "close_workspace w9"]);
        // A protocol 2 cloud's `driver` hint is ignored.
        dispatch(
            &host,
            "create_workspace",
            json!({"cwd": "/x", "label": "x", "driver": "terminal"}),
        )
        .await
        .unwrap();
        assert_eq!(host.log().last().unwrap(), "create_workspace /x x");
    }

    /// The same against a live herdr, in a throwaway session.
    #[tokio::test]
    async fn scratch_tabs_in_a_live_herdr() {
        let Some(scratch) = crate::testhost::ScratchHerdr::new() else {
            return;
        };
        let host = &scratch.cli;
        host.start_server().unwrap();
        let end = std::time::Instant::now() + Duration::from_secs(15);
        while host.list_workspaces().await.is_err() {
            assert!(std::time::Instant::now() < end, "no server");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let home = tempfile::tempdir().unwrap();
        // herdr reports the resolved path (macOS: /var is /private/var).
        let home_path = home.path().canonicalize().unwrap();
        let args = |label: &str| json!({"cwd": home_path, "label": label, "scratch": true});
        let a = dispatch(host, "create_workspace", args("a")).await.unwrap();
        let b = dispatch(host, "create_workspace", args("b")).await.unwrap();
        let ws = host.list_workspaces().await.unwrap();
        let ours: Vec<_> = ws.iter().filter(|w| w.label == SCRATCH_LABEL).collect();
        assert_eq!(ours.len(), 1, "{ws:?}");
        assert_eq!(a["cwd"], json!(home_path));
        assert_eq!(a["workspace_id"], b["workspace_id"]);
        assert_eq!(a["workspace_id"], json!(ours[0].workspace_id));
        dispatch(host, "close_tab", json!({"tab": a["tab_id"]}))
            .await
            .unwrap();
        // The other scratch task is left alone...
        assert_eq!(host.list_workspaces().await.unwrap().len(), 1);
        dispatch(host, "close_tab", json!({"tab": b["tab_id"]}))
            .await
            .unwrap();
        // ...and herdr closes the workspace with its last tab.
        assert!(host.list_workspaces().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn bad_calls_are_answered_not_run() {
        let host = FakeHost::default();
        let e = dispatch(&host, "shell", json!({"cmd": "ls"}))
            .await
            .unwrap_err();
        assert_eq!(e.kind, "bad_call");
        let e = dispatch(&host, "read", json!({"agent": "notes"}))
            .await
            .unwrap_err();
        assert_eq!(e.kind, "bad_call");
        let e = dispatch(
            &host,
            "start_agent",
            json!({"name": "t", "kind": "rm -rf", "pane": "w1:p1"}),
        )
        .await
        .unwrap_err();
        assert_eq!(e.kind, "bad_call");
        assert!(host.log().is_empty());
    }

    #[tokio::test]
    async fn herdr_errors_keep_their_kind_and_code() {
        let host = FakeHost::default();
        let e = dispatch(&host, "agent", json!({"agent": "missing"}))
            .await
            .unwrap_err();
        assert_eq!(e.kind, "api");
        assert_eq!(e.code.as_deref(), Some("agent_not_found"));
        assert_eq!(
            serde_json::to_value(&e).unwrap()["code"],
            json!("agent_not_found")
        );
    }
}
