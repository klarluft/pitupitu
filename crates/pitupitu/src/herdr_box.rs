//! herdr on this box: what the inventory says about it, installing it
//! when the person says yes in setup, and updating it when they ask.
//!
//! herdr's server is the person's own: they run `herdr` in a terminal of
//! their login session, and the box only connects to it. The box never
//! starts, restarts or stops a server. herdr is updated the way it was
//! installed (`brew upgrade herdr` under a Cellar, `herdr update` for
//! herdr's own installer) and never on the box's own initiative; the
//! running server stays as it is until the person restarts herdr.

use crate::update::{self, Refused, Updated};
use herdr::{HerdrCli, Status};
use serde::Serialize;
use std::path::{Path, PathBuf};

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
    // Everything compared as the real path: on macOS a temporary or home
    // folder is often reached through a symlink (`/var` is `/private/var`).
    let real_path = |p: PathBuf| std::fs::canonicalize(&p).unwrap_or(p);
    let home = crate::config::home_dir();
    let mut dirs = vec![real_path(home.join(".local/bin"))];
    if let Some(d) = std::env::var_os("HERDR_INSTALL_DIR") {
        dirs.push(real_path(PathBuf::from(d)));
    }
    let real = real_path(binary.to_path_buf());
    let dir = real.parent().unwrap_or(Path::new(""));
    if dirs.iter().any(|d| d == dir) || real.starts_with(real_path(home.join(".herdr"))) {
        "installer"
    } else {
        "other"
    }
}

/// herdr's own installer.
pub const INSTALLER: &str = "https://herdr.dev/install.sh";
/// The command that installs herdr, as the person would type it.
pub const INSTALL_COMMAND: &str = "curl -fsSL https://herdr.dev/install.sh | sh";
/// What the person does when herdr is installed but its server does not
/// answer: the box never starts one itself.
pub const OPEN_HINT: &str = "open herdr in a terminal (run `herdr`) and leave it open";

/// The herdr this box drives: `herdr` on `PATH` (or `$HERDR_BIN`), else
/// where herdr's installer puts it, so a herdr installed after the
/// service was written, into a folder its `PATH` lacks, is still found.
/// The session from `$PITUPITU_HERDR_SESSION` either way.
pub fn cli() -> HerdrCli {
    let cli = HerdrCli::new();
    if std::env::var_os("HERDR_BIN").is_some() || on_path("herdr").is_some() {
        return cli;
    }
    match find_binary() {
        Some(bin) => {
            let found = HerdrCli::with_binary(bin.display().to_string());
            match cli.session() {
                Some(s) => found.with_session(s),
                None => found,
            }
        }
        None => cli,
    }
}

/// `name` on `PATH`, as the shell would find it.
pub fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|f| f.is_file())
}

/// The `herdr` binary: on `PATH`, else where herdr's installer puts it
/// (which a fresh install may not have on `PATH` yet).
pub fn find_binary() -> Option<PathBuf> {
    on_path("herdr").or_else(|| {
        let dir = std::env::var_os("HERDR_INSTALL_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| crate::config::home_dir().join(".local/bin"));
        Some(dir.join("herdr")).filter(|f| f.is_file())
    })
}

/// Install herdr with its own installer, its output shown as it runs.
/// Returns where the binary landed.
pub fn install() -> anyhow::Result<PathBuf> {
    let script = format!(
        "set -e; f=\"$(mktemp)\"; trap 'rm -f \"$f\"' EXIT; curl -fsSL {INSTALLER} -o \"$f\"; sh \"$f\""
    );
    let status = std::process::Command::new("sh")
        .args(["-c", &script])
        .stdin(std::process::Stdio::null())
        .status()
        .map_err(|e| anyhow::anyhow!("running sh: {e}"))?;
    if !status.success() {
        anyhow::bail!("herdr's installer exited with {status}");
    }
    find_binary()
        .ok_or_else(|| anyhow::anyhow!("herdr's installer ran, but no herdr is to be found"))
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
        return Err(Refused::Unavailable(format!(
            "herdr is not installed here; `{INSTALL_COMMAND}` installs it"
        )));
    };
    Ok(HerdrCheck {
        install: install_kind(status.binary.as_deref()),
        newest: update::herdr_newest().await,
        status,
    })
}

/// Update herdr the way it was installed. The running server and its
/// panes stay as they are; the new version runs once the person
/// restarts herdr.
pub async fn apply(cli: &HerdrCli, c: &HerdrCheck) -> Result<Updated, Refused> {
    let from = c.status.version.clone();
    match c.install {
        "brew" => {
            let bin = c.status.binary.clone().unwrap_or_else(|| "herdr".into());
            update::run_brew(&bin, "herdr").await?;
        }
        "installer" => {
            cli.update().await.map_err(|e| {
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
pub async fn update(cli: &HerdrCli) -> Result<Updated, Refused> {
    let c = check(cli).await?;
    if !c.newer() {
        return Err(Refused::Nothing(format!(
            "herdr {} is the newest release",
            c.status.version
        )));
    }
    apply(cli, &c).await
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
            server_version: None,
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
