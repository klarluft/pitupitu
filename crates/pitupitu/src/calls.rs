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
struct Pane {
    pane: PaneId,
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

/// Stop a session, one the person started by hand included: close the
/// tab that holds `pane`. When it was its workspace's only tab, herdr
/// closes the workspace with it. `not_found` when there is no such pane
/// (or it went away before its tab closed).
async fn close_session(host: &dyn AgentHost, pane: &PaneId) -> Result<(), CallError> {
    let gone = |e: herdr::Error| match e.code() {
        Some("pane_not_found" | "tab_not_found") => {
            CallError::api("not_found", format!("no pane {pane} on this box"))
        }
        _ => CallError::from(e),
    };
    let tab = host.pane_tab(pane).await.map_err(gone)?;
    host.close_tab(&tab).await.map_err(gone)
}

/// Bring a session to the front where herdr is open on this machine:
/// focus `pane` itself, or the workspace and the tab that hold it when no
/// agent is in it. Nothing is opened, closed or typed. `not_found` when there is no such pane (or it went
/// away on the way).
async fn focus_session(host: &dyn AgentHost, pane: &PaneId) -> Result<(), CallError> {
    host.focus_pane(pane).await.map_err(|e| match e.code() {
        Some("pane_not_found" | "tab_not_found" | "workspace_not_found") => {
            CallError::api("not_found", format!("no pane {pane} on this box"))
        }
        _ => CallError::from(e),
    })
}

/// How long `last_message` may take: it reads one file.
pub const LAST_MESSAGE_TIMEOUT: Duration = Duration::from_secs(8);

/// `last_message`: the agent's whole last reply from its own log, found by
/// the session herdr reports in its pane. `logs` is the home folder the
/// agents keep their logs in, `None` while history is off on this box, and
/// then nothing is looked at, not even herdr.
pub async fn last_message(
    host: &dyn AgentHost,
    a: Value,
    logs: Option<PathBuf>,
) -> Result<Value, CallError> {
    let a: Agent = args("last_message", a)?;
    let Some(home) = logs else {
        return Err(CallError::api(
            "history_off",
            "reading the agents' logs is switched off on this box",
        ));
    };
    let info = host.agent(&a.agent).await?;
    let found = tokio::task::spawn_blocking(move || {
        let (path, agent) =
            crate::history::find_log(&home, &info.agent, info.agent_session.as_ref(), &info.cwd)?;
        crate::history::last_reply(&path, agent)
    })
    .await
    .map_err(|e| CallError {
        kind: "io",
        code: None,
        message: e.to_string(),
    })?;
    match found {
        Ok(reply) => ok(reply),
        Err(crate::history::NoReply::NoLog(why)) => Err(CallError::api("no_log", why)),
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
        ("last_message", _) => LAST_MESSAGE_TIMEOUT,
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
        "close_session" => {
            let a: Pane = args(call, a)?;
            ok(close_session(host, &a.pane).await?)
        }
        "focus_session" => {
            let a: Pane = args(call, a)?;
            ok(focus_session(host, &a.pane).await?)
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
        // The link runs it with the history setting; here it is off.
        "last_message" => last_message(host, a, None).await,
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
    async fn close_session_closes_the_tab_that_holds_the_pane() {
        let host = FakeHost::default();
        assert_eq!(
            dispatch(&host, "close_session", json!({"pane": "w3:p2"})).await,
            Ok(Value::Null)
        );
        assert_eq!(host.log(), ["close_tab w3:t2"]);
        let e = dispatch(&host, "close_session", json!({"pane": "w404:p1"}))
            .await
            .unwrap_err();
        assert_eq!((e.kind, e.code.as_deref()), ("api", Some("not_found")));
        let e = dispatch(&host, "close_session", json!({}))
            .await
            .unwrap_err();
        assert_eq!(e.kind, "bad_call");
        assert_eq!(host.log().len(), 1);
    }

    /// The same against a live herdr: a tab of a workspace with two goes
    /// alone; the last one takes its workspace with it.
    #[tokio::test]
    async fn close_session_in_a_live_herdr() {
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
        let dir = tempfile::tempdir().unwrap();
        let first = host.create_workspace(dir.path(), "mine").await.unwrap();
        let second = host
            .create_tab(&first.workspace_id, None, "two")
            .await
            .unwrap();
        let e = dispatch(host, "close_session", json!({"pane": "w99:p99"}))
            .await
            .unwrap_err();
        assert_eq!(e.code.as_deref(), Some("not_found"), "{e:?}");
        dispatch(host, "close_session", json!({"pane": second.root_pane}))
            .await
            .unwrap();
        assert_eq!(host.list_workspaces().await.unwrap().len(), 1);
        let e = dispatch(host, "close_session", json!({"pane": second.root_pane}))
            .await
            .unwrap_err();
        assert_eq!(e.code.as_deref(), Some("not_found"), "{e:?}");
        dispatch(host, "close_session", json!({"pane": first.root_pane}))
            .await
            .unwrap();
        assert!(host.list_workspaces().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn focus_session_focuses_the_workspace_and_tab_that_hold_the_pane() {
        let host = FakeHost::default();
        assert_eq!(
            dispatch(&host, "focus_session", json!({"pane": "w3:p2"})).await,
            Ok(Value::Null)
        );
        assert_eq!(host.log(), ["focus_workspace w3", "focus_tab w3:t2"]);
        let e = dispatch(&host, "focus_session", json!({"pane": "w404:p1"}))
            .await
            .unwrap_err();
        assert_eq!((e.kind, e.code.as_deref()), ("api", Some("not_found")));
        let e = dispatch(&host, "focus_session", json!({}))
            .await
            .unwrap_err();
        assert_eq!(e.kind, "bad_call");
        assert_eq!(host.log().len(), 2);
    }

    /// The same against a live herdr: the workspace and tab that hold the
    /// pane come to the front, and nothing is opened or closed. In a split
    /// tab the agent's own pane gets the focus, the one a person's typing
    /// goes to; a pane with no agent gets its tab.
    #[tokio::test]
    async fn focus_session_in_a_live_herdr() {
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
        let dir = tempfile::tempdir().unwrap();
        let first = host.create_workspace(dir.path(), "first").await.unwrap();
        let other = host.create_workspace(dir.path(), "other").await.unwrap();
        let second = host
            .create_tab(&other.workspace_id, None, "two")
            .await
            .unwrap();
        let focused_ws = |ws: &[herdr::Workspace]| {
            ws.iter()
                .find(|w| w.focused)
                .map(|w| w.workspace_id.clone())
        };
        // Whether herdr has this pane in front (its tab focused).
        let pane_focused = |pane: &herdr::PaneId| {
            let out = std::process::Command::new("herdr")
                .args(["--session", host.session().unwrap(), "pane", "get", &pane.0])
                .output()
                .unwrap();
            let v: Value = serde_json::from_slice(&out.stdout).unwrap();
            v["result"]["pane"]["focused"] == true
        };
        let before = host.list_workspaces().await.unwrap();
        assert_eq!(focused_ws(&before), Some(first.workspace_id.clone()));
        assert!(!pane_focused(&second.root_pane));
        dispatch(host, "focus_session", json!({"pane": second.root_pane}))
            .await
            .unwrap();
        let after = host.list_workspaces().await.unwrap();
        assert_eq!(after.len(), 2);
        assert_eq!(focused_ws(&after), Some(other.workspace_id.clone()));
        assert!(pane_focused(&second.root_pane));
        assert!(!pane_focused(&other.root_pane));
        let e = dispatch(host, "focus_session", json!({"pane": "w99:p99"}))
            .await
            .unwrap_err();
        assert_eq!(e.code.as_deref(), Some("not_found"), "{e:?}");

        // A split tab that is not in front, its second pane holding an
        // agent: without `agent focus` the first pane keeps the focus.
        let herdr = |args: &[&str]| {
            let out = std::process::Command::new("herdr")
                .args(["--session", host.session().unwrap()])
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "{args:?}: {out:?}");
            serde_json::from_slice::<Value>(&out.stdout).unwrap_or(Value::Null)
        };
        let split = host
            .create_tab(&other.workspace_id, None, "split")
            .await
            .unwrap();
        let agent_pane = herdr(&[
            "pane",
            "split",
            "--pane",
            &split.root_pane.0,
            "--direction",
            "right",
            "--no-focus",
        ]);
        let agent_pane = agent_pane["result"]["pane"]["pane_id"]
            .as_str()
            .unwrap()
            .to_string();
        herdr(&[
            "pane",
            "report-agent",
            &agent_pane,
            "--source",
            "test",
            "--agent",
            "claude",
            "--state",
            "idle",
        ]);
        herdr(&["workspace", "focus", &first.workspace_id.0]);
        dispatch(host, "focus_session", json!({"pane": agent_pane}))
            .await
            .unwrap();
        let after = host.list_workspaces().await.unwrap();
        assert_eq!(focused_ws(&after), Some(other.workspace_id.clone()));
        assert!(pane_focused(&herdr::PaneId(agent_pane.clone())));
        assert!(!pane_focused(&split.root_pane));

        // The tab's other pane has no agent: its tab comes up all the same.
        herdr(&["workspace", "focus", &first.workspace_id.0]);
        dispatch(host, "focus_session", json!({"pane": second.root_pane}))
            .await
            .unwrap();
        let after = host.list_workspaces().await.unwrap();
        assert_eq!(focused_ws(&after), Some(other.workspace_id.clone()));
        assert!(pane_focused(&second.root_pane));
    }

    #[tokio::test]
    async fn last_message_reads_the_log_only_with_history_on() {
        let home = tempfile::tempdir().unwrap();
        let logs = home.path().join(".claude/projects/-work");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(
            logs.join("s-1.jsonl"),
            [
                json!({"type": "user", "isSidechain": false, "timestamp": "2026-10-08T12:00:00Z",
                       "message": {"role": "user", "content": "Name the boat"}}),
                json!({"type": "assistant", "isSidechain": false, "timestamp": "2026-10-08T12:00:03Z",
                       "message": {"role": "assistant", "content": [{"type": "text", "text": "Call it Puffin."}]}}),
            ]
            .map(|v| v.to_string())
            .join("\n")
                + "\n",
        )
        .unwrap();
        let host = FakeHost::default();
        let with = |name: &str, pane: &str, kind: &str, id: Option<&str>| {
            let mut a = crate::testhost::agent(name, pane, herdr::AgentStatus::Done, 3);
            a.agent = kind.into();
            a.agent_session = id.map(|id| herdr::AgentSession {
                agent: kind.into(),
                kind: "id".into(),
                value: id.into(),
            });
            a
        };
        host.agents.lock().unwrap().extend([
            with("boat", "w1:p1", "claude", Some("s-1")),
            with("lost", "w1:p2", "claude", Some("s-2")),
            with("bare", "w1:p3", "claude", None),
            with("gem", "w1:p4", "gemini", Some("g-1")),
        ]);
        let on = Some(home.path().to_path_buf());
        let out = last_message(&host, json!({"agent": "boat"}), on.clone())
            .await
            .unwrap();
        assert_eq!(
            out,
            json!({"text": "Call it Puffin.", "at": "2026-10-08T12:00:03Z", "turn": 1, "cut": false})
        );
        // By pane, as for every call that takes `agent`.
        let out = last_message(&host, json!({"agent": "w1:p1"}), on.clone()).await;
        assert!(out.is_ok(), "{out:?}");
        for name in ["lost", "bare", "gem"] {
            let e = last_message(&host, json!({"agent": name}), on.clone())
                .await
                .unwrap_err();
            assert_eq!(
                (e.kind, e.code.as_deref()),
                ("api", Some("no_log")),
                "{name}"
            );
        }
        let e = last_message(&host, json!({"agent": "missing"}), on.clone())
            .await
            .unwrap_err();
        assert_eq!(e.code.as_deref(), Some("agent_not_found"));
        let e = last_message(&host, json!({}), on).await.unwrap_err();
        assert_eq!(e.kind, "bad_call");

        // History off: refused before anything is looked at.
        let before = host.log().len();
        let e = last_message(&host, json!({"agent": "boat"}), None)
            .await
            .unwrap_err();
        assert_eq!((e.kind, e.code.as_deref()), ("api", Some("history_off")));
        let e = dispatch(&host, "last_message", json!({"agent": "boat"}))
            .await
            .unwrap_err();
        assert_eq!(e.code.as_deref(), Some("history_off"));
        assert_eq!(host.log().len(), before);
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
