//! One box link call → one `AgentHost` method. The cloud can ask for these
//! calls and nothing else: no shell, no arbitrary herdr command.

use herdr::{AgentHost, AgentStatus, PaneId, StartOpts, TabId, WorkspaceId};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
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
    /// The cloud's hint where the session should run (`terminal` for
    /// scratch work); honoured only while the driver setting is `auto`.
    #[serde(default)]
    driver: Option<String>,
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
}

#[derive(Deserialize)]
struct SendKeys {
    agent: String,
    keys: Vec<String>,
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
            ok(host
                .create_workspace_hinted(&a.cwd, &a.label, a.driver.as_deref())
                .await?)
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
            ok(host.read(&a.agent, a.lines.min(MAX_LINES)).await?)
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
