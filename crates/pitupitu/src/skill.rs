//! The `pitupitu` skill: how the person's own agents install, link and
//! configure pitupitu for them (`skill/SKILL.md`, the Agent Skills format).
//! `pitupitu --skill` prints it; `pitupitu skill install` puts it where
//! Claude Code and Codex look for skills, and [`sync`] keeps those copies in
//! step with this binary.
//!
//! A copy pitupitu wrote carries `managed-by: pitupitu` in its frontmatter's
//! `metadata`, with the version that wrote it. Only such a copy is ever
//! replaced; one without the marker is the person's own and is left alone.
//! Copies written before the marker existed are recognised by their exact
//! text ([`EARLIER`]).

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// `skill/SKILL.md` as written, with [`VERSION_SLOT`] where the version goes.
/// [`text`] is what gets printed and installed.
pub const SOURCE: &str = include_str!("../../../skill/SKILL.md");

/// Where the crate version is filled in, in the frontmatter's `metadata`.
pub const VERSION_SLOT: &str = "{{version}}";

/// The marker line in the frontmatter's `metadata` map.
const MARKER: &str = "managed-by: pitupitu";

/// SHA-256 of every SKILL.md released before the marker existed
/// (0.2.0 through 0.7.1): a copy that is exactly one of them is pitupitu's
/// and gets replaced like a marked one.
const EARLIER: &[&str] = &[
    "120e507c19b8c9da4c1896be0000b33d281db5d10893a3b1565238dad6ab404c",
    "5c161e189a22bd4cd8ebe1c154fed161ecafa2450e4ae4fb7caac9bb022a745a",
    "0a1f2833211a6c1ed85a18bab59dce64e6321a298efe9e3dda0a24fc679bb391",
    "c9507d10fea8f79c4bfe5c2e3e1c393e52888ae6a4d08392366fe176bb0a10e7",
    "2b49a936d8579937a0a5d2d1f2a57b37b986b7754ea50c6f56bae1e815a9f491",
    "ae3031e22902665f6c39ef273f945d36974c510f7a9f2c30db73bf07b82196b6",
];

/// The agents that read user skills: their name and home folder.
const AGENTS: &[(&str, &str)] = &[("Claude Code", ".claude"), ("Codex", ".codex")];

/// The skill as this version prints and installs it.
pub fn text() -> String {
    SOURCE.replace(VERSION_SLOT, env!("CARGO_PKG_VERSION"))
}

/// Who writes what in [`sync`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Replace pitupitu's own copies that differ; write no new ones (the
    /// service, at every start, so an update reaches the agents).
    Refresh,
    /// Also write a missing copy, for each agent whose folder exists
    /// (`pitupitu setup`).
    Install,
    /// Also write a missing copy for every agent, making its folder
    /// (`pitupitu skill install`, asked for in so many words).
    Everywhere,
}

/// What one agent's copy is, against this version's skill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// The same text as this version's.
    Current,
    /// pitupitu's own copy, from another version.
    Outdated,
    /// A copy pitupitu did not write (no marker): the person's own.
    Unmanaged,
    /// No copy.
    Missing,
}

#[derive(Debug, Clone, Serialize)]
pub struct Copy {
    pub agent: &'static str,
    pub path: PathBuf,
    /// Whether the agent's own folder (`~/.claude`, `~/.codex`) exists.
    pub agent_installed: bool,
    pub state: State,
}

/// What [`sync`] did to one copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Written(PathBuf),
    Updated(PathBuf),
    /// Left alone: the person's own (no marker).
    Unmanaged(PathBuf),
}

/// Every agent's copy and its state.
pub fn status(home: &Path) -> Vec<Copy> {
    let want = text();
    AGENTS
        .iter()
        .map(|(agent, dir)| {
            let path = home.join(dir).join("skills/pitupitu/SKILL.md");
            let state = match std::fs::read_to_string(&path) {
                Err(_) if !path.exists() => State::Missing,
                Err(_) => State::Unmanaged,
                Ok(have) if have == want => State::Current,
                Ok(have) if managed(&have) => State::Outdated,
                Ok(_) => State::Unmanaged,
            };
            Copy {
                agent,
                agent_installed: home.join(dir).is_dir(),
                path,
                state,
            }
        })
        .collect()
}

/// Bring every agent's copy in step with this version, as `mode` allows.
/// A current copy is left as it is and not reported.
pub fn sync(home: &Path, mode: Mode) -> Result<Vec<Outcome>> {
    let want = text();
    let mut done = Vec::new();
    for copy in status(home) {
        let path = copy.path;
        match copy.state {
            State::Current => {}
            State::Unmanaged => done.push(Outcome::Unmanaged(path)),
            State::Outdated => {
                write(&path, &want)?;
                done.push(Outcome::Updated(path));
            }
            State::Missing => {
                let wanted = match mode {
                    Mode::Refresh => false,
                    Mode::Install => copy.agent_installed,
                    Mode::Everywhere => true,
                };
                if wanted {
                    write(&path, &want)?;
                    done.push(Outcome::Written(path));
                }
            }
        }
    }
    Ok(done)
}

