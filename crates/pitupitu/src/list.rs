//! The list, reminders and schedules from this box: `pitupitu todo`,
//! `idea`, `remind`, `schedule` and `when`, over the cloud's `/v1/todos`,
//! `/v1/automations` and `/v1/when` with this box's key. Times are
//! understood by the cloud; the box sends the phrase as typed and prints
//! the words it answers with, so a misread time is seen at once.

use crate::config::Config;
use crate::{history, settings};
use anyhow::{bail, Context, Result};
use reqwest::Method;
use serde_json::{json, Map, Value};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

/// A request the cloud refused, with its reason in plain words.
#[derive(Debug, Clone, PartialEq)]
pub struct Refused {
    pub status: u16,
    /// `bad_when`, `too_often`, `not_found`, `ambiguous`, `limit`, …
    pub kind: String,
    pub message: String,
    /// For `ambiguous`: the short ids the prefix matched.
    pub matches: Vec<String>,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Refused {}

/// One call to the cloud: the JSON it answered (null for an empty
/// answer), or [`Refused`] in its words.
pub async fn call(
    cfg: &Config,
    method: Method,
    path: &str,
    query: &[(&str, String)],
    body: Option<&Value>,
) -> Result<Value> {
    let (url, key) = settings::endpoint_at(cfg, path)?;
    let mut req = reqwest::Client::new()
        .request(method, &url)
        .bearer_auth(key)
        .query(query);
    if let Some(b) = body {
        req = req.json(b);
    }
    let resp = req
        .send()
        .await
        .with_context(|| format!("could not reach pitupitu cloud at {url}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if status.is_success() {
        return Ok(body);
    }
    if status.as_u16() == 401 {
        bail!("pitupitu cloud rejected this box's key; run `pitupitu setup --relink`");
    }
    Err(refused(status.as_u16(), &body, &text).into())
}

/// The cloud's error body, as [`Refused`].
pub fn refused(status: u16, body: &Value, text: &str) -> Refused {
    let e = &body["error"];
    let message = e["message"]
        .as_str()
        .filter(|m| !m.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            let t = text.trim();
            if t.is_empty() {
                format!("pitupitu cloud answered {status}")
            } else {
                format!("pitupitu cloud answered {status}: {t}")
            }
        });
    let matches = e["matches"]
        .as_array()
        .or_else(|| body["matches"].as_array())
        .into_iter()
        .flatten()
        .filter_map(|m| m.as_str().map(str::to_string))
        .collect();
    Refused {
        status,
        kind: e["type"].as_str().unwrap_or("").to_string(),
        message,
        matches,
    }
}

/// Who is asking: `agent` when there is no terminal (an agent running the
/// command in its session), else `cli`.
pub fn via() -> &'static str {
    if std::io::stdin().is_terminal() {
        "cli"
    } else {
        "agent"
    }
}

// ---------------------------------------------------------------------
// `--here`: the project the current folder belongs to.

/// The folder a `--here` is about: its repository's `origin` (without
/// user info) and root, else the folder itself.
#[derive(Debug, Clone, PartialEq)]
pub struct Here {
    pub remote: Option<String>,
    pub cwd: PathBuf,
}

impl Here {
    pub fn of(dir: &Path) -> Here {
        match history::repo_of(dir) {
            Some(g) => Here {
                remote: g.remote,
                cwd: g.root,
            },
            None => Here {
                remote: None,
                cwd: dir.to_path_buf(),
            },
        }
    }

    pub fn current() -> Result<Here> {
        let dir = std::env::current_dir().context("reading the current folder")?;
        Ok(Here::of(&dir))
    }

    /// What an add sends: `remote` when there is one, and `cwd`.
    pub fn fields(&self, into: &mut Map<String, Value>) {
        if let Some(r) = &self.remote {
            into.insert("remote".into(), json!(r));
        }
        into.insert("cwd".into(), json!(self.cwd.to_string_lossy()));
    }

    /// The same as query parameters, for listing this project's items.
    pub fn query(&self) -> Vec<(&'static str, String)> {
        let mut q = Vec::new();
        if let Some(r) = &self.remote {
            q.push(("remote", r.clone()));
        }
        q.push(("cwd", self.cwd.to_string_lossy().into_owned()));
        q
    }
}

