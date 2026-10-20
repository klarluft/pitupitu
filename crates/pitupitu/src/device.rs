//! Signing this box in to pitupitu cloud with a device code (RFC 8628):
//! the box is often headless, so it shows a short code and a URL, the
//! person approves on any screen, and the box polls until the key is
//! issued. Payment never happens here.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::time::Duration;

/// Never ask the cloud more often than this, whatever it says.
const MIN_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Debug, Deserialize)]
struct DeviceCode {
    device_code: String,
    user_code: String,
    verification_url_complete: String,
    expires_in: u64,
    interval: u64,
}

#[derive(Debug, Deserialize)]
struct TokenError {
    error: String,
}

/// What the cloud hands over once the person approved the box.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Issued {
    pub api_key: String,
    pub email: String,
    #[serde(rename = "box")]
    pub box_name: String,
}

/// `GET /v1/me`.
#[derive(Debug, Clone, Deserialize)]
pub struct Me {
    pub email: String,
    pub plan: String,
    pub balance_seconds: i64,
    #[serde(rename = "box")]
    pub box_name: String,
}

impl Me {
    pub fn describe(&self) -> String {
        let minutes = self.balance_seconds.max(0) as f64 / 60.0;
        format!(
            "signed in as {}\nbox: {}\nplan: {}\nvoice minutes left: {minutes:.0}",
            self.email, self.box_name, self.plan
        )
    }
}

/// Ask for a device code, show it, poll until the person decides. Prints
/// to stderr. `open_browser` tries the desktop's opener when there is a
/// display; failing that is fine, the URL is on screen.
pub async fn login(url: &str, box_name: &str, open_browser: bool) -> Result<Issued> {
    let base = url.trim_end_matches('/');
    let http = reqwest::Client::new();
    let resp = http
        .post(format!("{base}/v1/device/code"))
        .json(&serde_json::json!({ "box": box_name }))
        .send()
        .await
        .with_context(|| format!("reaching {base}"))?;
    if !resp.status().is_success() {
        bail!(
            "{base}/v1/device/code returned {}: {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        );
    }
    let code: DeviceCode = resp.json().await.context("device code response")?;

    eprintln!();
    eprintln!("  Sign in to pitupitu on any screen and enter this code:");
    eprintln!();
    eprintln!("      {}", code.user_code);
    eprintln!();
    eprintln!("  {}", code.verification_url_complete);
    eprintln!();
    eprintln!("  waiting for you to approve it (press ctrl-c to stop) …");
    if open_browser {
        try_open(&code.verification_url_complete);
    }

    let mut interval = Duration::from_secs(code.interval).max(MIN_INTERVAL);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(code.expires_in);
    loop {
        tokio::time::sleep(interval).await;
        if tokio::time::Instant::now() > deadline {
            bail!("the code expired before it was approved; run `pitupitu setup` again");
        }
        let resp = http
            .post(format!("{base}/v1/device/token"))
            .json(&serde_json::json!({ "device_code": code.device_code }))
            .send()
            .await
            .with_context(|| format!("reaching {base}"))?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if status.is_success() {
            return serde_json::from_str::<Issued>(&body).context("token response");
        }
        let error = serde_json::from_str::<TokenError>(&body)
            .map(|e| e.error)
            .unwrap_or_else(|_| format!("{status}"));
        match error.as_str() {
            "authorization_pending" => {}
            "slow_down" => interval += Duration::from_secs(1),
            "access_denied" => {
                bail!("this machine was not approved; run `pitupitu setup` to try again")
            }
            "expired_token" => {
                bail!("the code expired before it was approved; run `pitupitu setup` again")
            }
            other => bail!("{base}/v1/device/token: {other}"),
        }
    }
}

fn try_open(url: &str) {
    let has_display = cfg!(target_os = "macos")
        || std::env::var_os("DISPLAY").is_some()
        || std::env::var_os("WAYLAND_DISPLAY").is_some();
    if !has_display {
        return;
    }
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(opener)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Why `me` failed: the key is refused, or the cloud could not be asked.
#[derive(Debug)]
pub enum MeError {
    Rejected,
    Other(anyhow::Error),
}

impl std::fmt::Display for MeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MeError::Rejected => {
                f.write_str("pitupitu cloud rejected this box's key; run `pitupitu setup --relink`")
            }
            MeError::Other(e) => write!(f, "{e:#}"),
        }
    }
}

