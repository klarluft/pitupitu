//! herdr on this box: what the inventory says about it, keeping its
//! server running, and updating it when the person asks.
//!
//! The box starts `herdr server` when none answers, at its own start and
//! whenever the socket stops answering, with a growing pause between
//! tries; it never stops one. The person opening `herdr` attaches to the
//! same server. herdr is updated the way it was installed (`brew upgrade
//! herdr` under a Cellar, `herdr update` for herdr's own installer) and
//! never on the box's own initiative.

use crate::update::{self, Refused, Updated};
use herdr::{HerdrCli, Status};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// The first pause after starting a server that did not come up.
const RETRY_MIN: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(300);
/// How long the box waits for a server it started to answer, at most.
const COME_UP: Duration = Duration::from_secs(5);

/// The `herdr` object of the inventory (`docs/box-link.md`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HerdrInfo {
    pub version: String,
    pub protocol: Option<u64>,
    pub running: bool,
    /// A newer herdr release than the installed one.
    pub latest: Option<String>,
    pub restart_needed: bool,
    /// `brew`, `installer` or `other` (mise, Nix, a build of one's own).
    pub install: &'static str,
}

impl HerdrInfo {
    /// What is known from `status`, whether the server answers right now
    /// and the newest herdr release.
    pub fn new(status: &Status, running: bool, newest: Option<&str>) -> Self {
        HerdrInfo {
            version: status.version.clone(),
            protocol: status.protocol,
            running,
            latest: newest
                .filter(|n| update::is_newer(n, &status.version))
                .map(str::to_string),
            restart_needed: status.restart_needed,
            install: install_kind(status.binary.as_deref()),
        }
    }
}

/// How herdr was installed: `brew` under a Cellar; `installer` where
/// herdr's installer puts it (`~/.local/bin`, `$HERDR_INSTALL_DIR`, or
/// under `~/.herdr`); else `other`, which the box does not update.
pub fn install_kind(binary: Option<&Path>) -> &'static str {
    let Some(binary) = binary else {
        return "other";
    };
    if update::is_brew(binary) {
        return "brew";
    }
    let home = crate::config::home_dir();
    let mut dirs = vec![home.join(".local/bin")];
    if let Some(d) = std::env::var_os("HERDR_INSTALL_DIR") {
        dirs.push(PathBuf::from(d));
    }
    let real = std::fs::canonicalize(binary).unwrap_or_else(|_| binary.to_path_buf());
    let dir = real.parent().unwrap_or(Path::new(""));
    if dirs.iter().any(|d| d == dir) || real.starts_with(home.join(".herdr")) {
        "installer"
    } else {
        "other"
    }
}

/// `herdr status`, or for a herdr too old for it, `--version` alone.
/// `None` when herdr is not installed (or cannot be run at all).
pub async fn status(cli: &HerdrCli) -> Option<Status> {
    match cli.status().await {
        Ok(s) => Some(s),
        Err(herdr::Error::Spawn(_)) => None,
        Err(_) => {
            let v = cli.version().await.ok()?;
            Some(Status {
                version: v.trim_start_matches("herdr").trim().to_string(),
                ..Status::default()
            })
        }
    }
}

/// Keeps herdr's server running: started when it does not answer, with
/// a growing pause between tries so a herdr that cannot start is not
/// spun on.
pub struct Keeper {
    cli: HerdrCli,
    next: Mutex<(Instant, Duration)>,
}

impl Keeper {
    pub fn new(cli: HerdrCli) -> Self {
        Keeper {
            cli,
            next: Mutex::new((Instant::now(), RETRY_MIN)),
        }
    }

    pub fn cli(&self) -> &HerdrCli {
        &self.cli
    }

    /// The server answers again: the next failure is retried at once.
    pub fn answered(&self) {
        *self.next.lock().unwrap() = (Instant::now(), RETRY_MIN);
    }

    /// herdr did not answer `err`: start a server if none runs and the
    /// pause since the last try is over. `true` when one was started.
    pub async fn revive(&self, err: &herdr::Error) -> bool {
        if matches!(err, herdr::Error::Spawn(_)) {
            return false; // not installed
        }
        {
            let (at, _) = *self.next.lock().unwrap();
            if Instant::now() < at {
                return false;
            }
        }
        let not_running = err.code() == Some(herdr::SERVER_NOT_RUNNING)
            || matches!(self.cli.status().await, Ok(s) if !s.running);
        if !not_running {
            // A server answers, for something else: look again later.
            self.next.lock().unwrap().0 = Instant::now() + RETRY_MIN;
            return false;
        }
        {
            let mut next = self.next.lock().unwrap();
            next.0 = Instant::now() + next.1;
            next.1 = (next.1 * 2).min(RETRY_MAX);
        }
        match self.cli.start_server() {
            Ok(()) => {
                info!(session = ?self.cli.session(), "started herdr's server");
                true
            }
            Err(e) => {
                warn!("could not start herdr's server: {e}");
                false
            }
        }
    }

