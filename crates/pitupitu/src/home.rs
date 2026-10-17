//! The home folder (`~/pitupitu` by default): where sessions that belong
//! to no project run, and pitupitu skills (`skills/<name>/SKILL.md`). Those
//! skill files, and nothing else from the folder, are sent to the cloud.
//! What pitupitu knows about the person lives in the cloud since 0.2
//! (`pitupitu prefs`); a `PITUPITU.md` here is no longer read.

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::Path;

/// Each file sent is cut to this many bytes.
pub const MAX_FILE: usize = 64 * 1024;

/// One file as the `home` note carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HomeFile {
    pub path: String,
    pub text: String,
}

/// Create what is missing: the folder and `skills/`. Returns the paths
/// created.
pub fn ensure(home: &Path) -> Result<Vec<String>> {
    let mut created = Vec::new();
    if !home.is_dir() {
        std::fs::create_dir_all(home).with_context(|| format!("creating {}", home.display()))?;
        created.push(home.display().to_string());
    }
    let skills = home.join("skills");
    if !skills.is_dir() {
        std::fs::create_dir_all(&skills)
            .with_context(|| format!("creating {}", skills.display()))?;
        created.push(skills.display().to_string());
    }
    Ok(created)
}

/// Every `skills/*/SKILL.md`, sorted by path. Missing or unreadable files
/// are skipped.
pub fn scan(home: &Path) -> Vec<HomeFile> {
    let mut files = Vec::new();
    let mut skills: Vec<_> = std::fs::read_dir(home.join("skills"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    skills.sort();
    for name in skills {
        if let Some(text) = read_capped(&home.join("skills").join(&name).join("SKILL.md")) {
            files.push(HomeFile {
                path: format!("skills/{name}/SKILL.md"),
                text,
            });
        }
    }
    files
}

fn read_capped(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if text.len() > MAX_FILE {
        let mut end = MAX_FILE;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_pitupitu_files_are_sent() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("pitupitu");
        let created = ensure(&home).unwrap();
        assert_eq!(created.len(), 2);
        assert!(ensure(&home).unwrap().is_empty(), "nothing is overwritten");

        std::fs::write(home.join("AGENTS.md"), "not sent").unwrap();
        std::fs::write(home.join("notes.txt"), "not sent").unwrap();
        for name in ["deploy", "audit"] {
            std::fs::create_dir_all(home.join("skills").join(name)).unwrap();
            std::fs::write(home.join("skills").join(name).join("SKILL.md"), name).unwrap();
        }
        std::fs::create_dir_all(home.join("skills/empty")).unwrap();
        std::fs::write(home.join("skills/deploy/script.sh"), "not sent").unwrap();

        // An old PITUPITU.md is not read any more.
        std::fs::write(home.join("PITUPITU.md"), "old instructions").unwrap();
        let files = scan(&home);
        let paths: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["skills/audit/SKILL.md", "skills/deploy/SKILL.md"]);
        assert_eq!(files[0].text, "audit");
    }

    #[test]
    fn big_files_are_cut_on_a_character_boundary() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("skills/big")).unwrap();
        std::fs::write(dir.path().join("skills/big/SKILL.md"), "ż".repeat(MAX_FILE)).unwrap();
        let files = scan(dir.path());
        assert!(files[0].text.len() <= MAX_FILE);
        assert!(files[0].text.len() > MAX_FILE - 2);
    }
}
