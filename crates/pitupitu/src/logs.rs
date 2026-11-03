//! Where each agent keeps its session logs, listed without being read:
//! the session reader (`history.rs`), the inventory's `recent` folders and
//! `last_message` all start here.
//!
//! - Claude Code: `~/.claude/projects/*/<id>.jsonl` (the subagents' files
//!   one level further down are not among them).
//! - Codex: `~/.codex/sessions/YYYY/MM/DD/rollout-…-<id>.jsonl`.
//! - Kiro CLI: `~/.kiro/sessions/<hash>/sess_<id>/messages.jsonl`, beside
//!   its `session.json`.
//! - pi: `~/.pi/agent/sessions/--<folder>--/<time>_<id>.jsonl`.
//! - Qwen Code: `~/.qwen/projects/<folder>/chats/<id>.jsonl`.
//! - Cline: `~/.cline/data/sessions/<id>/<id>.messages.json`, beside its
//!   `<id>.json`.
//! - OpenCode and Kilo Code: one SQLite database each,
//!   `~/.local/share/opencode/opencode.db` and `~/.local/share/kilo/kilo.db`,
//!   opened read-only. Only their `session`, `message` and `part` tables are
//!   queried, never another table (those databases also keep the
//!   harnesses' accounts and credentials). A session there is named
//!   `<database>#<session id>`.
//!
//! Nothing else under those folders is opened: not the agents' settings,
//! their credentials, or their process registries.

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime};

/// Database sessions listed per database, the newest.
const DB_SESSIONS_MAX: usize = 300;
/// Lines of a session file read looking for its working directory.
const LINES_MAX: usize = 25;
/// A metadata file larger than this is not read (Cline's `<id>.json`
/// carries the system prompt; a few hundred KB at most).
const META_MAX: u64 = 8 * 1024 * 1024;
/// How long a database query waits for a writer before giving up.
const DB_BUSY: Duration = Duration::from_millis(250);

/// One session's log.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LogRef {
    /// `claude`, `codex`, `kiro`, `pi`, `qwen`, `cline`, `opencode` or
    /// `kilo`: herdr's name for the agent.
    pub agent: &'static str,
    /// The file, or `<database>#<session id>` for a database session.
    pub path: PathBuf,
    /// When it was last written.
    pub modified: SystemTime,
    /// Its size; for a database session, the size of its parts.
    pub size: u64,
    /// The session's folder, when the listing already knows it.
    pub cwd: Option<PathBuf>,
}

/// The session logs of every agent the box reads, unordered.
pub(crate) fn list(home: &Path) -> Vec<LogRef> {
    let mut logs = Vec::new();
    // Claude Code: one folder per project, one file per session.
    for dir in dirs_in(&home.join(".claude/projects")) {
        for f in files_in(&dir) {
            push_file(&mut logs, f, "jsonl", "claude");
        }
    }
    // Codex: sessions/YYYY/MM/DD/rollout-….jsonl.
    walk(&home.join(".codex/sessions"), 4, &mut |p| {
        push_file(&mut logs, p, "jsonl", "codex")
    });
    // Kiro: sessions/<hash of the folder>/sess_<id>/messages.jsonl.
    for dir in dirs_in(&home.join(".kiro/sessions")) {
        for s in dirs_in(&dir) {
            push_file(&mut logs, s.join("messages.jsonl"), "jsonl", "kiro");
        }
    }
    // pi: sessions/--<folder>--/<time>_<id>.jsonl.
    for dir in dirs_in(&home.join(".pi/agent/sessions")) {
        for f in files_in(&dir) {
            push_file(&mut logs, f, "jsonl", "pi");
        }
    }
    // Qwen Code: projects/<folder>/chats/<id>.jsonl.
    for dir in dirs_in(&home.join(".qwen/projects")) {
        for f in files_in(&dir.join("chats")) {
            push_file(&mut logs, f, "jsonl", "qwen");
        }
    }
    // Cline: data/sessions/<id>/<id>.messages.json.
    for dir in dirs_in(&home.join(".cline/data/sessions")) {
        if let Some(id) = dir.file_name().map(|n| n.to_string_lossy().into_owned()) {
            push_file(
                &mut logs,
                dir.join(format!("{id}.messages.json")),
                "json",
                "cline",
            );
        }
    }
    for (agent, db) in databases(home) {
        logs.extend(db_sessions(&db, agent));
    }
    logs
}

