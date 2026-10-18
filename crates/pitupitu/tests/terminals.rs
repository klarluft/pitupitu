//! The terminal host end to end, through the real binary: the holder,
//! its PTYs, the hooks and the socket protocol. The agent is a stand-in
//! `claude` script that behaves like Claude Code's hooks would: it reports
//! that it started, then answers each line it reads and reports the turn
//! done, and asks for permission when told to.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

const FAKE_CLAUDE: &str = r#"#!/bin/sh
# Stand-in for Claude Code: hooks via the pitupitu binary, like --settings would.
hook() { "$FAKE_PITUPITU" hook "$PITUPITU_PANE" "$1" </dev/null >/dev/null 2>&1; }
printf '\033]2;Stand-in session\007'
echo "fake claude ready (args: $*)"
hook start
while IFS= read -r line; do
  hook prompt
  case "$line" in
    ask*) printf 'Do you want to proceed?\n 1. Yes\n 2. No\n'; IFS= read -r answer; echo "answered $answer";;
    quit) exit 0;;
    *) echo "answer: $line";;
  esac
  hook stop
done
"#;

/// The holder, stopped when the test ends, passed or not: a holder with
/// terminals open keeps running by design.
struct Holder(std::process::Child);

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn call(socket: &Path, req: Value) -> Value {
    let stream = UnixStream::connect(socket).await.unwrap();
    let (r, mut w) = stream.into_split();
    let mut line = req.to_string();
    line.push('\n');
    w.write_all(line.as_bytes()).await.unwrap();
    let mut reply = String::new();
    BufReader::new(r).read_line(&mut reply).await.unwrap();
    serde_json::from_str(&reply).unwrap()
}

async fn agent(socket: &Path) -> Value {
    call(socket, json!({"op": "agent", "agent": "pitu-test"})).await["ok"].clone()
}

