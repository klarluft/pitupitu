//! The box link: one outbound WebSocket to pitupitu cloud (`GET /v1/box`),
//! over which the cloud drives the agents on this machine, through herdr.
//! Nothing connects in.
//! The protocol is in `docs/box-link.md`.

use crate::calls::{self, CallError};
use crate::herdr_box::{self, HerdrInfo, Keeper};
use crate::history::{self, Setting};
use crate::home;
use crate::inventory::{self, Available, Inventory};
use crate::update::{self, Refused};
use anyhow::{anyhow, Context, Result};
use futures_util::{SinkExt, StreamExt};
use herdr::{AgentHost, AgentInfo, Status};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Notify};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

/// The box link protocol this box speaks.
pub const PROTOCOL: u32 = 5;

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
/// How often GitHub is asked for the newest releases.
const UPDATE_EVERY: Duration = Duration::from_secs(6 * 3600);
/// How often `herdr status` is read again (an update made at the
/// keyboard, a server that now needs a restart).
const STATUS_EVERY: Duration = Duration::from_secs(60);
/// The longest an `update` call may take.
const UPDATE_TIMEOUT: Duration = Duration::from_secs(600);

pub struct Link {
    pub url: String,
    pub api_key: String,
    pub box_name: String,
    pub home: PathBuf,
    pub host: Arc<dyn AgentHost>,
    /// herdr itself: its status, its server, its updates. `None` when a
    /// test stands in for herdr; the inventory's `herdr` is then null.
    pub keeper: Option<Arc<Keeper>>,
    /// The agent CLIs found on `PATH` when the agent started.
    pub available: Vec<Available>,
    /// Where the agents keep their session history (the user's home
    /// directory); `None` sends no recent folders and never reads the
    /// sessions, whatever the cloud's `settings` say.
    pub history: Option<PathBuf>,
    /// Where the session reader remembers what it sent.
    pub state_dir: PathBuf,
    /// Shortened in tests.
    pub ping_timeout: Duration,
    /// Ask GitHub for the newest releases now and then (`latest` and
    /// `herdr.latest` in the inventory). Off in tests.
    pub updates: bool,
    /// The newest releases as last asked, kept across reconnects.
    pub releases: Arc<Mutex<Releases>>,
}

/// What GitHub said last, and when.
#[derive(Debug, Clone, Default)]
pub struct Releases {
    /// A newer release of the box agent than this one.
    pub latest: Option<String>,
    /// The newest herdr release, whatever is installed.
    pub herdr: Option<String>,
    pub asked: Option<Instant>,
}

impl Releases {
    async fn ask() -> Releases {
        let (latest, herdr) = tokio::join!(update::newer(), update::herdr_newest());
        Releases {
            latest,
            herdr,
            asked: Some(Instant::now()),
        }
    }

    fn due(&self) -> bool {
        self.asked.is_none_or(|t| t.elapsed() >= UPDATE_EVERY)
    }
}

/// The inventory as last sent, shared by the poller and the calls that
/// change it.
type Shared = Arc<Mutex<Inventory>>;

/// What goes out on the socket, in order.
enum Out {
    Frame(Value),
    /// Everything before this has been sent: restart the service (after
    /// the box agent updated itself).
    Restart,
}

