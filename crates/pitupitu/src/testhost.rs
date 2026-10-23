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
        Ok(agent(name, "w1:p1", AgentStatus::Idle, 1))
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
/// runs in it at first, and it is stopped and deleted on drop, passed or
/// not. The person's own session is never touched. `None` when `herdr`
/// is not on `PATH`, and the test skips.
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
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let name = format!("pitutest-{}-{nanos}", std::process::id());
        Some(ScratchHerdr {
            cli: herdr::HerdrCli::with_binary("herdr").with_session(&name),
            name,
        })
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
