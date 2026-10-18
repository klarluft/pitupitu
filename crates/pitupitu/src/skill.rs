//! The `pitupitu` skill: how the person's own agents install, link and
//! configure pitupitu for them (`skill/SKILL.md`, the Agent Skills format).
//! `pitupitu --skill` prints it; `pitupitu skill install` puts it where
//! Claude Code, Codex, Gemini CLI and Antigravity CLI look for skills.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub const SKILL: &str = include_str!("../../../skill/SKILL.md");

/// Where each agent reads user skills from, under the home directory.
const DIRS: &[&str] = &[".claude/skills", ".codex/skills"];
/// The same for the agents a box may not have, each with the folder that
/// shows it is there: Gemini CLI, Antigravity CLI.
const IF_THERE: &[(&str, &str)] = &[
    (".gemini/skills", ".gemini"),
    (".gemini/antigravity-cli/skills", ".gemini/antigravity-cli"),
];

/// Write the skill for every agent; returns the files written.
pub fn install(home: &Path) -> Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    let present = IF_THERE
        .iter()
        .filter(|(_, there)| home.join(there).is_dir())
        .map(|(dir, _)| dir);
    for dir in DIRS.iter().chain(present) {
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
        // Gemini CLI's and Antigravity's folders only where they are used.
        std::fs::create_dir_all(home.path().join(".gemini/antigravity-cli")).unwrap();
        let files = install(home.path()).unwrap();
        assert_eq!(files.len(), 4);
        for f in files {
            assert_eq!(std::fs::read_to_string(f).unwrap(), SKILL);
        }
    }
}
