//! Sessions from the keyboard: `pitupitu follow`, `unfollow`, `sessions`
//! and `show`, over `GET /v1/sessions`, `PATCH /v1/settings` and
//! `POST /v1/sessions/show` with this box's key. A followed session is one
//! pitupitu brings up on its own: it tells the person when the session
//! finishes or needs them. `show` brings one to the front in herdr on its
//! machine, this one or another of the person's.

use crate::config::Config;
use crate::settings;
use anyhow::{bail, Context, Result};
use reqwest::StatusCode;
use serde_json::{json, Value};

/// What following means, in one line.
pub const FOLLOW_SAYS: &str = "pitupitu tells you when it finishes or needs you";

/// Said when `pitupitu <command>` names no session and runs outside herdr.
pub fn not_in_herdr(command: &str) -> String {
    format!("Run this inside a session in herdr, or name one: pitupitu {command} <title>")
}

/// The pane herdr runs this command in, if any.
pub const PANE_ENV: &str = "HERDR_PANE_ID";

/// The sessions of this box, or of the box `on`, as the cloud sees them:
/// `{"sessions": [{"key", "title", "agent", "cwd", "status", "followed", "task"}]}`.
pub async fn list(cfg: &Config, on: Option<&str>) -> Result<Value> {
    let (url, key) = settings::endpoint_at(cfg, "/v1/sessions")?;
    let mut req = reqwest::Client::new().get(&url).bearer_auth(key);
    if let Some(b) = on {
        req = req.query(&[("box", b)]);
    }
    let resp = req
        .send()
        .await
        .with_context(|| format!("reaching {url}"))?;
    settings::answer(resp).await
}

/// Follow the session `key` (a pane id), or stop following it.
pub async fn follow(cfg: &Config, key: &str, on: bool) -> Result<Value> {
    settings::patch(cfg, json!({"follow": {"key": key, "on": on}})).await
}

/// Bring the session `key` (a pane id) to the front in herdr on this box,
/// or on the box `on`: `{"key", "box"}`. A refusal is the cloud's own
/// sentence ("pitupitu on laptop needs an update to do this."), said as it is.
pub async fn show(cfg: &Config, key: &str, on: Option<&str>) -> Result<Value> {
    let (url, api_key) = settings::endpoint_at(cfg, "/v1/sessions/show")?;
    let mut asked = json!({"key": key});
    if let Some(b) = on {
        asked["box"] = json!(b);
    }
    let resp = reqwest::Client::new()
        .post(&url)
        .bearer_auth(api_key)
        .json(&asked)
        .send()
        .await
        .with_context(|| format!("reaching {url}"))?;
    if resp.status() == StatusCode::CONFLICT {
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        match body["error"]["message"].as_str() {
            Some(said) => bail!("{said}"),
            None => bail!("pitupitu cloud could not show it"),
        }
    }
    settings::answer(resp).await
}

/// Which session a name means.
#[derive(Debug, PartialEq)]
pub enum Found<'a> {
    One(&'a Value),
    None,
    Many(Vec<&'a Value>),
}

fn all(body: &Value) -> impl Iterator<Item = &Value> {
    body["sessions"].as_array().into_iter().flatten()
}

/// A pane id exactly, else every word in the title (any case).
pub fn find<'a>(body: &'a Value, query: &str) -> Found<'a> {
    let query = query.trim();
    if let Some(s) = all(body).find(|s| s["key"].as_str() == Some(query)) {
        return Found::One(s);
    }
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return Found::None;
    }
    let mut hits: Vec<&Value> = all(body)
        .filter(|s| {
            let title = s["title"].as_str().unwrap_or("").to_lowercase();
            words.iter().all(|w| title.contains(w.as_str()))
        })
        .collect();
    match hits.len() {
        0 => Found::None,
        1 => Found::One(hits.remove(0)),
        _ => Found::Many(hits),
    }
}

/// A session's title, or its key when it has none.
pub fn title(session: &Value) -> String {
    session["title"]
        .as_str()
        .filter(|t| !t.trim().is_empty())
        .or_else(|| session["key"].as_str())
        .unwrap_or("?")
        .to_string()
}