/// Wait until the agent's status is `want`.
async fn until(socket: &Path, want: &str) -> Value {
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        let a = agent(socket).await;
        if a["agent_status"] == want {
            return a;
        }
        assert!(Instant::now() < end, "never {want}: {a}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn an_agent_in_the_box_s_own_terminals() {
    let exe = PathBuf::from(env!("CARGO_BIN_EXE_pitupitu"));
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    // A login shell would read the profiles and lose the stand-in's PATH.
    // Slow to start, like a login shell with a long rc: the holder must
    // not type into the pane before the shell is reading.
    let shell = bin.join("testshell");
    std::fs::write(&shell, "#!/bin/sh\nsleep 0.3\nexec /bin/sh\n").unwrap();
    let fake = bin.join("claude");
    std::fs::write(&fake, FAKE_CLAUDE).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        for f in [&fake, &shell] {
            std::fs::set_permissions(f, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let work = dir.path().join("project");
    std::fs::create_dir_all(&work).unwrap();
    let socket = dir.path().join("terms.sock");
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());

    let mut holder = Holder(
        std::process::Command::new(&exe)
            .arg("terminals")
            .env("PITUPITU_TERMS_SOCKET", &socket)
            .env("PATH", &path)
            .env("SHELL", &shell)
            .env("FAKE_PITUPITU", &exe)
            .spawn()
            .unwrap(),
    );
    let end = Instant::now() + Duration::from_secs(5);
    while UnixStream::connect(&socket).await.is_err() {
        assert!(Instant::now() < end, "holder did not start");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // A workspace for the folder, with one shell pane.
    let created = call(
        &socket,
        json!({"op": "create_workspace", "cwd": work, "label": "project"}),
    )
    .await["ok"]
        .clone();
    // Numbered from the clock, so a holder started again never reuses one.
    let ws_id = created["workspace_id"].as_str().unwrap().to_string();
    assert!(ws_id.starts_with("tw") && ws_id.len() > 4, "{ws_id}");
    let pane = created["root_pane"].as_str().unwrap().to_string();
    assert_eq!(pane, format!("{ws_id}:p1"));
    let ws = call(&socket, json!({"op": "list_workspaces"})).await;
    assert_eq!(ws["ok"][0]["label"], "project");

    // Only claude and codex.
    let refused = call(
        &socket,
        json!({"op": "start_agent", "name": "x", "kind": "gemini", "pane": pane}),
    )
    .await;
    assert_eq!(refused["err"]["code"], "unsupported_agent_kind");

    // Started, ready through its start hook, with the cloud's arguments.
    // The command is over 1024 bytes wherever the binary is: typed into a
    // PTY still in canonical mode, macOS would drop its tail, Enter
    // included, and the agent would never run.
    let _ = 0;
    let started = call(
        &socket,
        json!({"op": "start_agent", "name": "pitu-test", "kind": "claude", "pane": pane,
               "accept_trust_prompt": true,
               "agent_args": ["--model", "haiku", ],
               "timeout_ms": 10000}),
    )
    .await;
    let info = &started["ok"];
    assert_eq!(info["agent_status"], "idle", "{started}");
    assert_eq!(info["interactive_ready"], true);
    assert_eq!(info["workspace_id"], ws_id.as_str());
    let screen = call(
        &socket,
        json!({"op": "read", "agent": "pitu-test", "lines": 200}),
    )
    .await;
    let screen = screen["ok"].as_str().unwrap();
    assert!(screen.contains("--model haiku"), "{screen}");
    // Typed once the shell was reading: echoed by the shell alone, not by
    // the kernel before it.
    assert_eq!(
        screen.matches("claude --settings").count(),
        1,
        "the command typed before the shell was up: {screen}"
    );
    assert!(
        screen.contains("--settings"),
        "hooks for this session only: {screen}"
    );
    assert!(
        !screen.contains("dangerously"),
        "no permission flags: {screen}"
    );

    // A prompt: working, then done with the answer on screen.
    let before = call(
        &socket,
        json!({"op": "prompt", "agent": "pitu-test", "text": "hello there"}),
    )
    .await;
    let seq = before["ok"]["state_change_seq"].as_u64().unwrap();
    let done = until(&socket, "done").await;
    assert!(done["state_change_seq"].as_u64().unwrap() > seq);
    assert_eq!(done["completion_seq"], 1);
    assert_eq!(done["terminal_title_stripped"], "Stand-in session");
    let read = call(
        &socket,
        json!({"op": "read", "agent": "pitu-test", "lines": 10}),
    )
    .await;
    assert!(read["ok"].as_str().unwrap().contains("answer: hello there"));
    // Reading it counts as having looked.
    assert_eq!(agent(&socket).await["agent_status"], "idle");

    // A question on screen is blocked; a prompt is refused; keys answer it.
    call(
        &socket,
        json!({"op": "prompt", "agent": "pitu-test", "text": "ask me"}),
    )
    .await;
    until(&socket, "blocked").await;
    let refused = call(
        &socket,
        json!({"op": "prompt", "agent": "pitu-test", "text": "x"}),
    )
    .await;
    assert_eq!(refused["err"]["code"], "agent_blocked");
    call(
        &socket,
        json!({"op": "send_keys", "agent": "pitu-test", "keys": ["1", "enter"]}),
    )
    .await;
    until(&socket, "done").await;
    let read = call(
        &socket,
        json!({"op": "read", "agent": "pitu-test", "lines": 5}),
    )
    .await;
    assert!(read["ok"].as_str().unwrap().contains("answered 1"));

    // The agent exits: it leaves the list, the shell stays.
    call(
        &socket,
        json!({"op": "prompt", "agent": "pitu-test", "text": "quit"}),
    )
    .await;
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        let list = call(&socket, json!({"op": "list_agents"})).await;
        if list["ok"].as_array().unwrap().is_empty() {
            break;
        }
        assert!(Instant::now() < end, "agent never left: {list}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Closing the last workspace ends the holder.
    call(
        &socket,
        json!({"op": "close_workspace", "workspace": ws_id}),
    )
    .await;
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        if holder.0.try_wait().unwrap().is_some() {
            break;
        }
        assert!(Instant::now() < end, "holder did not exit");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