// ---------------------------------------------------------------------
// Naming one item: a short id, or words from its text.

/// Which row an `<id | words>` means.
#[derive(Debug, PartialEq)]
pub enum Pick<'a> {
    One(&'a Value),
    /// Looks like an id but is not among the rows read: the cloud decides.
    Id(String),
    None,
    Many(Vec<&'a Value>),
}

/// The id printed for a row: the cloud's short form, else the first six
/// characters of the id.
pub fn short(row: &Value) -> String {
    row["short"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| row["id"].as_str().unwrap_or("?").chars().take(6).collect())
}

fn looks_like_id(q: &str) -> bool {
    q.len() >= 4 && q.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// An id (any prefix of at least 4, among all `rows`), else every word in
/// `field` (any case) among the rows `eligible` keeps.
pub fn pick<'a>(
    rows: &'a [Value],
    query: &str,
    field: &str,
    eligible: impl Fn(&Value) -> bool,
) -> Pick<'a> {
    let query = query.trim();
    let q = query.to_lowercase();
    let id_like = looks_like_id(&q);
    if id_like {
        let mut hits: Vec<&Value> = rows
            .iter()
            .filter(|r| {
                r["id"].as_str().is_some_and(|id| id.starts_with(&q))
                    || r["short"].as_str().is_some_and(|s| s.starts_with(&q))
            })
            .collect();
        match hits.len() {
            0 => {}
            1 => return Pick::One(hits.remove(0)),
            _ => return Pick::Many(hits),
        }
    }
    let words: Vec<&str> = q.split_whitespace().collect();
    if words.is_empty() {
        return Pick::None;
    }
    let mut hits: Vec<&Value> = rows
        .iter()
        .filter(|r| eligible(r))
        .filter(|r| {
            let text = r[field].as_str().unwrap_or("").to_lowercase();
            words.iter().all(|w| text.contains(w))
        })
        .collect();
    match hits.len() {
        0 if id_like => Pick::Id(q),
        0 => Pick::None,
        1 => Pick::One(hits.remove(0)),
        _ => Pick::Many(hits),
    }
}

/// The lines that list several matches, for a person to pick from.
pub fn choices(hits: &[&Value], field: &str) -> String {
    hits.iter()
        .map(|r| format!("  {}  {}\n", short(r), r[field].as_str().unwrap_or("")))
        .collect()
}

// ---------------------------------------------------------------------
// The list.

pub async fn todos(cfg: &Config, query: &[(&str, String)]) -> Result<Value> {
    call(cfg, Method::GET, "/v1/todos", query, None).await
}

pub async fn add_todo(cfg: &Config, body: &Value) -> Result<Value> {
    call(cfg, Method::POST, "/v1/todos", &[], Some(body)).await
}

pub async fn change_todo(cfg: &Config, id: &str, change: &Value) -> Result<Value> {
    call(
        cfg,
        Method::PATCH,
        &format!("/v1/todos/{id}"),
        &[],
        Some(change),
    )
    .await
}

pub async fn remove_todo(cfg: &Config, id: &str) -> Result<Value> {
    call(cfg, Method::DELETE, &format!("/v1/todos/{id}"), &[], None).await
}

fn rows(body: &Value, key: &str) -> Vec<Value> {
    body[key].as_array().cloned().unwrap_or_default()
}

pub fn todo_rows(body: &Value) -> Vec<Value> {
    rows(body, "todos")
}

/// One line of the list: `<short>  <text>`, then what else there is to
/// know in brackets (an idea, done, its project, its reminder).
pub fn todo_line(t: &Value) -> String {
    let mut extra = Vec::new();
    if t["kind"] == "idea" {
        extra.push("idea".to_string());
    }
    match t["status"].as_str() {
        Some("done") => extra.push("done".into()),
        Some("dropped") => extra.push("dropped".into()),
        _ => {}
    }
    if let Some(p) = t["project"]["name"]
        .as_str()
        .or_else(|| t["project"]["key"].as_str())
    {
        extra.push(p.to_string());
    }
    if let Some(r) = t["remind_words"]
        .as_str()
        .or_else(|| t["remind_at"].as_str())
    {
        extra.push(format!("reminder {r}"));
    }
    let mut line = format!("{}  {}", short(t), t["text"].as_str().unwrap_or(""));
    if !extra.is_empty() {
        line.push_str(&format!("  ({})", extra.join(", ")));
    }
    line
}

