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
        Ok(vec![])
    }

    async fn create_workspace(&self, cwd: &Path, label: &str) -> Result<Created> {
        self.note(format!("create_workspace {} {label}", cwd.display()));
        Ok(Created {
            workspace_id: WorkspaceId("w9".into()),
            tab_id: TabId("w9:t1".into()),
            root_pane: PaneId("w9:p1".into()),
            cwd: cwd.to_path_buf(),
        })
    }

    async fn create_tab(&self, _: &WorkspaceId, _: Option<&Path>, _: &str) -> Result<Created> {
        unimplemented!()
    }

    async fn close_workspace(&self, _: &WorkspaceId) -> Result<()> {
        Ok(())
    }

    async fn close_tab(&self, tab: &TabId) -> Result<()> {
        self.note(format!("close_tab {tab}"));
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
