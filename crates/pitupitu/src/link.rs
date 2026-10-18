//! The box link: one outbound WebSocket to pitupitu cloud (`GET /v1/box`),
//! over which the cloud drives the agents on this machine, through herdr
//! or the terminal host (`router.rs`). Nothing connects in.
//! The protocol is in `docs/box-link.md`.

use crate::calls::{self, CallError};
use crate::home;
use crate::inventory::{self, Available, Inventory};
use crate::router::Router;
use anyhow::{anyhow, Context, Result};
use futures_util::{SinkExt, StreamExt};
use herdr::{AgentHost, AgentInfo};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

/// The box link protocol this box speaks.
pub const PROTOCOL: u32 = 2;

/// How often the agent list and the home folder are looked at.
const POLL: Duration = Duration::from_secs(1);
/// The cloud pings every 20 s; this long without one means a dead socket.
pub const PING_TIMEOUT: Duration = Duration::from_secs(60);
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// A link that stayed up this long starts the backoff over.
const STABLE: Duration = Duration::from_secs(30);
/// How often the recent project folders are looked at again.
const INVENTORY_EVERY: Duration = Duration::from_secs(300);

pub struct Link {
    pub url: String,
    pub api_key: String,
    pub box_name: String,
    pub home: PathBuf,
    pub herdr_version: String,
    pub host: Arc<dyn AgentHost>,
    /// The agent CLIs found on `PATH` when the agent started.
    pub available: Vec<Available>,
    /// Where the agents keep their session history (the user's home
    /// directory); `None` sends no recent folders.
    pub history: Option<PathBuf>,
    /// herdr and the terminal host behind `host`, when it is the router:
    /// says which driver serves new sessions and takes the cloud's
    /// setting. `None` (tests): herdr only.
    pub router: Option<Arc<Router>>,
    /// Shortened in tests.
    pub ping_timeout: Duration,
}

/// The inventory as last sent, shared by the poller and the handler of
/// the cloud's notes.
type Shared = Arc<Mutex<Inventory>>;

/// Why a link ended.
#[derive(Debug, PartialEq)]
pub enum End {
    /// Another box with the same key took over (4006): stop, don't fight it.
    Replaced,
    /// Closed by the cloud with this code and reason; reconnect.
    Closed(u16, String),
}

impl Link {
    /// Connect, serve, reconnect with backoff, until another box replaces
    /// this one.
    pub async fn run(&self) -> Result<()> {
        let mut backoff = BACKOFF_MIN;
        loop {
            let started = Instant::now();
            match self.session().await {
                Ok(End::Replaced) => {
                    warn!("another box connected with this key; stopping here");
                    return Ok(());
                }
                Ok(End::Closed(code, reason)) => match code {
                    4001 => warn!(
                        "pitupitu cloud rejected this box's key; run `pitupitu setup --relink`"
                    ),
                    4007 => warn!(
                        %reason,
                        "pitupitu is out of date: run `curl -fsSL https://pitupitu.chat/install | sh` or `brew upgrade pitupitu`"
                    ),
                    _ => info!(code, %reason, "box link closed"),
                },
                Err(e) => warn!("box link: {e:#}"),
            }
            if started.elapsed() > STABLE {
                backoff = BACKOFF_MIN;
            }
            let jitter = backoff.mul_f64(rand::random_range(0.0..0.25));
            tokio::time::sleep(backoff + jitter).await;
            backoff = (backoff * 2).min(BACKOFF_MAX);
        }
    }

