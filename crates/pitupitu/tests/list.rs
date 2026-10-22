//! `pitupitu todo`, `idea`, `remind`, `schedule` and `when` end to end
//! against a stand-in cloud: what they send, what they print, and the exit
//! code when they cannot tell which item is meant or cannot ask.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{Arc, Mutex};

const KEY: &str = "ptp_test";

/// What the stand-in cloud was asked: method, path with query, body.
type Asked = Arc<Mutex<Vec<(String, String, Value)>>>;

fn todos() -> Value {
    json!({"todos": [
        {"id": "3f9a2c11-0000-4000-8000-000000000001", "short": "3f9a2c", "kind": "todo",
         "text": "Call the bank", "status": "open", "project": null,
         "remind_at": "2026-10-23T09:00:00+02:00", "remind_words": "Thu 23 Oct, 09:00"},
        {"id": "3f9b0d22-0000-4000-8000-000000000002", "short": "3f9b0d", "kind": "idea",
         "text": "A bank holiday calendar", "status": "open",
         "project": {"key": "github.com/Org/acme-api", "name": "acme-api"}},
        {"id": "aa11bb33-0000-4000-8000-000000000003", "short": "aa11bb", "kind": "todo",
         "text": "Renew the domain", "status": "done", "project": null}
    ]})
}

fn automations() -> Value {
    json!({"automations": [
        {"id": "b71e0400-0000-4000-8000-000000000001", "short": "b71e04", "title": "Call the bank",
         "what": "reminder", "words": "Thu 23 Oct, 09:00", "enabled": true, "paused_reason": null,
         "action": {"kind": "remind", "text": "Call the bank"}},
        {"id": "c0ffee00-0000-4000-8000-000000000002", "short": "c0ffee",
         "title": "Nightly dependency check", "what": "session",
         "words": "every weekday at 09:00", "enabled": false, "paused_reason": "unseen",
         "action": {"kind": "start_session", "box": "laptop", "place": "acme-api",
                    "agent": "claude", "prompt": "check the dependencies"}},
        {"id": "c0ffee11-0000-4000-8000-000000000003", "short": "c0ffe1",
         "title": "Weekly dependency report", "what": "session",
         "words": "every monday at 08:00", "enabled": true, "paused_reason": null,
         "action": {"kind": "start_session", "box": "laptop", "place": "acme-api",
                    "prompt": "report"}}
    ]})
}

fn error(status: StatusCode, kind: &str, message: &str, extra: Value) -> Response {
    let mut e = json!({"type": kind, "message": message});
    if let Value::Object(m) = extra {
        e.as_object_mut().unwrap().extend(m);
    }
    (status, Json(json!({"error": e}))).into_response()
}

