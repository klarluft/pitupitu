//! An `AgentHost` for tests: records calls, answers from memory.

use async_trait::async_trait;
use herdr::{
    AgentHost, AgentInfo, AgentKind, AgentStatus, Created, Error, PaneId, Result, StartOpts, TabId,
    Workspace, WorkspaceId,
};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Default)]
pub struct FakeHost {
    log: Arc<Mutex<Vec<String>>>,
    pub agents: Arc<Mutex<Vec<AgentInfo>>>,
    pub workspaces: Arc<Mutex<Vec<Workspace>>>,
}

pub fn agent(name: &str, pane: &str, status: AgentStatus, seq: u64) -> AgentInfo {
    AgentInfo {
        agent: "claude".into(),
        name: Some(name.into()),
        agent_status: status,
        pane_id: PaneId(pane.into()),
        workspace_id: WorkspaceId("w1".into()),
        tab_id: Some(TabId("w1:t1".into())),
        cwd: "/tmp".into(),
        state_change_seq: seq,
        completion_seq: Some(seq),
        launch_pending: false,
        interactive_ready: true,
        terminal_title_stripped: None,
        agent_session: None,
        background: false,
    }
}

impl FakeHost {
    pub fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    fn note(&self, line: String) {
        self.log.lock().unwrap().push(line);
    }
}

#[async_trait]
impl AgentHost for FakeHost {
    async fn list_workspaces(&self) -> Result<Vec<Workspace>> {
        Ok(self.workspaces.lock().unwrap().clone())
    }

    async fn create_workspace(&self, cwd: &Path, label: &str) -> Result<Created> {
        self.note(format!("create_workspace {} {label}", cwd.display()));
        let mut ws = self.workspaces.lock().unwrap();
        let id = format!("w{}", 9 + ws.len());
        ws.push(Workspace {
            workspace_id: WorkspaceId(id.clone()),
            label: label.into(),
            agent_status: None,
            focused: false,
        });
        Ok(Created {
            workspace_id: WorkspaceId(id.clone()),
            tab_id: TabId(format!("{id}:t1")),
            root_pane: PaneId(format!("{id}:p1")),
            cwd: cwd.to_path_buf(),
        })
    }

    async fn create_tab(
        &self,
        workspace: &WorkspaceId,
        cwd: Option<&Path>,
        label: &str,
    ) -> Result<Created> {
        let n = self.log().len() + 2;
        self.note(format!("create_tab {workspace} {label}"));
        Ok(Created {
            workspace_id: workspace.clone(),
            tab_id: TabId(format!("{workspace}:t{n}")),
            root_pane: PaneId(format!("{workspace}:p{n}")),
            cwd: cwd.map(Path::to_path_buf).unwrap_or_default(),
        })
    }

    async fn close_workspace(&self, workspace: &WorkspaceId) -> Result<()> {
        self.note(format!("close_workspace {workspace}"));
        Ok(())
    }

    async fn close_tab(&self, tab: &TabId) -> Result<()> {
        self.note(format!("close_tab {tab}"));
        Ok(())
    }

    /// A pane of `w404` does not exist.
    async fn close_pane(&self, pane: &PaneId) -> Result<()> {
        if pane.0.starts_with("w404:") {
            return Err(herdr::Error::Api {
                code: "pane_not_found".into(),
                message: format!("pane {pane} not found"),
            });
        }
        self.note(format!("close_pane {pane}"));
        Ok(())
    }

    /// `wN:pM` lives in tab `wN:tM`; a pane of `w404` does not exist.
    async fn pane_tab(&self, pane: &PaneId) -> Result<TabId> {
        match pane.0.split_once(":p") {
            Some((ws, n)) if ws != "w404" => Ok(TabId(format!("{ws}:t{n}"))),
            _ => Err(Error::Api {
                code: "pane_not_found".into(),
                message: format!("pane {pane} not found"),
            }),
        }
    }

    /// Focuses `wN` then `wN:tM` for `wN:pM`, as [`Self::pane_tab`] finds it.
    async fn focus_pane(&self, pane: &PaneId) -> Result<()> {
        let tab = self.pane_tab(pane).await?;
        let ws = tab.0.split_once(":t").map_or("", |(w, _)| w).to_string();
        self.note(format!("focus_workspace {ws}"));
        self.note(format!("focus_tab {tab}"));
        Ok(())
    }