pub fn todo_table(body: &Value) -> String {
    let rows = todo_rows(body);
    if rows.is_empty() {
        return "Nothing on the list.\n".to_string();
    }
    rows.iter().map(|t| todo_line(t) + "\n").collect()
}

// ---------------------------------------------------------------------
// Reminders and schedules.

pub async fn automations(cfg: &Config) -> Result<Value> {
    call(cfg, Method::GET, "/v1/automations", &[], None).await
}

pub async fn add_automation(cfg: &Config, body: &Value) -> Result<Value> {
    call(cfg, Method::POST, "/v1/automations", &[], Some(body)).await
}

pub async fn change_automation(cfg: &Config, id: &str, change: &Value) -> Result<Value> {
    call(
        cfg,
        Method::PATCH,
        &format!("/v1/automations/{id}"),
        &[],
        Some(change),
    )
    .await
}

pub async fn remove_automation(cfg: &Config, id: &str) -> Result<Value> {
    call(
        cfg,
        Method::DELETE,
        &format!("/v1/automations/{id}"),
        &[],
        None,
    )
    .await
}

/// What a phrase means, nothing made: `{"schedule", "words", "next_run_at"}`.
/// `for_` (`remind`, `start_session`, `digest`) applies its minimum step.
pub async fn when(cfg: &Config, phrase: &str, for_: Option<&str>) -> Result<Value> {
    let mut body = json!({"when": phrase});
    if let Some(f) = for_ {
        body["for"] = json!(f);
    }
    call(cfg, Method::POST, "/v1/when", &[], Some(&body)).await
}

pub fn automation_rows(body: &Value) -> Vec<Value> {
    rows(body, "automations")
}

/// Why a schedule is not running, in a few words; `None` while it runs.
pub fn not_running(a: &Value) -> Option<&'static str> {
    if a["enabled"] != false {
        return None;
    }
    Some(match a["paused_reason"].as_str() {
        Some("unseen") => "paused: nobody looked at its last three sessions",
        Some("todo_closed") => "paused: its todo is closed",
        Some(_) => "paused",
        None => "finished",
    })
}

/// `<short>  <words>  <what>: <title>`, and why it is paused.
pub fn automation_line(a: &Value) -> String {
    let what = a["what"]
        .as_str()
        .or_else(|| a["action"]["kind"].as_str())
        .unwrap_or("");
    let mut line = format!(
        "{}  {}  {what}: {}",
        short(a),
        a["words"].as_str().unwrap_or(""),
        a["title"].as_str().unwrap_or("")
    );
    if let Some(why) = not_running(a) {
        line.push_str(&format!("  ({why})"));
    }
    line
}

pub fn automation_table(body: &Value) -> String {
    let rows = automation_rows(body);
    if rows.is_empty() {
        return "No reminders or schedules.\n".to_string();
    }
    rows.iter().map(|a| automation_line(a) + "\n").collect()
}

/// The agent as a person calls it.
pub fn agent_name(kind: &str) -> String {
    match kind {
        "claude" => "Claude Code".into(),
        "codex" => "Codex".into(),
        other => other.to_string(),
    }
}

/// The question before a session is scheduled: "Start a Claude Code
/// session in acme-api on laptop every weekday at 09:00, next Thu 23 Oct?"
pub fn session_question(agent: Option<&str>, place: &str, box_name: &str, words: &str) -> String {
    let who = match agent {
        Some(a) => format!("a {} session", agent_name(a)),
        None => "a session".to_string(),
    };
    format!("Start {who} in {place} on {box_name} {words}?")
}