/// The line `follow` and `unfollow` print.
pub fn said(title: &str, on: bool) -> String {
    if on {
        format!("Following \"{title}\": {FOLLOW_SAYS}.")
    } else {
        format!("Not following \"{title}\" any more.")
    }
}

/// The line `show` prints. herdr cannot say whether a window is open on
/// that machine, so it never promises more.
pub fn shown(title: &str, on: &str) -> String {
    format!("\"{title}\" is in front in herdr on {on}, if herdr is open there.")
}

/// The sessions as a table: followed or not, title, agent, status, key,
/// and whether pitupitu started it.
pub fn table(body: &Value, on: Option<&str>) -> String {
    let rows: Vec<[String; 6]> = all(body)
        .map(|s| {
            let text = |k: &str| s[k].as_str().unwrap_or("").to_string();
            [
                if s["followed"] == true { "●" } else { "○" }.to_string(),
                title(s),
                text("agent"),
                text("status"),
                text("key"),
                if s["task"] == true { "by pitupitu" } else { "" }.to_string(),
            ]
        })
        .collect();
    if rows.is_empty() {
        return match on {
            Some(b) => format!("No sessions on {b} right now.\n"),
            None => "No sessions on this box right now.\n".to_string(),
        };
    }
    let mut width = [0usize; 6];
    for r in &rows {
        for (w, c) in width.iter_mut().zip(r) {
            *w = (*w).max(c.chars().count());
        }
    }
    let mut out = String::new();
    for r in &rows {
        let mut line = String::new();
        for (i, (c, w)) in r.iter().zip(width).enumerate() {
            if i > 0 {
                line.push_str("  ");
            }
            line.push_str(c);
            line.extend(std::iter::repeat_n(' ', w - c.chars().count()));
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out.push_str(&format!("● followed: {FOLLOW_SAYS}.\n"));
    out
}

/// The lines that list several matches, for a person to pick from.
pub fn choices(hits: &[&Value]) -> String {
    hits.iter()
        .map(|s| format!("  {}  ({})\n", title(s), s["key"].as_str().unwrap_or("?")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body() -> Value {
        json!({"sessions": [
            {"key": "w1:p1", "title": "Fix the login page", "agent": "claude", "cwd": "/a",
             "status": "working", "followed": true, "task": true},
            {"key": "w2:p3", "title": "Login tests", "agent": "codex", "cwd": "/b",
             "status": "idle", "followed": false, "task": false},
            {"key": "w3:p1", "title": null, "agent": "claude", "cwd": "/c",
             "status": "blocked", "followed": false, "task": false}
        ]})
    }

    #[test]
    fn a_pane_id_then_every_word_of_the_title() {
        let b = body();
        assert_eq!(find(&b, "w2:p3"), Found::One(&b["sessions"][1]));
        assert_eq!(find(&b, "LOGIN page"), Found::One(&b["sessions"][0]));
        assert_eq!(find(&b, "tests login"), Found::One(&b["sessions"][1]));
        assert!(matches!(find(&b, "login"), Found::Many(h) if h.len() == 2));
        assert_eq!(find(&b, "deploy"), Found::None);
        assert_eq!(find(&b, "  "), Found::None);
        assert_eq!(find(&b, "w9:p9"), Found::None);
    }

    #[test]
    fn the_lines_people_read() {
        assert_eq!(
            said("Fix the login page", true),
            "Following \"Fix the login page\": pitupitu tells you when it finishes or needs you."
        );
        assert_eq!(said("x", false), "Not following \"x\" any more.");
        assert_eq!(
            shown("Login tests", "laptop"),
            "\"Login tests\" is in front in herdr on laptop, if herdr is open there."
        );
        let t = table(&body(), None);
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(
            lines[0],
            "●  Fix the login page  claude  working  w1:p1  by pitupitu"
        );
        assert_eq!(lines[1], "○  Login tests         codex   idle     w2:p3");
        assert_eq!(lines[2], "○  w3:p1               claude  blocked  w3:p1");
        assert!(lines[3].starts_with("● followed: "));
        assert_eq!(
            table(&json!({"sessions": []}), None),
            "No sessions on this box right now.\n"
        );
        assert_eq!(
            table(&json!({"sessions": []}), Some("laptop")),
            "No sessions on laptop right now.\n"
        );
    }
}
