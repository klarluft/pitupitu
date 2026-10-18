//! `pitupitu terminals`: the process that holds the terminal host's
//! terminals. Each pane is a PTY running the person's login shell, with a
//! screen emulator holding what it shows and 5000 lines of scrollback.
//! Agents are started by typing their command into the shell, as a person
//! would. The box agent (and `pitupitu attach`, and the agents' hooks)
//! talk to it over a Unix socket, one JSON request per line.
//!
//! How an agent's state is known: Claude Code's hooks, passed on its
//! command line for that session only (`pitupitu hook`); Codex's `notify`
//! program for turn completion, passed with `-c`; and for both the bottom
//! of the screen (an approval, a question, the trust prompt, a login
//! screen means blocked), and the shell being in the foreground again
//! (the agent exited).

use super::detect::{self, Dialog};
use super::PREFIX;
use anyhow::{Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;
use tracing::info;

const ROWS: u16 = 40;
const COLS: u16 = 120;
const SCROLLBACK: usize = 5000;
/// How often screens and processes are looked at.
const TICK: Duration = Duration::from_millis(500);
/// A holder nobody used exits after this long.
const IDLE_EXIT: Duration = Duration::from_secs(600);
/// What a running Claude Code session sets for the processes it starts.
/// Configuration (auth, Bedrock, …) is left alone.
const SESSION_MARKERS: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_PID",
    "CLAUDE_JOB_DIR",
    "CLAUDE_EFFORT",
    "CLAUDE_CODE_AGENT",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_SESSION_ID",
];
/// The agent kinds the terminal host starts.
pub const KINDS: &[&str] = &["claude", "codex"];

/// Keeps the window title the agent sets (Claude Code's session summary).
struct Title(Option<String>);

impl vt100::Callbacks for Title {
    /// Without the spinner or status mark the agents put in front
    /// (Claude Code's `✳ `, Codex's `⠋ | `): the title is spoken.
    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        let t = String::from_utf8_lossy(title)
            .trim_start_matches(|c: char| !c.is_alphanumeric())
            .trim()
            .to_string();
        self.0 = (!t.is_empty()).then_some(t);
    }
}

type Screen = Arc<Mutex<vt100::Parser<Title>>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Idle,
    Working,
    Blocked,
    Done,
    Unknown,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Idle => "idle",
            Status::Working => "working",
            Status::Blocked => "blocked",
            Status::Done => "done",
            Status::Unknown => "unknown",
        }
    }
}

struct Agent {
    name: String,
    kind: String,
    status: Status,
    seq: u64,
    completion: u64,
    /// Accepts prompts (it said so, or its screen settled).
    ready: bool,
    started: Instant,
    trust_answered: bool,
    last_screen: String,
    stable_since: Instant,
    /// The last completed turn, and whether the agent worked since: Codex
    /// calls its `notify` twice for one turn.
    last_done: Option<Instant>,
    worked_since_done: bool,
}

/// A second "turn done" this soon after the first, with no work between,
/// is the same turn.
const SAME_TURN: Duration = Duration::from_secs(5);

impl Agent {
    fn set(&mut self, status: Status) {
        if self.status != status {
            self.status = status;
            self.seq += 1;
        }
        if status == Status::Working {
            self.worked_since_done = true;
        }
    }

    /// A turn completed, unless this is a repeat of the last one.
    fn done(&mut self) {
        let repeat =
            !self.worked_since_done && self.last_done.is_some_and(|t| t.elapsed() < SAME_TURN);
        if repeat {
            return;
        }
        self.ready = true;
        self.set(Status::Done);
        self.completion += 1;
        self.last_done = Some(Instant::now());
        self.worked_since_done = false;
    }
}

struct Pane {
    ws: String,
    tab: String,
    cwd: PathBuf,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    shell_pid: Option<u32>,
    screen: Screen,
    out: broadcast::Sender<Vec<u8>>,
    agent: Option<Agent>,
}

struct Ws {
    label: String,
    cwd: PathBuf,
    tabs: u32,
}

