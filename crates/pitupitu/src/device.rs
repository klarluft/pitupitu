//! Signing this box in to pitupitu cloud with a device code (RFC 8628):
//! the box is often headless, so it shows a short code and a URL, the
//! person approves on any screen, and the box polls until the key is
//! issued. Payment never happens here.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::io::IsTerminal;
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

/// How the sign-in link is offered besides printing it.
#[derive(Debug, Clone, Copy)]
pub struct Offer {
    /// Try the desktop's opener when there is a display; failing that is
    /// fine, the URL is on screen.
    pub open_browser: bool,
    /// Draw the link as a QR code too, for a phone's camera, when stderr
    /// is a terminal that can draw it (`qr_wanted`).
    pub qr: bool,
}

/// Ask for a device code, show it, poll until the person decides. Prints
/// to stderr.
pub async fn login(url: &str, box_name: &str, offer: Offer) -> Result<Issued> {
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
    let no_color = std::env::var("NO_COLOR").ok();
    let term = std::env::var("TERM").ok();
    if qr_wanted(
        offer.qr,
        std::io::stderr().is_terminal(),
        no_color.as_deref(),
        term.as_deref(),
    ) {
        if let Some(lines) = qr_lines(&code.verification_url_complete) {
            eprintln!("  or scan this with your phone's camera:");
            eprintln!();
            for line in lines {
                eprintln!("  {line}");
            }
            eprintln!();
        }
    }
    eprintln!("  {}", code.verification_url_complete);
    eprintln!();
    eprintln!("  waiting for you to approve it (press ctrl-c to stop) …");
    if offer.open_browser {
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

/// Whether to draw the QR code: the person did not say `--no-qr`, and
/// stderr is a terminal that can draw it: not a pipe or a log file, not
/// `TERM=dumb`, and not one where colour was switched off (`NO_COLOR`,
/// see no-color.org), since the code is drawn with colours.
fn qr_wanted(asked: bool, stderr_is_tty: bool, no_color: Option<&str>, term: Option<&str>) -> bool {
    asked && stderr_is_tty && no_color.is_none_or(str::is_empty) && term != Some("dumb")
}

/// Light modules around the code on every side. The standard asks for 4;
/// 2 reads as well on a screen and keeps the code short enough for a
/// 24-line terminal.
const QUIET_ZONE: usize = 2;

/// `url` as a QR code, two modules per character cell with half blocks.
///
/// Phone cameras read dark-on-light codes best, and terminals are dark
/// as often as light, so the colours are not left to the terminal: every
/// line is black on bright white (`ESC[30;107m`), dark modules are drawn
/// in the foreground (`▀`, `▄`, `█`) and light ones, the quiet zone
/// included, are the white background. That looks the same on a dark and
/// a light theme. Error correction L: a screen is not scuffed, and the
/// lowest level gives the smallest code (29×29 for a sign-in link).
fn qr_lines(url: &str) -> Option<Vec<String>> {
    let code = qrcode::QrCode::with_error_correction_level(url, qrcode::EcLevel::L).ok()?;
    let width = code.width();
    let colors = code.to_colors();
    let size = width + 2 * QUIET_ZONE;
    let dark = |x: usize, y: usize| {
        let (Some(x), Some(y)) = (x.checked_sub(QUIET_ZONE), y.checked_sub(QUIET_ZONE)) else {
            return false;
        };
        x < width && y < width && colors[y * width + x] == qrcode::Color::Dark
    };
    let lines = (0..size)
        .step_by(2)
        .map(|y| {
            let row: String = (0..size)
                .map(|x| match (dark(x, y), dark(x, y + 1)) {
                    (true, true) => '█',
                    (true, false) => '▀',
                    (false, true) => '▄',
                    (false, false) => ' ',
                })
                .collect();
            format!("\x1b[30;107m{row}\x1b[0m")
        })
        .collect();
    Some(lines)
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

    const QUIET: Offer = Offer {
        open_browser: false,
        qr: false,
    };
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
        let issued = login(&url, "testbox", QUIET).await.unwrap();
        assert_eq!(
            issued,
            Issued {
                api_key: KEY.into(),
                email: "m@example.com".into(),
                box_name: "testbox".into()
            }
        );
        let err = login(&url, "testbox", QUIET).await.unwrap_err();
        assert!(err.to_string().contains("not_found"), "{err}");

        let account = me(&url, KEY).await.unwrap();
        assert!(account.describe().contains("minutes left: 30"));
        assert!(matches!(
            me(&url, "ptp_wrong").await,
            Err(MeError::Rejected)
        ));
    }

    #[test]
    fn the_sign_in_link_draws_as_a_qr_code() {
        let lines = qr_lines("https://pitupitu.chat/device?code=BCDF-GHJK").unwrap();
        // 29 modules (version 3, level L) and 2 of quiet zone on each side
        // make 33 rows: 17 lines of two rows each, 33 cells wide.
        assert_eq!(lines.len(), 17);
        for line in &lines {
            let cells = line
                .strip_prefix("\x1b[30;107m")
                .and_then(|l| l.strip_suffix("\x1b[0m"))
                .unwrap();
            assert_eq!(cells.chars().count(), 33, "{line:?}");
            assert!(cells.chars().all(|c| " ▀▄█".contains(c)));
        }
        // The quiet zone: the first line and the edges are light.
        assert!(lines[0].contains(&" ".repeat(33)));
        // The top-left finder pattern starts right after the quiet zone.
        assert!(lines[1].contains("  █▀▀▀▀▀█ "));
    }

    #[test]
    fn the_qr_code_is_skipped_when_asked_or_when_it_cannot_be_drawn() {
        assert!(qr_wanted(true, true, None, Some("xterm-256color")));
        assert!(qr_wanted(true, true, Some(""), None));
        assert!(
            !qr_wanted(false, true, None, Some("xterm-256color")),
            "--no-qr"
        );
        assert!(
            !qr_wanted(true, false, None, Some("xterm-256color")),
            "not a terminal"
        );
        assert!(
            !qr_wanted(true, true, Some("1"), Some("xterm-256color")),
            "NO_COLOR"
        );
        assert!(!qr_wanted(true, true, None, Some("dumb")), "TERM=dumb");
    }
}