/// `GET /v1/me` with the box's key.
pub async fn me(url: &str, api_key: &str) -> std::result::Result<Me, MeError> {
    let base = url.trim_end_matches('/');
    let resp = reqwest::Client::new()
        .get(format!("{base}/v1/me"))
        .bearer_auth(api_key)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .with_context(|| format!("reaching {base}"))
        .map_err(MeError::Other)?;
    match resp.status().as_u16() {
        200 => resp
            .json()
            .await
            .context("/v1/me response")
            .map_err(MeError::Other),
        401 => Err(MeError::Rejected),
        s => Err(MeError::Other(anyhow::anyhow!(
            "{base}/v1/me returned {s}: {}",
            resp.text().await.unwrap_or_default()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use serde_json::{json, Value};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    const KEY: &str = "ptp_0123456789abcdefghijklmnopqrstuvwxyzABCD";

    /// A cloud that approves on the third poll and issues the key once.
    async fn mock_cloud() -> String {
        let polls = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route(
                "/v1/device/code",
                post(|Json(body): Json<Value>| async move {
                    assert_eq!(body["box"], "testbox");
                    Json(json!({
                        "device_code": "dev-secret",
                        "user_code": "BCDF-GHJK",
                        "verification_url": "https://pitupitu.chat/device",
                        "verification_url_complete": "https://pitupitu.chat/device?code=BCDF-GHJK",
                        "expires_in": 900,
                        "interval": 0
                    }))
                }),
            )
            .route(
                "/v1/device/token",
                post(
                    |State(polls): State<Arc<AtomicUsize>>, Json(body): Json<Value>| async move {
                        assert_eq!(body["device_code"], "dev-secret");
                        let (status, body) = match polls.fetch_add(1, Ordering::SeqCst) {
                            0 => (
                                StatusCode::PRECONDITION_REQUIRED,
                                json!({"error": "authorization_pending"}),
                            ),
                            1 => (StatusCode::TOO_MANY_REQUESTS, json!({"error": "slow_down"})),
                            2 => (
                                StatusCode::OK,
                                json!({"api_key": KEY, "email": "m@example.com", "box": "testbox"}),
                            ),
                            _ => (StatusCode::NOT_FOUND, json!({"error": "not_found"})),
                        };
                        (status, Json(body))
                    },
                ),
            )
            .route(
                "/v1/me",
                get(|headers: axum::http::HeaderMap| async move {
                    let auth = headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("");
                    if auth == format!("Bearer {KEY}") {
                        (
                            StatusCode::OK,
                            Json(json!({"email": "m@example.com", "plan": "free",
                                "balance_seconds": 1800, "box": "testbox", "workers_available": true})),
                        )
                    } else {
                        (
                            StatusCode::UNAUTHORIZED,
                            Json(json!({"error": "invalid_key"})),
                        )
                    }
                }),
            )
            .with_state(polls);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn the_key_is_issued_once_and_works() {
        let url = mock_cloud().await;
        let issued = login(&url, "testbox", false).await.unwrap();
        assert_eq!(
            issued,
            Issued {
                api_key: KEY.into(),
                email: "m@example.com".into(),
                box_name: "testbox".into()
            }
        );
        let err = login(&url, "testbox", false).await.unwrap_err();
        assert!(err.to_string().contains("not_found"), "{err}");

        let account = me(&url, KEY).await.unwrap();
        assert!(account.describe().contains("minutes left: 30"));
        assert!(matches!(
            me(&url, "ptp_wrong").await,
            Err(MeError::Rejected)
        ));
    }
}
