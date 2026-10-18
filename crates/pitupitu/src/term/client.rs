//! The terminal host as an `AgentHost`: every call is one request to the
//! holder (`pitupitu terminals`), which is started on first need and left
//! running on its own, so it outlives the box agent.

use super::socket_path;
use async_trait::async_trait;
use herdr::{
    AgentHost, AgentInfo, AgentKind, AgentStatus, Created, Error, PaneId, Result, StartOpts, TabId,
    Workspace, WorkspaceId,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

pub struct TermHost {
    pub socket: PathBuf,
}

impl Default for TermHost {
    fn default() -> Self {
        Self {
            socket: socket_path(),
        }
    }
}

/// One request, one reply line.
pub async fn request(socket: &Path, req: &Value) -> Result<Value> {
    let stream = UnixStream::connect(socket).await.map_err(|e| Error::Api {
        code: "terminals_not_running".into(),
        message: format!("the terminal holder is not running: {e}"),
    })?;
    let (read, mut write) = stream.into_split();
    let mut line = req.to_string();
    line.push('\n');
    write
        .write_all(line.as_bytes())
        .await
        .map_err(|e| Error::Shape(format!("writing to the terminal holder: {e}")))?;
    let mut reply = String::new();
    BufReader::new(read)
        .read_line(&mut reply)
        .await
        .map_err(|e| Error::Shape(format!("reading from the terminal holder: {e}")))?;
    let v: Value = serde_json::from_str(&reply)
        .map_err(|e| Error::Shape(format!("terminal holder reply: {e}")))?;
    if let Some(err) = v.get("err") {
        let code = err["code"].as_str().unwrap_or("unknown").to_string();
        let message = err["message"].as_str().unwrap_or_default().to_string();
        return Err(if code == "agent_not_ready" {
            Error::NotReady {
                name: req["name"].as_str().unwrap_or_default().to_string(),
                why: message,
            }
        } else {
            Error::Api { code, message }
        });
    }
    Ok(v.get("ok").cloned().unwrap_or(Value::Null))
}

impl TermHost {
    async fn call<T: DeserializeOwned>(&self, req: Value) -> Result<T> {
        let v = request(&self.socket, &req).await?;
        serde_json::from_value(v).map_err(|e| Error::Shape(format!("{}: {e}", req["op"])))
    }

    /// Whether the holder answers.
    pub async fn running(&self) -> bool {
        request(&self.socket, &json!({"op": "ping"})).await.is_ok()
    }

    /// Start the holder if it is not running: this binary, detached in a
    /// session of its own so it outlives the box agent and its service.
    pub async fn ensure(&self) -> Result<()> {
        if self.running().await {
            return Ok(());
        }
        let exe = std::env::current_exe().map_err(Error::Spawn)?;
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("terminals")
            .env("PITUPITU_TERMS_SOCKET", &self.socket)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // SAFETY: setsid is async-signal-safe and touches nothing else.
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        cmd.spawn().map_err(Error::Spawn)?;
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if self.running().await {
                tracing::info!("terminal holder started");
                return Ok(());
            }
        }
        Err(Error::Api {
            code: "terminals_not_running".into(),
            message: "the terminal holder did not start".into(),
        })
    }

    /// For calls that only look: no holder means no terminals.
    async fn look<T: DeserializeOwned + Default>(&self, req: Value) -> Result<T> {
        match self.call(req).await {
            Err(Error::Api { code, .. }) if code == "terminals_not_running" => Ok(T::default()),
            r => r,
        }
    }
}

#[async_trait]
impl AgentHost for TermHost {
    async fn list_workspaces(&self) -> Result<Vec<Workspace>> {
        self.look(json!({"op": "list_workspaces"})).await
    }

    async fn create_workspace(&self, cwd: &Path, label: &str) -> Result<Created> {
        self.ensure().await?;
        self.call(json!({"op": "create_workspace", "cwd": cwd, "label": label}))
            .await
    }

    async fn create_tab(
        &self,
        workspace: &WorkspaceId,
        cwd: Option<&Path>,
        label: &str,
    ) -> Result<Created> {
        self.call(json!({"op": "create_tab", "workspace": workspace, "cwd": cwd, "label": label}))
            .await
    }

