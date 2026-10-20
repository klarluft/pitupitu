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

/// The release pages: `$PITUPITU_RELEASES_URL` (tests), else GitHub's.
fn releases_url() -> String {
    std::env::var("PITUPITU_RELEASES_URL")
        .unwrap_or_else(|_| format!("https://github.com/{REPO}/releases"))
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
    let url = format!("{}/latest", releases_url());
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
    fn the_checksum_file_must_match() {
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        verify(b"abc", &format!("{abc}  pitupitu-x.tar.gz\n")).unwrap();
        verify(b"abc", &abc.to_uppercase()).unwrap();
        assert!(verify(b"abd", abc).is_err());
        assert!(verify(b"abc", "").is_err());
    }
}
