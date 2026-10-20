//! Against the real herdr, in a throwaway named session: no server runs
//! there at first, `start_server` starts one that answers, and the test
//! stops and deletes the session afterwards, passed or not. The person's
//! own herdr session is never touched. Skipped when `herdr` is not on
//! `PATH`.

use herdr::{AgentHost, HerdrCli, SERVER_NOT_RUNNING};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

/// The scratch session, stopped and deleted on drop.
struct Scratch(String);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = Command::new("herdr")
            .args(["--session", &self.0, "server", "stop"])
            .output();
        let _ = Command::new("herdr")
            .args(["session", "delete", &self.0])
            .output();
    }
}

fn herdr_on_path() -> bool {
    Command::new("herdr").arg("--version").output().is_ok()
}

#[tokio::test]
async fn a_server_started_by_the_box_answers_and_is_used() {
    if !herdr_on_path() {
        eprintln!("skipped: herdr is not on PATH");
        return;
    }
    let name = format!("pitutest-{}-{}", std::process::id(), rand_suffix());
    let _scratch = Scratch(name.clone());
    let cli = HerdrCli::new().with_session(&name);

    // Nothing runs in a fresh session: the API says so, status agrees.
    let e = cli.list_agents().await.unwrap_err();
    assert_eq!(e.code(), Some(SERVER_NOT_RUNNING), "{e}");
    let s = cli.status().await.unwrap();
    assert!(!s.running, "{s:?}");
    assert!(!s.version.is_empty() && s.protocol.is_some(), "{s:?}");

    cli.start_server().unwrap();
    let end = Instant::now() + Duration::from_secs(15);
    while cli.list_agents().await.is_err() {
        assert!(Instant::now() < end, "the server never answered");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(cli.status().await.unwrap().running);
    assert!(cli.kinds().await.iter().any(|k| k == "claude"));

    // A workspace in it, then gone again.
    let created = cli
        .create_workspace(Path::new(env!("CARGO_MANIFEST_DIR")), "pitutest")
        .await
        .unwrap();
    let listed = cli.list_workspaces().await.unwrap();
    assert!(listed
        .iter()
        .any(|w| w.workspace_id == created.workspace_id));
    cli.close_workspace(&created.workspace_id).await.unwrap();
}

fn rand_suffix() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos()
}
