//! `pitupitu follow`, `unfollow` and `sessions` end to end against a
//! stand-in cloud: what they ask, what they print, and the exit code
//! when they cannot tell which session is meant.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, patch};
use axum::{Json, Router};
use serde_json::{json, Value};
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

/// The cloud's two routes; every `PATCH /v1/settings` body is kept.
async fn cloud() -> (String, Arc<Mutex<Vec<Value>>>) {
    let patches = Arc::new(Mutex::new(Vec::new()));
    let app =
        Router::new()
            .route(
                "/v1/sessions",
                get(|h: HeaderMap| async move {
                    if authorized(&h) {
                        (StatusCode::OK, Json(sessions()))
                    } else {
                        (
                            StatusCode::UNAUTHORIZED,
                            Json(json!({"error": "invalid_key"})),
                        )
                    }
                }),
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