/// OpenCode's and Kilo Code's databases under `home`.
pub(crate) fn databases(home: &Path) -> [(&'static str, PathBuf); 2] {
    let data = home.join(".local/share");
    [
        ("opencode", data.join("opencode/opencode.db")),
        ("kilo", data.join("kilo/kilo.db")),
    ]
}

fn dirs_in(dir: &Path) -> impl Iterator<Item = PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
}

fn files_in(dir: &Path) -> impl Iterator<Item = PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
}

fn push_file(logs: &mut Vec<LogRef>, p: PathBuf, ext: &str, agent: &'static str) {
    if !p.extension().is_some_and(|x| x == ext) {
        return;
    }
    let Ok(m) = p.metadata() else {
        return;
    };
    if !m.is_file() {
        return;
    }
    if let Ok(modified) = m.modified() {
        logs.push(LogRef {
            agent,
            size: m.len(),
            path: p,
            modified,
            cwd: None,
        });
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

/// The folder a session worked in. Of the session's own files only what
/// names the folder is kept: a `cwd` in the first lines of a JSONL log,
/// Kiro's `session.json` `workspacePaths`, Cline's `<id>.json` `cwd`.
pub(crate) fn cwd_of(log: &LogRef) -> Option<PathBuf> {
    if let Some(c) = &log.cwd {
        return Some(c.clone());
    }
    let abs = |c: &str| c.starts_with('/').then(|| PathBuf::from(c));
    match log.agent {
        "kiro" => {
            let meta = kiro_meta(&log.path)?;
            meta.cwd.as_deref().and_then(abs)
        }
        "cline" => {
            let meta = cline_meta(&log.path)?;
            meta.cwd.as_deref().and_then(abs)
        }
        _ => {
            let reader = BufReader::new(std::fs::File::open(&log.path).ok()?);
            for line in reader.lines().take(LINES_MAX) {
                let line = line.ok()?;
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let cwd = v
                    .get("cwd")
                    .or_else(|| v.get("payload").and_then(|p| p.get("cwd")))
                    .and_then(Value::as_str);
                if let Some(c) = cwd.and_then(abs) {
                    return Some(c);
                }
            }
            None
        }
    }
}

/// What a session's metadata file says about it.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Meta {
    pub id: Option<String>,
    pub cwd: Option<String>,
    pub title: Option<String>,
}

fn read_json(path: &Path) -> Option<Value> {
    if path.metadata().ok()?.len() > META_MAX {
        return None;
    }
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Kiro's `session.json` beside `messages.jsonl`: `id`, the first of
/// `workspacePaths` (else `rootPaths`), `title`.
pub(crate) fn kiro_meta(messages: &Path) -> Option<Meta> {
    let v = read_json(&messages.with_file_name("session.json"))?;
    let first = |k: &str| v[k].as_array()?.first()?.as_str().map(str::to_string);
    Some(Meta {
        id: v["id"].as_str().map(str::to_string),
        cwd: first("workspacePaths").or_else(|| first("rootPaths")),
        title: v["title"].as_str().map(str::to_string),
    })
}

/// Cline's `<id>.json` beside `<id>.messages.json`: `session_id`, `cwd`,
/// `metadata.title`. Its prompt and system prompt are not kept.
pub(crate) fn cline_meta(messages: &Path) -> Option<Meta> {
    let name = messages.file_name()?.to_str()?;
    let id = name.strip_suffix(".messages.json")?;
    let v = read_json(&messages.with_file_name(format!("{id}.json")))?;
    Some(Meta {
        id: v["session_id"].as_str().map(str::to_string),
        cwd: v["cwd"].as_str().map(str::to_string),
        title: v["metadata"]["title"].as_str().map(str::to_string),
    })
}

// ---------------------------------------------------------------------
// OpenCode's and Kilo Code's databases.

/// A database session's name: `<database>#<session id>`.
pub(crate) fn db_path(db: &Path, id: &str) -> PathBuf {
    let mut s = db.as_os_str().to_os_string();
    s.push("#");
    s.push(id);
    PathBuf::from(s)
}

/// The database and the session id of a database session's name.
pub(crate) fn split_db_path(path: &Path) -> Option<(PathBuf, String)> {
    let s = path.to_str()?;
    let (db, id) = s.rsplit_once('#')?;
    Some((PathBuf::from(db), id.to_string()))
}

/// Read-only, waiting at most a moment for a writer: the harness is never
/// held up. WAL databases are read with what the WAL holds.
fn open_db(db: &Path) -> rusqlite::Result<Connection> {
    let c = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    c.busy_timeout(DB_BUSY)?;
    Ok(c)
}

/// The database's and its WAL's size and time: when neither changed, the
/// database was not written.
fn db_stamp(db: &Path) -> Option<Vec<(u64, SystemTime)>> {
    let stamp = |p: &Path| {
        p.metadata()
            .ok()
            .and_then(|m| Some((m.len(), m.modified().ok()?)))
    };
    let main = stamp(db)?;
    let mut wal = db.as_os_str().to_os_string();
    wal.push("-wal");
    Some(vec![
        main,
        stamp(Path::new(&wal)).unwrap_or((0, SystemTime::UNIX_EPOCH)),
    ])
}

/// The listing of each database, kept while the database is unchanged.
type Listing = (Vec<(u64, SystemTime)>, Vec<LogRef>);
static LISTED: LazyLock<Mutex<HashMap<PathBuf, Listing>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The newest main sessions of a database (not subagents' sessions, not
/// archived ones), queried again only when the database or its WAL
/// changed.
fn db_sessions(db: &Path, agent: &'static str) -> Vec<LogRef> {
    let Some(stamp) = db_stamp(db) else {
        return Vec::new();
    };
    let mut listed = LISTED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((s, logs)) = listed.get(db) {
        if *s == stamp {
            return logs.clone();
        }
    }
    let logs = match query_sessions(db, agent) {
        Ok(l) => l,
        Err(e) => {
            tracing::debug!("listing a session database: {e}");
            return Vec::new();
        }
    };
    listed.insert(db.to_path_buf(), (stamp, logs.clone()));
    logs
}

fn query_sessions(db: &Path, agent: &'static str) -> rusqlite::Result<Vec<LogRef>> {
    let c = open_db(db)?;
    let mut q = c.prepare(
        "SELECT s.id, s.directory, s.time_updated,
                (SELECT COALESCE(MAX(p.time_updated), 0) FROM part p WHERE p.session_id = s.id),
                (SELECT COALESCE(SUM(LENGTH(p.data)), 0) FROM part p WHERE p.session_id = s.id)
           FROM session s
          WHERE s.parent_id IS NULL AND s.time_archived IS NULL
          ORDER BY s.time_updated DESC
          LIMIT ?1",
    )?;
    let rows = q.query_map([DB_SESSIONS_MAX as i64], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<i64>>(2)?.unwrap_or(0),
            r.get::<_, Option<i64>>(3)?.unwrap_or(0),
            r.get::<_, Option<i64>>(4)?.unwrap_or(0),
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, dir, updated, part_updated, size) = row?;
        let ms = updated.max(part_updated).max(0) as u64;
        out.push(LogRef {
            agent,
            path: db_path(db, &id),
            modified: SystemTime::UNIX_EPOCH + Duration::from_millis(ms),
            size: size.max(0) as u64,
            cwd: dir.filter(|d| d.starts_with('/')).map(PathBuf::from),
        });
    }
    Ok(out)
}

/// One part of a database session, with only what the reader may look at:
/// never a tool's output, a reasoning part's text or a file's contents.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DbPart {
    /// The message's role: `user` or `assistant`.
    pub role: String,
    /// When the part was written, ms since the epoch.
    pub at: u64,
    /// `text`, `tool`, …
    pub kind: String,
    /// A text part's text (not a reasoning part's).
    pub text: Option<String>,
    /// Added by the harness, not typed or said (`synthetic`, `ignored`).
    pub added: bool,
    /// A tool part's tool.
    pub tool: Option<String>,
    /// A tool part's input.
    pub input: Value,
}

/// A database session: its folder, title and start, and its parts in
/// order.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct DbSession {
    pub cwd: Option<String>,
    pub title: Option<String>,
    pub created: u64,
    pub parts: Vec<DbPart>,
}

