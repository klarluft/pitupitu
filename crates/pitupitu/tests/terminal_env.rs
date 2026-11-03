//! A herdr server the box starts gives its panes what a terminal would.
//!
//! `PITUPITU_LIVE_HERDR=1 cargo test --test terminal_env`, best run the way
//! the service runs (`systemd-run --user --wait --pipe …` on Linux), since
//! from a terminal there is little to add. It starts a server in a
//! throwaway session, reads the environment of a shell in it, and stops
//! and deletes the session afterwards.

use herdr::{AgentHost, HerdrCli};
use std::collections::HashMap;
use std::process::Command;
use std::time::{Duration, Instant};

struct Scratch(String);

impl Drop for Scratch {
    fn drop(&mut self) {
        let herdr = |args: &[&str]| {
            let _ = Command::new("herdr").args(args).output();
        };
        herdr(&["--session", &self.0, "server", "stop"]);
        herdr(&["session", "delete", &self.0]);
    }
}

/// Runs `script` in a fresh pane of the scratch session and returns what
/// it wrote to `out`.
async fn in_pane(host: &HerdrCli, session: &str, script: &str) -> String {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    let created = host
        .create_workspace(dir.path(), "pitutest-env")
        .await
        .unwrap();
    let line = format!("{{ {script} ; }} > {} 2>&1; mv {0} {0}.done", out.display());
    let run = Command::new("herdr")
        .args([
            "--session",
            session,
            "pane",
            "run",
            &created.root_pane.0,
            &line,
        ])
        .output()
        .unwrap();
    assert!(run.status.success(), "{run:?}");
    let done = out.with_extension("done");
    let end = Instant::now() + Duration::from_secs(30);
    while !done.exists() {
        assert!(Instant::now() < end, "the pane never ran the script");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let _ = host.close_workspace(&created.workspace_id).await;
    std::fs::read_to_string(done).unwrap()
}

#[tokio::test]
async fn panes_get_what_a_terminal_would() {
    if std::env::var("PITUPITU_LIVE_HERDR").is_err() {
        eprintln!("skipped: set PITUPITU_LIVE_HERDR=1 to run against a live herdr");
        return;
    }
    let session = format!("pitutest-env-{}", std::process::id());
    let _scratch = Scratch(session.clone());
    let env = pitupitu::terminal_env::for_herdr().await;
    let host = HerdrCli::new()
        .with_session(&session)
        .with_server_env(env.clone());
    host.start_server().expect("herdr runs");
    let end = Instant::now() + Duration::from_secs(15);
    while host.list_agents().await.is_err() {
        assert!(Instant::now() < end, "herdr's server never answered");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Everything the box added reaches the pane's shell. Its startup files
    // may change a value (PATH is checked separately), so only what the
    // box worked out itself is compared exactly.
    let printed = in_pane(&host, &session, "env -0").await;
    let pane: HashMap<&str, &str> = printed
        .split('\0')
        .filter_map(|e| e.split_once('='))
        .collect();
    for (k, v) in &env.set {
        let (k, v) = (k.to_str().unwrap(), v.to_str().unwrap());
        if k == "PATH" {
            continue;
        }
        assert!(pane.contains_key(k), "{k} did not reach the pane");
        if k.starts_with("WSL") || matches!(k, "DISPLAY" | "WAYLAND_DISPLAY" | "PULSE_SERVER") {
            assert_eq!(pane.get(k).copied(), Some(v), "{k} changed on the way");
        }
    }
    for k in &env.remove {
        let k = k.to_str().unwrap();
        if k.starts_with("HERDR_") {
            continue; // herdr sets its own in every pane
        }
        assert!(!pane.contains_key(k), "{k} reached the pane");
    }
    let path = pane.get("PATH").copied().unwrap_or_default();
    let wanted = env
        .set
        .iter()
        .find(|(k, _)| k == "PATH")
        .map(|(_, v)| v.to_str().unwrap().to_string());
    if let Some(wanted) = wanted {
        for dir in wanted.split(':') {
            assert!(
                path.split(':').any(|d| d == dir),
                "{dir} is not on the pane's PATH"
            );
        }
    }

    // On WSL, Windows programs run from the pane as from a terminal.
    if pitupitu::terminal_env::is_wsl() {
        let out = in_pane(
            &host,
            &session,
            "cd /mnt/c && cmd.exe /d /c ver && command -v explorer.exe",
        )
        .await;
        assert!(out.contains("Microsoft Windows"), "cmd.exe: {out}");
        assert!(out.contains("explorer.exe"), "Windows PATH: {out}");
        // VS Code's `code`, when it is installed on the Windows side.
        if path.contains("Microsoft VS Code/bin") {
            let out = in_pane(&host, &session, "code --version").await;
            assert!(out.lines().count() >= 2, "code: {out}");
        }
    }
    let names: Vec<_> = env.set.iter().map(|(k, _)| k.to_string_lossy()).collect();
    eprintln!("added or changed: {names:?}; removed: {:?}", env.remove);
}