async fn answer(
    State(asked): State<Asked>,
    method: Method,
    uri: Uri,
    h: HeaderMap,
    body: Bytes,
) -> Response {
    if h.get("authorization").and_then(|v| v.to_str().ok()) != Some(&format!("Bearer {KEY}")) {
        return error(
            StatusCode::UNAUTHORIZED,
            "invalid_key",
            "bad key",
            json!(null),
        );
    }
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let path = uri.path().to_string();
    let full = uri.path_and_query().unwrap().to_string();
    asked
        .lock()
        .unwrap()
        .push((method.to_string(), full, body.clone()));
    let id = path.rsplit('/').next().unwrap_or("").to_string();
    match (method.as_str(), path.as_str()) {
        ("GET", "/v1/todos") => Json(todos()).into_response(),
        ("POST", "/v1/todos") => {
            if body["text"] == "too many" {
                return error(
                    StatusCode::CONFLICT,
                    "limit",
                    "The list has 500 open items; mark some done first.",
                    json!(null),
                );
            }
            let t = json!({"id": "d00d0000-0000-4000-8000-000000000009", "short": "d00d00",
                "kind": body["kind"], "text": body["text"], "status": "open"});
            (StatusCode::CREATED, Json(t)).into_response()
        }
        ("PATCH", p) if p.starts_with("/v1/todos/") => {
            if id == "abcd" {
                return error(
                    StatusCode::CONFLICT,
                    "ambiguous",
                    "More than one item starts with abcd.",
                    json!({"matches": ["abcd01", "abcd02"]}),
                );
            }
            if id == "ffff" {
                return error(
                    StatusCode::NOT_FOUND,
                    "not_found",
                    "Nothing on the list has that id.",
                    json!(null),
                );
            }
            let t = todos()["todos"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["id"] == id.as_str())
                .cloned();
            match t {
                Some(mut t) => {
                    t["status"] = body["status"].clone();
                    Json(t).into_response()
                }
                None => error(StatusCode::NOT_FOUND, "not_found", "no", json!(null)),
            }
        }
        ("DELETE", _) => StatusCode::NO_CONTENT.into_response(),
        ("GET", "/v1/automations") => Json(automations()).into_response(),
        ("POST", "/v1/automations") => {
            if body["when"] == "sometime" {
                return error(
                    StatusCode::BAD_REQUEST,
                    "bad_when",
                    "I did not understand \"sometime\"; try \"in 20 minutes\", \"tomorrow 9am\" or \"every monday 8:30\".",
                    json!(null),
                );
            }
            let what = match body["action"]["kind"].as_str() {
                Some("remind") => "reminder",
                Some("digest") => "digest",
                _ => "session",
            };
            let a = json!({"id": "e1e1e100-0000-4000-8000-000000000009", "short": "e1e1e1",
                "title": body["title"].as_str().unwrap_or("made"), "what": what,
                "words": "Thu 23 Oct, 09:00", "enabled": true, "action": body["action"]});
            (StatusCode::CREATED, Json(a)).into_response()
        }
        ("PATCH", p) if p.starts_with("/v1/automations/") => {
            let mut a = automations()["automations"]
                .as_array()
                .unwrap()
                .iter()
                .find(|a| a["id"] == id.as_str())
                .cloned()
                .unwrap();
            if let Some(e) = body["enabled"].as_bool() {
                a["enabled"] = json!(e);
                a["paused_reason"] = if e { json!(null) } else { json!("by_person") };
            }
            if body["snooze"].is_string() {
                a["words"] = json!("every monday at 08:00, next Tue 27 Oct, 10:00");
            }
            Json(a).into_response()
        }
        ("POST", "/v1/when") => Json(
            json!({"schedule": {"kind": "once", "at": "2026-10-23T09:00:00+02:00"},
            "words": format!("({}) Thu 23 Oct, 09:00", body["when"].as_str().unwrap_or("")),
            "next_run_at": "2026-10-23T09:00:00+02:00"}),
        )
        .into_response(),
        _ => error(StatusCode::NOT_FOUND, "not_found", "no route", json!(null)),
    }
}

async fn cloud() -> (String, Asked) {
    let asked: Asked = Arc::default();
    let app = Router::new().fallback(answer).with_state(asked.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, asked)
}

struct Run<'a> {
    url: &'a str,
    dir: Option<&'a Path>,
    pane: Option<&'a str>,
}

impl Run<'_> {
    /// The binary as box "testbox" with the stand-in cloud and no terminal.
    async fn go(&self, args: &[&str]) -> (i32, String, String) {
        let conf = tempfile::tempdir().unwrap();
        let file = conf.path().join("pitupitu.toml");
        std::fs::write(&file, "[cloud]\nbox = \"testbox\"\n").unwrap();
        let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_pitupitu"));
        cmd.args(args)
            .env("PITUPITU_CONFIG", &file)
            .env("PITUPITU_CLOUD_URL", self.url)
            .env("PITUPITU_CLOUD_API_KEY", KEY)
            .env_remove("HERDR_PANE_ID")
            .stdin(std::process::Stdio::null());
        if let Some(d) = self.dir {
            cmd.current_dir(d);
        }
        if let Some(p) = self.pane {
            cmd.env("HERDR_PANE_ID", p);
        }
        let out = cmd.output().await.unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

fn run(url: &str) -> Run<'_> {
    Run {
        url,
        dir: None,
        pane: None,
    }
}

/// The requests other than the reads used to find an item.
fn writes(asked: &Asked) -> Vec<(String, String, Value)> {
    let all = std::mem::take(&mut *asked.lock().unwrap());
    all.into_iter().filter(|(m, _, _)| m != "GET").collect()
}

#[tokio::test]
async fn the_list_and_its_filters() {
    let (url, asked) = cloud().await;
    let (code, out, _) = run(&url).go(&["todo"]).await;
    assert_eq!(code, 0);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        [
            "3f9a2c  Call the bank  (reminder Thu 23 Oct, 09:00)",
            "3f9b0d  A bank holiday calendar  (idea, acme-api)",
            "aa11bb  Renew the domain  (done)",
        ]
    );
    let (code, out, _) = run(&url).go(&["todo", "list", "--json"]).await;
    assert_eq!(code, 0);
    assert_eq!(serde_json::from_str::<Value>(&out).unwrap(), todos());
    run(&url)
        .go(&["todo", "--done", "--ideas", "bank", "day"])
        .await;
    run(&url).go(&["todo", "list", "--all"]).await;
    let paths: Vec<String> = std::mem::take(&mut *asked.lock().unwrap())
        .into_iter()
        .map(|(_, p, _)| p)
        .collect();
    assert_eq!(
        paths,
        [
            "/v1/todos?status=open",
            "/v1/todos?status=open",
            "/v1/todos?status=done&kind=idea&q=bank+day",
            "/v1/todos?status=all",
        ]
    );
}

