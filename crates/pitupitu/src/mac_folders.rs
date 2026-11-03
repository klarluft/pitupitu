//! macOS asks the person before a program opens their Desktop, Documents,
//! Downloads or iCloud Drive, and remembers the answer for that program.
//! Everything running in a herdr server the box agent started (the shells,
//! the agents) is asked about as the box agent: a server started from a
//! terminal app is asked about as that app instead.
//!
//! So that the question comes while the person is at the machine, and not
//! later while they are away with only their phone (when nobody can answer
//! and the agent just gets "Operation not permitted"), setup leaves a
//! request beside the config and restarts the service; the service, as it
//! starts, opens each folder once, which is when macOS asks, and writes
//! down what was allowed. Release builds are signed, so the answer holds
//! across updates.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The folders macOS guards, by the name System Settings shows and their
/// place under the home folder.
pub const FOLDERS: &[(&str, &str)] = &[
    ("Desktop", "Desktop"),
    ("Documents", "Documents"),
    ("Downloads", "Downloads"),
    (
        "iCloud Drive",
        "Library/Mobile Documents/com~apple~CloudDocs",
    ),
];

/// Where System Settings lets the person change the answers.
pub const SETTINGS: &str = "System Settings › Privacy & Security › Files and Folders › pitupitu";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Folder {
    pub name: String,
    pub allowed: bool,
}

fn request_file(config: &Path) -> PathBuf {
    config.with_file_name("folders-ask")
}

fn result_file(config: &Path) -> PathBuf {
    config.with_file_name("folders.json")
}

/// Setup: ask the service to open the folders when it next starts.
pub fn request(config: &Path) -> std::io::Result<()> {
    let _ = std::fs::remove_file(result_file(config));
    if let Some(dir) = config.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(request_file(config), b"")
}

/// The service, at its start: when setup asked, open each folder once (in
/// the background: macOS holds each open until the person answers) and
/// write down what was allowed.
pub fn answer_if_asked(config: &Path) {
    if !cfg!(target_os = "macos") || std::fs::remove_file(request_file(config)).is_err() {
        return;
    }
    let config = config.to_path_buf();
    std::thread::spawn(move || {
        let home = crate::config::home_dir();
        let found = check(&home);
        let allowed = found.iter().filter(|f| f.allowed).count();
        tracing::info!(folders = found.len(), allowed, "macOS folder access");
        if let Ok(json) = serde_json::to_vec(&found) {
            let _ = std::fs::write(result_file(&config), json);
        }
    });
}

/// Opens each folder that exists under `home`: allowed when it can be
/// listed.
fn check(home: &Path) -> Vec<Folder> {
    FOLDERS
        .iter()
        .filter(|(_, rel)| home.join(rel).is_dir())
        .map(|(name, rel)| Folder {
            name: (*name).into(),
            allowed: std::fs::read_dir(home.join(rel)).is_ok(),
        })
        .collect()
}

/// What the service found last time, if it has looked.
pub fn last(config: &Path) -> Option<Vec<Folder>> {
    let text = std::fs::read(result_file(config)).ok()?;
    serde_json::from_slice(&text).ok()
}

/// Setup: wait for the service's answer, at most `limit`.
pub fn wait(config: &Path, limit: Duration) -> Option<Vec<Folder>> {
    let end = Instant::now() + limit;
    loop {
        if let Some(found) = last(config) {
            return Some(found);
        }
        if Instant::now() >= end {
            return None;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// One line for `pitupitu status`.
pub fn summary(found: &[Folder]) -> String {
    let names = |allowed: bool| {
        found
            .iter()
            .filter(|f| f.allowed == allowed)
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    match (names(true), names(false)) {
        (yes, no) if no.is_empty() => format!("{yes} allowed"),
        (yes, no) if yes.is_empty() => format!("not allowed: {no} (to change: {SETTINGS})"),
        (yes, no) => format!("{yes} allowed; not allowed: {no} (to change: {SETTINGS})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_folders_that_exist_are_opened() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join("Documents")).unwrap();
        std::fs::create_dir(home.path().join("Desktop")).unwrap();
        let found = check(home.path());
        assert_eq!(
            found,
            vec![
                Folder {
                    name: "Desktop".into(),
                    allowed: true
                },
                Folder {
                    name: "Documents".into(),
                    allowed: true
                },
            ]
        );
    }

    #[test]
    fn a_request_is_answered_once_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("pitupitu.toml");
        request(&config).unwrap();
        assert!(request_file(&config).exists());
        assert_eq!(last(&config), None);
        let found = vec![Folder {
            name: "Documents".into(),
            allowed: false,
        }];
        std::fs::write(result_file(&config), serde_json::to_vec(&found).unwrap()).unwrap();
        assert_eq!(wait(&config, Duration::from_millis(10)), Some(found));
        // A new request forgets the old answer.
        request(&config).unwrap();
        assert_eq!(last(&config), None);
    }

    #[test]
    fn the_summary_says_how_to_change_a_no() {
        let f = |name: &str, allowed| Folder {
            name: name.into(),
            allowed,
        };
        assert_eq!(
            summary(&[f("Desktop", true), f("Documents", true)]),
            "Desktop, Documents allowed"
        );
        let s = summary(&[f("Desktop", true), f("Downloads", false)]);
        assert!(
            s.starts_with("Desktop allowed; not allowed: Downloads (to change: System Settings")
        );
    }
}