impl From<Value> for Out {
    fn from(v: Value) -> Self {
        Out::Frame(v)
    }
}

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
                        "pitupitu is out of date: run `pitupitu update`, `curl -fsSL https://pitupitu.chat/install | sh` or `brew upgrade pitupitu`"
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

        if let Some(k) = &self.keeper {
            k.ensure(&*self.host).await;
        }
        let (agents, herdr_up) = match self.host.list_agents().await {
            Ok(a) => (a, true),
            Err(e) => {
                warn!("agent list: {e}");
                (Vec::new(), false)
            }
        };
        if self.updates && self.releases.lock().unwrap().due() {
            let asked = Releases::ask().await;
            *self.releases.lock().unwrap() = asked;
        }
        let releases = self.releases.lock().unwrap().clone();
        let status = match &self.keeper {
            Some(k) => herdr_box::status(k.cli()).await,
            None => None,
        };
        let inv = Inventory {
            install: update::install_kind(),
            latest: releases.latest.clone(),
            herdr: status
                .as_ref()
                .map(|s| HerdrInfo::new(s, herdr_up, releases.herdr.as_deref())),
            kinds: if herdr_up {
                self.host.kinds().await
            } else {
                Vec::new()
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

        let (tx, mut rx) = mpsc::channel::<Out>(64);
        let refresh = Arc::new(Notify::new());
        // The session reader, on a thread of its own: it waits for the
        // cloud's `settings` and ends with this link.
        let (settings, settings_rx) = std::sync::mpsc::channel::<Setting>();
        if let Some(home) = self.history.clone() {
            let (state_dir, tx) = (self.state_dir.clone(), tx.clone());
            std::thread::spawn(move || {
                history::run(home, state_dir, settings_rx, move |note| {
                    tx.blocking_send(note.into()).is_ok()
                })
            });
        }
        let poller = tokio::spawn(poll(
            Polled {
                host: self.host.clone(),
                keeper: self.keeper.clone(),
                home_dir: self.home.clone(),
                history: self.history.clone(),
                inventory: shared.clone(),
                updates: self.updates,
                releases: self.releases.clone(),
                status,
                refresh: refresh.clone(),
            },
            herdr_up,
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
                        Message::Text(t) => self.incoming(t.as_str(), &tx, &refresh, &settings),
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
                Some(out) = rx.recv() => match out {
                    // `send` flushes: the frame is on the wire when it returns.
                    Out::Frame(v) => sink.send(text(&v)).await?,
                    Out::Restart => {
                        info!("box agent updated; restarting the service");
                        match tokio::task::spawn_blocking(crate::service::restart_soon).await {
                            Ok(Ok(true)) => {}
                            Ok(Ok(false)) => {
                                warn!("updated, but no service runs this box agent; restart `pitupitu connect` to run the new version")
                            }
                            Ok(Err(e)) => warn!("restarting the service: {e:#}"),
                            Err(e) => warn!("restarting the service: {e}"),
                        }
                    }
                },
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
    fn incoming(
        &self,
        frame: &str,
        tx: &mpsc::Sender<Out>,
        refresh: &Arc<Notify>,
        settings: &std::sync::mpsc::Sender<Setting>,
    ) {
        let Ok(value) = serde_json::from_str::<Value>(frame) else {
            warn!("unreadable frame from the cloud");
            return;
        };
        if let Some(note) = value.get("note").and_then(Value::as_str) {
            // `settings` carries `history` (and `resend`) for the session
            // reader; other fields, such as protocol 2's driver, are not
            // acted on.
            match (note, Setting::from_note(&value)) {
                ("settings", Some(s)) => {
                    debug!(history = s.history, resend = s.resend, "settings");
                    let _ = settings.send(s);
                }
                _ => debug!(note, "note from the cloud"),
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
        if call == "update" {
            let keeper = self.keeper.clone();
            let tx = tx.clone();
            let refresh = refresh.clone();
            tokio::spawn(async move { update_call(id, args, keeper, tx, refresh).await });
            return;
        }
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
            let _ = tx.send(reply.into()).await;
        });
    }
}

/// The `update` call: the box agent or herdr, the way each was installed.
/// For the box agent the reply goes out first, then the service restarts
/// and the next `hello` carries the new version. For herdr the inventory
/// is read again at once.
async fn update_call(
    id: Value,
    args: Value,
    keeper: Option<Arc<Keeper>>,
    tx: mpsc::Sender<Out>,
    refresh: Arc<Notify>,
) {
    #[derive(serde::Deserialize)]
    struct Args {
        what: String,
        #[serde(default)]
        handoff: bool,
    }
    let result = match serde_json::from_value::<Args>(args) {
        Err(e) => Err(CallError::bad_call(format!("update: {e}"))),
        Ok(a) => {
            let run = async {
                match a.what.as_str() {
                    "box" => update::update_box().await,
                    "herdr" => match &keeper {
                        Some(k) => herdr_box::update(k.cli(), a.handoff).await,
                        None => Err(Refused::Unavailable("herdr is not driven here".into())),
                    },
                    other => Err(Refused::Unavailable(format!(
                        "`{other}` is not something this box updates (box or herdr)"
                    ))),
                }
            };
            match tokio::time::timeout(UPDATE_TIMEOUT, run).await {
                Err(_) => Err(CallError::timeout(format!(
                    "update took over {UPDATE_TIMEOUT:?}"
                ))),
                Ok(Ok(done)) => Ok(done),
                Ok(Err(r)) => Err(CallError::api(r.code(), r.message())),
            }
        }
    };
    let reply = match &result {
        Ok(done) => {
            info!(what = done.what, from = %done.from, to = %done.to, restart = done.restart, "updated");
            json!({"id": id, "ok": done})
        }
        Err(err) => {
            info!(call = "update", kind = err.kind, code = ?err.code, "call failed");
            json!({"id": id, "err": err})
        }
    };
    if tx.send(reply.into()).await.is_err() {
        return;
    }
    match result {
        Ok(done) if done.what == "box" && done.restart == "done" => {
            let _ = tx.send(Out::Restart).await;
        }
        Ok(_) => refresh.notify_one(),
        Err(_) => {}
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
    keeper: Option<Arc<Keeper>>,
    home_dir: PathBuf,
    history: Option<PathBuf>,
    updates: bool,
    inventory: Shared,
    releases: Arc<Mutex<Releases>>,
    /// `herdr status` as last read.
    status: Option<Status>,
    /// Read herdr's status again now (after an update).
    refresh: Arc<Notify>,
}

impl Polled {
    /// Send an `inventory` note if `change` alters what was last sent.
    async fn inventory(&self, tx: &mpsc::Sender<Out>, change: impl FnOnce(&mut Inventory)) -> bool {
        let note = {
            let mut inv = self.inventory.lock().unwrap();
            let before = inv.clone();
            change(&mut inv);
            (*inv != before).then(|| inventory_note(&inv))
        };
        match note {
            Some(n) => tx.send(n.into()).await.is_ok(),
            None => true,
        }
    }

    async fn read_status(&mut self) {
        if let Some(k) = &self.keeper {
            self.status = herdr_box::status(k.cli()).await;
        }
    }

    /// The inventory's herdr half from what is known now.
    async fn herdr_part(&self, up: bool) -> (Option<HerdrInfo>, Vec<String>) {
        let newest = self.releases.lock().unwrap().herdr.clone();
        let info = self
            .status
            .as_ref()
            .map(|s| HerdrInfo::new(s, up, newest.as_deref()));
        let kinds = if up {
            self.host.kinds().await
        } else {
            Vec::new()
        };
        (info, kinds)
    }
}

/// Once a second: send `agents` when herdr's list changed, `home` when the
/// pitupitu files did, `inventory` when herdr starts or stops answering
/// (starting its server when it does not); every minute, `inventory` when
/// herdr's status changed; every five minutes when the recent folders
/// did; every six hours when a newer release appeared.
async fn poll(
    mut p: Polled,
    mut up: bool,
    mut last_agents: Signature,
    mut last_files: Vec<home::HomeFile>,
    tx: mpsc::Sender<Out>,
) {
    let host = p.host.clone();
    let home_dir = p.home_dir.clone();
    let mut tick = tokio::time::interval(POLL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut recent_tick = tokio::time::interval(INVENTORY_EVERY);
    recent_tick.tick().await;
    let mut status_tick = tokio::time::interval(STATUS_EVERY);
    status_tick.tick().await;
    // Asked when the link came up; next in six hours.
    let mut update_tick = tokio::time::interval(UPDATE_EVERY);
    update_tick.tick().await;
    let refresh = p.refresh.clone();
    loop {
        let mut herdr_changed = false;
        tokio::select! {
            _ = tick.tick() => {}
            _ = update_tick.tick(), if p.updates => {
                let asked = Releases::ask().await;
                let latest = asked.latest.clone();
                *p.releases.lock().unwrap() = asked;
                let (herdr, _) = p.herdr_part(up).await;
                if !p.inventory(&tx, |i| { i.latest = latest; i.herdr = herdr; }).await {
                    return;
                }
                continue;
            }
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
            _ = status_tick.tick() => herdr_changed = true,
            _ = refresh.notified() => herdr_changed = true,
        }
        let listed = host.list_agents().await;
        let now_up = listed.is_ok();
        if let (Err(e), Some(k)) = (&listed, &p.keeper) {
            k.revive(e).await;
        }
        if now_up != up {
            up = now_up;
            herdr_changed = true;
            if up {
                info!("herdr answers");
                if let Some(k) = &p.keeper {
                    k.answered();
                }
            } else {
                warn!("herdr does not answer");
            }
        }
        if herdr_changed {
            p.read_status().await;
            let (herdr, kinds) = p.herdr_part(up).await;
            if !p
                .inventory(&tx, |i| {
                    i.herdr = herdr;
                    i.kinds = kinds;
                })
                .await
            {
                return;
            }
        }
        match listed {
            Ok(agents) => {
                let sig = signature(&agents);
                if sig != last_agents {
                    last_agents = sig;
                    if tx
                        .send(json!({"note": "agents", "agents": agents}).into())
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
                .send(json!({"note": "home", "files": files}).into())
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
            home: home.clone(),
            host: Arc::new(host),
            keeper: None,
            available: vec![Available {
                kind: "claude".into(),
                version: "2.3.1 (Claude Code)".into(),
            }],
            history: None,
            state_dir: home.join(".state"),
            ping_timeout: PING_TIMEOUT,
            updates: false,
            releases: Default::default(),
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
        assert_eq!(hello["protocol"], 5);
        assert_eq!(hello["box"], "testbox");
        assert_eq!(hello["herdr"], Value::Null);
        assert!(hello.get("driver").is_none());
        assert_eq!(hello["install"], "installer");
        assert_eq!(hello["latest"], Value::Null);
        assert_eq!(hello["kinds"], json!(["claude", "codex"]));
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

        ws.close(Some(CloseFrame {
            code: CloseCode::from(4006),
            reason: "replaced".into(),
        }))
        .await
        .unwrap();
        assert_eq!(session.await.unwrap().unwrap(), End::Replaced);
    }

    /// Protocol 4: the session reader starts on the cloud's `settings`
    /// with `history`, and not before.
    #[tokio::test]
    async fn history_follows_the_settings_note() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let logs = dir.path().join(".claude/projects/-work");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(
            logs.join("s-9.jsonl"),
            format!(
                "{}\n",
                json!({"type": "user", "isSidechain": false, "cwd": work, "sessionId": "s-9",
                       "timestamp": "2026-10-20T10:00:00Z",
                       "message": {"role": "user", "content": "Tidy the docs"}})
            ),
        )
        .unwrap();
        let (url, server) = cloud().await;
        let mut l = link(url, FakeHost::default(), dir.path().to_path_buf());
        l.history = Some(dir.path().to_path_buf());
        let session = tokio::spawn(async move { l.session().await });
        let mut ws = server.await.unwrap();
        next_json(&mut ws).await; // hello
        next_json(&mut ws).await; // home
        ws.send(text(
            &json!({"note": "settings", "history": true, "future": 1}),
        ))
        .await
        .unwrap();
        let note = tokio::time::timeout(Duration::from_secs(10), next_json(&mut ws))
            .await
            .expect("a session note");
        assert_eq!(note["note"], "session");
        assert_eq!(note["session"]["id"], "s-9");
        assert_eq!(note["session"]["turns"][0]["text"], "Tidy the docs");
        session.abort();
    }

    #[tokio::test]
    async fn updates_are_refused_with_the_calls_codes() {
        let dir = tempfile::tempdir().unwrap();
        let (url, server) = cloud().await;
        let l = link(url, FakeHost::default(), dir.path().to_path_buf());
        let session = tokio::spawn(async move { l.session().await });
        let mut ws = server.await.unwrap();
        next_json(&mut ws).await; // hello
        next_json(&mut ws).await; // home
        for frame in [
            json!({"id": 1, "call": "update", "args": {"what": "toaster"}}),
            json!({"id": 2, "call": "update", "args": {"what": "herdr"}}),
            json!({"id": 3, "call": "update", "args": {}}),
        ] {
            ws.send(text(&frame)).await.unwrap();
        }
        let mut replies = Vec::new();
        for _ in 0..3 {
            replies.push(next_json(&mut ws).await);
        }
        let by_id = |id: u64| replies.iter().find(|r| r["id"] == id).unwrap().clone();
        assert_eq!(by_id(1)["err"]["code"], "update_unavailable");
        // No herdr behind this link: nothing to update it with.
        assert_eq!(by_id(2)["err"]["code"], "update_unavailable");
        assert_eq!(by_id(3)["err"]["kind"], "bad_call");
        session.abort();
    }

    /// A live herdr in a throwaway session: none runs there, so the box
    /// starts the server before `hello`, which says so.
    #[tokio::test]
    async fn the_box_starts_herdrs_server_and_reports_it() {
        let Some(scratch) = crate::testhost::ScratchHerdr::new() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let (url, server) = cloud().await;
        let mut l = link(url, FakeHost::default(), dir.path().to_path_buf());
        l.host = Arc::new(scratch.cli.clone());
        l.keeper = Some(Arc::new(Keeper::new(scratch.cli.clone())));
        let session = tokio::spawn(async move { l.session().await });
        let mut ws = server.await.unwrap();
        let hello = next_json(&mut ws).await;
        assert_eq!(hello["herdr"]["running"], true, "{hello}");
        assert!(hello["herdr"]["version"].as_str().unwrap().contains('.'));
        assert!(hello["herdr"]["protocol"].is_u64(), "{hello}");
        let kinds = hello["kinds"].as_array().unwrap();
        assert!(kinds.iter().any(|k| k == "claude"), "{hello}");
        session.abort();
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