#[tokio::test]
async fn adding_to_the_list() {
    let (url, asked) = cloud().await;
    let (code, out, _) = run(&url).go(&["todo", "add", "buy", "milk"]).await;
    assert_eq!(code, 0);
    assert_eq!(out.trim(), "Added d00d00: buy milk");
    let (code, out, _) = run(&url)
        .go(&["idea", "a", "quieter", "fan", "--project", "acme", "--json"])
        .await;
    assert_eq!(code, 0);
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap()["short"],
        "d00d00"
    );
    assert_eq!(
        writes(&asked),
        [
            (
                "POST".into(),
                "/v1/todos".into(),
                json!({"text": "buy milk", "kind": "todo", "via": "agent"})
            ),
            (
                "POST".into(),
                "/v1/todos".into(),
                json!({"text": "a quieter fan", "kind": "idea", "project": "acme", "via": "agent"})
            ),
        ]
    );

    // Refused: the cloud's words, exit 1.
    let (code, out, err) = run(&url).go(&["todo", "add", "too", "many"]).await;
    assert_eq!(code, 1);
    assert_eq!(out, "");
    assert_eq!(
        err.trim(),
        "The list has 500 open items; mark some done first."
    );
}

fn git(dir: &Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?}");
}

#[tokio::test]
async fn here_is_the_repository_or_the_folder() {
    if std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let (url, asked) = cloud().await;
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path().canonicalize().unwrap();
    git(&root, &["init", "-q"]);
    git(
        &root,
        &[
            "remote",
            "add",
            "origin",
            "https://me:s3cret@GitHub.com/Org/acme-api.git",
        ],
    );
    let sub = root.join("src");
    std::fs::create_dir(&sub).unwrap();
    let at = |dir| Run {
        url: &url,
        dir: Some(dir),
        pane: None,
    };
    let (code, _, _) = at(&sub).go(&["todo", "add", "fix", "ci", "--here"]).await;
    assert_eq!(code, 0);
    at(&sub).go(&["todo", "--here"]).await;

    let plain = tempfile::tempdir().unwrap();
    let plain = plain.path().canonicalize().unwrap();
    at(&plain).go(&["idea", "--here", "notes"]).await;
    at(&plain).go(&["todo", "list", "--here"]).await;

    let asked = std::mem::take(&mut *asked.lock().unwrap());
    let root_s = root.to_string_lossy();
    let plain_s = plain.to_string_lossy();
    assert_eq!(
        asked[0].2,
        json!({"text": "fix ci", "kind": "todo", "remote": "https://GitHub.com/Org/acme-api.git",
               "cwd": root_s, "via": "agent"})
    );
    assert_eq!(
        asked[1].1,
        "/v1/todos?status=open&project=github.com%2FOrg%2Facme-api"
    );
    assert_eq!(
        asked[2].2,
        json!({"text": "notes", "kind": "idea", "cwd": plain_s, "via": "agent"})
    );
    let key: String = url_encode(&format!("testbox:{plain_s}"));
    assert_eq!(asked[3].1, format!("/v1/todos?status=open&project={key}"));
}

fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'*' => {
                (b as char).to_string()
            }
            b' ' => "+".into(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[tokio::test]
async fn done_drop_reopen_rm_by_id_or_words() {
    let (url, asked) = cloud().await;
    let (code, out, _) = run(&url).go(&["todo", "done", "3f9a"]).await;
    assert_eq!(code, 0);
    assert_eq!(out.trim(), "Done 3f9a2c: Call the bank");
    let (code, out, _) = run(&url).go(&["todo", "drop", "HOLIDAY"]).await;
    assert_eq!(code, 0);
    assert_eq!(out.trim(), "Dropped 3f9b0d: A bank holiday calendar");
    // `reopen` looks among the done ones.
    let (code, out, _) = run(&url).go(&["todo", "reopen", "domain", "--json"]).await;
    assert_eq!(code, 0);
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap()["status"],
        "open"
    );
    let (code, out, _) = run(&url).go(&["todo", "rm", "call", "bank"]).await;
    assert_eq!(code, 0);
    assert_eq!(out.trim(), "Removed 3f9a2c: Call the bank");
    let (_, out, _) = run(&url).go(&["todo", "rm", "3f9b0d", "--json"]).await;
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap(),
        json!({"removed": "3f9b0d22-0000-4000-8000-000000000002"})
    );
    let w: Vec<(String, String, Value)> = writes(&asked);
    assert_eq!(
        w,
        [
            (
                "PATCH".into(),
                "/v1/todos/3f9a2c11-0000-4000-8000-000000000001".into(),
                json!({"status": "done"})
            ),
            (
                "PATCH".into(),
                "/v1/todos/3f9b0d22-0000-4000-8000-000000000002".into(),
                json!({"status": "dropped"})
            ),
            (
                "PATCH".into(),
                "/v1/todos/aa11bb33-0000-4000-8000-000000000003".into(),
                json!({"status": "open"})
            ),
            (
                "DELETE".into(),
                "/v1/todos/3f9a2c11-0000-4000-8000-000000000001".into(),
                Value::Null
            ),
            (
                "DELETE".into(),
                "/v1/todos/3f9b0d22-0000-4000-8000-000000000002".into(),
                Value::Null
            ),
        ]
    );

    // More than one match: the choices, exit 2, nothing sent.
    let (code, _, err) = run(&url).go(&["todo", "done", "bank"]).await;
    assert_eq!(code, 2);
    assert!(err.contains("  3f9a2c  Call the bank\n"), "{err}");
    assert!(err.contains("  3f9b0d  A bank holiday calendar\n"), "{err}");
    // Done ones are not matched by words for `done`.
    let (code, _, err) = run(&url).go(&["todo", "done", "domain"]).await;
    assert_eq!(code, 2);
    assert!(
        err.contains("Nothing on the list matches \"domain\""),
        "{err}"
    );
    assert!(writes(&asked).is_empty());

    // An id it does not know goes to the cloud, which decides.
    let (code, _, err) = run(&url).go(&["todo", "done", "abcd"]).await;
    assert_eq!(code, 2);
    assert_eq!(
        err.trim(),
        "More than one item starts with abcd.\n  abcd01\n  abcd02"
    );
    let (code, _, err) = run(&url).go(&["todo", "done", "ffff"]).await;
    assert_eq!(code, 1);
    assert_eq!(err.trim(), "Nothing on the list has that id.");
}

#[tokio::test]
async fn reminders_and_times() {
    let (url, asked) = cloud().await;
    let (code, out, _) = run(&url)
        .go(&["remind", "tomorrow 9am", "call", "the", "bank"])
        .await;
    assert_eq!(code, 0);
    assert_eq!(
        out.trim(),
        "Reminder e1e1e1: Thu 23 Oct, 09:00: call the bank"
    );
    let (code, _, _) = run(&url)
        .go(&["remind", "20m", "check", "--todo", "3f9a2c", "--json"])
        .await;
    assert_eq!(code, 0);
    let (code, out, _) = run(&url).go(&["when", "tomorrow", "9am"]).await;
    assert_eq!(code, 0);
    assert_eq!(out.trim(), "(tomorrow 9am) Thu 23 Oct, 09:00");
    let (code, out, _) = run(&url).go(&["when", "--json", "9am"]).await;
    assert_eq!(code, 0);
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap()["next_run_at"],
        "2026-10-23T09:00:00+02:00"
    );
    let (code, _, err) = run(&url).go(&["remind", "sometime", "x"]).await;
    assert_eq!(code, 1);
    assert!(
        err.starts_with("I did not understand \"sometime\""),
        "{err}"
    );
    let w = writes(&asked);
    assert_eq!(
        w[0].2,
        json!({"when": "tomorrow 9am", "action": {"kind": "remind", "text": "call the bank"},
               "via": "agent"})
    );
    assert_eq!(
        w[1].2,
        json!({"when": "20m", "action": {"kind": "remind", "text": "check"}, "via": "agent",
               "todo": "3f9a2c"})
    );
    assert_eq!(
        w[2],
        (
            "POST".into(),
            "/v1/when".into(),
            json!({"when": "tomorrow 9am"})
        )
    );
}

