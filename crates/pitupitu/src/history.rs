//! Session history (box link protocol 4): what the person's agents did on
//! this box, read from the agents' own session logs, and the git facts of
//! the repositories they worked in. On unless the person switched it off
//! for this box (`pitupitu box history off`, or on the page); the cloud says
//! which in its `settings` note and the box keeps nothing about the setting
//! itself.
//!
//! Read: Claude Code's `~/.claude/projects/*/*.jsonl` and Codex's
//! `~/.codex/sessions/YYYY/MM/DD/*.jsonl`, the same files `recent` lists.
//! Sent, per session: the folder, the repository it is in, when it ran, a
//! title, and its turns: what the person typed, what the agent answered,
//! and for each tool call its name and one target (a file path, the start
//! of a command). Per repository: the branch, HEAD and the subjects of
//! the branch's recent commits with how many files each touched.
//!
//! Never read or sent: tool results and command output, file contents (an
//! edit's text, a written file, a patch's body), subagent transcripts,
//! anything else under `~/.claude` or `~/.codex`, credentials of any kind.
//! Everything sent is cut short and anything that looks like a secret
//! becomes `[redacted]` first.
//!
//! The reader remembers, in `history.json` under the state directory, the
//! size and time of each log as last sent and each repository's HEAD as
//! last sent, so a restart does not send everything again. That file is
//! removed when history is switched off.

use crate::inventory;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};
use tracing::{debug, info};

/// How often the logs are looked at.
pub const POLL: Duration = Duration::from_secs(10);
/// A session that keeps changing is sent at most this often.
const RESEND_FLOOR: Duration = Duration::from_secs(30);
/// How often the repositories' HEADs are looked at.
const GIT_EVERY: Duration = Duration::from_secs(60);
/// How long a folder's repository is remembered before asking git again.
const GIT_CACHE: Duration = Duration::from_secs(60);
/// The pause after each note: no more than ten a second.
const PACE: Duration = Duration::from_millis(100);
/// Sessions and commits older than this are left alone (7 days).
const WINDOW: u64 = 7 * 86_400;
/// At most this many sessions, the newest.
const SESSIONS_MAX: usize = 300;
/// A log written this recently (5 minutes) is a live session.
const LIVE: u64 = 300;
/// What the person typed, at most.
const YOU_MAX: usize = 1000;
/// What the agent answered, at most.
const AGENT_MAX: usize = 600;
/// A commit subject, at most.
const SUBJECT_MAX: usize = 2000;
const TARGET_MAX: usize = 120;
const TITLE_MAX: usize = 80;
/// One session record, as JSON, at most (32 KB).
const RECORD_MAX: usize = 32 * 1024;
const COMMITS_MAX: usize = 50;
const GIT_TIMEOUT: Duration = Duration::from_secs(10);
const STATE_FILE: &str = "history.json";

/// The cloud's `settings` note: whether to read the logs, and whether to
/// send everything again from the start.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Setting {
    pub history: bool,
    pub resend: bool,
}

impl Setting {
    /// From `{"note": "settings", "history": …, "resend"?: …}`; other
    /// fields are ignored, and a note without `history` (protocol 3's
    /// `driver`) is no setting at all.
    pub fn from_note(note: &Value) -> Option<Setting> {
        Some(Setting {
            history: note.get("history")?.as_bool()?,
            resend: note.get("resend").and_then(Value::as_bool).unwrap_or(false),
        })
    }
}

/// One turn of a session.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Turn {
    /// RFC 3339, as the log wrote it.
    pub at: String,
    #[serde(flatten)]
    pub kind: Kind,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum Kind {
    /// What the person typed.
    You { text: String },
    /// What the agent answered (its text, not its thinking).
    Agent { text: String },
    /// A tool call: its name and one target, never its result.
    Tool {
        name: String,
        target: Option<String>,
    },
}

/// The repository a folder is in.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Git {
    pub root: PathBuf,
    pub remote: Option<String>,
    pub branch: Option<String>,
}

/// A `session` note's record.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Session {
    pub agent: &'static str,
    pub id: String,
    pub cwd: PathBuf,
    pub git: Option<Git>,
    pub started_at: String,
    pub last_at: String,
    pub title: Option<String>,
    pub turns: Vec<Turn>,
    /// How many turns there were before the record was cut to size.
    pub turns_total: usize,
    pub live: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Commit {
    pub sha: String,
    pub at: String,
    pub subject: String,
    pub files: u64,
    pub branch: Option<String>,
}

/// A `git` note's repository.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Repo {
    pub root: PathBuf,
    pub remote: Option<String>,
    pub branch: Option<String>,
    pub head: String,
    pub commits: Vec<Commit>,
}

// ---------------------------------------------------------------------
// Redaction and cutting.

static SECRETS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    let name = r"[A-Za-z0-9_.-]*(?i:password|passwd|secret|token|api[_-]?key|key)";
    let quoted = r#""[^"]*"|'[^']*'"#;
    [
        (
            r"(?i)\b(bearer)\s+[A-Za-z0-9._~+/=-]{8,}".to_string(),
            "$1 [redacted]",
        ),
        (
            format!(r#"\b({name})(\s*=\s*)({quoted}|[^\s"'&,;]+)"#),
            "$1$2[redacted]",
        ),
        (
            format!(r#"\b({name})("?\s*:\s*)({quoted})"#),
            "$1$2[redacted]",
        ),
        (format!(r#"\b({name})(:)([^\s"'&,;]+)"#), "$1$2[redacted]"),
        (r"\bsk-[A-Za-z0-9_-]{8,}".into(), "[redacted]"),
        (r"\bptp_[A-Za-z0-9_-]{6,}".into(), "[redacted]"),
        (r"\bgh[pousr]_[A-Za-z0-9]{8,}".into(), "[redacted]"),
        (r"\bgithub_pat_[A-Za-z0-9_]{8,}".into(), "[redacted]"),
        (r"\bxox[a-z]-[A-Za-z0-9-]{8,}".into(), "[redacted]"),
        (r"\bAKIA[0-9A-Z]{16}\b".into(), "[redacted]"),
    ]
    .into_iter()
    .map(|(re, to)| (Regex::new(&re).expect("secret pattern"), to))
    .collect()
});

/// Runs of base64 or hex characters (and the separators keys carry).
static RUNS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Za-z0-9+/=_.-]{32,}").expect("run pattern"));

/// Anything that looks like a secret becomes `[redacted]`: known key
/// prefixes, `Bearer` tokens, `password=`/`token=`/`key=` values, and runs
/// of 32 or more base64 or hex characters. A long path or a UUID is not
/// such a run: a run counts when a piece of it between separators is 20
/// characters or more and has both letters and digits in it.
pub fn redact(text: &str) -> String {
    let mut s = text.to_string();
    for (re, to) in SECRETS.iter() {
        if re.is_match(&s) {
            s = re.replace_all(&s, *to).into_owned();
        }
    }
    RUNS.replace_all(&s, |c: &regex::Captures| {
        let run = &c[0];
        let keyish = run.split(['/', '-', '_', '.']).any(|piece| {
            piece.len() >= 20
                && piece.bytes().any(|b| b.is_ascii_digit())
                && piece.bytes().any(|b| b.is_ascii_alphabetic())
        });
        if keyish {
            "[redacted]".to_string()
        } else {
            run.to_string()
        }
    })
    .into_owned()
}

/// The first `max` characters of `s`, and `…` when there were more.
fn cut(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

/// Text for a turn: redacted, then cut to `max` characters. A huge paste
/// is shortened before it is looked at, still well past the cut.
fn turn_text(s: &str, max: usize) -> Option<String> {
    let s = cut(s, max * 2);
    let s = redact(s.trim());
    let s = s.trim();
    (!s.is_empty()).then(|| cut(s, max))
}

/// One line, redacted, cut to `max` characters with no `…`.
fn one_line(s: &str, max: usize) -> Option<String> {
    let flat: String = cut(s, max * 4)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let r = redact(&flat);
    let r: String = r.chars().take(max).collect();
    let r = r.trim();
    (!r.is_empty()).then(|| r.to_string())
}

fn title_of(s: &str) -> Option<String> {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let r = redact(&cut(&flat, TITLE_MAX * 4));
    let r = r.trim();
    (!r.is_empty()).then(|| cut(r, TITLE_MAX))
}

// ---------------------------------------------------------------------
// Tool targets.

/// Tools whose target is the file they work on.
const FILE_TOOLS: &[&str] = &["Edit", "Write", "Read", "MultiEdit", "NotebookEdit"];
/// Tools whose target is the command they run.
const SHELL_TOOLS: &[&str] = &[
    "Bash",
    "Monitor",
    "shell",
    "exec_command",
    "local_shell",
    "shell_command",
    "container.exec",
];
/// Arguments that hold file contents or edits: never a target.
const CONTENT_ARGS: &[&str] = &[
    "content",
    "contents",
    "new_string",
    "old_string",
    "new_source",
    "edits",
    "patch",
    "input",
    "body",
    "data",
    "file_text",
];

/// A command as one string: an argv's last element after `-c`/`-lc`
/// (`["bash", "-lc", "cargo test"]`), else its words joined.
fn command_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Array(a) => {
            let words: Vec<&str> = a.iter().filter_map(Value::as_str).collect();
            match words.as_slice() {
                [_, flag, cmd] if flag.starts_with('-') && flag.ends_with('c') => {
                    Some(cmd.to_string())
                }
                w => Some(w.join(" ")),
            }
        }
        _ => None,
    }
}

