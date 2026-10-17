//! The `pitupitu` skill: how the person's own agents install, link and
//! configure pitupitu for them (`skill/SKILL.md`, the Agent Skills format).
//! `pitupitu --skill` prints it; `pitupitu skill install` puts it where
//! Claude Code and Codex look for skills.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub const SKILL: &str = include_str!("../../../skill/SKILL.md");

/// Where each agent reads user skills from, under the home directory.
const DIRS: &[&str] = &[".claude/skills", ".codex/skills"];

/// Write the skill for every agent; returns the files written.
pub fn install(home: &Path) -> Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    for dir in DIRS {
        let file = home.join(dir).join("pitupitu").join("SKILL.md");
        std::fs::create_dir_all(file.parent().unwrap())
            .with_context(|| format!("creating {}", file.display()))?;
        std::fs::write(&file, SKILL).with_context(|| format!("writing {}", file.display()))?;
        written.push(file);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_skill_has_its_frontmatter_and_installs_for_both_agents() {
        assert!(SKILL.starts_with("---\nname: pitupitu\ndescription: "));
        let home = tempfile::tempdir().unwrap();
        let files = install(home.path()).unwrap();
        assert_eq!(files.len(), 2);
        for f in files {
            assert_eq!(std::fs::read_to_string(f).unwrap(), SKILL);
        }
    }
}