#[tokio::test]
async fn schedules_need_a_yes() {
    let (url, asked) = cloud().await;
    // No terminal and no --yes: exit 2, nothing asked of the cloud.
    let (code, out, err) = run(&url)
        .go(&[
            "schedule",
            "add",
            "every weekday 9am",
            "--place",
            "acme",
            "--",
            "check",
            "deps",
        ])
        .await;
    assert_eq!(code, 2);
    assert_eq!(out, "");
    assert!(err.contains("--yes"), "{err}");
    let (code, _, _) = run(&url)
        .go(&["schedule", "digest", "every", "day", "6pm"])
        .await;
    assert_eq!(code, 2);
    assert!(asked.lock().unwrap().is_empty());

    let r = Run {
        url: &url,
        dir: None,
        pane: Some("w2:p1"),
    };
    let (code, out, _) = r
        .go(&[
            "schedule",
            "add",
            "every weekday 9am",
            "--place",
            "acme",
            "--agent",
            "Claude",
            "--title",
            "Deps",
            "--yes",
            "--",
            "check",
            "the",
            "deps",
        ])
        .await;
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        out.trim(),
        "Scheduled e1e1e1  Thu 23 Oct, 09:00  session: Deps"
    );
    let (code, out, _) = run(&url)
        .go(&[
            "schedule", "digest", "every", "day", "6pm", "--yes", "--json",
        ])
        .await;
    assert_eq!(code, 0);
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap()["what"],
        "digest"
    );
    assert_eq!(
        writes(&asked),
        [
            (
                "POST".into(),
                "/v1/automations".into(),
                json!({"when": "every weekday 9am",
                       "action": {"kind": "start_session", "place": "acme",
                                  "prompt": "check the deps", "agent": "claude"},
                       "via": "agent", "title": "Deps", "pane": "w2:p1"})
            ),
            (
                "POST".into(),
                "/v1/automations".into(),
                json!({"when": "every day 6pm", "action": {"kind": "digest"}, "via": "agent"})
            ),
        ]
    );
}

#[tokio::test]
async fn the_schedules_and_changing_them() {
    let (url, asked) = cloud().await;
    let (code, out, _) = run(&url).go(&["schedule"]).await;
    assert_eq!(code, 0);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        [
            "b71e04  Thu 23 Oct, 09:00  reminder: Call the bank",
            "c0ffee  every weekday at 09:00  session: Nightly dependency check  \
             (paused: nobody looked at its last three sessions)",
            "c0ffe1  every monday at 08:00  session: Weekly dependency report",
        ]
    );
    let (_, out, _) = run(&url).go(&["schedule", "list", "--json"]).await;
    assert_eq!(serde_json::from_str::<Value>(&out).unwrap(), automations());

    // Words among the running ones for pause: only the weekly one runs.
    let (code, out, _) = run(&url).go(&["schedule", "pause", "dependency"]).await;
    assert_eq!(code, 0);
    assert!(out.starts_with("Paused c0ffe1  "), "{out}");
    // Resuming a session schedule asks first; without a terminal, --yes.
    let (code, _, err) = run(&url).go(&["schedule", "resume", "nightly"]).await;
    assert_eq!(code, 2);
    assert!(err.contains("--yes"), "{err}");
    let (code, out, _) = run(&url)
        .go(&["schedule", "resume", "nightly", "--yes"])
        .await;
    assert_eq!(code, 0);
    assert!(
        out.starts_with("Resumed c0ffee  every weekday at 09:00  session: Nightly"),
        "{out}"
    );
    let (code, out, _) = run(&url)
        .go(&["schedule", "snooze", "weekly", "report", "tomorrow 10am"])
        .await;
    assert_eq!(code, 0);
    assert!(out.contains("next Tue 27 Oct, 10:00"), "{out}");
    let (code, out, _) = run(&url).go(&["schedule", "rm", "b71e"]).await;
    assert_eq!(code, 0);
    assert_eq!(
        out.trim(),
        "Removed b71e04  Thu 23 Oct, 09:00  reminder: Call the bank"
    );
    // Two titles match: the choices, exit 2.
    let (code, _, err) = run(&url).go(&["schedule", "rm", "dependency"]).await;
    assert_eq!(code, 2);
    assert!(
        err.contains("  c0ffee  Nightly dependency check\n"),
        "{err}"
    );
    assert!(
        err.contains("  c0ffe1  Weekly dependency report\n"),
        "{err}"
    );

    assert_eq!(
        writes(&asked),
        [
            (
                "PATCH".into(),
                "/v1/automations/c0ffee11-0000-4000-8000-000000000003".into(),
                json!({"enabled": false})
            ),
            (
                "PATCH".into(),
                "/v1/automations/c0ffee00-0000-4000-8000-000000000002".into(),
                json!({"enabled": true})
            ),
            (
                "PATCH".into(),
                "/v1/automations/c0ffee11-0000-4000-8000-000000000003".into(),
                json!({"snooze": "tomorrow 10am"})
            ),
            (
                "DELETE".into(),
                "/v1/automations/b71e0400-0000-4000-8000-000000000001".into(),
                Value::Null
            ),
        ]
    );
}
