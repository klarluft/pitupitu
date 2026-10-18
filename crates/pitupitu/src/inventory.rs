//! What this box has, for the cloud (box link protocol 2): which agent
//! CLIs are on `PATH`, and the project folders agents worked in recently.
//!
//! Recent folders come from the agents' own session history: Claude Code's
//! `~/.claude/projects/*/*.jsonl`, Codex's `~/.codex/sessions/**/*.jsonl`
//! and Gemini CLI's `~/.gemini/projects.json` with the chats it keeps per
//! project. Only the working directory of each session and when it was
//! last written are read and sent; nothing else from those files leaves
//! the box. Their
//! credentials are never read: whether an agent is logged in is not
//! checked at all.

use serde::Serialize;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Agent CLIs looked for on `PATH`.
const KINDS: &[&str] = &["claude", "codex", "gemini", "agy", "opencode"];
/// Recent folders sent, newest first.
const RECENT_MAX: usize = 30;
/// Session files looked at, newest first.
const FILES_MAX: usize = 300;
/// Lines of a session file read looking for its working directory.
const LINES_MAX: usize = 25;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Available {
    pub kind: String,
    pub version: String,
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
    /// What serves the calls: `herdr` (the terminal host comes later).
    pub driver: String,
    /// herdr's version; empty when it is not there.
    pub herdr: String,
    pub available: Vec<Available>,
    pub recent: Vec<Recent>,
}

/// The agent CLIs on `PATH`, each with what `--version` printed.
pub async fn available() -> Vec<Available> {
    let mut out = Vec::new();
    for kind in KINDS {
        let run = tokio::process::Command::new(kind)
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output();
        if let Ok(Ok(o)) = tokio::time::timeout(Duration::from_secs(5), run).await {
            if o.status.success() {
                let text = String::from_utf8_lossy(&o.stdout);
                out.push(Available {
                    kind: kind.to_string(),
                    version: text.lines().next().unwrap_or("").trim().to_string(),
                });
            }
        }
    }
    out
}

/// Recent project folders from the agents' session history under `home`
/// (the user's home directory), newest first, folders that still exist.
pub fn recent(home: &Path) -> Vec<Recent> {
    let mut files: Vec<(SystemTime, PathBuf, &str)> = Vec::new();
    // Claude Code: one folder per project, one file per session.
    if let Ok(projects) = std::fs::read_dir(home.join(".claude/projects")) {
        for dir in projects.flatten() {
            if let Ok(sessions) = std::fs::read_dir(dir.path()) {
                for f in sessions.flatten() {
                    push_jsonl(&mut files, f.path(), "claude");
                }
            }
        }
    }
    // Codex: sessions/YYYY/MM/DD/rollout-….jsonl.
    walk(&home.join(".codex/sessions"), 4, &mut |p| {
        push_jsonl(&mut files, p, "codex")
    });
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    files.truncate(FILES_MAX);
    // Gemini CLI: its chats do not record the folder; its project list
    // names each project's folder under ~/.gemini/tmp.
    let mut sessions: Vec<(SystemTime, Session, &str)> = files
        .into_iter()
        .map(|(t, f, agent)| (t, Session::File(f), agent))
        .collect();
    for (t, cwd) in gemini_projects(home) {
        sessions.push((t, Session::Folder(cwd), "gemini"));
    }
    sessions.sort_by_key(|f| std::cmp::Reverse(f.0));

    let mut seen: HashMap<PathBuf, ()> = HashMap::new();
    let mut out = Vec::new();
    for (modified, session, agent) in sessions {
        let cwd = match session {
            Session::Folder(cwd) => cwd,
            Session::File(file) => {
                let Some(cwd) = session_cwd(&file) else {
                    continue;
                };
                cwd
            }
        };
        if seen.insert(cwd.clone(), ()).is_some() || !cwd.is_dir() {
            continue;
        }
        out.push(Recent {
            path: cwd,
            agent: agent.to_string(),
            at: rfc3339(modified),
        });
        if out.len() == RECENT_MAX {
            break;
        }
    }
    out
}