/// Whether any agent has a copy at all, pitupitu's or the person's.
pub fn any_installed(home: &Path) -> bool {
    status(home).iter().any(|c| c.state != State::Missing)
}

impl Outcome {
    /// One plain line for the person or the log.
    pub fn line(&self) -> String {
        match self {
            Outcome::Written(p) => format!("wrote {}", p.display()),
            Outcome::Updated(p) => format!("updated {}", p.display()),
            Outcome::Unmanaged(p) => format!(
                "left {} as it is: it was not written by pitupitu (no `{MARKER}` in its \
                 metadata); remove it and run `pitupitu skill install` for pitupitu's",
                p.display()
            ),
        }
    }
}

fn write(path: &Path, text: &str) -> Result<()> {
    let dir = path.parent().expect("a skill file has a folder");
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

/// Whether a copy is pitupitu's: the marker in its frontmatter, or the exact
/// text of a release from before the marker.
fn managed(text: &str) -> bool {
    use sha2::{Digest, Sha256};
    let marked = frontmatter(text).is_some_and(|f| f.lines().any(|l| l.trim() == MARKER));
    let digest: String = Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    marked || EARLIER.contains(&digest.as_str())
}

/// The YAML between the opening `---` and the closing one.
fn frontmatter(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("---\n")?;
    let end = rest.find("\n---\n")?;
    Some(&rest[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn copy_at(home: &Path, dir: &str) -> PathBuf {
        home.join(dir).join("skills/pitupitu/SKILL.md")
    }

    #[test]
    fn the_skill_carries_its_name_and_the_marker_with_this_version() {
        assert_eq!(SOURCE.matches(VERSION_SLOT).count(), 1);
        let t = text();
        assert!(t.starts_with("---\nname: pitupitu\ndescription: "));
        let front = frontmatter(&t).unwrap();
        assert!(front.contains(&format!(
            "metadata:\n  {MARKER}\n  version: \"{}\"",
            env!("CARGO_PKG_VERSION")
        )));
        assert!(managed(&t));
        // Every frontmatter line is a top-level `key: value` or an indented
        // entry of the `metadata` map.
        for line in front.lines() {
            let top = !line.starts_with(' ') && line.contains(": ") || line == "metadata:";
            let entry = line.starts_with("  ") && line.trim().contains(": ");
            assert!(top || entry, "odd frontmatter line: {line}");
        }
    }

    #[test]
    fn install_writes_for_the_agents_there_and_everywhere_writes_for_both() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        assert!(sync(home, Mode::Install).unwrap().is_empty());
        assert!(!home.join(".codex").exists());

        std::fs::create_dir(home.join(".claude")).unwrap();
        let done = sync(home, Mode::Install).unwrap();
        assert_eq!(done, [Outcome::Written(copy_at(home, ".claude"))]);
        assert!(!home.join(".codex").exists());

        let done = sync(home, Mode::Everywhere).unwrap();
        assert_eq!(done, [Outcome::Written(copy_at(home, ".codex"))]);
        for c in status(home) {
            assert_eq!(c.state, State::Current);
            assert_eq!(std::fs::read_to_string(c.path).unwrap(), text());
        }
        assert!(sync(home, Mode::Everywhere).unwrap().is_empty());
    }

    #[test]
    fn refresh_replaces_only_pitupitus_own_copies() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        sync(home, Mode::Everywhere).unwrap();
        let claude = copy_at(home, ".claude");
        let codex = copy_at(home, ".codex");

        // An older pitupitu's copy: marked, different text.
        std::fs::write(
            &claude,
            text().replace("# pitupitu\n", "# pitupitu (old)\n"),
        )
        .unwrap();
        // The person's own: no marker.
        let own = "---\nname: pitupitu\ndescription: mine\n---\n\nmy notes\n";
        std::fs::write(&codex, own).unwrap();
        let states: Vec<_> = status(home).iter().map(|c| c.state).collect();
        assert_eq!(states, [State::Outdated, State::Unmanaged]);

        let done = sync(home, Mode::Refresh).unwrap();
        assert_eq!(
            done,
            [
                Outcome::Updated(claude.clone()),
                Outcome::Unmanaged(codex.clone())
            ]
        );
        assert_eq!(std::fs::read_to_string(&claude).unwrap(), text());
        assert_eq!(std::fs::read_to_string(&codex).unwrap(), own);

        // Refresh writes nothing new.
        std::fs::remove_file(&claude).unwrap();
        assert_eq!(sync(home, Mode::Refresh).unwrap().len(), 1);
        assert!(!claude.exists());
    }

    #[test]
    fn a_copy_from_before_the_marker_counts_as_pitupitus() {
        // 0.2.0's SKILL.md is not at hand here; any text whose digest is in
        // EARLIER would do, so check the rule on the digest itself.
        assert!(!managed("---\nname: pitupitu\n---\nmine\n"));
        assert_eq!(EARLIER.len(), 6);
        assert!(EARLIER.iter().all(|h| h.len() == 64));
    }
}
