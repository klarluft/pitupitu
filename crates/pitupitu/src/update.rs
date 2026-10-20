//! `pitupitu update`, and the check behind "a newer version exists". The
//! box never updates itself: the person runs the update, sees what would
//! change and what is running, and says yes each time. Replacing the
//! binary and restarting the box agent touches no session: herdr is a
//! separate program, and its server is not part of the service.
//!
//! Releases are GitHub releases of `klarluft/pitupitu`, packaged by
//! `.github/workflows/release.yml`: `<name>.tar.gz` holding
//! `<name>/pitupitu`, and `<name>.tar.gz.sha256` beside it.

use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const REPO: &str = "klarluft/pitupitu";
/// herdr's releases, for "a newer herdr exists".
pub const HERDR_REPO: &str = "herdrdev/herdr";

/// The release pages: `$PITUPITU_RELEASES_URL` (tests), else GitHub's.
fn releases_url() -> String {
    std::env::var("PITUPITU_RELEASES_URL")
        .unwrap_or_else(|_| format!("https://github.com/{REPO}/releases"))
}

/// herdr's release pages: `$PITUPITU_HERDR_RELEASES_URL` (tests), else
/// GitHub's.
fn herdr_releases_url() -> String {
    std::env::var("PITUPITU_HERDR_RELEASES_URL")
        .unwrap_or_else(|_| format!("https://github.com/{HERDR_REPO}/releases"))
}

pub fn current() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Where to read about a release.
pub fn notes_url(version: &str) -> String {
    format!("https://github.com/{REPO}/releases/tag/v{version}")
}

/// The release asset for this machine, as the release workflow names it.
pub fn target() -> Option<String> {
    target_for(std::env::consts::ARCH, std::env::consts::OS)
}

fn target_for(arch: &str, os: &str) -> Option<String> {
    let arch = match arch {
        "x86_64" | "aarch64" => arch,
        _ => return None,
    };
    let os = match os {
        "linux" => "unknown-linux-musl",
        "macos" => "apple-darwin",
        _ => return None,
    };
    Some(format!("pitupitu-{arch}-{os}"))
}

/// `1.2.3` or `v1.2.3` as numbers; anything else is not a version.
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let mut parts = s.trim().trim_start_matches('v').split('.');
    let mut next = || parts.next()?.parse::<u64>().ok();
    let v = (next()?, next()?, next()?);
    parts.next().is_none().then_some(v)
}

pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(format!("pitupitu/{}", current()))
        .timeout(Duration::from_secs(60))
        .build()
        .context("http client")
}

/// The newest release's version. GitHub answers `releases/latest` with a
/// redirect to `releases/tag/v<version>`; the tag is read from where the
/// redirect ends, so no API and no rate limit is involved.
pub async fn latest() -> Result<String> {
    latest_at(&releases_url()).await
}

/// The newest herdr release, read the same way.
pub async fn herdr_latest() -> Result<String> {
    latest_at(&herdr_releases_url()).await
}

async fn latest_at(releases: &str) -> Result<String> {
    let url = format!("{releases}/latest");
    let resp = client()?
        .get(&url)
        .send()
        .await
        .with_context(|| format!("asking {url}"))?;
    let landed = resp.url().to_string();
    let tag = landed.rsplit('/').next().unwrap_or_default();
    parse_version(tag)
        .map(|_| tag.trim_start_matches('v').to_string())
        .ok_or_else(|| anyhow!("no release version in {landed}"))
}

/// A newer release than this binary, when there is one and GitHub could
/// be reached; never fatal, for the inventory and `status`.
pub async fn newer() -> Option<String> {
    match latest().await {
        Ok(v) if is_newer(&v, current()) => Some(v),
        Ok(_) => None,
        Err(e) => {
            tracing::debug!("update check: {e:#}");
            None
        }
    }
}

/// The newest herdr release, whatever is installed; `None` when GitHub
/// could not be reached.
pub async fn herdr_newest() -> Option<String> {
    herdr_latest()
        .await
        .map_err(|e| tracing::debug!("herdr update check: {e:#}"))
        .ok()
}

/// How a program was installed, so it is updated the same way: `brew`
/// when its binary lives under a Homebrew `Cellar` (links resolved).
pub fn is_brew(binary: &Path) -> bool {
    let real = std::fs::canonicalize(binary).unwrap_or_else(|_| binary.to_path_buf());
    real.components().any(|c| c.as_os_str() == "Cellar")
}

/// How this box agent was installed: `brew` or `installer` (the release
/// tarball, from `install.sh` or `pitupitu update`).
pub fn install_kind() -> &'static str {
    match std::env::current_exe() {
        Ok(exe) if is_brew(&exe) => "brew",
        _ => "installer",
    }
}