    /// At the box agent's start: when herdr does not answer, start its
    /// server and give it a moment to come up.
    pub async fn ensure(&self, host: &dyn herdr::AgentHost) {
        let Err(e) = host.list_agents().await else {
            return;
        };
        if !self.revive(&e).await {
            return;
        }
        let end = Instant::now() + COME_UP;
        while Instant::now() < end {
            tokio::time::sleep(Duration::from_millis(200)).await;
            if host.list_agents().await.is_ok() {
                self.answered();
                return;
            }
        }
    }
}

/// herdr as it is and as it could be, before an update.
#[derive(Debug, Clone)]
pub struct HerdrCheck {
    pub status: Status,
    pub install: &'static str,
    /// The newest release, when GitHub could be asked.
    pub newest: Option<String>,
}

impl HerdrCheck {
    /// A newer release exists, or it could not be told (then the update
    /// itself finds out).
    pub fn newer(&self) -> bool {
        self.newest
            .as_deref()
            .is_none_or(|n| update::is_newer(n, &self.status.version))
    }
}

pub async fn check(cli: &HerdrCli) -> Result<HerdrCheck, Refused> {
    let Some(status) = status(cli).await else {
        return Err(Refused::Unavailable(
            "herdr is not installed here; `curl -fsSL https://herdr.dev/install.sh | sh` \
             installs it"
                .into(),
        ));
    };
    Ok(HerdrCheck {
        install: install_kind(status.binary.as_deref()),
        newest: update::herdr_newest().await,
        status,
    })
}

/// Update herdr the way it was installed. The running server and its
/// panes stay as they are unless `handoff` (herdr's installer only):
/// then herdr restarts the server with the panes kept, best effort.
pub async fn apply(cli: &HerdrCli, c: &HerdrCheck, handoff: bool) -> Result<Updated, Refused> {
    let from = c.status.version.clone();
    match c.install {
        "brew" => {
            let bin = c.status.binary.clone().unwrap_or_else(|| "herdr".into());
            update::run_brew(&bin, "herdr").await?;
        }
        "installer" => {
            cli.update(handoff).await.map_err(|e| {
                Refused::Failed(match e {
                    herdr::Error::Exit { stderr, .. } if !stderr.is_empty() => stderr,
                    e => e.to_string(),
                })
            })?;
        }
        _ => {
            return Err(Refused::Unavailable(format!(
                "herdr at {} was installed some other way (mise, Nix, a build of your \
                 own); update it that way",
                c.status
                    .binary
                    .as_deref()
                    .map_or("an unknown path".into(), |b| b.display().to_string())
            )))
        }
    }
    let after = status(cli).await.unwrap_or_default();
    if after.version.is_empty() || !update::is_newer(&after.version, &from) {
        return Err(Refused::Nothing(format!(
            "herdr {from} is the newest {} offers",
            if c.install == "brew" {
                "Homebrew"
            } else {
                "herdr's installer"
            }
        )));
    }
    let restart = if after.restart_needed {
        "needed"
    } else if handoff && c.install == "installer" {
        "done"
    } else {
        "none"
    };
    Ok(Updated {
        what: "herdr",
        from,
        to: after.version,
        install: c.install,
        restart,
    })
}

/// The `update` call for herdr: check, then update.
pub async fn update(cli: &HerdrCli, handoff: bool) -> Result<Updated, Refused> {
    let c = check(cli).await?;
    if !c.newer() {
        return Err(Refused::Nothing(format!(
            "herdr {} is the newest release",
            c.status.version
        )));
    }
    apply(cli, &c, handoff).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_install_is_told_by_where_the_binary_is() {
        let home = crate::config::home_dir();
        assert_eq!(
            install_kind(Some(&home.join(".local/bin/herdr-not-here"))),
            "installer"
        );
        assert_eq!(
            install_kind(Some(Path::new(
                "/opt/homebrew/Cellar/herdr/0.9.5/bin/herdr"
            ))),
            "brew"
        );
        assert_eq!(
            install_kind(Some(
                &home.join(".local/share/mise/installs/herdr/bin/herdr")
            )),
            "other"
        );
        assert_eq!(install_kind(None), "other");
    }

    #[test]
    fn latest_is_only_a_newer_release() {
        let s = Status {
            version: "0.9.3".into(),
            protocol: Some(22),
            binary: None,
            running: true,
            restart_needed: false,
        };
        assert_eq!(
            HerdrInfo::new(&s, true, Some("0.9.5")).latest.as_deref(),
            Some("0.9.5")
        );
        assert_eq!(HerdrInfo::new(&s, true, Some("0.9.3")).latest, None);
        assert_eq!(HerdrInfo::new(&s, false, None).latest, None);
        let json = serde_json::to_value(HerdrInfo::new(&s, false, None)).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"version": "0.9.3", "protocol": 22, "running": false,
                "latest": null, "restart_needed": false, "install": "other"})
        );
    }
}