/// The one thing a tool call is about: the file for file tools, the start
/// of the command for shell tools, else its first string argument.
pub fn target(name: &str, input: &Value) -> Option<String> {
    let path = ["file_path", "notebook_path"]
        .iter()
        .chain(if FILE_TOOLS.contains(&name) {
            ["path"].iter()
        } else {
            [].iter()
        })
        .find_map(|k| input.get(*k).and_then(Value::as_str));
    if let Some(p) = path {
        return one_line(p, TARGET_MAX);
    }
    let shell = SHELL_TOOLS.contains(&name) || input.get("command").is_some();
    if shell {
        if let Some(c) = ["command", "cmd"]
            .iter()
            .find_map(|k| input.get(*k).and_then(command_text))
        {
            return one_line(&c, TARGET_MAX);
        }
    }
    let first = input.as_object().and_then(|o| {
        o.iter()
            .filter(|(k, _)| !CONTENT_ARGS.contains(&k.as_str()))
            .find_map(|(_, v)| v.as_str())
    });
    first.and_then(|s| one_line(s, TARGET_MAX))
}

/// The file a Codex patch touches first (`*** Update File: src/main.rs`);
/// the patch's body is not looked at.
fn patch_target(patch: &str) -> Option<String> {
    patch.lines().find_map(|l| {
        ["*** Update File:", "*** Add File:", "*** Delete File:"]
            .iter()
            .find_map(|p| l.strip_prefix(p))
            .and_then(|p| one_line(p, TARGET_MAX))
    })
}

// ---------------------------------------------------------------------
// Parsing the logs.

static PASTED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"</?pasted_content[^>]*>").expect("paste pattern"));

/// What the person typed, as Claude Code logs it; `None` for what Claude
/// Code itself put in the person's place (command output, notifications,
/// reminders, an interruption).
fn claude_you(text: &str) -> Option<String> {
    let mut s = text.to_string();
    while let (Some(a), Some(b)) = (s.find("<system-reminder>"), s.find("</system-reminder>")) {
        if b < a {
            break;
        }
        s.replace_range(a..b + "</system-reminder>".len(), "");
    }
    // Text pasted in comes wrapped; the wrapper is not what was said.
    let s = PASTED.replace_all(&s, "");
    let t = s.trim();
    const NOT_TYPED: &[&str] = &[
        "<local-command-stdout",
        "<local-command-stderr",
        "<local-command-caveat",
        "<bash-stdout",
        "<bash-stderr",
        "<task-notification",
        "<system-reminder",
        "[Request interrupted",
    ];
    if t.is_empty() || NOT_TYPED.iter().any(|p| t.starts_with(p)) {
        return None;
    }
    let between = |a: &str, b: &str| -> Option<String> {
        let start = t.find(a)? + a.len();
        let end = t[start..].find(b)? + start;
        Some(t[start..end].trim().to_string())
    };
    // A slash command, as `/name args`.
    if let Some(name) = between("<command-name>", "</command-name>") {
        let args = between("<command-args>", "</command-args>").unwrap_or_default();
        return Some(format!("{name} {args}").trim().to_string());
    }
    // A shell command the person ran with `!`; its output is not kept.
    if let Some(cmd) = between("<bash-input>", "</bash-input>") {
        return Some(format!("! {cmd}"));
    }
    Some(t.to_string())
}

/// What the person typed, as Codex logs it; `None` for the context Codex
/// adds as user messages (instructions, environment).
fn codex_you(text: &str) -> Option<String> {
    let t = text.trim();
    const ADDED: &[&str] = &[
        "<environment_context",
        "<user_instructions",
        "<permissions instructions",
        "<turn_aborted",
        "<user_shell_command",
        "<subagent_notification",
        "<collaboration_mode",
        "# AGENTS.md instructions",
    ];
    if t.is_empty() || ADDED.iter().any(|p| t.starts_with(p)) {
        None
    } else {
        Some(t.to_string())
    }
}

/// A session as read so far.
#[derive(Debug)]
struct Parsed {
    agent: &'static str,
    /// Bytes of the log read, up to the last whole line.
    offset: u64,
    id: Option<String>,
    cwd: Option<String>,
    started_at: Option<String>,
    last_at: Option<String>,
    title: Option<String>,
    first_text: Option<String>,
    turns: Vec<Turn>,
    first_you: Option<usize>,
    total: usize,
    bytes: usize,
}

fn turn_size(t: &Turn) -> usize {
    serde_json::to_string(t).map(|s| s.len() + 1).unwrap_or(0)
}

/// Drop the oldest turns until the rest fit in `budget` bytes, keeping
/// the first `you` turn (what the session was for).
fn cap(turns: &mut Vec<Turn>, first_you: &mut Option<usize>, budget: usize) -> usize {
    let sizes: Vec<usize> = turns.iter().map(turn_size).collect();
    let mut total: usize = sizes.iter().sum();
    let mut drop = vec![false; turns.len()];
    for (i, size) in sizes.iter().enumerate() {
        if total <= budget {
            break;
        }
        if Some(i) == *first_you {
            continue;
        }
        drop[i] = true;
        total -= size;
    }
    if let Some(f) = first_you.as_mut() {
        *f -= drop[..*f].iter().filter(|d| **d).count();
    }
    let mut i = 0;
    turns.retain(|_| {
        i += 1;
        !drop[i - 1]
    });
    total
}

impl Parsed {
    fn new(agent: &'static str) -> Parsed {
        Parsed {
            agent,
            offset: 0,
            id: None,
            cwd: None,
            started_at: None,
            last_at: None,
            title: None,
            first_text: None,
            turns: Vec::new(),
            first_you: None,
            total: 0,
            bytes: 0,
        }
    }