    /// One connection, from the upgrade to the close.
    pub async fn session(&self) -> Result<End> {
        let mut request = self
            .url
            .as_str()
            .into_client_request()
            .with_context(|| format!("bad cloud url {}", self.url))?;
        request.headers_mut().insert(
            "authorization",
            format!("Bearer {}", self.api_key)
                .parse()
                .context("api key is not a valid header value")?,
        );
        let (ws, _) = tokio_tungstenite::connect_async(request)
            .await
            .with_context(|| format!("connecting to {}", self.url))?;
        let (mut sink, mut stream) = ws.split();

        let (agents, herdr_up) = match self.host.list_agents().await {
            Ok(a) => (a, true),
            Err(e) => {
                warn!("agent list: {e}");
                (Vec::new(), false)
            }
        };
        let inv = Inventory {
            driver: self
                .router
                .as_ref()
                .map_or("herdr", |r| r.driver())
                .to_string(),
            herdr: if self.router.as_ref().map_or(herdr_up, |r| r.herdr_up()) {
                self.herdr_version.clone()
            } else {
                String::new()
            },
            available: self.available.clone(),
            recent: self.recent().await,
        };
        let mut hello = json!({
            "note": "hello",
            "protocol": PROTOCOL,
            "version": env!("CARGO_PKG_VERSION"),
            "box": self.box_name,
            "home": self.home,
            "agents": agents,
        });
        if let (Value::Object(h), Ok(Value::Object(i))) = (&mut hello, serde_json::to_value(&inv)) {
            h.extend(i);
        }
        let shared: Shared = Arc::new(Mutex::new(inv));
        sink.send(text(&hello)).await?;
        let files = home::scan(&self.home);
        sink.send(text(&json!({"note": "home", "files": files})))
            .await?;
        info!(url = %self.url, "box link up");

        let (tx, mut rx) = mpsc::channel::<Value>(64);
        let poller = tokio::spawn(poll(
            Polled {
                host: self.host.clone(),
                home_dir: self.home.clone(),
                history: self.history.clone(),
                herdr_version: self.herdr_version.clone(),
                inventory: shared.clone(),
                router: self.router.clone(),
            },
            signature(&agents),
            files,
            tx.clone(),
        ));
        let _poller = AbortOnDrop(poller);

        let mut last_ping = Instant::now();
        let mut liveness = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                frame = stream.next() => {
                    let msg = match frame {
                        None => return Err(anyhow!("socket ended")),
                        Some(m) => m.context("reading from the cloud")?,
                    };
                    match msg {
                        Message::Text(t) => self.incoming(t.as_str(), &tx, &shared),
                        // Pong answers are queued by the library and flushed
                        // on the next read or write.
                        Message::Ping(_) => last_ping = Instant::now(),
                        Message::Close(frame) => {
                            let (code, reason) = frame
                                .map(|f| (u16::from(f.code), f.reason.to_string()))
                                .unwrap_or((u16::from(CloseCode::Normal), String::new()));
                            return Ok(if code == 4006 {
                                End::Replaced
                            } else {
                                End::Closed(code, reason)
                            });
                        }
                        _ => {}
                    }
                }
                Some(out) = rx.recv() => sink.send(text(&out)).await?,
                _ = liveness.tick() => {
                    if last_ping.elapsed() > self.ping_timeout {
                        return Err(anyhow!("no ping from the cloud in {:?}", self.ping_timeout));
                    }
                }
            }
        }
    }

    /// A frame from the cloud: a call is run on its own task, the reply
    /// goes back through `tx`. Only the call's name is logged, never its
    /// arguments or result.
    fn incoming(&self, frame: &str, tx: &mpsc::Sender<Value>, shared: &Shared) {
        let Ok(value) = serde_json::from_str::<Value>(frame) else {
            warn!("unreadable frame from the cloud");
            return;
        };
        if let Some(note) = value.get("note").and_then(Value::as_str) {
            if note == "settings" {
                self.settings(&value, tx, shared);
            } else {
                debug!(note, "note from the cloud");
            }
            return;
        }
        let Some(id) = value.get("id").cloned() else {
            warn!("frame with neither a note nor an id");
            return;
        };
        let call = value
            .get("call")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let args = value.get("args").cloned().unwrap_or(Value::Null);
        let host = self.host.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let started = Instant::now();
            let deadline = calls::deadline(&call, &args);
            let result = tokio::time::timeout(deadline, calls::dispatch(&*host, &call, args))
                .await
                .unwrap_or_else(|_| {
                    Err(CallError::timeout(format!("{call} took over {deadline:?}")))
                });
            let reply = match result {
                Ok(ok) => {
                    debug!(%call, ms = started.elapsed().as_millis() as u64, "call ok");
                    json!({"id": id, "ok": ok})
                }
                Err(err) => {
                    info!(%call, kind = err.kind, code = ?err.code, "call failed");
                    json!({"id": id, "err": err})
                }
            };
            let _ = tx.send(reply).await;
        });
    }
}