#[derive(Default)]
struct State {
    next_ws: u32,
    workspaces: BTreeMap<String, Ws>,
    panes: BTreeMap<String, Pane>,
    used: bool,
}

/// A refusal, as `{"err": {"code", "message"}}`.
#[derive(Debug)]
struct Refused {
    code: &'static str,
    message: String,
}

fn refused(code: &'static str, message: impl Into<String>) -> Refused {
    Refused {
        code,
        message: message.into(),
    }
}

type Reply = std::result::Result<Value, Refused>;

pub struct Holder {
    state: Mutex<State>,
    /// This binary: the agents' hooks call it.
    exe: PathBuf,
    socket: PathBuf,
    born: Instant,
}

/// Run the holder until it has no terminals left.
pub async fn run(socket: PathBuf) -> Result<()> {
    if UnixStream::connect(&socket).await.is_ok() {
        info!("a terminal holder is already running");
        return Ok(());
    }
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        set_mode(dir, 0o700);
    }
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket)
        .with_context(|| format!("listening on {}", socket.display()))?;
    set_mode(&socket, 0o600);
    let holder = Arc::new(Holder {
        state: Mutex::new(State {
            next_ws: first_workspace_number(),
            ..State::default()
        }),
        exe: std::env::current_exe().context("finding this binary")?,
        socket: socket.clone(),
        born: Instant::now(),
    });
    info!(socket = %socket.display(), "terminal holder up");
    let mut tick = tokio::time::interval(TICK);
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                if let Ok((stream, _)) = accepted {
                    let h = holder.clone();
                    tokio::spawn(async move { h.serve(stream).await });
                }
            }
            _ = tick.tick() => {
                if holder.tick() {
                    info!("no terminals left; terminal holder exiting");
                    let _ = std::fs::remove_file(&socket);
                    return Ok(());
                }
            }
        }
    }
}

/// Where this holder's workspace numbers start: from the clock, so a
/// holder started again never hands out an id an earlier one used. The
/// cloud keeps tasks by pane id, and a reused one would be taken for the
/// old task.
fn first_workspace_number() -> u32 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    (secs % 100_000_000) as u32 * 10
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

