//! One scenario against a live herdr with a real Claude Code
//! (`docs/sessions.md`), when `PITUPITU_LIVE_HERDR=1`: what every state,
//! counter and error code means is checked here, not in a support ticket.

use herdr::{AgentHost, AgentInfo, AgentKind, AgentStatus, HerdrCli, StartOpts, WorkspaceId};
use std::path::Path;
use std::time::{Duration, Instant};

/// What the agent under test is asked, and how it answers.
struct Script<'a> {
    kind: &'a str,
    agent_args: Vec<String>,
    /// A prompt and a word its answer must contain.
    question: &'a str,
    expect: &'a str,
    /// A prompt that makes the agent ask a question, and the keys that
    /// answer it, when the agent can be made to.
    ask: Option<(&'a str, &'a [&'a str], &'a str)>,
}

fn ready(s: AgentStatus) -> bool {
    matches!(s, AgentStatus::Idle | AgentStatus::Done)
}

/// Poll `agent` until `f` holds or `limit` passes, the way the cloud does:
/// by the counters, never by one status seen once.
async fn until(
    host: &dyn AgentHost,
    name: &str,
    limit: Duration,
    what: &str,
    f: impl Fn(&AgentInfo) -> bool,
) -> AgentInfo {
    let end = Instant::now() + limit;
    loop {
        let info = host
            .agent(name)
            .await
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        if f(&info) {
            return info;
        }
        assert!(Instant::now() < end, "never {what}: {info:?}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn scenario(host: &dyn AgentHost, cwd: &Path, name: &str, s: Script<'_>) {
    let kind: AgentKind = s.kind.parse().unwrap();

    // Kinds are known and the one under test is among them.
    let kinds = host.kinds().await;
    assert!(kinds.iter().any(|k| k == s.kind), "{kinds:?}");
    let bogus: AgentKind = "no-such-agent".parse().unwrap();
    let e = host
        .create_workspace_for(cwd, "conformance", Some(&bogus))
        .await
        .unwrap_err();
    assert_eq!(e.code(), Some("unsupported_agent_kind"), "{e}");

    // Nothing by that name yet.
    let e = host.agent(name).await.unwrap_err();
    assert_eq!(e.code(), Some("agent_not_found"), "{e}");

    // A workspace with one shell pane, listed.
    let created = host
        .create_workspace_for(cwd, "conformance", Some(&kind))
        .await
        .unwrap();
    assert_eq!(created.cwd, cwd);
    let listed = host.list_workspaces().await.unwrap();
    assert!(listed
        .iter()
        .any(|w| w.workspace_id == created.workspace_id));

    // Started and ready.
    let info = host
        .start_agent(
            name,
            kind,
            &created.root_pane,
            StartOpts {
                accept_trust_prompt: true,
                timeout: Some(Duration::from_secs(60)),
                agent_args: s.agent_args.clone(),
            },
        )
        .await
        .unwrap();
    assert!(ready(info.agent_status), "{info:?}");
    assert!(info.interactive_ready && !info.launch_pending, "{info:?}");
    assert_eq!(info.agent, s.kind);
    assert_eq!(info.name.as_deref(), Some(name));
    assert_eq!(info.pane_id, created.root_pane);
    assert_eq!(info.workspace_id, created.workspace_id);
    let agents = host.list_agents().await.unwrap();
    assert!(agents.iter().any(|a| a.name.as_deref() == Some(name)));

    // A prompt: the state from before, then the counters move and the
    // answer is on screen. The status alone is never the signal.
    let before = host.prompt(name, s.question).await.unwrap();
    let after = until(host, name, Duration::from_secs(90), "finished", |a| {
        a.state_change_seq > before.state_change_seq
            && !matches!(a.agent_status, AgentStatus::Working)
            && a.completion_seq > before.completion_seq
    })
    .await;
    assert!(ready(after.agent_status), "{after:?}");
    let text = host.read(name, 60).await.unwrap();
    assert!(text.contains(s.expect), "{text}");

    // A question on screen: blocked, a prompt refused, keys answer it.
    if let Some((ask, keys, answered)) = s.ask {
        let before = host.prompt(name, ask).await.unwrap();
        let blocked = until(host, name, Duration::from_secs(30), "blocked", |a| {
            a.agent_status == AgentStatus::Blocked
        })
        .await;
        assert!(blocked.state_change_seq > before.state_change_seq);
        let e = host.prompt(name, "x").await.unwrap_err();
        assert_eq!(e.code(), Some("agent_blocked"), "{e}");
        let waited = host
            .wait(name, &[AgentStatus::Blocked], Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(waited.agent_status, AgentStatus::Blocked);
        host.send_keys(name, keys).await.unwrap();
        until(host, name, Duration::from_secs(30), "answered", |a| {
            a.completion_seq > blocked.completion_seq && ready(a.agent_status)
        })
        .await;
        let text = host.read(name, 20).await.unwrap();
        assert!(text.contains(answered), "{text}");
    }

    // Closing the workspace ends the session: not listed, not found.
    host.close_workspace(&created.workspace_id).await.unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        let ws: Vec<WorkspaceId> = host
            .list_workspaces()
            .await
            .unwrap()
            .into_iter()
            .map(|w| w.workspace_id)
            .collect();
        let gone = !ws.contains(&created.workspace_id)
            && host
                .agent(name)
                .await
                .err()
                .and_then(|e| e.code().map(str::to_string))
                == Some("agent_not_found".into());
        if gone {
            break;
        }
        assert!(Instant::now() < end, "the session did not end: {ws:?}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// A throwaway herdr session, stopped and deleted on drop: the scenario
/// never runs in the person's own herdr.
struct Scratch(String);

impl Drop for Scratch {
    fn drop(&mut self) {
        let herdr = |args: &[&str]| {
            let _ = std::process::Command::new("herdr").args(args).output();
        };
        herdr(&["--session", &self.0, "server", "stop"]);
        herdr(&["session", "delete", &self.0]);
    }
}

/// `PITUPITU_LIVE_HERDR=1 cargo test --test conformance`: herdr and a
/// signed-in Claude Code on this machine. It starts a herdr server in a
/// throwaway session, works in a temporary folder, and stops and deletes
/// the session afterwards.
#[tokio::test]
async fn herdr_passes_the_scenario() {
    if std::env::var("PITUPITU_LIVE_HERDR").is_err() {
        eprintln!("skipped: set PITUPITU_LIVE_HERDR=1 to run against a live herdr");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let session = format!("pitutest-conf-{}", std::process::id());
    let _scratch = Scratch(session.clone());
    let host = HerdrCli::new().with_session(&session);
    host.start_server().expect("herdr runs");
    let end = Instant::now() + Duration::from_secs(15);
    while host.list_agents().await.is_err() {
        assert!(Instant::now() < end, "herdr's server never answered");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let name = format!("conf-{}", std::process::id());
    scenario(
        &host,
        dir.path(),
        &name,
        Script {
            kind: "claude",
            agent_args: vec!["--model".into(), "haiku".into()],
            question: "Reply with exactly the word pong and nothing else.",
            expect: "pong",
            ask: None,
        },
    )
    .await;
}