/// The question before a digest is scheduled.
pub fn digest_question(words: &str) -> String {
    format!("Make a digest of your work {words}?")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn here_as_a_body_and_as_a_query() {
        let plain = Here {
            remote: None,
            cwd: "/home/me/notes".into(),
        };
        let mut m = Map::new();
        plain.fields(&mut m);
        assert_eq!(Value::Object(m), json!({"cwd": "/home/me/notes"}));
        assert_eq!(plain.query(), [("cwd", "/home/me/notes".to_string())]);
        let repo = Here {
            remote: Some("git@github.com:o/r.git".into()),
            cwd: "/w/r".into(),
        };
        assert_eq!(
            repo.query(),
            [
                ("remote", "git@github.com:o/r.git".to_string()),
                ("cwd", "/w/r".to_string())
            ]
        );
    }

    fn list() -> Vec<Value> {
        todo_rows(&json!({"todos": [
            {"id": "3f9a2c11-0000-4000-8000-000000000001", "short": "3f9a2c", "kind": "todo",
             "text": "Call the bank", "status": "open"},
            {"id": "3f9b0d22-0000-4000-8000-000000000002", "short": "3f9b0d", "kind": "idea",
             "text": "A bank holiday calendar", "status": "open"},
            {"id": "beef0000-0000-4000-8000-000000000003", "short": "beef00", "kind": "todo",
             "text": "Buy beef", "status": "done"}
        ]}))
    }

    #[test]
    fn an_id_prefix_then_every_word() {
        let rows = list();
        let open = |t: &Value| t["status"] == "open";
        assert_eq!(pick(&rows, "3f9a", "text", open), Pick::One(&rows[0]));
        assert!(matches!(pick(&rows, "3f9", "text", open), Pick::None));
        assert!(matches!(pick(&rows, "3f9A2C", "text", open), Pick::One(_)));
        assert!(matches!(pick(&rows, "3f9b", "text", open), Pick::One(r) if r == &rows[1]));
        assert_eq!(pick(&rows, "BANK call", "text", open), Pick::One(&rows[0]));
        assert!(matches!(pick(&rows, "bank", "text", open), Pick::Many(h) if h.len() == 2));
        // An id is found whatever its status; words only among the eligible.
        assert_eq!(pick(&rows, "beef", "text", open), Pick::One(&rows[2]));
        assert_eq!(pick(&rows, "buy", "text", open), Pick::None);
        // A hex word that is no id here and no text: the cloud decides.
        assert_eq!(
            pick(&rows, "abcd12", "text", open),
            Pick::Id("abcd12".into())
        );
        assert_eq!(pick(&rows, "  ", "text", open), Pick::None);
    }

    #[test]
    fn the_lines_people_read() {
        let t = json!({"id": "3f9a2c11-0000", "kind": "idea", "text": "Rename the box",
            "status": "open", "project": {"key": "github.com/o/acme-api", "name": "acme-api"},
            "remind_words": "Thu 23 Oct, 09:00"});
        assert_eq!(
            todo_line(&t),
            "3f9a2c  Rename the box  (idea, acme-api, reminder Thu 23 Oct, 09:00)"
        );
        assert_eq!(
            todo_line(
                &json!({"short": "aa11bb", "kind": "todo", "text": "x", "status": "done", "project": null})
            ),
            "aa11bb  x  (done)"
        );
        assert_eq!(todo_table(&json!({"todos": []})), "Nothing on the list.\n");
        let a = json!({"short": "c0ffee", "words": "every weekday at 09:00, next Thu 23 Oct",
            "what": "session", "title": "Nightly dependency check", "enabled": false,
            "paused_reason": "unseen"});
        assert_eq!(
            automation_line(&a),
            "c0ffee  every weekday at 09:00, next Thu 23 Oct  session: Nightly dependency check  \
             (paused: nobody looked at its last three sessions)"
        );
        assert_eq!(
            session_question(
                Some("claude"),
                "acme-api",
                "laptop",
                "every weekday at 09:00, next Thu 23 Oct"
            ),
            "Start a Claude Code session in acme-api on laptop every weekday at 09:00, next Thu 23 Oct?"
        );
        assert_eq!(
            session_question(None, "acme", "mac", "today 17:00"),
            "Start a session in acme on mac today 17:00?"
        );
    }

    #[test]
    fn refusals_carry_the_clouds_words() {
        let r = refused(
            409,
            &json!({"error": {"type": "ambiguous", "message": "More than one matches 3f9.",
                "matches": ["3f9a2c", "3f9b0d"]}}),
            "",
        );
        assert_eq!(r.kind, "ambiguous");
        assert_eq!(r.to_string(), "More than one matches 3f9.");
        assert_eq!(r.matches, ["3f9a2c", "3f9b0d"]);
        assert_eq!(
            refused(502, &Value::Null, "").to_string(),
            "pitupitu cloud answered 502"
        );
    }
}