impl Holder {
    /// One connection: requests, one per line, until it closes or turns
    /// into an attached terminal.
    async fn serve(self: Arc<Self>, stream: UnixStream) {
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read);
        let mut line = String::new();
        loop {
            line.clear();
            match lines.read_line(&mut line).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            let Ok(req) = serde_json::from_str::<Value>(&line) else {
                let _ = write
                    .write_all(b"{\"err\":{\"code\":\"bad_request\",\"message\":\"not JSON\"}}\n")
                    .await;
                continue;
            };
            let op = req["op"].as_str().unwrap_or_default().to_string();
            if op == "attach" {
                self.attach(req, lines, write).await;
                return;
            }
            let reply = match self.handle(&op, &req).await {
                Ok(ok) => json!({ "ok": ok }),
                Err(r) => json!({"err": {"code": r.code, "message": r.message}}),
            };
            let mut text = reply.to_string();
            text.push('\n');
            if write.write_all(text.as_bytes()).await.is_err() {
                return;
            }
        }
    }

    async fn handle(&self, op: &str, req: &Value) -> Reply {
        let s = |k: &str| req[k].as_str().unwrap_or_default().to_string();
        match op {
            "ping" => Ok(json!("pitupitu terminals")),
            "list_workspaces" => Ok(self.list_workspaces()),
            "list_agents" => Ok(self.list_agents()),
            "create_workspace" => self.create_workspace(Path::new(&s("cwd")), &s("label")),
            "create_tab" => {
                let cwd = req["cwd"].as_str().map(PathBuf::from);
                self.create_tab(&s("workspace"), cwd.as_deref())
            }
            "close_workspace" => self.close_workspace(&s("workspace")),
            "close_tab" => self.close_tab(&s("tab")),
            "start_agent" => self.start_agent(req).await,
            "prompt" => self.prompt(&s("agent"), &s("text")).await,
            "agent" => self.with_agent(&s("agent"), |_, info| Ok(info)),
            "read" => {
                let lines = req["lines"].as_u64().unwrap_or(60).min(2000) as usize;
                self.read(&s("agent"), lines)
            }
            "send_keys" => {
                let keys: Vec<String> = req["keys"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|k| k.as_str().map(str::to_string))
                    .collect();
                self.send_keys(&s("agent"), &keys).await
            }
            "hook" => {
                self.hook(&s("pane"), &s("event"), &s("detail"));
                Ok(Value::Null)
            }
            other => Err(refused("bad_request", format!("unknown op {other}"))),
        }
    }

    // Workspaces and tabs.

    fn list_workspaces(&self) -> Value {
        let st = self.state.lock().unwrap();
        Value::Array(
            st.workspaces
                .iter()
                .map(|(id, w)| json!({"workspace_id": id, "label": w.label, "focused": false}))
                .collect(),
        )
    }

    fn create_workspace(&self, cwd: &Path, label: &str) -> Reply {
        if !cwd.is_dir() {
            return Err(refused(
                "no_such_directory",
                format!("{} is not a folder on this box", cwd.display()),
            ));
        }
        let mut st = self.state.lock().unwrap();
        st.used = true;
        st.next_ws += 1;
        let ws = format!("{PREFIX}{}", st.next_ws);
        st.workspaces.insert(
            ws.clone(),
            Ws {
                label: label.to_string(),
                cwd: cwd.to_path_buf(),
                tabs: 0,
            },
        );
        self.add_tab(&mut st, &ws, cwd)
    }

    fn create_tab(&self, ws: &str, cwd: Option<&Path>) -> Reply {
        let mut st = self.state.lock().unwrap();
        let Some(w) = st.workspaces.get(ws) else {
            return Err(refused("workspace_not_found", format!("no workspace {ws}")));
        };
        let cwd = cwd.map(Path::to_path_buf).unwrap_or_else(|| w.cwd.clone());
        if !cwd.is_dir() {
            return Err(refused(
                "no_such_directory",
                format!("{} is not a folder on this box", cwd.display()),
            ));
        }
        self.add_tab(&mut st, ws, &cwd)
    }

    fn add_tab(&self, st: &mut State, ws: &str, cwd: &Path) -> Reply {
        let w = st.workspaces.get_mut(ws).expect("workspace exists");
        w.tabs += 1;
        let tab = format!("{ws}:t{}", w.tabs);
        let pane_id = format!("{ws}:p{}", w.tabs);
        let pane = self
            .spawn_pane(ws, &tab, &pane_id, cwd)
            .map_err(|e| refused("spawn_failed", format!("{e:#}")))?;
        st.panes.insert(pane_id.clone(), pane);
        info!(pane = %pane_id, "terminal opened");
        Ok(json!({"workspace_id": ws, "tab_id": tab, "root_pane": pane_id, "cwd": cwd}))
    }

    fn spawn_pane(&self, ws: &str, tab: &str, pane_id: &str, cwd: &Path) -> Result<Pane> {
        let pty = native_pty_system().openpty(PtySize {
            rows: ROWS,
            cols: COLS,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
        let mut cmd = CommandBuilder::new(&shell);
        cmd.arg("-l");
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("PITUPITU_TERMS_SOCKET", &self.socket);
        cmd.env("PITUPITU_PANE", pane_id);
        // Started from inside a Claude Code session (a person running
        // `pitupitu connect` there), the holder carries that session's
        // markers; an agent started here is its own session, not a child.
        for marker in SESSION_MARKERS {
            cmd.env_remove(marker);
        }
        let child = pty.slave.spawn_command(cmd)?;
        drop(pty.slave);
        let mut reader = pty.master.try_clone_reader()?;
        let writer = pty.master.take_writer()?;
        let screen: Screen = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            ROWS,
            COLS,
            SCROLLBACK,
            Title(None),
        )));
        let (out, _) = broadcast::channel(512);
        {
            let screen = screen.clone();
            let out = out.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            screen.lock().unwrap().process(&buf[..n]);
                            let _ = out.send(buf[..n].to_vec());
                        }
                    }
                }
            });
        }
        Ok(Pane {
            ws: ws.to_string(),
            tab: tab.to_string(),
            cwd: cwd.to_path_buf(),
            shell_pid: child.process_id(),
            master: pty.master,
            writer,
            child,
            screen,
            out,
            agent: None,
        })
    }

    fn close_workspace(&self, ws: &str) -> Reply {
        let mut st = self.state.lock().unwrap();
        if st.workspaces.remove(ws).is_none() {
            return Err(refused("workspace_not_found", format!("no workspace {ws}")));
        }
        let ids: Vec<String> = st
            .panes
            .iter()
            .filter(|(_, p)| p.ws == ws)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            if let Some(mut p) = st.panes.remove(&id) {
                let _ = p.child.kill();
            }
        }
        info!(workspace = ws, "workspace closed");
        Ok(Value::Null)
    }

    fn close_tab(&self, tab: &str) -> Reply {
        let mut st = self.state.lock().unwrap();
        let Some(id) = st
            .panes
            .iter()
            .find(|(_, p)| p.tab == tab)
            .map(|(id, _)| id.clone())
        else {
            return Err(refused("tab_not_found", format!("no tab {tab}")));
        };
        let mut p = st.panes.remove(&id).expect("pane exists");
        let _ = p.child.kill();
        if !st.panes.values().any(|x| x.ws == p.ws) {
            st.workspaces.remove(&p.ws);
        }
        info!(tab, "tab closed");
        Ok(Value::Null)
    }

    // Agents.

    fn info(id: &str, p: &Pane) -> Option<Value> {
        let a = p.agent.as_ref()?;
        let title = p.screen.lock().unwrap().callbacks().0.clone();
        Some(json!({
            "agent": a.kind,
            "name": a.name,
            "agent_status": a.status.as_str(),
            "pane_id": id,
            "workspace_id": p.ws,
            "tab_id": p.tab,
            "cwd": p.cwd,
            "state_change_seq": a.seq,
            "completion_seq": a.completion,
            "launch_pending": !a.ready,
            "interactive_ready": a.ready,
            "terminal_title_stripped": title,
        }))
    }

    fn list_agents(&self) -> Value {
        let st = self.state.lock().unwrap();
        Value::Array(
            st.panes
                .iter()
                .filter_map(|(id, p)| Self::info(id, p))
                .collect(),
        )
    }

    /// The pane an agent target means: its name, or its pane id.
    fn find(st: &State, target: &str) -> Option<String> {
        if st.panes.get(target).is_some_and(|p| p.agent.is_some()) {
            return Some(target.to_string());
        }
        st.panes
            .iter()
            .find(|(_, p)| p.agent.as_ref().is_some_and(|a| a.name == target))
            .map(|(id, _)| id.clone())
    }

    fn with_agent(&self, target: &str, f: impl FnOnce(&mut Pane, Value) -> Reply) -> Reply {
        let mut st = self.state.lock().unwrap();
        let Some(id) = Self::find(&st, target) else {
            return Err(refused("agent_not_found", format!("no agent {target}")));
        };
        let p = st.panes.get_mut(&id).expect("pane exists");
        let info = Self::info(&id, p).expect("has an agent");
        f(p, info)
    }

    fn write(&self, target: &str, bytes: &[u8]) -> std::result::Result<(), Refused> {
        let mut st = self.state.lock().unwrap();
        let id = Self::find(&st, target)
            .ok_or_else(|| refused("agent_not_found", format!("no agent {target}")))?;
        let p = st.panes.get_mut(&id).expect("pane exists");
        p.writer
            .write_all(bytes)
            .and_then(|_| p.writer.flush())
            .map_err(|e| refused("io", e.to_string()))
    }

    async fn start_agent(&self, req: &Value) -> Reply {
        let name = req["name"].as_str().unwrap_or_default().to_string();
        let kind = req["kind"].as_str().unwrap_or("claude").to_string();
        let pane = req["pane"].as_str().unwrap_or_default().to_string();
        let accept_trust = req["accept_trust_prompt"].as_bool().unwrap_or(false);
        let timeout = Duration::from_millis(req["timeout_ms"].as_u64().unwrap_or(30_000));
        let args: Vec<String> = req["agent_args"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| a.as_str().map(str::to_string))
            .collect();
        if !KINDS.contains(&kind.as_str()) {
            return Err(refused(
                "unsupported_agent_kind",
                format!("this box's own terminals run claude and codex, not {kind}"),
            ));
        }
        let line = self.command_line(&kind, &pane, &args);
        {
            let mut st = self.state.lock().unwrap();
            if Self::find(&st, &name).is_some() {
                return Err(refused("agent_name_taken", format!("{name} is taken")));
            }
            let Some(p) = st.panes.get_mut(&pane) else {
                return Err(refused("pane_not_found", format!("no pane {pane}")));
            };
            if p.agent.is_some() {
                return Err(refused(
                    "pane_busy",
                    format!("{pane} already runs an agent"),
                ));
            }
            let now = Instant::now();
            p.agent = Some(Agent {
                name: name.clone(),
                kind: kind.clone(),
                status: Status::Unknown,
                seq: 1,
                completion: 0,
                ready: false,
                started: now,
                trust_answered: false,
                last_screen: String::new(),
                stable_since: now,
                last_done: None,
                worked_since_done: false,
            });
            let typed = format!("{line}\r");
            if let Err(e) = p
                .writer
                .write_all(typed.as_bytes())
                .and_then(|_| p.writer.flush())
            {
                p.agent = None;
                return Err(refused("io", e.to_string()));
            }
        }
        info!(pane = %pane, kind = %kind, "agent starting");

        // Up and ready, answering the trust prompt on the way if allowed.
        let deadline = Instant::now() + timeout;
        loop {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let mut trust_keys = None;
            {
                let mut st = self.state.lock().unwrap();
                let Some(p) = st.panes.get_mut(&pane) else {
                    return Err(refused("pane_not_found", format!("{pane} closed")));
                };
                let Some(a) = p.agent.as_mut() else {
                    return Err(refused(
                        "agent_not_ready",
                        format!("{kind} exited right after starting; is it installed?"),
                    ));
                };
                let screen = p.screen.lock().unwrap().screen().contents();
                match detect::dialog(&screen) {
                    Some(Dialog::Trust) if accept_trust && !a.trust_answered => {
                        a.trust_answered = true;
                        trust_keys = detect::trust_keys(&screen);
                    }
                    Some(d @ (Dialog::Trust | Dialog::Login))
                        if a.started.elapsed() > Duration::from_secs(4) =>
                    {
                        let what = if d == Dialog::Login {
                            format!(
                                "{kind} is not logged in on this box; run `{kind}` once and log in"
                            )
                        } else {
                            "it asks whether to trust this folder".to_string()
                        };
                        return Err(refused(
                            "agent_not_ready",
                            format!("blocked during startup ({what}):\n{}", tail(&screen, 20)),
                        ));
                    }
                    _ => {}
                }
                if a.ready {
                    return Ok(Self::info(&pane, p).expect("has an agent"));
                }
            }
            if let Some(keys) = trust_keys {
                for k in keys {
                    if let Some(b) = detect::key_bytes(k) {
                        let _ = self.write(&pane, &b);
                        tokio::time::sleep(Duration::from_millis(40)).await;
                    }
                }
            }
            if Instant::now() > deadline {
                let mut st = self.state.lock().unwrap();
                let screen = st
                    .panes
                    .get(&pane)
                    .map(|p| p.screen.lock().unwrap().screen().contents())
                    .unwrap_or_default();
                if let Some(p) = st.panes.get_mut(&pane) {
                    p.agent = None;
                }
                return Err(refused(
                    "agent_not_ready",
                    format!(
                        "timed out waiting for agent startup:\n{}",
                        tail(&screen, 20)
                    ),
                ));
            }
        }
    }

    /// The command typed into the shell: the agent and the cloud's own
    /// arguments (a model), plus how it reports its state for this session
    /// only. Nothing else, and no permission flags.
    fn command_line(&self, kind: &str, pane: &str, args: &[String]) -> String {
        let exe = self.exe.display().to_string();
        let hook = |event: &str| {
            format!(
                "{} hook {} {event}",
                detect::shell_quote(&exe),
                detect::shell_quote(pane)
            )
        };
        let mut words: Vec<String> = vec![kind.to_string()];
        match kind {
            "claude" => {
                let entry =
                    |event: &str| json!([{"hooks": [{"type": "command", "command": hook(event)}]}]);
                let settings = json!({"hooks": {
                    "SessionStart": entry("start"),
                    "UserPromptSubmit": entry("prompt"),
                    "PostToolUse": entry("tool"),
                    "Notification": entry("notify"),
                    "Stop": entry("stop"),
                }});
                words.push("--settings".into());
                words.push(settings.to_string());
            }
            "codex" => {
                let notify = json!([exe, "hook", pane, "codex"]);
                words.push("-c".into());
                words.push(format!("notify={notify}"));
            }
            _ => {}
        }
        words.extend(args.iter().cloned());
        words
            .iter()
            .map(|w| detect::shell_quote(w))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Type a prompt: a paste (so new lines stay in the prompt), then
    /// Enter. Returns the state from before the agent reacted.
    async fn prompt(&self, target: &str, text: &str) -> Reply {
        let before = self.with_agent(target, |p, info| {
            let a = p.agent.as_ref().expect("has an agent");
            if a.status == Status::Blocked {
                return Err(refused(
                    "agent_blocked",
                    "the agent is waiting for an answer to a question or an approval",
                ));
            }
            Ok(info)
        })?;
        let pasted = if text.contains('\n') {
            format!("\x1b[200~{text}\x1b[201~")
        } else {
            text.to_string()
        };
        self.write(target, pasted.as_bytes())?;
        tokio::time::sleep(Duration::from_millis(120)).await;
        self.write(target, b"\r")?;
        self.with_agent(target, |p, info| {
            if let Some(a) = p.agent.as_mut() {
                a.set(Status::Working);
            }
            Ok(info)
        })?;
        Ok(before)
    }

    /// The last `lines` lines: scrollback, then the screen. Reading a
    /// finished turn counts as looking at it.
    fn read(&self, target: &str, lines: usize) -> Reply {
        self.with_agent(target, |p, _| {
            let text = transcript(&mut p.screen.lock().unwrap(), lines);
            if let Some(a) = p.agent.as_mut() {
                if a.status == Status::Done {
                    a.set(Status::Idle);
                }
            }
            Ok(json!(text))
        })
    }

    async fn send_keys(&self, target: &str, keys: &[String]) -> Reply {
        let mut bytes = Vec::new();
        for k in keys {
            bytes.push(
                detect::key_bytes(k)
                    .ok_or_else(|| refused("bad_key", format!("no key called {k}")))?,
            );
        }
        for b in bytes {
            self.write(target, &b)?;
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
        Ok(Value::Null)
    }

    /// What an agent's hook says (`pitupitu hook <pane> <event>`).
    fn hook(&self, pane: &str, event: &str, detail: &str) {
        let mut st = self.state.lock().unwrap();
        let Some(a) = st.panes.get_mut(pane).and_then(|p| p.agent.as_mut()) else {
            return;
        };
        match event {
            "start" => {
                a.ready = true;
                if a.status != Status::Blocked {
                    a.set(Status::Idle);
                }
            }
            "prompt" => a.set(Status::Working),
            "tool" if a.status == Status::Blocked => a.set(Status::Working),
            "notify" if detail == "permission" => a.set(Status::Blocked),
            "stop" | "done" => a.done(),
            _ => {}
        }
    }

    /// Twice a second: processes and screens. Returns true when the holder
    /// should exit.
    fn tick(&self) -> bool {
        let mut st = self.state.lock().unwrap();
        let mut gone = Vec::new();
        for (id, p) in st.panes.iter_mut() {
            if matches!(p.child.try_wait(), Ok(Some(_))) {
                gone.push(id.clone());
                continue;
            }
            let shell_in_front = match (p.master.process_group_leader(), p.shell_pid) {
                (Some(leader), Some(shell)) => leader as u32 == shell,
                _ => false,
            };
            let Some(a) = p.agent.as_mut() else { continue };
            if shell_in_front && a.started.elapsed() > Duration::from_secs(3) {
                info!(pane = %id, "agent exited");
                p.agent = None;
                continue;
            }
            let screen = p.screen.lock().unwrap().screen().contents();
            match detect::dialog(&screen) {
                Some(Dialog::Trust) if !a.ready => {}
                Some(_) => a.set(Status::Blocked),
                None if a.status == Status::Blocked => a.set(Status::Working),
                None => {}
            }
            // Agents without a start hook (Codex): ready once the screen
            // has settled with no dialog on it.
            if screen != a.last_screen {
                a.last_screen = screen;
                a.stable_since = Instant::now();
            }
            let settle = if a.kind == "claude" { 8 } else { 2 };
            if !a.ready
                && a.status != Status::Blocked
                && a.started.elapsed() > Duration::from_secs(settle)
                && a.stable_since.elapsed() > Duration::from_millis(1500)
                && !a.last_screen.trim().is_empty()
            {
                a.ready = true;
                a.set(Status::Idle);
            }
        }
        for id in gone {
            if let Some(p) = st.panes.remove(&id) {
                info!(pane = %id, "terminal closed");
                if !st.panes.values().any(|x| x.ws == p.ws) {
                    st.workspaces.remove(&p.ws);
                }
            }
        }
        if st.used {
            st.panes.is_empty()
        } else {
            self.born.elapsed() > IDLE_EXIT
        }
    }

    /// `pitupitu attach`: the connection becomes the terminal. The screen
    /// as it is first, then everything the pane prints; what the person
    /// types goes to the pane. The pane takes the viewer's size while
    /// attached. Looking counts as having seen a finished turn.
    async fn attach(
        &self,
        req: Value,
        mut input: BufReader<tokio::net::unix::OwnedReadHalf>,
        mut output: tokio::net::unix::OwnedWriteHalf,
    ) {
        let target = req["agent"].as_str().unwrap_or_default().to_string();
        let rows = req["rows"].as_u64().unwrap_or(ROWS as u64).clamp(5, 500) as u16;
        let cols = req["cols"].as_u64().unwrap_or(COLS as u64).clamp(20, 1000) as u16;
        let found = {
            let mut st = self.state.lock().unwrap();
            let id = Self::find(&st, &target)
                .or_else(|| st.panes.contains_key(&target).then(|| target.clone()));
            id.and_then(|id| {
                let p = st.panes.get_mut(&id)?;
                let _ = p.master.resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                });
                let mut parser = p.screen.lock().unwrap();
                parser.screen_mut().set_size(rows, cols);
                let dump = parser.screen().contents_formatted();
                drop(parser);
                if let Some(a) = p.agent.as_mut() {
                    if a.status == Status::Done {
                        a.set(Status::Idle);
                    }
                }
                Some((id, dump, p.out.subscribe()))
            })
        };
        let Some((id, dump, mut rx)) = found else {
            let _ = output
                .write_all(
                    b"{\"err\":{\"code\":\"agent_not_found\",\"message\":\"no such session\"}}\n",
                )
                .await;
            return;
        };
        if output.write_all(b"{\"ok\":null}\n").await.is_err() {
            return;
        }
        let _ = output.write_all(b"\x1b[2J\x1b[H").await;
        let _ = output.write_all(&dump).await;
        let to_viewer = async {
            while let Ok(bytes) = rx.recv().await {
                if output.write_all(&bytes).await.is_err() {
                    break;
                }
            }
        };
        let from_viewer = async {
            let pending = input.buffer().to_vec();
            if !pending.is_empty() {
                self.write_pane(&id, &pending);
            }
            let mut buf = [0u8; 1024];
            loop {
                match input.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => self.write_pane(&id, &buf[..n]),
                }
            }
        };
        tokio::select! {
            _ = to_viewer => {}
            _ = from_viewer => {}
        }
        let mut st = self.state.lock().unwrap();
        if let Some(p) = st.panes.get_mut(&id) {
            let _ = p.master.resize(PtySize {
                rows: ROWS,
                cols: COLS,
                pixel_width: 0,
                pixel_height: 0,
            });
            p.screen.lock().unwrap().screen_mut().set_size(ROWS, COLS);
        }
    }

    fn write_pane(&self, id: &str, bytes: &[u8]) {
        let mut st = self.state.lock().unwrap();
        if let Some(p) = st.panes.get_mut(id) {
            let _ = p.writer.write_all(bytes);
            let _ = p.writer.flush();
        }
    }
}

