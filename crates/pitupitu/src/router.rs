//! What serves the box link's calls: herdr, the terminal host, or both at
//! once. New sessions go where the box's driver setting says (`auto`: herdr
//! when it answers, else the terminal host); a session already running on
//! the other side is still served there until it ends. The terminal host's
//! ids start with `tw`, so an id says where it lives; an agent name is
//! looked up in the terminal host first.

use crate::term;
use async_trait::async_trait;
use herdr::{
    AgentHost, AgentInfo, AgentKind, AgentStatus, Created, PaneId, Result, StartOpts, TabId,
    Workspace, WorkspaceId,
};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct Router {
    herdr: Arc<dyn AgentHost>,
    terms: Arc<dyn AgentHost>,
    /// `auto`, `herdr` or `terminal`, as the cloud last said.
    wanted: Mutex<String>,
    /// Whether herdr answered the last `list_agents`.
    herdr_up: AtomicBool,
}

impl Router {
    pub fn new(herdr: Arc<dyn AgentHost>, terms: Arc<dyn AgentHost>) -> Self {
        Self {
            herdr,
            terms,
            wanted: Mutex::new("auto".into()),
            herdr_up: AtomicBool::new(false),
        }
    }

    pub fn set_wanted(&self, driver: &str) {
        *self.wanted.lock().unwrap() = driver.to_string();
    }

    pub fn herdr_up(&self) -> bool {
        self.herdr_up.load(Ordering::Relaxed)
    }

    /// Where new sessions go: `herdr` or `terminal`.
    pub fn driver(&self) -> &'static str {
        match self.wanted.lock().unwrap().as_str() {
            "herdr" => "herdr",
            "terminal" => "terminal",
            _ if self.herdr_up() => "herdr",
            _ => "terminal",
        }
    }

    fn new_sessions(&self) -> &dyn AgentHost {
        if self.driver() == "herdr" {
            &*self.herdr
        } else {
            &*self.terms
        }
    }

    fn by_id(&self, id: &str) -> &dyn AgentHost {
        if term::is_ours(id) {
            &*self.terms
        } else {
            &*self.herdr
        }
    }

    async fn by_agent(&self, agent: &str) -> &dyn AgentHost {
        if term::is_ours(agent) {
            return &*self.terms;
        }
        match self.terms.list_agents().await {
            Ok(list) if list.iter().any(|a| a.target() == agent) => &*self.terms,
            _ => &*self.herdr,
        }
    }
}

#[async_trait]
impl AgentHost for Router {
    async fn list_workspaces(&self) -> Result<Vec<Workspace>> {
        let mut all = self.herdr.list_workspaces().await.unwrap_or_default();
        all.extend(self.terms.list_workspaces().await?);
        Ok(all)
    }

    async fn create_workspace(&self, cwd: &Path, label: &str) -> Result<Created> {
        self.new_sessions().create_workspace(cwd, label).await
    }

    /// The cloud's hint (`terminal` for scratch work such as lookups, so
    /// herdr is not littered with them) counts only while the setting is
    /// `auto`; a forced `herdr` or `terminal` wins.
    async fn create_workspace_hinted(
        &self,
        cwd: &Path,
        label: &str,
        driver: Option<&str>,
    ) -> Result<Created> {
        let wanted = self.wanted.lock().unwrap().clone();
        let auto = !matches!(wanted.as_str(), "herdr" | "terminal");
        let host: &dyn AgentHost = match (auto, driver) {
            (true, Some("terminal")) => &*self.terms,
            (true, Some("herdr")) if self.herdr_up() => &*self.herdr,
            _ => self.new_sessions(),
        };
        host.create_workspace(cwd, label).await
    }

    async fn create_tab(
        &self,
        workspace: &WorkspaceId,
        cwd: Option<&Path>,
        label: &str,
    ) -> Result<Created> {
        self.by_id(&workspace.0)
            .create_tab(workspace, cwd, label)
            .await
    }

    async fn close_workspace(&self, workspace: &WorkspaceId) -> Result<()> {
        self.by_id(&workspace.0).close_workspace(workspace).await
    }

    async fn close_tab(&self, tab: &TabId) -> Result<()> {
        self.by_id(&tab.0).close_tab(tab).await
    }

    async fn start_agent(
        &self,
        name: &str,
        kind: AgentKind,
        pane: &PaneId,
        opts: StartOpts,
    ) -> Result<AgentInfo> {
        self.by_id(&pane.0)
            .start_agent(name, kind, pane, opts)
            .await
    }