    fn push(&mut self, at: &str, kind: Kind) {
        // Codex can log one command twice (the call and the finished
        // item); the same tool and target twice in a row is one turn.
        if let (Kind::Tool { .. }, Some(last), "codex") = (&kind, self.turns.last(), self.agent) {
            if last.kind == kind {
                return;
            }
        }
        if let Kind::You { text } = &kind {
            if self.first_you.is_none() {
                self.first_you = Some(self.turns.len());
            }
            if self.first_text.is_none() {
                self.first_text = Some(text.clone());
            }
        }
        let turn = Turn {
            at: at.to_string(),
            kind,
        };
        self.bytes += turn_size(&turn);
        self.turns.push(turn);
        self.total += 1;
        // Memory stays near one record's size, however long the session.
        if self.bytes > 2 * RECORD_MAX {
            self.bytes = cap(&mut self.turns, &mut self.first_you, RECORD_MAX);
        }
    }

    fn meta(&mut self, id: Option<&str>, cwd: Option<&str>, at: Option<&str>) {
        if self.id.is_none() {
            self.id = id.filter(|s| !s.is_empty()).map(str::to_string);
        }
        if self.cwd.is_none() {
            self.cwd = cwd.filter(|c| c.starts_with('/')).map(str::to_string);
        }
        if let Some(at) = at.filter(|s| !s.is_empty()) {
            if self.started_at.is_none() {
                self.started_at = Some(at.to_string());
            }
            self.last_at = Some(at.to_string());
        }
    }

    fn you(&mut self, at: &str, text: Option<String>) {
        if let Some(t) = text.as_deref().and_then(|t| turn_text(t, YOU_MAX)) {
            self.push(at, Kind::You { text: t });
        }
    }

    fn said(&mut self, at: &str, text: &str) {
        if let Some(t) = turn_text(text, AGENT_MAX) {
            self.push(at, Kind::Agent { text: t });
        }
    }

    fn tool(&mut self, at: &str, name: &str, target: Option<String>) {
        let name: String = name.chars().take(TARGET_MAX).collect();
        self.push(at, Kind::Tool { name, target });
    }

    fn line(&mut self, line: &str) {
        // Most lines of a Claude Code log are neither turns nor the title;
        // they are skipped before they are parsed.
        if self.agent == "claude"
            && !["\"user\"", "\"assistant\"", "\"ai-title\""]
                .iter()
                .any(|t| line.contains(t))
        {
            return;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return;
        };
        if self.agent == "claude" {
            self.claude(&v)
        } else {
            self.codex(&v)
        }
    }

    /// A line of Claude Code's log: `user` and `assistant` entries of the
    /// main conversation (not a subagent's), and `ai-title`.
    fn claude(&mut self, v: &Value) {
        let ty = v["type"].as_str().unwrap_or_default();
        if ty == "ai-title" {
            if let Some(t) = v["aiTitle"].as_str().and_then(title_of) {
                self.title = Some(t);
            }
            return;
        }
        if (ty != "user" && ty != "assistant") || v["isSidechain"] == true {
            return;
        }
        self.meta(
            v["sessionId"].as_str(),
            v["cwd"].as_str(),
            v["timestamp"].as_str(),
        );
        let at = v["timestamp"].as_str().unwrap_or_default().to_string();
        let content = &v["message"]["content"];
        if ty == "user" {
            // Claude Code's own additions (a skill's text, an image note),
            // and messages from other agents or notifications.
            if v["isMeta"] == true {
                return;
            }
            if v["origin"]["kind"].as_str().is_some_and(|k| k != "human") {
                return;
            }
            match content {
                Value::String(s) => self.you(&at, claude_you(s)),
                Value::Array(blocks) => {
                    for b in blocks {
                        // `tool_result` blocks are never read.
                        if b["type"] == "text" {
                            if let Some(s) = b["text"].as_str() {
                                self.you(&at, claude_you(s));
                            }
                        }
                    }
                }
                _ => {}
            }
            return;
        }
        for b in content.as_array().into_iter().flatten() {
            match b["type"].as_str() {
                Some("text") => {
                    if let Some(s) = b["text"].as_str() {
                        self.said(&at, s);
                    }
                }
                Some("tool_use") => {
                    let name = b["name"].as_str().unwrap_or("tool");
                    self.tool(&at, name, target(name, &b["input"]));
                }
                _ => {}
            }
        }
    }

    /// A line of a Codex rollout: `session_meta`, then the person's and
    /// the agent's messages and the tool calls.
    fn codex(&mut self, v: &Value) {
        let p = &v["payload"];
        let at = v["timestamp"].as_str().unwrap_or_default().to_string();
        match v["type"].as_str().unwrap_or_default() {
            "session_meta" => {
                self.meta(p["id"].as_str(), p["cwd"].as_str(), p["timestamp"].as_str());
                return;
            }
            "turn_context" => self.meta(None, p["cwd"].as_str(), None),
            "response_item" => match p["type"].as_str().unwrap_or_default() {
                "message" => {
                    let role = p["role"].as_str().unwrap_or_default();
                    for b in p["content"].as_array().into_iter().flatten() {
                        let Some(text) = b["text"].as_str() else {
                            continue;
                        };
                        match (role, b["type"].as_str()) {
                            ("user", Some("input_text")) => self.you(&at, codex_you(text)),
                            ("assistant", Some("output_text")) => self.said(&at, text),
                            _ => {}
                        }
                    }
                }
                "function_call" => {
                    let name = p["name"].as_str().unwrap_or("tool");
                    let args = p["arguments"]
                        .as_str()
                        .and_then(|a| serde_json::from_str::<Value>(a).ok())
                        .unwrap_or(Value::Null);
                    let shown = if SHELL_TOOLS.contains(&name) {
                        "shell"
                    } else {
                        name
                    };
                    self.tool(&at, shown, target(name, &args));
                }
                "local_shell_call" => {
                    let cmd = command_text(&p["action"]["command"]);
                    self.tool(&at, "shell", cmd.and_then(|c| one_line(&c, TARGET_MAX)));
                }
                "custom_tool_call" => {
                    let name = p["name"].as_str().unwrap_or("tool");
                    let input = p["input"].as_str().unwrap_or_default();
                    match name {
                        "apply_patch" => self.tool(&at, name, patch_target(input)),
                        // Code run for the agent: what it ran shows up as
                        // a command item below.
                        "exec" => {}
                        _ => self.tool(
                            &at,
                            name,
                            input.lines().next().and_then(|l| one_line(l, TARGET_MAX)),
                        ),
                    }
                }
                // Outputs and reasoning are never read.
                _ => {}
            },
            "event_msg" if p["type"] == "item_completed" => {
                let item = &p["item"];
                if item["type"] == "CommandExecution" {
                    let cmd = command_text(&item["command"]);
                    self.tool(&at, "shell", cmd.and_then(|c| one_line(&c, TARGET_MAX)));
                }
            }
            _ => {}
        }
        if !at.is_empty() {
            self.last_at = Some(at);
        }
    }

    /// Read what was added to `file` since the last read, whole lines only.
    fn read(&mut self, file: &Path) -> std::io::Result<()> {
        let mut f = std::fs::File::open(file)?;
        f.seek(SeekFrom::Start(self.offset))?;
        let mut r = BufReader::new(f);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            let n = r.read_until(b'\n', &mut buf)?;
            if n == 0 || buf.last() != Some(&b'\n') {
                // The end, or a line still being written.
                return Ok(());
            }
            self.offset += n as u64;
            self.line(&String::from_utf8_lossy(&buf));
        }
    }
}

// ---------------------------------------------------------------------
// git.

