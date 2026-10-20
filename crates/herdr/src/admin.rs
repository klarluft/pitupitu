//! herdr itself rather than its panes: which version is installed, whether
//! its server answers, starting one, updating herdr and installing its
//! agent integrations.
//!
//! herdr starts no server on its own: with none running, every API call
//! fails with `server_not_running`. The box starts `herdr server` in a
//! session of its own and never stops one; whoever is first wins, so a
//! herdr the person opened is used as it is, and one the box started is
//! what `herdr` attaches to later.

use crate::{Error, HerdrCli, Result};
use serde_json::Value;
use std::path::PathBuf;
use std::process::Stdio;

/// The error code of any API call while no server runs.
pub const SERVER_NOT_RUNNING: &str = "server_not_running";

/// What `herdr status` says about the installed binary and the server.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Status {
    /// The installed (client) version, e.g. `0.9.3`.
    pub version: String,
    /// The client's socket protocol, e.g. `22`.
    pub protocol: Option<u64>,
    /// Where the binary is, when herdr says.
    pub binary: Option<PathBuf>,
    /// Whether a server answers on the session's socket.
    pub running: bool,
    /// The running server is older than the binary (after an update): it
    /// keeps working, new server-side features wait for a restart.
    pub restart_needed: bool,
}

impl HerdrCli {
    /// `herdr status`: the JSON form when this herdr has it, else its text.
    pub async fn status(&self) -> Result<Status> {
        let (ok, stdout, stderr) = self.exec(&["status", "--json"]).await?;
        if ok.success() {
            if let Some(s) = parse_status_json(&stdout) {
                return Ok(s);
            }
        }
        let (ok, stdout, stderr2) = self.exec(&["status"]).await?;
        match parse_status_text(&stdout) {
            Some(s) if ok.success() || !s.version.is_empty() => Ok(s),
            _ => Err(Error::Exit {
                status: ok.code().unwrap_or(-1),
                stderr: format!("{}{}", stderr.trim(), stderr2.trim()),
            }),
        }
    }

    /// Start `herdr server` detached: a session and process group of its
    /// own, no terminal, its output dropped (herdr keeps its own logs),
    /// never waited on by the caller. Stopping or restarting the box agent
    /// does not stop it; nothing here ever does.
    pub fn start_server(&self) -> Result<()> {
        let mut cmd = std::process::Command::new(&self.bin);
        if let Some(s) = &self.session {
            cmd.args(["--session", s]);
        }
        cmd.arg("server")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // SAFETY: setsid is async-signal-safe and touches no memory
            // of the parent.
            unsafe {
                cmd.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let mut child = cmd.spawn().map_err(Error::Spawn)?;
        // Reaped if it ever exits while the box agent runs; once the box
        // agent exits, it belongs to init like any daemon.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }

    /// `herdr update`, with `--handoff` to restart the server with its
    /// panes kept (best effort). Without it the running server and its
    /// panes stay as they are. The error carries the command's last line.
    pub async fn update(&self, handoff: bool) -> Result<String> {
        let mut args = vec!["update"];
        if handoff {
            args.push("--handoff");
        }
        self.checked(&args).await
    }

    /// `herdr integration install <kind>`: herdr's own hooks for that
    /// agent, so it resumes the agent's sessions after a server restart.
    pub async fn integration_install(&self, kind: &str) -> Result<String> {
        self.checked(&["integration", "install", kind]).await
    }

    /// Run a command for its text; a failure carries its last line.
    async fn checked(&self, args: &[&str]) -> Result<String> {
        let (status, stdout, stderr) = self.exec(args).await?;
        if !status.success() {
            return Err(Error::Exit {
                status: status.code().unwrap_or(-1),
                stderr: last_line(&stderr)
                    .or_else(|| last_line(&stdout))
                    .unwrap_or_default(),
            });
        }
        Ok(format!("{stdout}{stderr}"))
    }
}

pub(crate) fn last_line(text: &str) -> Option<String> {
    text.lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

fn parse_status_json(text: &str) -> Option<Status> {
    let v: Value = serde_json::from_str(text.trim()).ok()?;
    let client = v.get("client")?;
    let server = v.get("server");
    let flag = |section: Option<&Value>, key: &str| {
        section
            .and_then(|s| s.get(key))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    Some(Status {
        version: client.get("version")?.as_str()?.to_string(),
        protocol: client.get("protocol").and_then(Value::as_u64),
        binary: client
            .get("binary")
            .and_then(Value::as_str)
            .map(PathBuf::from),
        running: flag(server, "running"),
        restart_needed: flag(v.get("update"), "restart_needed") || flag(server, "restart_needed"),
    })
}

/// The text form, sections of `key: value` lines:
/// `client:` (`version`, `protocol`), `server:` (`status: running`),
/// `update:` (`restart_needed: yes`).
fn parse_status_text(text: &str) -> Option<Status> {
    let mut s = Status::default();
    let mut section = "";
    for line in text.lines() {
        let indented = line.starts_with(' ') || line.starts_with('\t');
        let Some((key, value)) = line.trim().split_once(':') else {
            continue;
        };
        let value = value.trim();
        if !indented {
            section = key.trim();
            continue;
        }
        match (section, key.trim()) {
            ("client", "version") => s.version = value.to_string(),
            ("client", "protocol") => s.protocol = value.parse().ok(),
            ("client", "binary") => s.binary = Some(PathBuf::from(value)),
            ("server", "status") => s.running = value == "running",
            ("update", "restart_needed") => s.restart_needed = value == "yes",
            _ => {}
        }
    }
    (!s.version.is_empty()).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_is_read_from_json() {
        let json = r#"{"client":{"version":"0.9.3","channel":"stable","protocol":22,"binary":"/home/me/.local/bin/herdr","session":null},
            "server":{"status":"running","running":true,"version":"0.9.3","protocol":22,"restart_needed":false},
            "update":{"restart_needed":true,"server_binary_stale":false}}"#;
        let s = parse_status_json(json).unwrap();
        assert_eq!(s.version, "0.9.3");
        assert_eq!(s.protocol, Some(22));
        assert_eq!(s.binary, Some(PathBuf::from("/home/me/.local/bin/herdr")));
        assert!(s.running && s.restart_needed);
        let none = r#"{"client":{"version":"0.9.3","protocol":22},"server":{"status":"not_running","running":false,"version":null},"update":{"restart_needed":false}}"#;
        let s = parse_status_json(none).unwrap();
        assert!(!s.running && !s.restart_needed);
        assert!(parse_status_json("client:\n  version: 1").is_none());
    }

    #[test]
    fn status_is_read_from_text() {
        let text = "client:\n  version: 0.9.3\n  channel: stable\n  protocol: 22\n\nserver:\n  status: running\n  version: 0.9.3\n\nupdate:\n  restart_needed: no\n  server_binary_stale: no\n";
        let s = parse_status_text(text).unwrap();
        assert_eq!((s.version.as_str(), s.protocol), ("0.9.3", Some(22)));
        assert!(s.running && !s.restart_needed);
        let stopped = "client:\n  version: 0.9.5\n  protocol: 23\n\nserver:\n  status: not_running\n\nupdate:\n  restart_needed: yes\n";
        let s = parse_status_text(stopped).unwrap();
        assert!(!s.running && s.restart_needed);
        assert!(parse_status_text("nothing here").is_none());
    }

    #[test]
    fn the_last_line_is_the_one_with_text() {
        assert_eq!(
            last_line("a\nerror: no network\n\n").unwrap(),
            "error: no network"
        );
        assert_eq!(last_line(" \n"), None);
    }
}