/// Read one session of a database. Only the fields named here are taken
/// out of the parts' JSON; tool output stays in the database.
pub(crate) fn db_session(db: &Path, id: &str) -> rusqlite::Result<Option<DbSession>> {
    let c = open_db(db)?;
    let head = c
        .query_row(
            "SELECT directory, title, time_created FROM session
              WHERE id = ?1 AND parent_id IS NULL",
            [id],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                ))
            },
        )
        .optional()?;
    let Some((cwd, title, created)) = head else {
        return Ok(None);
    };
    let mut q = c.prepare(
        "SELECT json_extract(m.data, '$.role'),
                p.time_created,
                json_extract(p.data, '$.type'),
                CASE WHEN json_extract(p.data, '$.type') = 'text'
                     THEN json_extract(p.data, '$.text') END,
                COALESCE(json_extract(p.data, '$.synthetic'), 0)
                  OR COALESCE(json_extract(p.data, '$.ignored'), 0),
                json_extract(p.data, '$.tool'),
                CASE WHEN json_extract(p.data, '$.type') = 'tool'
                     THEN json_extract(p.data, '$.state.input') END
           FROM message m JOIN part p ON p.message_id = m.id
          WHERE m.session_id = ?1
          ORDER BY m.time_created, m.id, p.id",
    )?;
    let rows = q.query_map([id], |r| {
        Ok(DbPart {
            role: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
            at: r.get::<_, Option<i64>>(1)?.unwrap_or(0).max(0) as u64,
            kind: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            text: r.get(3)?,
            added: r.get::<_, Option<bool>>(4)?.unwrap_or(false),
            tool: r.get(5)?,
            input: r
                .get::<_, Option<String>>(6)?
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or(Value::Null),
        })
    })?;
    let parts = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Some(DbSession {
        cwd,
        title,
        created: created.max(0) as u64,
        parts,
    }))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A made-up OpenCode database: the three tables the box reads, and
    /// one it must never read.
    pub(crate) fn make_db(path: &Path) -> Connection {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let c = Connection::open(path).unwrap();
        c.execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY, project_id TEXT, parent_id TEXT,
                directory TEXT, title TEXT, time_created INTEGER, time_updated INTEGER,
                time_archived INTEGER);
             CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER,
                time_updated INTEGER, data TEXT);
             CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT,
                time_created INTEGER, time_updated INTEGER, data TEXT);
             CREATE TABLE credential (id TEXT, secret TEXT);
             INSERT INTO credential VALUES ('c', 'sk-NEVER-READ-THIS-CREDENTIAL');",
        )
        .unwrap();
        c
    }

    #[test]
    fn database_session_names_round_trip() {
        let p = db_path(Path::new("/h/.local/share/opencode/opencode.db"), "ses_1");
        assert_eq!(
            p,
            PathBuf::from("/h/.local/share/opencode/opencode.db#ses_1")
        );
        assert_eq!(
            split_db_path(&p),
            Some((
                PathBuf::from("/h/.local/share/opencode/opencode.db"),
                "ses_1".to_string()
            ))
        );
    }

    #[test]
    fn every_agents_logs_are_listed() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        let put = |rel: &str, text: &str| {
            let p = h.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, text).unwrap();
        };
        put(".claude/projects/-w/a.jsonl", "{}\n");
        put(".codex/sessions/2026/10/01/rollout-x-b.jsonl", "{}\n");
        put(
            ".kiro/sessions/0123456789abcdef/sess_c/messages.jsonl",
            "{}\n",
        );
        put(".kiro/sessions/0123456789abcdef/sess_c/session.json", "{}");
        put(".kiro/sessions/cli/sess_c.history", "typed\n");
        put(
            ".pi/agent/sessions/--w--/2026-10-01T00-00-00-000Z_d.jsonl",
            "{}\n",
        );
        put(".pi/agent/auth.json", "{}");
        put(".qwen/projects/-w/chats/e.jsonl", "{}\n");
        put(".qwen/projects/-w/chats/e.runtime.json", "{}");
        put(".qwen/settings.json", "{}");
        put(".cline/data/sessions/1_f/1_f.messages.json", "{}");
        put(".cline/data/sessions/1_f/1_f.json", "{}");
        put(".cline/data/settings/providers.json", "{}");
        let c = make_db(&h.join(".local/share/opencode/opencode.db"));
        c.execute_batch(
            "INSERT INTO session VALUES ('ses_g', 'p', NULL, '/w', 'T', 1, 2, NULL);
             INSERT INTO session VALUES ('ses_sub', 'p', 'ses_g', '/w', 'T', 1, 2, NULL);
             INSERT INTO session VALUES ('ses_old', 'p', NULL, '/w', 'T', 1, 2, 3);",
        )
        .unwrap();
        drop(c);
        let mut got: Vec<(&str, String)> = list(h)
            .into_iter()
            .map(|l| {
                let name = l.path.file_name().unwrap().to_string_lossy().into_owned();
                (l.agent, name)
            })
            .collect();
        got.sort();
        assert_eq!(
            got,
            [
                ("claude", "a.jsonl".to_string()),
                ("cline", "1_f.messages.json".to_string()),
                ("codex", "rollout-x-b.jsonl".to_string()),
                ("kiro", "messages.jsonl".to_string()),
                ("opencode", "opencode.db#ses_g".to_string()),
                ("pi", "2026-10-01T00-00-00-000Z_d.jsonl".to_string()),
                ("qwen", "e.jsonl".to_string()),
            ]
        );
    }
}
