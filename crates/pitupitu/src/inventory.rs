//! What this box has, for the cloud (box link protocol 4): how the box
//! agent and herdr were installed and whether newer releases exist, the
//! kinds herdr can start, which agent CLIs are on `PATH`, and the project
//! folders agents worked in recently.
//!
//! Recent folders come from the agents' own session history, the logs
//! `logs.rs` lists (Claude Code, Codex, Kiro, pi, Qwen Code, Cline,
//! OpenCode, Kilo Code). Only the working directory of each session and
//! when it was last written are read and sent; nothing else from those
//! files leaves the box. Their credentials are never read: whether an
//! agent is logged in is not checked at all.

use crate::herdr_box::HerdrInfo;
use crate::logs;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Recent folders sent, newest first.
const RECENT_MAX: usize = 30;
/// Session files looked at, newest first.
const FILES_MAX: usize = 300;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Available {
    pub kind: String,
    pub version: String,
    /// The command herdr runs for the kind (`kiro-cli` for `kiro`).
    pub command: String,
    /// From the harness's lister; `None` when it cannot say.
    pub signed_in: Option<bool>,
    /// From the harness's lister; `None` when not listed.
    pub models: Option<Vec<crate::harness::Model>>,
    pub models_at: Option<String>,
    pub models_error: Option<String>,
}

/// `a` with what its lister last said.
pub fn with_listed(mut a: Available) -> Available {
    if let Some(l) = crate::harness::listed(&a.kind) {
        a.signed_in = l.signed_in;
        a.models = l.models;
        a.models_at = l.at;
        a.models_error = l.error;
    }
    a
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Recent {
    pub path: PathBuf,
    pub agent: String,
    /// RFC 3339, UTC.
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Inventory {
    /// How this box agent was installed: `installer` or `brew`.
    pub install: &'static str,
    /// A newer release than this box runs, when GitHub said so.
    pub latest: Option<String>,
    /// herdr's version, server and updates; `None` when it is not
    /// installed.
    pub herdr: Option<HerdrInfo>,
    /// The agent kinds herdr can start; empty while it does not answer.
    pub kinds: Vec<String>,
    pub available: Vec<Available>,
    pub recent: Vec<Recent>,
}

/// The harnesses on `PATH` (every profile's command, `docs/box-link.md`,
/// "Harnesses"), each with what `--version` printed and what its lister
/// last said.
pub async fn available() -> Vec<Available> {
    let checks = crate::harness::profiles().into_iter().map(|p| async move {
        if !crate::harness::command_ok(&p.command) {
            return None;
        }
        let run = tokio::process::Command::new(&p.command)
            .arg("--version")
            .env("NO_COLOR", "1")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .output();
        match tokio::time::timeout(Duration::from_secs(5), run).await {
            Ok(Ok(o)) if o.status.success() => {
                let text = String::from_utf8_lossy(&o.stdout);
                Some(with_listed(Available {
                    kind: p.kind.clone(),
                    version: text.lines().next().unwrap_or("").trim().to_string(),
                    command: p.command.clone(),
                    signed_in: None,
                    models: None,
                    models_at: None,
                    models_error: None,
                }))
            }
            _ => None,
        }
    });
    futures_util::future::join_all(checks)
        .await
        .into_iter()
        .flatten()
        .collect()
}

/// Recent project folders from the agents' session history under `home`
/// (the user's home directory), newest first, folders that still exist.
pub fn recent(home: &Path) -> Vec<Recent> {
    let mut files = logs::list(home);
    files.sort_by_key(|f| std::cmp::Reverse(f.modified));
    files.truncate(FILES_MAX);

    let mut seen: HashMap<PathBuf, ()> = HashMap::new();
    let mut out = Vec::new();
    for log in files {
        let Some(cwd) = logs::cwd_of(&log) else {
            continue;
        };
        if seen.insert(cwd.clone(), ()).is_some() || !cwd.is_dir() {
            continue;
        }
        out.push(Recent {
            path: cwd,
            agent: log.agent.to_string(),
            at: rfc3339(log.modified),
        });
        if out.len() == RECENT_MAX {
            break;
        }
    }
    out
}

/// A time as `YYYY-MM-DDTHH:MM:SSZ`, without a date crate.
pub(crate) fn rfc3339(t: SystemTime) -> String {
    let secs = t
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's days-to-civil.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_are_rfc3339() {
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_792_281_600);
        assert_eq!(rfc3339(t), "2026-10-18T00:00:00Z");
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(951_827_696);
        assert_eq!(rfc3339(t), "2000-02-29T12:34:56Z");
        assert_eq!(rfc3339(SystemTime::UNIX_EPOCH), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn recent_folders_come_from_both_agents_histories() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        let app = h.join("code/app");
        let api = h.join("code/api");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::create_dir_all(&api).unwrap();
        let claude = h.join(".claude/projects/-code-app");
        std::fs::create_dir_all(&claude).unwrap();
        std::fs::write(
            claude.join("s1.jsonl"),
            format!(
                "{{\"type\":\"summary\"}}\n{{\"type\":\"user\",\"cwd\":\"{}\",\"message\":\"secret prompt\"}}\n",
                app.display()
            ),
        )
        .unwrap();
        let codex = h.join(".codex/sessions/2026/10/17");
        std::fs::create_dir_all(&codex).unwrap();
        std::fs::write(
            codex.join("rollout-1.jsonl"),
            format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"cwd\":\"{}\"}}}}\n",
                api.display()
            ),
        )
        .unwrap();
        // A session in a folder that is gone is left out.
        std::fs::write(claude.join("s2.jsonl"), "{\"cwd\":\"/no/such/folder\"}\n").unwrap();

        let r = recent(h);
        let paths: Vec<&PathBuf> = r.iter().map(|x| &x.path).collect();
        assert_eq!(r.len(), 2, "{r:?}");
        assert!(paths.contains(&&app) && paths.contains(&&api));
        assert_eq!(r.iter().find(|x| x.path == api).unwrap().agent, "codex");
        // Only the folder, the agent and the time: no prompt text.
        let sent = serde_json::to_string(&r).unwrap();
        assert!(!sent.contains("secret"));
    }
}
