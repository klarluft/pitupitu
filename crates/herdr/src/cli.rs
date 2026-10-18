use crate::{
    AgentHost, AgentInfo, AgentKind, AgentStatus, Created, Error, PaneId, Result, StartOpts, TabId,
    Workspace, WorkspaceId,
};
use async_trait::async_trait;
use serde_json::Value;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tracing::{debug, warn};

/// Drives herdr by shelling out to its CLI.
///
/// The CLI only works from inside a herdr-managed pane or with the server's
/// socket reachable; it inherits that context from the environment.
#[derive(Debug, Clone)]
pub struct HerdrCli {
    bin: String,
}

impl Default for HerdrCli {
    fn default() -> Self {
        Self::new()
    }
}

impl HerdrCli {
    /// Uses `$HERDR_BIN` if set, else `herdr` from `PATH`.
    pub fn new() -> Self {
        Self {
            bin: std::env::var("HERDR_BIN").unwrap_or_else(|_| "herdr".to_string()),
        }
    }

    pub fn with_binary(bin: impl Into<String>) -> Self {
        Self { bin: bin.into() }
    }

    /// `herdr --version`, e.g. `herdr 0.9.3`.
    pub async fn version(&self) -> Result<String> {
        let (status, stdout, stderr) = self.exec(&["--version"]).await?;
        if !status.success() {
            return Err(Error::Exit {
                status: status.code().unwrap_or(-1),
                stderr: stderr.trim().to_string(),
            });
        }
        Ok(stdout.trim().to_string())
    }

    /// Run a command whose success output is `{"id":…,"result":{…}}` and
    /// return the `result` object. Errors come back as `{"error":{…}}` on
    /// stderr with exit status 1.
    async fn run_json(&self, args: &[&str]) -> Result<Value> {
        let (status, stdout, stderr) = self.exec(args).await?;
        if let Some(err) = parse_api_error(&stderr).or_else(|| parse_api_error(&stdout)) {
            return Err(err);
        }
        if !status.success() {
            return Err(Error::Exit {
                status: status.code().unwrap_or(-1),
                stderr: stderr.trim().to_string(),
            });
        }
        let value: Value = serde_json::from_str(&stdout)
            .map_err(|e| Error::Shape(format!("{e}: {}", stdout.trim())))?;
        value
            .get("result")
            .cloned()
            .ok_or_else(|| Error::Shape(format!("no `result` in {}", stdout.trim())))
    }

    /// The agent's recent transcript, with its colours when asked (older
    /// herdrs know no `--format`, so the plain read never passes it).
    async fn read_as(&self, agent: &str, lines: u32, ansi: bool) -> Result<String> {
        let lines = lines.to_string();
        let read = |source: &'static str| {
            let mut args = vec![
                "agent", "read", agent, "--source", source, "--lines", &lines,
            ];
            if ansi {
                args.extend(["--format", "ansi"]);
            }
            args
        };
        match self.run_text(&read("recent-unwrapped")).await {
            Ok(recent) if !blank(&recent) => return Ok(recent),
            Ok(_) => {}
            // An agent on the alternate screen has its history read by
            // scrolling, which herdr does only while it is idle; blocked
            // at a dialog it never is.
            Err(e) if e.code() == Some("agent_not_idle") => {}
            Err(e) => return Err(e),
        }
        // Dialogs (Claude Code's question UI, approvals) live on a screen
        // that `recent` does not see; the rendered viewport does.
        self.run_text(&read("visible")).await
    }

    /// Run a command whose success output is plain text (`agent read`).
    async fn run_text(&self, args: &[&str]) -> Result<String> {
        let (status, stdout, stderr) = self.exec(args).await?;
        if let Some(err) = parse_api_error(&stderr) {
            return Err(err);
        }
        if !status.success() {
            return Err(Error::Exit {
                status: status.code().unwrap_or(-1),
                stderr: stderr.trim().to_string(),
            });
        }
        Ok(stdout)
    }

    async fn exec(&self, args: &[&str]) -> Result<(std::process::ExitStatus, String, String)> {
        debug!(bin = %self.bin, ?args, "herdr");
        let out = Command::new(&self.bin)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(Error::Spawn)?;
        Ok((
            out.status,
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ))
    }

    /// Claude Code's first-run dialog in a directory it has not seen. The
    /// highlighted row carries `❯`; "No, exit" is the default.
    async fn answer_trust_prompt(&self, agent: &str) -> Result<bool> {
        let screen = self.read(agent, 40).await?;
        if !screen.contains("trust this folder") {
            return Ok(false);
        }
        let yes_highlighted = screen.lines().any(|l| l.contains('❯') && l.contains("Yes"));
        let keys: &[&str] = if yes_highlighted {
            &["enter"]
        } else {
            &["down", "enter"]
        };
        debug!(agent, ?keys, "answering trust prompt");
        self.send_keys(agent, keys).await?;
        Ok(true)
    }
}