    async fn close_workspace(&self, workspace: &WorkspaceId) -> Result<()> {
        self.call::<Value>(json!({"op": "close_workspace", "workspace": workspace}))
            .await
            .map(drop)
    }

    async fn close_tab(&self, tab: &TabId) -> Result<()> {
        self.call::<Value>(json!({"op": "close_tab", "tab": tab}))
            .await
            .map(drop)
    }

    async fn start_agent(
        &self,
        name: &str,
        kind: AgentKind,
        pane: &PaneId,
        opts: StartOpts,
    ) -> Result<AgentInfo> {
        let mut req = json!({
            "op": "start_agent",
            "name": name,
            "kind": kind,
            "pane": pane,
            "accept_trust_prompt": opts.accept_trust_prompt,
            "agent_args": opts.agent_args,
        });
        if let Some(t) = opts.timeout {
            req["timeout_ms"] = json!(t.as_millis() as u64);
        }
        self.call(req).await
    }

    async fn prompt(&self, agent: &str, text: &str) -> Result<AgentInfo> {
        self.call(json!({"op": "prompt", "agent": agent, "text": text}))
            .await
    }

    async fn agent(&self, agent: &str) -> Result<AgentInfo> {
        self.call(json!({"op": "agent", "agent": agent})).await
    }

    async fn list_agents(&self) -> Result<Vec<AgentInfo>> {
        self.look(json!({"op": "list_agents"})).await
    }

    /// Polled: the holder answers each look at once.
    async fn wait(
        &self,
        agent: &str,
        until: &[AgentStatus],
        timeout: Duration,
    ) -> Result<AgentInfo> {
        let end = tokio::time::Instant::now() + timeout;
        loop {
            let info = self.agent(agent).await?;
            let hit = if until.is_empty() {
                info.agent_status != AgentStatus::Working
            } else {
                until.contains(&info.agent_status)
            };
            if hit || tokio::time::Instant::now() >= end {
                return Ok(info);
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    async fn read(&self, agent: &str, lines: u32) -> Result<String> {
        self.call(json!({"op": "read", "agent": agent, "lines": lines}))
            .await
    }

    async fn read_colored(&self, agent: &str, lines: u32) -> Result<String> {
        self.call(json!({"op": "read", "agent": agent, "lines": lines, "color": true}))
            .await
    }

    async fn send_keys(&self, agent: &str, keys: &[&str]) -> Result<()> {
        self.call::<Value>(json!({"op": "send_keys", "agent": agent, "keys": keys}))
            .await
            .map(drop)
    }
}

/// `pitupitu hook <pane> <event> [json]`, run by an agent's own hooks.
/// Claude Code passes its event as JSON on stdin; Codex's `notify` passes
/// it as the last argument. Only the kind of event is used: whether a
/// notification is a permission prompt, whether a turn completed. Always
/// quiet and quick, and never fails the agent.
pub async fn hook(pane: &str, event: &str, arg: Option<&str>) {
    let (event, detail) = match event {
        "notify" => {
            let mut input = String::new();
            let _ = tokio::time::timeout(
                Duration::from_millis(300),
                tokio::io::AsyncReadExt::read_to_string(&mut tokio::io::stdin(), &mut input),
            )
            .await;
            let v: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
            let kind = v["notification_type"].as_str().unwrap_or_default();
            let message = v["message"].as_str().unwrap_or_default().to_lowercase();
            let permission = kind == "permission_prompt" || message.contains("permission");
            ("notify", if permission { "permission" } else { "" })
        }
        "codex" => {
            let v: Value = arg
                .and_then(|a| serde_json::from_str(a).ok())
                .unwrap_or(Value::Null);
            if v["type"] == "agent-turn-complete" {
                ("done", "")
            } else {
                return;
            }
        }
        other => (other, ""),
    };
    let req = json!({"op": "hook", "pane": pane, "event": event, "detail": detail});
    let _ =
        tokio::time::timeout(Duration::from_secs(2), request(&super::socket_path(), &req)).await;
}