    async fn start_agent(
        &self,
        name: &str,
        kind: AgentKind,
        pane: &PaneId,
        opts: StartOpts,
    ) -> Result<AgentInfo> {
        self.note(format!(
            "start {name} {kind} {pane} trust={} args={:?}",
            opts.accept_trust_prompt, opts.agent_args
        ));
        Ok(agent(name, &pane.0, AgentStatus::Idle, 1))
    }

    async fn kinds(&self) -> Vec<String> {
        vec!["claude".into(), "codex".into()]
    }

    async fn prompt(&self, agent_name: &str, _: &str) -> Result<AgentInfo> {
        self.note(format!("prompt {agent_name}"));
        Ok(agent(agent_name, "w1:p1", AgentStatus::Idle, 1))
    }

    async fn agent(&self, name: &str) -> Result<AgentInfo> {
        if name == "slow" {
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        if name == "missing" {
            return Err(Error::Api {
                code: "agent_not_found".into(),
                message: "no such agent".into(),
            });
        }
        // An agent a test put in the list, by name or pane, as it is there.
        let listed = self
            .agents
            .lock()
            .unwrap()
            .iter()
            .find(|a| a.name.as_deref() == Some(name) || a.pane_id.0 == name)
            .cloned();
        Ok(listed.unwrap_or_else(|| agent(name, "w1:p1", AgentStatus::Idle, 1)))
    }

    async fn list_agents(&self) -> Result<Vec<AgentInfo>> {
        Ok(self.agents.lock().unwrap().clone())
    }

    async fn wait(&self, name: &str, until: &[AgentStatus], t: Duration) -> Result<AgentInfo> {
        self.note(format!("wait {name} {until:?} {}ms", t.as_millis()));
        Ok(agent(name, "w1:p1", AgentStatus::Idle, 2))
    }

    async fn read(&self, name: &str, lines: u32) -> Result<String> {
        self.note(format!("read {name} {lines}"));
        Ok(format!("screen of {name}, {lines} lines"))
    }

    async fn send_keys(&self, _: &str, _: &[&str]) -> Result<()> {
        Ok(())
    }
}

/// A throwaway herdr session for tests that need a live herdr: no server
/// runs in it until the test starts one ([`ScratchHerdr::serve`]), and it
/// is stopped and deleted on drop, passed or not. The person's own
/// session is never touched. `None` when `herdr` is not on `PATH`, and
/// the test skips.
pub struct ScratchHerdr {
    pub cli: herdr::HerdrCli,
    name: String,
}

impl ScratchHerdr {
    pub fn new() -> Option<Self> {
        if std::process::Command::new("herdr")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skipped: herdr is not on PATH");
            return None;
        }
        // Unique within this process by a counter: the clock alone is not
        // (macOS gives microseconds, and tests start on parallel threads
        // in the same microsecond), and two tests sharing a session share
        // its server, which the first to finish stops under the other.
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let name = format!("pitutest-{}-{n}", std::process::id());
        Some(ScratchHerdr {
            cli: herdr::HerdrCli::with_binary("herdr").with_session(&name),
            name,
        })
    }

    /// Start `herdr server` in this session, as a person would, and wait
    /// until it answers. Only tests do this; the box never does.
    pub async fn serve(&self) {
        use std::os::unix::process::CommandExt;
        let mut server = std::process::Command::new("herdr")
            .args(["--session", &self.name, "server"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .spawn()
            .expect("herdr runs");
        // In a process group of its own, as from a terminal; reaped when
        // it stops (the session is stopped on drop).
        std::thread::spawn(move || {
            let _ = server.wait();
        });
        let end = std::time::Instant::now() + Duration::from_secs(15);
        while self.cli.list_workspaces().await.is_err() {
            assert!(std::time::Instant::now() < end, "no server");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

impl Drop for ScratchHerdr {
    fn drop(&mut self) {
        let herdr = |args: &[&str]| {
            let _ = std::process::Command::new("herdr").args(args).output();
        };
        herdr(&["--session", &self.name, "server", "stop"]);
        herdr(&["session", "delete", &self.name]);
    }
}