/// Run `git -C dir args…` with a timeout; its output when it succeeded.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let mut child = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "core.fsmonitor=false"])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut out = child.stdout.take()?;
    let reading = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        s
    });
    let deadline = Instant::now() + GIT_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let s = reading.join().ok()?;
                return status.success().then_some(s);
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

fn git_line(dir: &Path, args: &[&str]) -> Option<String> {
    git(dir, args)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// A remote URL without user info (`https://user:token@host/…` keeps only
/// `https://host/…`), redacted.
fn clean_remote(url: &str) -> String {
    let url = match url.find("://") {
        Some(i) => {
            let rest = &url[i + 3..];
            match rest.find('@') {
                Some(at) if !rest[..at].contains('/') => {
                    format!("{}{}", &url[..i + 3], &rest[at + 1..])
                }
                _ => url.to_string(),
            }
        }
        None => url.to_string(),
    };
    redact(&url)
}

/// The repository `dir` is in, if any.
pub fn repo_of(dir: &Path) -> Option<Git> {
    if !dir.is_dir() {
        return None;
    }
    let root = PathBuf::from(git_line(dir, &["rev-parse", "--show-toplevel"])?);
    Some(Git {
        remote: git_line(&root, &["remote", "get-url", "origin"]).map(|r| clean_remote(&r)),
        branch: branch(&root),
        root,
    })
}

fn branch(root: &Path) -> Option<String> {
    git_line(root, &["rev-parse", "--abbrev-ref", "HEAD"]).filter(|b| b != "HEAD")
}

/// The commits of the current branch from the last 7 days, newest first,
/// at most 50: hash, time, subject (redacted), how many files.
fn commits(root: &Path, branch: Option<&str>) -> Vec<Commit> {
    let max = format!("--max-count={COMMITS_MAX}");
    let Some(out) = git(
        root,
        &[
            "log",
            "--since=7.days",
            &max,
            "--format=%x1e%H%x1f%cI%x1f%s",
            "--shortstat",
            "HEAD",
        ],
    ) else {
        return Vec::new();
    };
    parse_log(&out, branch)
}

fn parse_log(out: &str, branch: Option<&str>) -> Vec<Commit> {
    out.split('\u{1e}')
        .filter_map(|entry| {
            let (head, stat) = entry.split_once('\n').unwrap_or((entry, ""));
            let mut f = head.split('\u{1f}');
            let (sha, at, subject) = (f.next()?.trim(), f.next()?, f.next().unwrap_or(""));
            if sha.is_empty() {
                return None;
            }
            let files = stat
                .split_whitespace()
                .collect::<Vec<_>>()
                .windows(2)
                .find(|w| w[1].starts_with("file"))
                .and_then(|w| w[0].parse().ok())
                .unwrap_or(0);
            Some(Commit {
                sha: sha.to_string(),
                at: at.trim().to_string(),
                subject: one_line(subject, SUBJECT_MAX).unwrap_or_default(),
                files,
                branch: branch.map(str::to_string),
            })
        })
        .collect()
}

// ---------------------------------------------------------------------
// The reader.

/// What was last sent, kept across restarts.
#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    /// Per log file: its size and modification time (ms) as last sent.
    #[serde(default)]
    files: BTreeMap<PathBuf, Sent>,
    /// Per repository root: HEAD and branch as last sent, and when a
    /// session last worked there (Unix seconds).
    #[serde(default)]
    repos: BTreeMap<PathBuf, RepoSent>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Sent {
    size: u64,
    mtime: u64,
    live: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RepoSent {
    head: Option<String>,
    branch: Option<String>,
    seen: u64,
}

/// A session log on disk.
#[derive(Debug, Clone)]
struct Log {
    path: PathBuf,
    agent: &'static str,
    size: u64,
    /// Modification time, ms since the epoch.
    mtime: u64,
}

impl Log {
    fn live(&self, now: u64) -> bool {
        now.saturating_sub(self.mtime / 1000) < LIVE
    }
}

fn unix(t: SystemTime) -> u64 {
    t.duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub struct Reader {
    home: PathBuf,
    state_file: PathBuf,
    state: State,
    /// Sessions being read, by log: live ones are kept between polls so
    /// only what was added is read.
    open: HashMap<PathBuf, Parsed>,
    sent_at: HashMap<PathBuf, Instant>,
    git_cache: HashMap<PathBuf, (Instant, Option<Git>)>,
    git_checked: Option<Instant>,
    new_roots: bool,
}

impl Reader {
    /// A reader of the logs under `home` (the user's home directory),
    /// remembering what it sent in `state_dir`.
    pub fn new(home: PathBuf, state_dir: &Path) -> Reader {
        let state_file = state_dir.join(STATE_FILE);
        let state = std::fs::read(&state_file)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Reader {
            home,
            state_file,
            state,
            open: HashMap::new(),
            sent_at: HashMap::new(),
            git_cache: HashMap::new(),
            git_checked: None,
            new_roots: false,
        }
    }

    /// Forget everything sent: the next look sends it all again.
    pub fn reset(&mut self) {
        self.state = State::default();
        self.open.clear();
        self.sent_at.clear();
        self.git_cache.clear();
        self.git_checked = None;
        self.new_roots = false;
        let _ = std::fs::remove_file(&self.state_file);
    }

    fn save(&self) {
        let Ok(text) = serde_json::to_vec(&self.state) else {
            return;
        };
        if let Some(dir) = self.state_file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = self.state_file.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &self.state_file);
        }
    }

    /// The logs of the last 7 days, at most the newest 300, oldest first.
    fn logs(&self, now: u64) -> Vec<Log> {
        let mut logs: Vec<Log> = inventory::session_files(&self.home)
            .into_iter()
            .filter_map(|(m, path, agent)| {
                let size = path.metadata().ok()?.len();
                Some(Log {
                    path,
                    agent,
                    size,
                    mtime: unix(m),
                })
            })
            .filter(|l| now.saturating_sub(l.mtime / 1000) <= WINDOW)
            .collect();
        logs.sort_by_key(|l| std::cmp::Reverse(l.mtime));
        logs.truncate(SESSIONS_MAX);
        logs.reverse();
        logs
    }

    /// The logs to send now: new, grown, rewritten, or no longer live,
    /// and not sent in the last 30 seconds.
    fn due(&mut self, now: u64) -> Vec<Log> {
        let logs = self.logs(now);
        let listed: HashSet<&PathBuf> = logs.iter().map(|l| &l.path).collect();
        self.state.files.retain(|p, _| listed.contains(p));
        self.open.retain(|p, _| listed.contains(p));
        logs.iter()
            .filter(|l| {
                let changed = match self.state.files.get(&l.path) {
                    None => true,
                    Some(s) => s.size != l.size || s.mtime != l.mtime || (s.live && !l.live(now)),
                };
                let recent = self
                    .sent_at
                    .get(&l.path)
                    .is_some_and(|t| t.elapsed() < RESEND_FLOOR);
                changed && !recent
            })
            .cloned()
            .collect()
    }

    fn git_for(&mut self, cwd: &Path) -> Option<Git> {
        if let Some((at, g)) = self.git_cache.get(cwd) {
            if at.elapsed() < GIT_CACHE {
                return g.clone();
            }
        }
        let g = repo_of(cwd);
        self.git_cache
            .insert(cwd.to_path_buf(), (Instant::now(), g.clone()));
        g
    }

    /// The record of one session log, read on from where it was left.
    /// `None` for a log with no turns or no folder.
    fn session(&mut self, log: &Log, now: u64) -> Option<Session> {
        let mut p = self
            .open
            .remove(&log.path)
            .filter(|p| p.offset <= log.size)
            .unwrap_or_else(|| Parsed::new(log.agent));
        if let Err(e) = p.read(&log.path) {
            debug!("reading a session log: {e}");
        }
        let live = log.live(now);
        let mtime = inventory::rfc3339(SystemTime::UNIX_EPOCH + Duration::from_millis(log.mtime));
        let record = (|| {
            if p.total == 0 {
                return None;
            }
            let cwd = PathBuf::from(p.cwd.clone()?);
            let id = p.id.clone().or_else(|| {
                log.path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
            })?;
            let last_at = p.last_at.clone().unwrap_or_else(|| mtime.clone());
            Some(Session {
                agent: log.agent,
                id,
                git: None,
                started_at: p.started_at.clone().unwrap_or_else(|| last_at.clone()),
                last_at,
                title: p
                    .title
                    .clone()
                    .or_else(|| p.first_text.as_deref().and_then(title_of)),
                turns: Vec::new(),
                turns_total: p.total,
                live,
                cwd,
            })
        })();
        let mut record = record?;
        record.git = self.git_for(&record.cwd);
        // Fit the record in 32 KB: what is left after everything but the
        // turns is for the turns, oldest dropped first.
        let base = serde_json::to_string(&record).map(|s| s.len()).unwrap_or(0) + 64;
        let mut turns = p.turns.clone();
        let mut first = p.first_you;
        cap(&mut turns, &mut first, RECORD_MAX.saturating_sub(base));
        record.turns = turns;
        if live {
            self.open.insert(log.path.clone(), p);
        }
        if let Some(g) = &record.git {
            let r = self.state.repos.entry(g.root.clone()).or_insert_with(|| {
                self.new_roots = true;
                RepoSent::default()
            });
            r.seen = r.seen.max(log.mtime / 1000);
        }
        Some(record)
    }

    /// `git` notes for the repositories whose HEAD or branch changed since
    /// the last note (every one, the first time), with the roots they are
    /// for.
    fn repos(&mut self, now: u64) -> Vec<(PathBuf, Repo)> {
        self.state
            .repos
            .retain(|root, r| now.saturating_sub(r.seen) <= WINDOW && root.is_dir());
        let mut out = Vec::new();
        for (root, sent) in &self.state.repos {
            let Some(head) = git_line(root, &["rev-parse", "HEAD"]) else {
                continue;
            };
            let branch = branch(root);
            if sent.head.as_deref() == Some(head.as_str()) && sent.branch == branch {
                continue;
            }
            out.push((
                root.clone(),
                Repo {
                    root: root.clone(),
                    remote: git_line(root, &["remote", "get-url", "origin"])
                        .map(|r| clean_remote(&r)),
                    commits: commits(root, branch.as_deref()),
                    branch,
                    head,
                },
            ));
        }
        out
    }

    /// One look: a `session` note per changed log, then, every minute or
    /// when a new repository turned up, a `git` note per changed
    /// repository. `send` says whether to go on. Returns how many notes
    /// went out.
    pub fn cycle(&mut self, send: &mut dyn FnMut(Value) -> bool) -> usize {
        let now = unix(SystemTime::now()) / 1000;
        let mut sent = 0;
        let mut go_on = true;
        for log in self.due(now) {
            if let Some(record) = self.session(&log, now) {
                if !send(json!({"note": "session", "session": record})) {
                    go_on = false;
                    break;
                }
                sent += 1;
                self.sent_at.insert(log.path.clone(), Instant::now());
            }
            self.state.files.insert(
                log.path.clone(),
                Sent {
                    size: log.size,
                    mtime: log.mtime,
                    live: log.live(now),
                },
            );
        }
        let git_due = self.git_checked.is_none_or(|t| t.elapsed() >= GIT_EVERY);
        if go_on && (git_due || self.new_roots) {
            self.git_checked = Some(Instant::now());
            self.new_roots = false;
            for (root, repo) in self.repos(now) {
                let (head, branch) = (repo.head.clone(), repo.branch.clone());
                if !send(json!({"note": "git", "repo": repo})) {
                    break;
                }
                sent += 1;
                if let Some(r) = self.state.repos.get_mut(&root) {
                    r.head = Some(head);
                    r.branch = branch;
                }
            }
        }
        self.save();
        sent
    }
}

/// The reader for one link: waits for the cloud's `settings`, and while
/// history is on looks at the logs every 10 seconds, sending notes through
/// `out` (false: the link is gone) no faster than ten a second. History
/// off removes the reader's state. Returns when the link ends (`settings`
/// is dropped or `out` fails).
pub fn run(
    home: PathBuf,
    state_dir: PathBuf,
    settings: Receiver<Setting>,
    mut out: impl FnMut(Value) -> bool,
) {
    // One reader at a time, even while an old link's is still finishing.
    static ONE: Mutex<()> = Mutex::new(());
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let mut reader = Reader::new(home, &state_dir);
    let mut on = false;
    let mut next: Option<Setting> = None;
    loop {
        let got = match next.take() {
            Some(s) => Ok(s),
            None => settings.recv_timeout(if on { POLL } else { Duration::from_secs(3600) }),
        };
        match got {
            Ok(s) if !s.history => {
                if on {
                    info!("history switched off; the reader's state is removed");
                }
                reader.reset();
                on = false;
                continue;
            }
            Ok(s) => {
                if s.resend {
                    info!("history: sending everything again");
                    reader.reset();
                } else if !on {
                    info!("history on");
                }
                on = true;
            }
            Err(RecvTimeoutError::Timeout) if on => {}
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        }
        let mut alive = true;
        let sent = reader.cycle(&mut |note| {
            match settings.try_recv() {
                Ok(s) => {
                    next = Some(s);
                    return false;
                }
                Err(TryRecvError::Disconnected) => {
                    alive = false;
                    return false;
                }
                Err(TryRecvError::Empty) => {}
            }
            if !out(note) {
                alive = false;
                return false;
            }
            std::thread::sleep(PACE);
            true
        });
        if sent > 0 {
            debug!(notes = sent, "history sent");
        }
        if !alive {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude_log() -> String {
        [
            r#"{"type":"permission-mode","permissionMode":"default","sessionId":"s-1"}"#,
            r#"{"type":"user","isSidechain":false,"cwd":"/work/app","gitBranch":"main","sessionId":"s-1","timestamp":"2026-10-01T10:00:00.000Z","message":{"role":"user","content":"Add a greeting to the README"},"origin":{"kind":"human"}}"#,
            r#"{"type":"assistant","isSidechain":false,"cwd":"/work/app","sessionId":"s-1","timestamp":"2026-10-01T10:00:05.000Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"private musing"}]}}"#,
            r#"{"type":"assistant","isSidechain":false,"cwd":"/work/app","sessionId":"s-1","timestamp":"2026-10-01T10:00:06.000Z","message":{"role":"assistant","content":[{"type":"text","text":"I'll read it first."}]}}"#,
            r#"{"type":"assistant","isSidechain":false,"cwd":"/work/app","sessionId":"s-1","timestamp":"2026-10-01T10:00:07.000Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/work/app/README.md"}}]}}"#,
            r#"{"type":"user","isSidechain":false,"cwd":"/work/app","sessionId":"s-1","timestamp":"2026-10-01T10:00:08.000Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"FILE CONTENTS HERE"}]},"toolUseResult":{"file":{"content":"FILE CONTENTS HERE"}}}"#,
            r#"{"type":"assistant","isSidechain":false,"cwd":"/work/app","sessionId":"s-1","timestamp":"2026-10-01T10:00:09.000Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"t2","name":"Edit","input":{"file_path":"/work/app/README.md","old_string":"OLD TEXT","new_string":"NEW TEXT"}}]}}"#,
            r#"{"type":"assistant","isSidechain":false,"cwd":"/work/app","sessionId":"s-1","timestamp":"2026-10-01T10:00:10.000Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"t3","name":"Bash","input":{"command":"cargo test -p app && echo done","description":"Run the tests"}}]}}"#,
            r#"{"type":"user","isSidechain":false,"cwd":"/work/app","sessionId":"s-1","timestamp":"2026-10-01T10:00:11.000Z","message":{"role":"user","content":"<bash-stdout>COMMAND OUTPUT</bash-stdout><bash-stderr></bash-stderr>"}}"#,
            r#"{"type":"user","isSidechain":false,"isMeta":true,"cwd":"/work/app","sessionId":"s-1","timestamp":"2026-10-01T10:00:12.000Z","message":{"role":"user","content":[{"type":"text","text":"Base directory for this skill: SKILL TEXT"}]}}"#,
            r#"{"type":"user","isSidechain":false,"cwd":"/work/app","sessionId":"s-1","timestamp":"2026-10-01T10:00:13.000Z","origin":{"kind":"task-notification"},"message":{"role":"user","content":"<task-notification>NOTIFICATION</task-notification>"}}"#,
            r#"{"type":"user","isSidechain":true,"cwd":"/work/app","sessionId":"s-1","timestamp":"2026-10-01T10:00:14.000Z","message":{"role":"user","content":"SUBAGENT PROMPT"}}"#,
            r#"{"type":"user","isSidechain":false,"cwd":"/work/app","sessionId":"s-1","timestamp":"2026-10-01T10:01:00.000Z","message":{"role":"user","content":[{"type":"text","text":"Thanks, use key=abc123 and <system-reminder>REMINDER</system-reminder>ship it"}]}}"#,
            r#"{"type":"ai-title","aiTitle":"Greeting in the README","sessionId":"s-1"}"#,
            r#"{"type":"assistant","isSidechain":false,"cwd":"/work/app","sessionId":"s-1","timestamp":"2026-10-01T10:01:05.000Z","message":{"role":"assistant","content":[{"type":"text","text":"Done."}]}}"#,
        ]
        .join("\n")
            + "\n"
    }

    fn parse(agent: &'static str, text: &str) -> Parsed {
        let mut p = Parsed::new(agent);
        for l in text.lines() {
            p.line(l);
        }
        p
    }

    fn roles(p: &Parsed) -> Vec<String> {
        p.turns
            .iter()
            .map(|t| match &t.kind {
                Kind::You { text } => format!("you: {text}"),
                Kind::Agent { text } => format!("agent: {text}"),
                Kind::Tool { name, target } => format!("tool: {name} {target:?}"),
            })
            .collect()
    }

    #[test]
    fn a_claude_log_becomes_turns() {
        let p = parse("claude", &claude_log());
        assert_eq!(p.id.as_deref(), Some("s-1"));
        assert_eq!(p.cwd.as_deref(), Some("/work/app"));
        assert_eq!(p.started_at.as_deref(), Some("2026-10-01T10:00:00.000Z"));
        assert_eq!(p.last_at.as_deref(), Some("2026-10-01T10:01:05.000Z"));
        assert_eq!(p.title.as_deref(), Some("Greeting in the README"));
        assert_eq!(
            roles(&p),
            [
                "you: Add a greeting to the README",
                "agent: I'll read it first.",
                "tool: Read Some(\"/work/app/README.md\")",
                "tool: Edit Some(\"/work/app/README.md\")",
                "tool: Bash Some(\"cargo test -p app && echo done\")",
                "you: Thanks, use key=[redacted] and ship it",
                "agent: Done.",
            ]
        );
        assert_eq!(p.total, 7);
        // Never sent: tool results, file contents, command output, Claude
        // Code's own additions, notifications, subagents, thinking.
        let sent = serde_json::to_string(&p.turns).unwrap();
        for never in [
            "FILE CONTENTS",
            "OLD TEXT",
            "NEW TEXT",
            "COMMAND OUTPUT",
            "SKILL TEXT",
            "NOTIFICATION",
            "SUBAGENT",
            "REMINDER",
            "private musing",
            "abc123",
        ] {
            assert!(!sent.contains(never), "{never} in {sent}");
        }
    }

    #[test]
    fn slash_and_shell_commands_are_what_was_typed() {
        assert_eq!(
            claude_you("<command-name>/review</command-name>\n<command-message>review</command-message>\n<command-args>the login page</command-args>").as_deref(),
            Some("/review the login page")
        );
        assert_eq!(
            claude_you("<bash-input>git status</bash-input>").as_deref(),
            Some("! git status")
        );
        assert_eq!(
            claude_you("<local-command-stdout>x</local-command-stdout>"),
            None
        );
        assert_eq!(claude_you("[Request interrupted by user]"), None);
        assert_eq!(
            claude_you("<pasted_content id=\"a1\">\nFix the build</pasted_content>").as_deref(),
            Some("Fix the build")
        );
    }

    #[test]
    fn a_codex_rollout_becomes_turns() {
        let log = [
            r##"{"timestamp":"2026-10-02T08:00:01.000Z","type":"session_meta","payload":{"id":"c-1","timestamp":"2026-10-02T08:00:00.000Z","cwd":"/work/api","originator":"codex_cli","cli_version":"0.99.0","base_instructions":{"text":"BASE INSTRUCTIONS"},"git":{"commit_hash":"abc","branch":"main"}}}"##,
            r##"{"timestamp":"2026-10-02T08:00:02.000Z","type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"DEVELOPER TEXT"}]}}"##,
            r##"{"timestamp":"2026-10-02T08:00:02.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions for /work/api\n\nAGENTS TEXT"},{"type":"input_text","text":"<environment_context>\n  <cwd>/work/api</cwd>\n</environment_context>"}]}}"##,
            r##"{"timestamp":"2026-10-02T08:00:03.000Z","type":"turn_context","payload":{"cwd":"/work/api","model":"m"}}"##,
            r##"{"timestamp":"2026-10-02T08:00:04.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Why do the API tests fail?"}]}}"##,
            r##"{"timestamp":"2026-10-02T08:00:05.000Z","type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","content":[{"type":"text","text":"Why do the API tests fail?"}]}}}"##,
            r##"{"timestamp":"2026-10-02T08:00:06.000Z","type":"response_item","payload":{"type":"reasoning","summary":[{"text":"REASONING"}]}}"##,
            r##"{"timestamp":"2026-10-02T08:00:07.000Z","type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"command\":[\"bash\",\"-lc\",\"npm test\"],\"workdir\":\"/work/api\"}","call_id":"f1"}}"##,
            r##"{"timestamp":"2026-10-02T08:00:08.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"f1","output":"TEST OUTPUT"}}"##,
            r##"{"timestamp":"2026-10-02T08:00:09.000Z","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","input":"text(await tools.exec_command({cmd:\"ls\"}))","call_id":"f2"}}"##,
            r##"{"timestamp":"2026-10-02T08:00:10.000Z","type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","command":["/bin/zsh","-lc","ls src"],"aggregated_output":"LISTING OUTPUT","exit_code":0}}}"##,
            r##"{"timestamp":"2026-10-02T08:00:11.000Z","type":"response_item","payload":{"type":"custom_tool_call","name":"apply_patch","input":"*** Begin Patch\n*** Update File: src/app.ts\n@@\n-PATCH OLD\n+PATCH NEW\n*** End Patch","call_id":"f3"}}"##,
            r##"{"timestamp":"2026-10-02T08:00:12.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Fixed: a missing import."}],"phase":"final_answer"}}"##,
        ]
        .join("\n");
        let p = parse("codex", &log);
        assert_eq!(p.id.as_deref(), Some("c-1"));
        assert_eq!(p.cwd.as_deref(), Some("/work/api"));
        assert_eq!(p.started_at.as_deref(), Some("2026-10-02T08:00:00.000Z"));
        assert_eq!(p.last_at.as_deref(), Some("2026-10-02T08:00:12.000Z"));
        assert_eq!(
            roles(&p),
            [
                "you: Why do the API tests fail?",
                "tool: shell Some(\"npm test\")",
                "tool: shell Some(\"ls src\")",
                "tool: apply_patch Some(\"src/app.ts\")",
                "agent: Fixed: a missing import.",
            ]
        );
        let sent = serde_json::to_string(&p.turns).unwrap();
        for never in [
            "BASE",
            "DEVELOPER",
            "AGENTS TEXT",
            "environment_context",
            "REASONING",
            "OUTPUT",
            "PATCH",
        ] {
            assert!(!sent.contains(never), "{never} in {sent}");
        }
    }

    #[test]
    fn secrets_are_redacted() {
        for (secret, kept) in [
            (
                "use sk-ant-api03-abcdefghijklmnop now",
                "use [redacted] now",
            ),
            ("key ptp_0123456789abcdef", "key [redacted]"),
            ("ghp_abcdefghijklmnopqrstuvwxyz0123456789", "[redacted]"),
            ("gho_abcdefghij1234", "[redacted]"),
            ("xoxb-1234-5678-abcdefgh", "[redacted]"),
            ("AKIAABCDEFGHIJKLMNOP", "[redacted]"),
            (
                "curl -H 'Authorization: Bearer eyJhbGciOi.payload'",
                "curl -H 'Authorization: Bearer [redacted]'",
            ),
            ("PASSWORD=hunter2 ./run", "PASSWORD=[redacted] ./run"),
            ("?access_token=abc&x=1", "?access_token=[redacted]&x=1"),
            (r#"{"api_key": "abc def"}"#, r#"{"api_key": [redacted]}"#),
            (
                "blob 3q2+7wAAAAAAAAAAAAAAAAAAAAAAAAAA1234567890ab== end",
                "blob [redacted] end",
            ),
            (
                "sha 0123456789abcdef0123456789abcdef01234567",
                "sha [redacted]",
            ),
        ] {
            assert_eq!(redact(secret), kept, "{secret}");
        }
        // Paths, ids and prose stay as they are.
        for fine in [
            "/home/me/code/some-project/crates/pitupitu/src/history_reader.rs",
            "session 5f0c2b1e-7a3d-4c9e-b2a1-3e8d9f6c0a47",
            "the key point is speed",
            "monkey business",
        ] {
            assert_eq!(redact(fine), fine);
        }
    }

    #[test]
    fn texts_and_targets_are_cut() {
        let long = "a".repeat(5000);
        let t = turn_text(&long, YOU_MAX).unwrap();
        assert_eq!(t.chars().count(), YOU_MAX + 1);
        assert!(t.ends_with('…'));
        let mut p = Parsed::new("claude");
        p.you("2026-10-01T00:00:00Z", Some(long.clone()));
        p.said("2026-10-01T00:00:01Z", &long);
        assert!(
            matches!(&p.turns[0].kind, Kind::You { text } if text.chars().count() == YOU_MAX + 1)
        );
        assert!(
            matches!(&p.turns[1].kind, Kind::Agent { text } if text.chars().count() == AGENT_MAX + 1)
        );
        let cmd = json!({"command": format!("echo {}", "b ".repeat(200))});
        assert_eq!(target("Bash", &cmd).unwrap().chars().count(), TARGET_MAX);
        // Another tool: its first string argument, never its contents.
        assert_eq!(
            target("WebSearch", &json!({"query": "rust regex", "mode": "x"})).as_deref(),
            Some("rust regex")
        );
        assert_eq!(
            target(
                "mcp__notes__save",
                &json!({"content": "SECRET NOTES", "n": 3})
            ),
            None
        );
        assert_eq!(
            target("Glob", &json!({"pattern": "**/*.rs"})).as_deref(),
            Some("**/*.rs")
        );
        assert_eq!(
            title_of(&"word ".repeat(40)).unwrap().chars().count(),
            TITLE_MAX + 1
        );
    }

    #[test]
    fn a_long_session_keeps_its_first_prompt_within_32_kb() {
        let mut p = Parsed::new("claude");
        p.meta(Some("s"), Some("/w"), Some("2026-10-01T00:00:00Z"));
        p.agent = "claude";
        p.you("2026-10-01T00:00:00Z", Some("THE FIRST ASK".into()));
        for i in 0..400 {
            p.said("2026-10-01T00:00:01Z", &format!("{i} {}", "x".repeat(1500)));
        }
        assert_eq!(p.total, 401);
        assert!(p.bytes <= 2 * RECORD_MAX + 2000);
        let mut turns = p.turns.clone();
        let mut first = p.first_you;
        cap(&mut turns, &mut first, RECORD_MAX - 500);
        assert_eq!(first, Some(0));
        assert_eq!(
            turns[0].kind,
            Kind::You {
                text: "THE FIRST ASK".into()
            }
        );
        let size: usize = turns.iter().map(turn_size).sum();
        assert!(size <= RECORD_MAX - 500);
        // The newest are kept.
        assert!(
            matches!(&turns.last().unwrap().kind, Kind::Agent { text } if text.starts_with("399 "))
        );
    }

    #[test]
    fn remotes_lose_their_user_info() {
        assert_eq!(
            clean_remote("https://me:secret@github.com/org/repo.git"),
            "https://github.com/org/repo.git"
        );
        assert_eq!(
            clean_remote("git@github.com:org/repo.git"),
            "git@github.com:org/repo.git"
        );
    }

    #[test]
    fn git_log_lines_become_commits() {
        let out = "\u{1e}aaa111\u{1f}2026-10-20T10:00:00+02:00\u{1f}Fix the token=abc bug\n\n 3 files changed, 10 insertions(+)\n\u{1e}bbb222\u{1f}2026-10-19T09:00:00+02:00\u{1f}Merge branch x\n";
        let c = parse_log(out, Some("main"));
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].sha, "aaa111");
        assert_eq!(c[0].subject, "Fix the token=[redacted] bug");
        assert_eq!(c[0].files, 3);
        assert_eq!(c[0].branch.as_deref(), Some("main"));
        assert_eq!(c[1].files, 0);
        assert_eq!(c[1].at, "2026-10-19T09:00:00+02:00");
    }

    #[test]
    fn settings_notes() {
        assert_eq!(
            Setting::from_note(&json!({"note": "settings", "history": true, "extra": 1})),
            Some(Setting {
                history: true,
                resend: false
            })
        );
        assert_eq!(
            Setting::from_note(&json!({"note": "settings", "history": true, "resend": true})),
            Some(Setting {
                history: true,
                resend: true
            })
        );
        assert_eq!(
            Setting::from_note(&json!({"note": "settings", "driver": "herdr"})),
            None
        );
    }

    fn write_claude(home: &Path, cwd: &Path, id: &str) -> PathBuf {
        let dir = home.join(".claude/projects/-work-app");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(format!("{id}.jsonl"));
        let log = claude_log()
            .replace("/work/app", &cwd.display().to_string())
            .replace("s-1", id);
        std::fs::write(&file, log).unwrap();
        file
    }

    /// The reader against a made-up home: a backfill, nothing again
    /// unchanged, a grown log sent again, the state file kept for a
    /// restart and removed by `reset`.
    #[test]
    fn the_reader_sends_what_changed() {
        let home = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let cwd = home.path().join("code/app");
        std::fs::create_dir_all(&cwd).unwrap();
        let file = write_claude(home.path(), &cwd, "s-1");
        // A subagent's transcript one level down is not read.
        let sub = home.path().join(".claude/projects/-work-app/s-1/subagents");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("agent-x.jsonl"), claude_log()).unwrap();
        // Nor is anything else in Claude Code's folder.
        std::fs::write(home.path().join(".claude/settings.json"), "{}").unwrap();

        let mut r = Reader::new(home.path().to_path_buf(), state.path());
        let mut notes = Vec::new();
        let n = r.cycle(&mut |n| {
            notes.push(n);
            true
        });
        assert_eq!(n, notes.len());
        let sessions: Vec<&Value> = notes.iter().filter(|n| n["note"] == "session").collect();
        assert_eq!(sessions.len(), 1, "{notes:?}");
        let s = &sessions[0]["session"];
        assert_eq!(s["agent"], "claude");
        assert_eq!(s["id"], "s-1");
        assert_eq!(s["cwd"], json!(cwd));
        assert_eq!(s["title"], "Greeting in the README");
        assert_eq!(s["turns_total"], 7);
        assert_eq!(s["live"], true);
        assert_eq!(
            s["turns"][0],
            json!({"at": "2026-10-01T10:00:00.000Z", "role": "you", "text": "Add a greeting to the README"})
        );
        assert_eq!(s["turns"][2]["role"], "tool");
        assert_eq!(s["turns"][2]["name"], "Read");
        assert!(state.path().join(STATE_FILE).exists());

        // Unchanged: nothing.
        let mut again = 0;
        r.cycle(&mut |n| {
            again += usize::from(n["note"] == "session");
            true
        });
        assert_eq!(again, 0);

        // A restart remembers what was sent.
        let mut r = Reader::new(home.path().to_path_buf(), state.path());
        let mut after_restart = 0;
        r.cycle(&mut |n| {
            after_restart += usize::from(n["note"] == "session");
            true
        });
        assert_eq!(after_restart, 0);

        // The log grows: sent again, read on from where it was.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .unwrap();
        std::io::Write::write_all(
            &mut f,
            br#"{"type":"user","isSidechain":false,"cwd":"/x","sessionId":"s-1","timestamp":"2026-10-01T11:00:00.000Z","message":{"role":"user","content":"One more thing"}}
"#,
        )
        .unwrap();
        let mut grown = Vec::new();
        r.cycle(&mut |n| {
            grown.push(n);
            true
        });
        let s = grown
            .iter()
            .find(|n| n["note"] == "session")
            .expect("sent again");
        assert_eq!(s["session"]["turns_total"], 8);
        assert_eq!(s["session"]["last_at"], "2026-10-01T11:00:00.000Z");

        // Live, so read on from where it was left: only the new line.
        assert!(r.open.contains_key(&file));
        std::io::Write::write_all(
            &mut f,
            br#"{"type":"assistant","isSidechain":false,"sessionId":"s-1","timestamp":"2026-10-01T11:00:05.000Z","message":{"role":"assistant","content":[{"type":"text","text":"On it."}]}}
"#,
        )
        .unwrap();
        r.sent_at.clear();
        let mut on = Vec::new();
        r.cycle(&mut |n| {
            on.push(n);
            true
        });
        let s = &on.iter().find(|n| n["note"] == "session").unwrap()["session"];
        assert_eq!(s["turns_total"], 9);
        assert_eq!(s["turns"][8]["text"], "On it.");

        // Reset (history off, or a resend) forgets it all.
        r.reset();
        assert!(!state.path().join(STATE_FILE).exists());
    }

    /// git facts from a scratch repository, when git is on PATH.
    #[test]
    fn sessions_in_a_repo_carry_git_facts() {
        let home = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let repo = home.path().join("code/app");
        std::fs::create_dir_all(&repo).unwrap();
        let g = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["-c", "user.name=T", "-c", "user.email=t@example.com"])
                .args(args)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        if !g(&["init", "-q", "-b", "main"]) {
            return;
        }
        std::fs::write(repo.join("a.txt"), "a").unwrap();
        std::fs::write(repo.join("b.txt"), "b").unwrap();
        assert!(g(&["add", "."]));
        assert!(g(&["commit", "-q", "-m", "First commit"]));
        assert!(g(&[
            "remote",
            "add",
            "origin",
            "https://me:pw@example.com/org/app.git"
        ]));
        write_claude(home.path(), &repo, "s-2");

        let mut r = Reader::new(home.path().to_path_buf(), state.path());
        let mut notes = Vec::new();
        r.cycle(&mut |n| {
            notes.push(n);
            true
        });
        let root = std::fs::canonicalize(&repo).unwrap();
        let s = &notes.iter().find(|n| n["note"] == "session").unwrap()["session"];
        assert_eq!(s["git"]["branch"], "main");
        assert_eq!(s["git"]["remote"], "https://example.com/org/app.git");
        assert_eq!(
            std::fs::canonicalize(s["git"]["root"].as_str().unwrap()).unwrap(),
            root
        );
        let git = &notes
            .iter()
            .find(|n| n["note"] == "git")
            .expect("a git note")["repo"];
        assert_eq!(git["branch"], "main");
        assert_eq!(git["commits"][0]["subject"], "First commit");
        assert_eq!(git["commits"][0]["files"], 2);
        assert_eq!(git["commits"][0]["branch"], "main");
        assert_eq!(git["head"], git["commits"][0]["sha"]);
        let sent = serde_json::to_string(&notes).unwrap();
        assert!(!sent.contains("t@example.com") && !sent.contains("pw@"));

        // Same HEAD: no new git note, even when looked at again.
        r.git_checked = None;
        let mut more = 0;
        r.cycle(&mut |n| {
            more += usize::from(n["note"] == "git");
            true
        });
        assert_eq!(more, 0);
        // A new commit: a new note.
        std::fs::write(repo.join("c.txt"), "c").unwrap();
        assert!(g(&["add", "."]));
        assert!(g(&["commit", "-q", "-m", "Second"]));
        r.git_checked = None;
        let mut newer = Vec::new();
        r.cycle(&mut |n| {
            newer.push(n);
            true
        });
        let git = &newer
            .iter()
            .find(|n| n["note"] == "git")
            .expect("a new git note")["repo"];
        assert_eq!(git["commits"][0]["subject"], "Second");
        assert_eq!(git["commits"].as_array().unwrap().len(), 2);
    }

    /// The thread: nothing before `history: true`, the backfill after it,
    /// the state removed by `history: false`, and an end with the link.
    #[test]
    fn run_follows_the_setting() {
        let home = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let cwd = home.path().join("app");
        std::fs::create_dir_all(&cwd).unwrap();
        write_claude(home.path(), &cwd, "s-3");
        let (tx, rx) = std::sync::mpsc::channel();
        let (out_tx, out_rx) = std::sync::mpsc::channel::<Value>();
        let h = home.path().to_path_buf();
        let sd = state.path().to_path_buf();
        let t = std::thread::spawn(move || run(h, sd, rx, move |v| out_tx.send(v).is_ok()));
        tx.send(Setting {
            history: false,
            resend: false,
        })
        .unwrap();
        assert!(out_rx.recv_timeout(Duration::from_millis(300)).is_err());
        tx.send(Setting {
            history: true,
            resend: false,
        })
        .unwrap();
        let note = out_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(note["note"], "session");
        assert_eq!(note["session"]["id"], "s-3");
        // A resend sends it again at once.
        tx.send(Setting {
            history: true,
            resend: true,
        })
        .unwrap();
        let note = out_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(note["session"]["id"], "s-3");
        tx.send(Setting {
            history: false,
            resend: false,
        })
        .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(!state.path().join(STATE_FILE).exists());
        drop(tx);
        t.join().unwrap();
    }
}