/// The last `lines` lines a pane printed: the scrollback above the screen,
/// then the screen, trailing blank lines dropped.
fn transcript(parser: &mut vt100::Parser<Title>, lines: usize) -> String {
    let rows = parser.screen().size().0 as usize;
    parser.screen_mut().set_scrollback(0);
    let mut screen: Vec<String> = parser
        .screen()
        .contents()
        .lines()
        .map(str::to_string)
        .collect();
    while screen.last().is_some_and(|l| l.trim().is_empty()) {
        screen.pop();
    }
    // How much scrollback there is: ask for all of it and see what stuck.
    parser.screen_mut().set_scrollback(usize::MAX);
    let available = parser.screen().scrollback();
    let mut out: Vec<String> = Vec::new();
    let mut want_above = lines.saturating_sub(screen.len()).min(available);
    // Views of `rows` lines each, from the oldest wanted down to the screen.
    while want_above > 0 {
        parser.screen_mut().set_scrollback(want_above);
        let view = parser.screen().contents();
        let take = want_above.min(rows);
        out.extend(view.lines().take(take).map(str::to_string));
        want_above -= take;
    }
    parser.screen_mut().set_scrollback(0);
    out.extend(screen);
    let skip = out.len().saturating_sub(lines);
    out[skip..].join("\n")
}