/// Whether a transcript shows nothing, its SGR codes aside.
fn blank(text: &str) -> bool {
    let mut in_code = false;
    for ch in text.chars() {
        match ch {
            '\x1b' => in_code = true,
            'm' if in_code => in_code = false,
            _ if in_code => {}
            c if !c.is_whitespace() => return false,
            _ => {}
        }
    }
    true
}

fn parse_api_error(text: &str) -> Option<Error> {
    let value: Value = serde_json::from_str(text.trim()).ok()?;
    let err = value.get("error")?;
    Some(Error::Api {
        code: err
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        message: err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

fn agent_from(result: Value) -> Result<AgentInfo> {
    let agent = result
        .get("agent")
        .cloned()
        .ok_or_else(|| Error::Shape(format!("no `agent` in {result}")))?;
    serde_json::from_value(agent).map_err(|e| Error::Shape(format!("agent: {e}")))
}

/// `workspace_created` / `tab_created` both carry `tab` and `root_pane`.
fn created_from(result: &Value) -> Result<Created> {
    let get = |path: &[&str]| -> Result<String> {
        let mut v = result;
        for key in path {
            v = v
                .get(key)
                .ok_or_else(|| Error::Shape(format!("created: missing {}", path.join("."))))?;
        }
        v.as_str()
            .map(str::to_string)
            .ok_or_else(|| Error::Shape(format!("created: {} not a string", path.join("."))))
    };
    Ok(Created {
        workspace_id: WorkspaceId(get(&["root_pane", "workspace_id"])?),
        tab_id: TabId(get(&["tab", "tab_id"])?),
        root_pane: PaneId(get(&["root_pane", "pane_id"])?),
        cwd: get(&["root_pane", "cwd"])?.into(),
    })
}

fn ms(d: Duration) -> String {
    d.as_millis().to_string()
}

#[async_trait]
impl AgentHost for HerdrCli {
    async fn list_workspaces(&self) -> Result<Vec<Workspace>> {
        let result = self.run_json(&["workspace", "list"]).await?;
        let ws = result
            .get("workspaces")
            .cloned()
            .ok_or_else(|| Error::Shape("workspace_list: no `workspaces`".into()))?;
        serde_json::from_value(ws).map_err(|e| Error::Shape(format!("workspace_list: {e}")))
    }

    async fn create_workspace(&self, cwd: &Path, label: &str) -> Result<Created> {
        let cwd = cwd.to_string_lossy();
        let result = self
            .run_json(&[
                "workspace",
                "create",
                "--cwd",
                &cwd,
                "--label",
                label,
                "--no-focus",
            ])
            .await?;
        created_from(&result)
    }

    async fn create_tab(
        &self,
        workspace: &WorkspaceId,
        cwd: Option<&Path>,
        label: &str,
    ) -> Result<Created> {
        let cwd = cwd.map(|c| c.to_string_lossy().into_owned());
        let mut args = vec![
            "tab",
            "create",
            "--workspace",
            &workspace.0,
            "--label",
            label,
            "--no-focus",
        ];
        if let Some(c) = &cwd {
            args.extend(["--cwd", c]);
        }
        let result = self.run_json(&args).await?;
        created_from(&result)
    }

    async fn close_workspace(&self, workspace: &WorkspaceId) -> Result<()> {
        self.run_json(&["workspace", "close", &workspace.0])
            .await
            .map(drop)
    }

    async fn close_tab(&self, tab: &TabId) -> Result<()> {
        self.run_json(&["tab", "close", &tab.0]).await.map(drop)
    }

    async fn start_agent(
        &self,
        name: &str,
        kind: AgentKind,
        pane: &PaneId,
        opts: StartOpts,
    ) -> Result<AgentInfo> {
        let timeout = opts.timeout.map(ms);
        let mut args = vec![
            "agent",
            "start",
            name,
            "--kind",
            kind.as_str(),
            "--pane",
            &pane.0,
        ];
        if let Some(t) = &timeout {
            args.extend(["--timeout", t]);
        }
        if !opts.agent_args.is_empty() {
            args.push("--");
            args.extend(opts.agent_args.iter().map(String::as_str));
        }
        match self.run_json(&args).await {
            Ok(_) => {}
            Err(e) if e.code() == Some("agent_not_ready") => {
                // The name is registered; the agent is sitting on a dialog.
                let answered = if opts.accept_trust_prompt {
                    self.answer_trust_prompt(name).await?
                } else {
                    false
                };
                if !answered {
                    let screen = self.read(name, 20).await.unwrap_or_default();
                    return Err(Error::NotReady {
                        name: name.to_string(),
                        why: format!("blocked during startup:\n{}", screen.trim()),
                    });
                }
            }
            Err(e) => return Err(e),
        }
        let settle = opts.timeout.unwrap_or(Duration::from_secs(30));
        let info = self
            .wait(name, &[AgentStatus::Idle, AgentStatus::Done], settle)
            .await?;
        if !info.agent_status.is_ready() {
            let screen = self.read(name, 20).await.unwrap_or_default();
            warn!(agent = name, status = %info.agent_status, "agent not ready after start");
            return Err(Error::NotReady {
                name: name.to_string(),
                why: format!("{}:\n{}", info.agent_status, screen.trim()),
            });
        }
        Ok(info)
    }

    async fn prompt(&self, agent: &str, text: &str) -> Result<AgentInfo> {
        self.run_json(&["agent", "prompt", agent, text])
            .await
            .and_then(agent_from)
    }

    async fn agent(&self, agent: &str) -> Result<AgentInfo> {
        self.run_json(&["agent", "get", agent])
            .await
            .and_then(agent_from)
    }

    async fn list_agents(&self) -> Result<Vec<AgentInfo>> {
        let result = self.run_json(&["agent", "list"]).await?;
        let agents = result
            .get("agents")
            .cloned()
            .ok_or_else(|| Error::Shape("agent_list: no `agents`".into()))?;
        serde_json::from_value(agents).map_err(|e| Error::Shape(format!("agent_list: {e}")))
    }

    async fn wait(
        &self,
        agent: &str,
        until: &[AgentStatus],
        timeout: Duration,
    ) -> Result<AgentInfo> {
        let timeout = ms(timeout);
        let mut args = vec!["agent", "wait", agent];
        for status in until {
            args.extend(["--until", status.as_str()]);
        }
        args.extend(["--timeout", &timeout]);
        self.run_json(&args).await.and_then(agent_from)
    }

    async fn read(&self, agent: &str, lines: u32) -> Result<String> {
        self.read_as(agent, lines, false).await
    }

    async fn read_colored(&self, agent: &str, lines: u32) -> Result<String> {
        self.read_as(agent, lines, true).await
    }

    async fn send_keys(&self, agent: &str, keys: &[&str]) -> Result<()> {
        let mut args = vec!["agent", "send-keys", agent];
        args.extend_from_slice(keys);
        self.run_json(&args).await.map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_transcript_of_codes_and_spaces_is_blank() {
        assert!(blank("\x1b[0m  \n\x1b[38;2;1;2;3m \x1b[m"));
        assert!(!blank("\x1b[31mok\x1b[m"));
        assert!(!blank("plain"));
    }

    /// A stand-in `herdr` that answers `agent read` by source: `recent`
    /// as given, `visible` with the dialog.
    #[cfg(unix)]
    fn fake_herdr(recent: &str) -> (HerdrCli, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "pitupitu-herdr-{}-{}",
            std::process::id(),
            recent.len()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("herdr");
        let script = format!(
            "#!/bin/sh\ncase \"$*\" in\n  *recent-unwrapped*) {recent} ;;\n  *visible*) printf 'Option one or option two?\\n> 1. Option one\\n  2. Option two\\n' ;;\nesac\n"
        );
        std::fs::write(&bin, script).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        (HerdrCli::with_binary(bin.to_string_lossy()), dir)
    }

    /// herdr 0.9 refuses `recent` for a blocked agent on the alternate
    /// screen.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_blocked_agent_on_the_alternate_screen_is_read_from_the_viewport() {
        let (cli, dir) = fake_herdr(
            r#"echo '{"error":{"code":"agent_not_idle","message":"cannot read 80 lines while x is blocked"},"id":"cli:agent:read"}' >&2; exit 1"#,
        );
        let text = cli.read("x", 80).await.unwrap();
        assert!(text.contains("2. Option two"), "{text}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn other_read_errors_still_come_back() {
        let (cli, dir) = fake_herdr(
            r#"echo '{"error":{"code":"agent_not_found","message":"agent target x not found"},"id":"cli:agent:read"}' >&2; exit 1"#,
        );
        let e = cli.read("x", 80).await.unwrap_err();
        assert_eq!(e.code(), Some("agent_not_found"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