async fn fetch(url: &str) -> Result<Vec<u8>> {
    let resp = client()?
        .get(url)
        .send()
        .await
        .with_context(|| format!("downloading {url}"))?;
    if !resp.status().is_success() {
        bail!("{url}: {}", resp.status());
    }
    Ok(resp.bytes().await.context("reading the download")?.to_vec())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The archive's SHA-256 must be the one its `.sha256` file names.
pub fn verify(archive: &[u8], sha256_file: &str) -> Result<()> {
    use sha2::{Digest, Sha256};
    let want = sha256_file
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_lowercase();
    let got = hex(&Sha256::digest(archive));
    if want.len() != 64 || got != want {
        bail!("the download's checksum does not match the release's: not installed");
    }
    Ok(())
}

/// Download, verify and unpack `version` for this machine. Returns the
/// folder to remove afterwards and the binary in it.
pub async fn download(version: &str) -> Result<(PathBuf, PathBuf)> {
    let name = target().ok_or_else(|| {
        anyhow!(
            "no release binary for {} on {}; build from source instead",
            std::env::consts::ARCH,
            std::env::consts::OS
        )
    })?;
    let url = format!("{}/download/v{version}/{name}.tar.gz", releases_url());
    let archive = fetch(&url).await?;
    let sum = fetch(&format!("{url}.sha256")).await?;
    verify(&archive, &String::from_utf8_lossy(&sum))?;
    let dir = std::env::temp_dir().join(format!("pitupitu-update-{}", std::process::id()));
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let tgz = dir.join(format!("{name}.tar.gz"));
    std::fs::write(&tgz, &archive).with_context(|| format!("writing {}", tgz.display()))?;
    let status = std::process::Command::new("tar")
        .arg("xzf")
        .arg(&tgz)
        .arg("-C")
        .arg(&dir)
        .status()
        .context("running tar")?;
    if !status.success() {
        bail!("tar could not unpack {}", tgz.display());
    }
    let binary = dir.join(&name).join("pitupitu");
    if !binary.is_file() {
        bail!("the release archive holds no {name}/pitupitu");
    }
    Ok((dir, binary))
}

/// Put `new` where `exe` is: a copy beside it, then one rename, so the
/// path is never half-written. The running process keeps its old binary
/// until it restarts.
pub fn install(new: &Path, exe: &Path) -> Result<()> {
    let dir = exe
        .parent()
        .ok_or_else(|| anyhow!("{} has no folder", exe.display()))?;
    let tmp = dir.join(".pitupitu.new");
    std::fs::copy(new, &tmp).with_context(|| {
        format!(
            "writing to {}: is it writable? If pitupitu came from Homebrew or cargo, update it that way",
            dir.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&tmp, exe).with_context(|| format!("replacing {}", exe.display()))?;
    Ok(())
}

/// What an update did, as the `update` call answers it
/// (`docs/box-link.md`): `restart` is `done` when the new version is
/// running (or, for the box agent, is about to be), `needed` when it waits
/// for a restart, `none` when nothing had to restart.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Updated {
    pub what: &'static str,
    pub from: String,
    pub to: String,
    pub install: &'static str,
    pub restart: &'static str,
}

/// Why an update did not happen, with the `update` call's error codes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// It ran and failed; the message is its last line.
    Failed(String),
    /// Not something the box can update: the message says what to run.
    Unavailable(String),
    /// Already the newest.
    Nothing(String),
}

impl Refused {
    pub fn code(&self) -> &'static str {
        match self {
            Refused::Failed(_) => "update_failed",
            Refused::Unavailable(_) => "update_unavailable",
            Refused::Nothing(_) => "nothing_to_update",
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Refused::Failed(m) | Refused::Unavailable(m) | Refused::Nothing(m) => m,
        }
    }
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

/// The box agent as it is and as it could be.
#[derive(Debug, Clone)]
pub struct BoxCheck {
    pub current: &'static str,
    pub latest: String,
    pub install: &'static str,
    pub exe: PathBuf,
}

impl BoxCheck {
    pub fn newer(&self) -> bool {
        is_newer(&self.latest, self.current)
    }
}

pub async fn check_box() -> Result<BoxCheck, Refused> {
    let latest = latest()
        .await
        .map_err(|e| Refused::Failed(format!("could not check for a newer version: {e:#}")))?;
    let exe = std::env::current_exe()
        .map_err(|e| Refused::Failed(format!("finding this binary: {e}")))?;
    Ok(BoxCheck {
        current: current(),
        latest,
        install: install_kind(),
        exe,
    })
}

/// Install the newer release the way this box agent was installed: `brew
/// upgrade pitupitu` under a Cellar, else the release tarball checked
/// against its SHA-256 and put in place of this binary. Nothing restarts
/// here; `restart` says whether a service will run the new binary.
pub async fn apply_box(c: &BoxCheck) -> Result<Updated, Refused> {
    let to = if c.install == "brew" {
        run_brew(&c.exe, "pitupitu").await?;
        // The tap follows a release within hours; what brew installed is
        // what counts.
        let now = installed_version()
            .await
            .unwrap_or_else(|| c.latest.clone());
        if !is_newer(&now, c.current) {
            return Err(Refused::Nothing(format!(
                "Homebrew has no newer pitupitu than {} yet; try again in a few hours",
                c.current
            )));
        }
        now
    } else {
        if target().is_none() {
            return Err(Refused::Unavailable(format!(
                "no release binary for {} on {}; build from source instead",
                std::env::consts::ARCH,
                std::env::consts::OS
            )));
        }
        let (dir, binary) = download(&c.latest)
            .await
            .map_err(|e| Refused::Failed(format!("{e:#}")))?;
        let installed = install(&binary, &c.exe);
        let _ = std::fs::remove_dir_all(&dir);
        installed.map_err(|e| Refused::Failed(format!("{e:#}")))?;
        c.latest.clone()
    };
    Ok(Updated {
        what: "box",
        from: c.current.to_string(),
        to,
        install: c.install,
        restart: if crate::service::installed() {
            "done"
        } else {
            "needed"
        },
    })
}

/// The `update` call for the box agent: check, then install.
pub async fn update_box() -> Result<Updated, Refused> {
    let c = check_box().await?;
    if !c.newer() {
        return Err(Refused::Nothing(format!(
            "pitupitu {} is the newest release",
            c.current
        )));
    }
    apply_box(&c).await
}

/// The version the service would run now (`pitupitu --version` of the
/// binary it points at).
async fn installed_version() -> Option<String> {
    let exe = crate::service::exe().ok()?;
    let out = tokio::process::Command::new(exe)
        .arg("--version")
        .output()
        .await
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let v = text.split_whitespace().last()?.to_string();
    parse_version(&v).map(|_| v)
}

/// The `brew` of the prefix `binary` was installed into (it may not be on
/// the service's PATH), else `brew` from PATH.
pub fn brew_for(binary: &Path) -> PathBuf {
    let real = std::fs::canonicalize(binary).unwrap_or_else(|_| binary.to_path_buf());
    real.ancestors()
        .find(|a| a.file_name().is_some_and(|n| n == "Cellar"))
        .and_then(Path::parent)
        .map(|prefix| prefix.join("bin/brew"))
        .filter(|b| b.is_file())
        .unwrap_or_else(|| PathBuf::from("brew"))
}

/// `brew upgrade <formula>`; a failure carries brew's last line.
pub async fn run_brew(binary: &Path, formula: &str) -> Result<(), Refused> {
    let brew = brew_for(binary);
    let out = tokio::process::Command::new(&brew)
        .args(["upgrade", formula])
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|e| Refused::Failed(format!("running {}: {e}", brew.display())))?;
    if out.status.success() {
        return Ok(());
    }
    let last = |b: &[u8]| {
        String::from_utf8_lossy(b)
            .lines()
            .rev()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(str::to_string)
    };
    Err(Refused::Failed(
        last(&out.stderr)
            .or_else(|| last(&out.stdout))
            .unwrap_or_else(|| format!("brew upgrade {formula} failed")),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_as_numbers() {
        assert_eq!(parse_version("v0.3.2"), Some((0, 3, 2)));
        assert_eq!(parse_version("10.0.0"), Some((10, 0, 0)));
        assert_eq!(parse_version("latest"), None);
        assert_eq!(parse_version("1.2"), None);
        assert!(is_newer("0.4.0", "0.3.2"));
        assert!(is_newer("0.3.10", "0.3.9"));
        assert!(!is_newer("0.3.2", "0.3.2"));
        assert!(!is_newer("0.3.1", "0.3.2"));
        assert!(!is_newer("nonsense", "0.3.2"));
    }

    #[test]
    fn the_asset_is_named_as_the_release_workflow_names_it() {
        assert_eq!(
            target_for("x86_64", "linux").unwrap(),
            "pitupitu-x86_64-unknown-linux-musl"
        );
        assert_eq!(
            target_for("aarch64", "macos").unwrap(),
            "pitupitu-aarch64-apple-darwin"
        );
        assert_eq!(target_for("x86_64", "windows"), None);
        assert_eq!(target_for("riscv64", "linux"), None);
    }

    #[test]
    fn a_homebrew_install_is_known_by_its_cellar() {
        assert!(is_brew(Path::new(
            "/opt/homebrew/Cellar/pitupitu/0.5.0/bin/pitupitu"
        )));
        assert!(!is_brew(Path::new("/home/me/.local/bin/pitupitu")));
        assert_eq!(
            brew_for(Path::new("/nonexistent/Cellar/herdr/0.9.5/bin/herdr")),
            PathBuf::from("brew")
        );
    }

    #[test]
    fn refusals_carry_the_calls_codes() {
        assert_eq!(Refused::Failed("x".into()).code(), "update_failed");
        assert_eq!(
            Refused::Unavailable("x".into()).code(),
            "update_unavailable"
        );
        assert_eq!(Refused::Nothing("x".into()).code(), "nothing_to_update");
    }

    #[test]
    fn the_checksum_file_must_match() {
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        verify(b"abc", &format!("{abc}  pitupitu-x.tar.gz\n")).unwrap();
        verify(b"abc", &abc.to_uppercase()).unwrap();
        assert!(verify(b"abd", abc).is_err());
        assert!(verify(b"abc", "").is_err());
    }
}