fn tail(screen: &str, n: usize) -> String {
    let lines: Vec<&str> = screen.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_transcript_reaches_into_the_scrollback() {
        let mut p = vt100::Parser::new_with_callbacks(5, 40, 100, Title(None));
        for i in 1..=12 {
            p.process(format!("line {i}\r\n").as_bytes());
        }
        let t = transcript(&mut p, 8);
        let got: Vec<&str> = t.lines().collect();
        assert_eq!(
            got,
            ["line 5", "line 6", "line 7", "line 8", "line 9", "line 10", "line 11", "line 12"]
        );
        // The screen is left where it was.
        assert_eq!(p.screen().scrollback(), 0);
        assert_eq!(transcript(&mut p, 2), "line 11\nline 12");
    }

    #[test]
    fn a_turn_reported_done_twice_counts_once() {
        let now = Instant::now();
        let mut a = Agent {
            name: "x".into(),
            kind: "codex".into(),
            status: Status::Idle,
            seq: 1,
            completion: 0,
            ready: true,
            started: now,
            trust_answered: false,
            last_screen: String::new(),
            stable_since: now,
            last_done: None,
            worked_since_done: false,
        };
        a.set(Status::Working);
        a.done();
        a.done();
        assert_eq!((a.status, a.completion), (Status::Done, 1));
        // Read in between (done -> idle), then the repeat: still one.
        a.set(Status::Idle);
        a.done();
        assert_eq!((a.status, a.completion), (Status::Idle, 1));
        // A new turn counts.
        a.set(Status::Working);
        a.done();
        assert_eq!((a.status, a.completion), (Status::Done, 2));
    }

    #[test]
    fn window_titles_are_kept() {
        let mut p = vt100::Parser::new_with_callbacks(5, 40, 0, Title(None));
        p.process(b"\x1b]2;Fix the login bug\x07");
        assert_eq!(p.callbacks().0.as_deref(), Some("Fix the login bug"));
        p.process("\x1b]2;✳ Pong reply\x07".as_bytes());
        assert_eq!(p.callbacks().0.as_deref(), Some("Pong reply"));
        p.process("\x1b]2;⠋ | place\x07".as_bytes());
        assert_eq!(p.callbacks().0.as_deref(), Some("place"));
        p.process("\x1b]2;⠋\x07".as_bytes());
        assert_eq!(p.callbacks().0, None);
    }
}
