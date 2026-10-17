//! `pitupitu.toml`: the box's key and where its home folder is. Nothing
//! else lives here; voice and other settings belong to the account.
//!
//! ```toml
//! [cloud]
//! url = "https://api.pitupitu.chat"  # default; omit
//! api_key = "ptp_…"                  # or $PITUPITU_CLOUD_API_KEY
//! box = "my-laptop"                  # the name on the dashboard
//!
//! [home]
//! path = "/home/me/pitupitu"
//! ```
//!
//! Other tables in the file are left alone, so an older config keeps working.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub const DEFAULT_CLOUD_URL: &str = "https://api.pitupitu.chat";

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Where the file is (or would be).
    pub path: PathBuf,
    pub cloud_url: String,
    pub api_key: Option<String>,
    pub box_name: Option<String>,
    pub home: Option<PathBuf>,
}

/// `--config`, else `$PITUPITU_CONFIG`, else `~/.config/pitupitu/pitupitu.toml`.
pub fn default_path() -> PathBuf {
    if let Some(p) = std::env::var_os("PITUPITU_CONFIG") {
        return PathBuf::from(p);
    }
    home_dir()
        .join(".config")
        .join("pitupitu")
        .join("pitupitu.toml")
}

pub fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// `~/pitupitu`, the home folder setup offers.
pub fn default_home() -> PathBuf {
    home_dir().join("pitupitu")
}

/// Expand a leading `~/` and make the path absolute.
pub fn expand(path: &str) -> PathBuf {
    let path = path.trim();
    let p = if path == "~" {
        home_dir()
    } else if let Some(rest) = path.strip_prefix("~/") {
        home_dir().join(rest)
    } else {
        PathBuf::from(path)
    };
    if p.is_absolute() {
        p
    } else {
        std::env::current_dir().map(|d| d.join(&p)).unwrap_or(p)
    }
}

fn read_doc(path: &Path) -> Result<toml_edit::DocumentMut> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    text.parse()
        .with_context(|| format!("{} is not valid TOML", path.display()))
}

fn get_str(doc: &toml_edit::DocumentMut, table: &str, key: &str) -> Option<String> {
    doc.get(table)?
        .get(key)?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

impl Config {
    /// Read the file (a missing one is an empty config), then the
    /// environment, which wins.
    pub fn load(path: &Path) -> Result<Config> {
        let doc = read_doc(path)?;
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        Ok(Config {
            path: path.to_path_buf(),
            cloud_url: env("PITUPITU_CLOUD_URL")
                .or_else(|| get_str(&doc, "cloud", "url"))
                .unwrap_or_else(|| DEFAULT_CLOUD_URL.to_string())
                .trim_end_matches('/')
                .to_string(),
            api_key: env("PITUPITU_CLOUD_API_KEY").or_else(|| get_str(&doc, "cloud", "api_key")),
            box_name: get_str(&doc, "cloud", "box"),
            home: get_str(&doc, "home", "path").map(|p| expand(&p)),
        })
    }

    /// The name the box goes by: the one setup stored, else the hostname.
    pub fn box_name(&self) -> String {
        self.box_name.clone().unwrap_or_else(hostname)
    }

    /// `wss://…/v1/box` for the configured cloud.
    pub fn box_url(&self) -> String {
        let base = if let Some(rest) = self.cloud_url.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = self.cloud_url.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            self.cloud_url.clone()
        };
        format!("{base}/v1/box")
    }
}

/// Set `[table] key = value` in the file, keeping everything else (comments
/// included) as it was. Returns whether anything changed. The file ends up
/// readable by its owner only, since it holds the key.
pub fn set(path: &Path, entries: &[(&str, &str, &str)]) -> Result<bool> {
    let mut doc = read_doc(path)?;
    let mut changed = false;
    for (table, key, value) in entries {
        if doc.get(table).is_none() {
            doc[table] = toml_edit::table();
        }
        if doc[table].get(key).and_then(|v| v.as_str()) != Some(value) {
            doc[table][key] = toml_edit::value(*value);
            changed = true;
        }
    }
    if changed {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        write_private(path, &doc.to_string())?;
    }
    Ok(changed)
}

#[cfg(unix)]
fn write_private(path: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("writing {}", path.display()))?;
    f.write_all(text.as_bytes())?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &Path, text: &str) -> Result<()> {
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

/// This machine's hostname, the default box name.
pub fn hostname() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "my-box".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_keeps_the_rest_of_the_file_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("pitupitu.toml");
        assert_eq!(Config::load(&path).unwrap().api_key, None);

        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "# keep me\n[speech]\nkind = \"cloud\" # old\n").unwrap();
        let entries = [
            ("cloud", "api_key", "ptp_test"),
            ("cloud", "box", "testbox"),
            ("home", "path", "/tmp/home"),
        ];
        assert!(set(&path, &entries).unwrap());
        assert!(
            !set(&path, &entries).unwrap(),
            "a second write changes nothing"
        );

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("# keep me") && text.contains("# old"),
            "{text}"
        );
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.api_key.as_deref(), Some("ptp_test"));
        assert_eq!(cfg.box_name(), "testbox");
        assert_eq!(cfg.home, Some(PathBuf::from("/tmp/home")));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn box_url_follows_the_cloud_url() {
        let mut cfg = Config {
            path: PathBuf::new(),
            cloud_url: DEFAULT_CLOUD_URL.into(),
            api_key: None,
            box_name: None,
            home: None,
        };
        assert_eq!(cfg.box_url(), "wss://api.pitupitu.chat/v1/box");
        cfg.cloud_url = "http://127.0.0.1:8787".into();
        assert_eq!(cfg.box_url(), "ws://127.0.0.1:8787/v1/box");
    }

    #[test]
    fn tilde_expands_to_the_home_directory() {
        assert_eq!(expand("~/pitupitu"), home_dir().join("pitupitu"));
        assert_eq!(expand("/abs"), PathBuf::from("/abs"));
    }
}