impl Link {
    async fn recent(&self) -> Vec<inventory::Recent> {
        match self.history.clone() {
            Some(h) => tokio::task::spawn_blocking(move || inventory::recent(&h))
                .await
                .unwrap_or_default(),
            None => Vec::new(),
        }
    }

    /// `{"note": "settings", "driver": …}`: which driver the person chose
    /// for this box (`auto`, `herdr`, `terminal`). Answered with the
    /// inventory, which says what serves new sessions now.
    fn settings(&self, value: &Value, tx: &mpsc::Sender<Value>, shared: &Shared) {
        let wanted = value
            .get("driver")
            .and_then(Value::as_str)
            .unwrap_or("auto");
        let note = {
            let mut inv = shared.lock().unwrap();
            if let Some(r) = &self.router {
                r.set_wanted(wanted);
                inv.driver = r.driver().to_string();
            }
            info!(wanted, driver = %inv.driver, "driver setting");
            inventory_note(&inv)
        };
        let tx = tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(note).await;
        });
    }
}

fn inventory_note(inv: &Inventory) -> Value {
    let mut note = serde_json::to_value(inv).unwrap_or_else(|_| json!({}));
    note["note"] = json!("inventory");
    note
}

fn text(v: &Value) -> Message {
    Message::Text(v.to_string().into())
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// What decides whether a new `agents` note is due: the set of panes and
/// each one's status and counters.
type Signature = Vec<(String, String, u64, Option<u64>)>;

fn signature(agents: &[AgentInfo]) -> Signature {
    let mut sig: Signature = agents
        .iter()
        .map(|a| {
            (
                a.pane_id.0.clone(),
                a.agent_status.as_str().to_string(),
                a.state_change_seq,
                a.completion_seq,
            )
        })
        .collect();
    sig.sort();
    sig
}

/// What the poller looks at.
struct Polled {
    host: Arc<dyn AgentHost>,
    home_dir: PathBuf,
    history: Option<PathBuf>,
    herdr_version: String,
    inventory: Shared,
    router: Option<Arc<Router>>,
}

impl Polled {
    /// Send an `inventory` note if `change` alters what was last sent.
    async fn inventory(
        &self,
        tx: &mpsc::Sender<Value>,
        change: impl FnOnce(&mut Inventory),
    ) -> bool {
        let note = {
            let mut inv = self.inventory.lock().unwrap();
            let before = inv.clone();
            change(&mut inv);
            (*inv != before).then(|| inventory_note(&inv))
        };
        match note {
            Some(n) => tx.send(n).await.is_ok(),
            None => true,
        }
    }
}

/// Once a second: send `agents` when herdr's list changed, `home` when the
/// pitupitu files did, `inventory` when herdr comes or goes; every five
/// minutes, `inventory` when the recent folders changed.
async fn poll(
    p: Polled,
    mut last_agents: Signature,
    mut last_files: Vec<home::HomeFile>,
    tx: mpsc::Sender<Value>,
) {
    let host = p.host.clone();
    let home_dir = p.home_dir.clone();
    let mut tick = tokio::time::interval(POLL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut recent_tick = tokio::time::interval(INVENTORY_EVERY);
    recent_tick.tick().await;
    let mut herdr_down = p.inventory.lock().unwrap().herdr.is_empty();
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            _ = recent_tick.tick() => {
                if let Some(h) = p.history.clone() {
                    let recent = tokio::task::spawn_blocking(move || inventory::recent(&h))
                        .await
                        .unwrap_or_default();
                    if !p.inventory(&tx, |i| i.recent = recent).await {
                        return;
                    }
                }
                continue;
            }
        }
        let listed = host.list_agents().await;
        // With the router, herdr's state is its own: the list comes from
        // the terminal host too.
        let herdr_ok = match &p.router {
            Some(r) => r.herdr_up(),
            None => listed.is_ok(),
        };
        if herdr_ok == herdr_down {
            herdr_down = !herdr_ok;
            if herdr_ok {
                info!("herdr answers");
            } else {
                warn!("herdr does not answer");
            }
        }
        let driver = p.router.as_ref().map(|r| r.driver().to_string());
        let version = if herdr_ok {
            p.herdr_version.clone()
        } else {
            String::new()
        };
        if !p
            .inventory(&tx, |i| {
                i.herdr = version;
                if let Some(d) = driver {
                    i.driver = d;
                }
            })
            .await
        {
            return;
        }
        match listed {
            Ok(agents) => {
                let sig = signature(&agents);
                if sig != last_agents {
                    last_agents = sig;
                    if tx
                        .send(json!({"note": "agents", "agents": agents}))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }
            Err(e) => tracing::debug!("listing agents: {e}"),
        }
        let dir = home_dir.clone();
        let files = tokio::task::spawn_blocking(move || home::scan(&dir))
            .await
            .unwrap_or_default();
        if files != last_files {
            last_files = files.clone();
            if tx
                .send(json!({"note": "home", "files": files}))
                .await
                .is_err()
            {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testhost::{agent, FakeHost};
    use herdr::AgentStatus;
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
    use tokio_tungstenite::tungstenite::protocol::CloseFrame;

    type Server = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

    /// A cloud end: accepts one socket, checks the key, hands it over.
    #[allow(clippy::result_large_err)] // the handshake callback's signature
    async fn cloud() -> (String, tokio::sync::oneshot::Receiver<Server>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/v1/box", listener.local_addr().unwrap());
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let ws = tokio_tungstenite::accept_hdr_async(tcp, |req: &Request, resp: Response| {
                assert_eq!(req.uri().path(), "/v1/box");
                assert_eq!(req.headers()["authorization"], "Bearer ptp_test");
                Ok(resp)
            })
            .await
            .unwrap();
            let _ = tx.send(ws);
        });
        (url, rx)
    }

    async fn next_json(ws: &mut Server) -> Value {
        loop {
            match ws.next().await.unwrap().unwrap() {
                Message::Text(t) => return serde_json::from_str(t.as_str()).unwrap(),
                _ => continue,
            }
        }
    }

    fn link(url: String, host: FakeHost, home: PathBuf) -> Link {
        Link {
            url,
            api_key: "ptp_test".into(),
            box_name: "testbox".into(),
            home,
            herdr_version: "herdr 0.9.3".into(),
            host: Arc::new(host),
            available: vec![Available {
                kind: "claude".into(),
                version: "2.3.1 (Claude Code)".into(),
            }],
            history: None,
            router: None,
            ping_timeout: PING_TIMEOUT,
        }
    }

    #[tokio::test]
    async fn hello_calls_notes_and_replaced() {
        let dir = tempfile::tempdir().unwrap();
        home::ensure(dir.path()).unwrap();
        let host = FakeHost::default();
        host.agents
            .lock()
            .unwrap()
            .push(agent("notes", "w1:p1", AgentStatus::Working, 5));
        let (url, server) = cloud().await;
        let l = link(url, host.clone(), dir.path().to_path_buf());
        let session = tokio::spawn(async move { l.session().await });
        let mut ws = server.await.unwrap();

        let hello = next_json(&mut ws).await;
        assert_eq!(hello["note"], "hello");
        assert_eq!(hello["protocol"], 2);
        assert_eq!(hello["box"], "testbox");
        assert_eq!(hello["herdr"], "herdr 0.9.3");
        assert_eq!(hello["driver"], "herdr");
        assert_eq!(hello["available"][0]["kind"], "claude");
        assert_eq!(hello["recent"], json!([]));
        assert_eq!(hello["agents"][0]["name"], "notes");
        assert_eq!(hello["home"], json!(dir.path()));
        let home_note = next_json(&mut ws).await;
        assert_eq!(home_note["note"], "home");
        assert_eq!(home_note["files"], json!([]));

        // Overlapping calls are answered in any order, each by its id.
        for frame in [
            json!({"id": 1, "call": "agent", "args": {"agent": "slow"}}),
            json!({"id": 2, "call": "read", "args": {"agent": "notes", "lines": 40}}),
            json!({"id": 3, "call": "format_disk"}),
        ] {
            ws.send(text(&frame)).await.unwrap();
        }
        let mut replies = Vec::new();
        for _ in 0..3 {
            replies.push(next_json(&mut ws).await);
        }
        assert_eq!(
            replies[2]["id"], 1,
            "the slow call answers last: {replies:?}"
        );
        let by_id = |id: u64| replies.iter().find(|r| r["id"] == id).unwrap().clone();
        assert_eq!(by_id(2)["ok"], "screen of notes, 40 lines");
        assert_eq!(by_id(3)["err"]["kind"], "bad_call");
        assert_eq!(by_id(1)["ok"]["name"], "slow");

        // A change in herdr becomes an agents note; so does an edit at home.
        host.agents.lock().unwrap()[0] = agent("notes", "w1:p1", AgentStatus::Done, 6);
        let note = next_json(&mut ws).await;
        assert_eq!(note["note"], "agents");
        assert_eq!(note["agents"][0]["agent_status"], "done");
        std::fs::create_dir_all(dir.path().join("skills/deploy")).unwrap();
        std::fs::write(dir.path().join("skills/deploy/SKILL.md"), "Deploy steps.").unwrap();
        let note = next_json(&mut ws).await;
        assert_eq!(note["note"], "home");
        assert_eq!(note["files"][0]["text"], "Deploy steps.");

        // The driver setting is answered with the inventory; this version
        // stays on herdr whatever is asked.
        ws.send(text(&json!({"note": "settings", "driver": "terminal"})))
            .await
            .unwrap();
        let note = next_json(&mut ws).await;
        assert_eq!(note["note"], "inventory");
        assert_eq!(note["driver"], "herdr");

        ws.close(Some(CloseFrame {
            code: CloseCode::from(4006),
            reason: "replaced".into(),
        }))
        .await
        .unwrap();
        assert_eq!(session.await.unwrap().unwrap(), End::Replaced);
    }

    #[tokio::test]
    async fn a_silent_cloud_is_a_dead_socket() {
        let dir = tempfile::tempdir().unwrap();
        let (url, server) = cloud().await;
        let mut l = link(url, FakeHost::default(), dir.path().to_path_buf());
        l.ping_timeout = Duration::from_millis(1500);
        let session = tokio::spawn(async move { l.session().await });
        let _ws = server.await.unwrap();
        let err = session.await.unwrap().unwrap_err();
        assert!(err.to_string().contains("no ping"), "{err}");
    }

    #[tokio::test]
    async fn other_close_codes_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let (url, server) = cloud().await;
        let l = link(url, FakeHost::default(), dir.path().to_path_buf());
        let session = tokio::spawn(async move { l.session().await });
        let mut ws = server.await.unwrap();
        next_json(&mut ws).await;
        ws.close(Some(CloseFrame {
            code: CloseCode::from(4007),
            reason: "protocol 2".into(),
        }))
        .await
        .unwrap();
        assert_eq!(
            session.await.unwrap().unwrap(),
            End::Closed(4007, "protocol 2".into())
        );
    }
}