    async fn prompt(&self, agent: &str, text: &str) -> Result<AgentInfo> {
        self.by_agent(agent).await.prompt(agent, text).await
    }

    async fn agent(&self, agent: &str) -> Result<AgentInfo> {
        self.by_agent(agent).await.agent(agent).await
    }

    /// herdr's agents and the terminal host's. herdr not answering is not
    /// an error here: it only means new sessions go to the terminal host
    /// (with `auto`).
    async fn list_agents(&self) -> Result<Vec<AgentInfo>> {
        let herdr = self.herdr.list_agents().await;
        self.herdr_up.store(herdr.is_ok(), Ordering::Relaxed);
        let mut all = herdr.unwrap_or_default();
        all.extend(self.terms.list_agents().await?);
        Ok(all)
    }

    async fn wait(
        &self,
        agent: &str,
        until: &[AgentStatus],
        timeout: Duration,
    ) -> Result<AgentInfo> {
        self.by_agent(agent).await.wait(agent, until, timeout).await
    }

    async fn read(&self, agent: &str, lines: u32) -> Result<String> {
        self.by_agent(agent).await.read(agent, lines).await
    }

    async fn send_keys(&self, agent: &str, keys: &[&str]) -> Result<()> {
        self.by_agent(agent).await.send_keys(agent, keys).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testhost::{agent, FakeHost};

    #[tokio::test]
    async fn new_sessions_follow_the_driver_and_ids_say_where_they_live() {
        let herdr = FakeHost::default();
        herdr
            .agents
            .lock()
            .unwrap()
            .push(agent("notes", "w1:p1", AgentStatus::Idle, 1));
        let terms = FakeHost::default();
        terms
            .agents
            .lock()
            .unwrap()
            .push(agent("pitu-x", "tw1:p1", AgentStatus::Working, 2));
        let r = Router::new(Arc::new(herdr.clone()), Arc::new(terms.clone()));

        // Both sides are listed; herdr answered, so auto means herdr.
        let all = r.list_agents().await.unwrap();
        assert_eq!(all.len(), 2);
        assert!(r.herdr_up());
        assert_eq!(r.driver(), "herdr");
        r.set_wanted("terminal");
        assert_eq!(r.driver(), "terminal");
        r.set_wanted("auto");

        // An agent name or a pane id goes where it lives.
        r.read("pitu-x", 10).await.unwrap();
        r.read("notes", 10).await.unwrap();
        r.read("tw1:p1", 10).await.unwrap();
        assert_eq!(terms.log(), ["read pitu-x 10", "read tw1:p1 10"]);
        assert_eq!(herdr.log(), ["read notes 10"]);
    }

    #[tokio::test]
    async fn the_clouds_hint_counts_only_under_auto() {
        let herdr = FakeHost::default();
        herdr
            .agents
            .lock()
            .unwrap()
            .push(agent("notes", "w1:p1", AgentStatus::Idle, 1));
        let terms = FakeHost::default();
        let r = Router::new(Arc::new(herdr.clone()), Arc::new(terms.clone()));
        r.list_agents().await.unwrap();
        assert!(r.herdr_up());

        // auto: a lookup goes to the terminal host when the cloud says so,
        // a project task to herdr as usual.
        let p = Path::new("/home/me/pitupitu");
        r.create_workspace_hinted(p, "pitupitu", Some("terminal"))
            .await
            .unwrap();
        r.create_workspace_hinted(Path::new("/home/me/code/app"), "app", None)
            .await
            .unwrap();
        assert_eq!(terms.log(), ["create_workspace /home/me/pitupitu pitupitu"]);
        assert_eq!(herdr.log(), ["create_workspace /home/me/code/app app"]);

        // Forced herdr: the hint is ignored.
        r.set_wanted("herdr");
        r.create_workspace_hinted(p, "pitupitu", Some("terminal"))
            .await
            .unwrap();
        assert_eq!(herdr.log().len(), 2);
        assert_eq!(terms.log().len(), 1);

        // Forced terminal: everything goes there, hint or not.
        r.set_wanted("terminal");
        r.create_workspace_hinted(Path::new("/home/me/code/app"), "app", Some("herdr"))
            .await
            .unwrap();
        assert_eq!(terms.log().len(), 2);
    }
}
