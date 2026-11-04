//! `pitupitu follow`, `unfollow`, `sessions` and `show` end to end against
//! a stand-in cloud: what they ask, what they print, and the exit code
//! when they cannot tell which session is meant.

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const KEY: &str = "ptp_test";

fn sessions() -> Value {
    json!({"sessions": [
        {"key": "w1:p1", "title": "Fix the login page", "agent": "claude", "cwd": "/a",
         "status": "working", "followed": false, "task": true},
        {"key": "w2:p3", "title": "Login tests", "agent": "codex", "cwd": "/b",
         "status": "idle", "followed": true, "task": false}
    ]})
}

fn authorized(h: &HeaderMap) -> bool {
    h.get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {KEY}"))
}

/// The sessions of the box `laptop`.
fn on_laptop() -> Value {
    json!({"sessions": [
        {"key": "w7:p1", "title": "Deploy the site", "agent": "claude", "cwd": "/c",
         "status": "idle", "followed": false, "task": false}
    ]})
}

/// The cloud's routes; every `PATCH /v1/settings` and
/// `POST /v1/sessions/show` body is kept.
async fn cloud() -> (String, Arc<Mutex<Vec<Value>>>) {
    let patches = Arc::new(Mutex::new(Vec::new()));
    let app =
        Router::new()
            .route(
                "/v1/sessions",
                get(
                    |h: HeaderMap, Query(q): Query<HashMap<String, String>>| async move {
                        if !authorized(&h) {
                            return (
                                StatusCode::UNAUTHORIZED,
                                Json(json!({"error": "invalid_key"})),
                            );
                        }
                        match q.get("box").map(String::as_str) {
                            None => (StatusCode::OK, Json(sessions())),
                            Some("laptop") => (StatusCode::OK, Json(on_laptop())),
                            Some("empty") => (StatusCode::OK, Json(json!({"sessions": []}))),
                            Some(b) => (
                                StatusCode::BAD_REQUEST,
                                Json(json!({"error": {
                                    "code": "bad_request",
                                    "message": format!("this account has no box called {b}"),
                                }})),
                            ),
                        }
                    },
                ),
            )
            .route(
                "/v1/sessions/show",
                post(
                    |State(p): State<Arc<Mutex<Vec<Value>>>>,
                     h: HeaderMap,
                     Json(body): Json<Value>| async move {
                        assert!(authorized(&h));
                        p.lock().unwrap().push(body.clone());
                        if body["key"] == "w2:p3" {
                            return (
                                StatusCode::CONFLICT,
                                Json(json!({"error": {
                                    "code": "not_shown",
                                    "message": "pitupitu on testbox needs an update to do this.",
                                }})),
                            );
                        }
                        let on = body["box"].as_str().unwrap_or("testbox");
                        (StatusCode::OK, Json(json!({"key": body["key"], "box": on})))
                    },
                ),
            )
            .route(
                "/v1/settings",
                patch(
                    |State(p): State<Arc<Mutex<Vec<Value>>>>,
                     h: HeaderMap,
                     Json(body): Json<Value>| async move {
                        assert!(authorized(&h));
                        p.lock().unwrap().push(body);
                        Json(json!({"box": "testbox"}))
                    },
                ),
            )
            .with_state(patches.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, patches)
}

/// The binary with this box's key and the stand-in cloud, no terminal,
/// and `HERDR_PANE_ID` only when given.
async fn run(url: &str, pane: Option<&str>, args: &[&str]) -> (i32, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_pitupitu"));
    cmd.args(args)
        .env("PITUPITU_CONFIG", dir.path().join("pitupitu.toml"))
        .env("PITUPITU_CLOUD_URL", url)
        .env("PITUPITU_CLOUD_API_KEY", KEY)
        .env_remove("HERDR_PANE_ID")
        .stdin(std::process::Stdio::null());
    if let Some(p) = pane {
        cmd.env("HERDR_PANE_ID", p);
    }
    let out = cmd.output().await.unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[tokio::test]
async fn follow_the_session_this_runs_in_or_one_by_name() {
    let (url, patches) = cloud().await;

    let (code, _, err) = run(&url, None, &["follow"]).await;
    assert_eq!(code, 2);
    assert_eq!(
        err.trim(),
        "Run this inside a session in herdr, or name one: pitupitu follow <title>"
    );

    let (code, out, _) = run(&url, Some("w1:p1"), &["follow"]).await;
    assert_eq!(code, 0);
    assert_eq!(
        out.trim(),
        "Following \"Fix the login page\": pitupitu tells you when it finishes or needs you."
    );

    let (code, out, _) = run(&url, None, &["follow", "login", "TESTS", "--json"]).await;
    assert_eq!(code, 0);
    let printed: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        printed,
        json!({"key": "w2:p3", "title": "Login tests", "followed": true})
    );

    let (code, out, _) = run(&url, None, &["unfollow", "w2:p3"]).await;
    assert_eq!(code, 0);
    assert_eq!(out.trim(), "Not following \"Login tests\" any more.");
    let (code, _, _) = run(&url, Some("w1:p1"), &["follow", "--off"]).await;
    assert_eq!(code, 0);

    // Ambiguous or unknown: the choices, exit 2, nothing sent.
    let (code, _, err) = run(&url, None, &["follow", "login"]).await;
    assert_eq!(code, 2);
    assert!(err.contains("Fix the login page  (w1:p1)"), "{err}");
    assert!(err.contains("Login tests  (w2:p3)"), "{err}");
    let (code, _, err) = run(&url, None, &["follow", "deploy"]).await;
    assert_eq!(code, 2);
    assert!(err.contains("pitupitu sessions"), "{err}");

    assert_eq!(
        *patches.lock().unwrap(),
        [
            json!({"follow": {"key": "w1:p1", "on": true}}),
            json!({"follow": {"key": "w2:p3", "on": true}}),
            json!({"follow": {"key": "w2:p3", "on": false}}),
            json!({"follow": {"key": "w1:p1", "on": false}}),
        ]
    );
}