enum Session {
    /// A session file that records its working directory.
    File(PathBuf),
    /// A project folder, known some other way.
    Folder(PathBuf),
}

/// Gemini CLI's projects (`~/.gemini/projects.json`: folder → name), each
/// with when its newest chat in `~/.gemini/tmp/<name>/chats` was written.
fn gemini_projects(home: &Path) -> Vec<(SystemTime, PathBuf)> {
    let Ok(text) = std::fs::read_to_string(home.join(".gemini/projects.json")) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (folder, name) in v["projects"].as_object().into_iter().flatten() {
        let Some(name) = name.as_str() else {
            continue;
        };
        if !folder.starts_with('/') || name.contains('/') || name.starts_with('.') {
            continue;
        }
        let chats = home.join(".gemini/tmp").join(name).join("chats");
        let newest = std::fs::read_dir(chats)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|f| {
                f.path()
                    .extension()
                    .is_some_and(|x| x == "jsonl" || x == "json")
            })
            .filter_map(|f| f.metadata().and_then(|m| m.modified()).ok())
            .max();
        if let Some(t) = newest {
            out.push((t, PathBuf::from(folder)));
        }
    }
    out
}

fn push_jsonl(
    files: &mut Vec<(SystemTime, PathBuf, &'static str)>,
    p: PathBuf,
    agent: &'static str,
) {
    if p.extension().is_some_and(|x| x == "jsonl") {
        if let Ok(m) = p.metadata().and_then(|m| m.modified()) {
            files.push((m, p, agent));
        }
    }
}

fn walk(dir: &Path, depth: u32, f: &mut dyn FnMut(PathBuf)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if depth > 0 {
                walk(&p, depth - 1, f);
            }
        } else {
            f(p);
        }
    }
}

/// The working directory a session file records: a top-level `cwd`
/// (Claude Code) or `payload.cwd` (Codex's `session_meta`), in its first
/// lines. Nothing else of the file is kept.
fn session_cwd(file: &Path) -> Option<PathBuf> {
    let reader = BufReader::new(std::fs::File::open(file).ok()?);
    for line in reader.lines().take(LINES_MAX) {
        let line = line.ok()?;
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let cwd = v
            .get("cwd")
            .or_else(|| v.get("payload").and_then(|p| p.get("cwd")))
            .and_then(|c| c.as_str());
        if let Some(c) = cwd.filter(|c| c.starts_with('/')) {
            return Some(PathBuf::from(c));
        }
    }
    None
}

/// A time as `YYYY-MM-DDTHH:MM:SSZ`, without a date crate.
fn rfc3339(t: SystemTime) -> String {
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
    fn recent_folders_come_from_the_agents_histories() {
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
        // Gemini CLI: the project list, and a chat in the project's folder.
        let web = h.join("code/web");
        std::fs::create_dir_all(&web).unwrap();
        let chats = h.join(".gemini/tmp/web/chats");
        std::fs::create_dir_all(&chats).unwrap();
        std::fs::write(
            chats.join("session-2026-10-18T10-00-abcd1234.jsonl"),
            "{\"sessionId\":\"abcd1234\"}\n{\"type\":\"user\",\"content\":\"secret\"}\n",
        )
        .unwrap();
        std::fs::write(
            h.join(".gemini/projects.json"),
            format!(
                "{{\"projects\":{{\"{}\":\"web\",\"/no/such/folder\":\"gone\",\"{}\":\"../x\"}}}}",
                web.display(),
                app.display()
            ),
        )
        .unwrap();

        let r = recent(h);
        let paths: Vec<&PathBuf> = r.iter().map(|x| &x.path).collect();
        assert_eq!(r.len(), 3, "{r:?}");
        assert!(paths.contains(&&app) && paths.contains(&&api) && paths.contains(&&web));
        assert_eq!(r.iter().find(|x| x.path == api).unwrap().agent, "codex");
        assert_eq!(r.iter().find(|x| x.path == web).unwrap().agent, "gemini");
        // Only the folder, the agent and the time: no prompt text.
        let sent = serde_json::to_string(&r).unwrap();
        assert!(!sent.contains("secret"));
    }
}