#[tokio::test]
async fn show_the_session_this_runs_in_or_one_by_name_on_any_box() {
    let (url, asked) = cloud().await;

    let (code, _, err) = run(&url, None, &["show"]).await;
    assert_eq!(code, 2);
    assert_eq!(
        err.trim(),
        "Run this inside a session in herdr, or name one: pitupitu show <title>"
    );

    let (code, out, _) = run(&url, Some("w1:p1"), &["show"]).await;
    assert_eq!(code, 0);
    assert_eq!(
        out.trim(),
        "\"Fix the login page\" is in front in herdr on testbox, if herdr is open there."
    );

    let (code, out, _) = run(&url, None, &["show", "deploy", "--box", "laptop", "--json"]).await;
    assert_eq!(code, 0);
    assert_eq!(
        serde_json::from_str::<Value>(&out).unwrap(),
        json!({"key": "w7:p1", "title": "Deploy the site", "box": "laptop"})
    );

    // Refused: the cloud's own sentence, not a status code.
    let (code, _, err) = run(&url, None, &["show", "login", "tests"]).await;
    assert_eq!(code, 1);
    assert_eq!(
        err.trim(),
        "Error: pitupitu on testbox needs an update to do this."
    );

    // Unclear which: exit 2, nothing sent beyond the three above.
    let (code, _, err) = run(&url, None, &["show", "login"]).await;
    assert_eq!(code, 2);
    assert!(err.contains("Login tests  (w2:p3)"), "{err}");
    let (code, _, err) = run(&url, None, &["show", "login", "--box", "laptop"]).await;
    assert_eq!(code, 2);
    assert!(err.contains("pitupitu sessions --box laptop"), "{err}");
    let (code, _, err) = run(&url, Some("w1:p1"), &["show", "--box", "laptop"]).await;
    assert_eq!(code, 2);
    assert!(err.contains("Name the session on laptop"), "{err}");
    let (code, _, err) = run(&url, None, &["show", "x", "--box", "nope"]).await;
    assert_eq!(code, 1);
    assert!(err.contains("no box called nope"), "{err}");

    assert_eq!(
        *asked.lock().unwrap(),
        [
            json!({"key": "w1:p1"}),
            json!({"key": "w7:p1", "box": "laptop"}),
            json!({"key": "w2:p3"}),
        ]
    );
}

#[tokio::test]
async fn sessions_of_another_box_by_name() {
    let (url, _) = cloud().await;
    let (code, out, _) = run(&url, None, &["sessions", "--box", "laptop"]).await;
    assert_eq!(code, 0);
    assert!(
        out.starts_with("○  Deploy the site  claude  idle  w7:p1"),
        "{out}"
    );
    let (code, out, _) = run(&url, None, &["sessions", "--box", "empty"]).await;
    assert_eq!(code, 0);
    assert_eq!(out, "No sessions on empty right now.\n");
}

#[tokio::test]
async fn sessions_as_a_table_or_as_the_cloud_sent_them() {
    let (url, _) = cloud().await;
    let (code, out, _) = run(&url, None, &["sessions", "--json"]).await;
    assert_eq!(code, 0);
    assert_eq!(serde_json::from_str::<Value>(&out).unwrap(), sessions());

    let (code, out, _) = run(&url, None, &["sessions"]).await;
    assert_eq!(code, 0);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines[0],
        "○  Fix the login page  claude  working  w1:p1  by pitupitu"
    );
    assert_eq!(lines[1], "●  Login tests         codex   idle     w2:p3");
}
