//! The folder trust dialog an agent shows in a directory it has not seen,
//! and the keys that answer it. Shared by the herdr path and the box's own
//! terminals.

/// What the dialogs say, as of Claude Code 2.x, Codex, Gemini CLI and
/// Antigravity CLI 1.4.
pub const TRUST_TEXT: &[&str] = &[
    "trust this folder",
    "trust the files in this folder",
    "trust the contents of this directory",
    "trust the contents of this project",
];

/// Does `screen` ask whether to trust the folder? Not once the agent's
/// prompt box is back under it ("? for shortcuts" at the bottom):
/// Antigravity leaves the answered dialog on the screen above it.
pub fn is_trust_prompt(screen: &str) -> bool {
    let last = screen.lines().rev().find(|l| !l.trim().is_empty());
    if last.is_some_and(|l| l.trim_start().starts_with("? for shortcuts")) {
        return false;
    }
    TRUST_TEXT.iter().any(|t| screen.contains(t))
}

/// The keys that answer a trust prompt without changing how the agent asks
/// for permission: "Yes, I trust this folder" (Claude Code, Antigravity;
/// older Claude: "Yes, proceed"), Gemini CLI's "Trust folder"; for Codex,
/// the option that keeps asking ("No, ask me to approve …"), never one that
/// switches approvals off. Options are one per line, numbered or not, the
/// highlighted one marked `❯`, `›`, `>` or `●`. `None` when the screen
/// offers no such option.
pub fn trust_keys(screen: &str) -> Option<Vec<&'static str>> {
    let lines: Vec<&str> = screen.lines().collect();
    let marked = |l: &str| {
        let t = l.trim_start().trim_start_matches(['│', '┃']).trim_start();
        t.starts_with('❯') || t.starts_with('›') || t.starts_with('>') || t.starts_with('●')
    };
    let current = lines.iter().rposition(|l| marked(l))?;
    let wanted = lines.iter().rposition(|l| {
        let l = l.to_lowercase();
        l.contains("ask me")
            || (l.contains("yes") && (l.contains("trust") || l.contains("proceed")))
            || l.contains("trust folder")
    })?;
    let mut keys = Vec::new();
    if wanted > current {
        keys.extend(std::iter::repeat_n("down", wanted - current));
    } else {
        keys.extend(std::iter::repeat_n("up", current - wanted));
    }
    keys.push("enter");
    Some(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_is_answered_without_switching_approvals_off() {
        let claude =
            " Do you trust the files in this folder?\n ❯ 1. Yes, proceed\n   2. No, exit\n";
        assert_eq!(trust_keys(claude), Some(vec!["enter"]));
        let claude_no =
            " Do you trust the files in this folder?\n   1. Yes, proceed\n ❯ 2. No, exit\n";
        assert_eq!(trust_keys(claude_no), Some(vec!["up", "enter"]));
        let claude_now = " Quick safety check: Is this a project you created or one you trust?\n ❯ No, exit\n   Yes, I trust this folder\n Enter to confirm · Esc to cancel\n";
        assert_eq!(trust_keys(claude_now), Some(vec!["down", "enter"]));
        let codex = "Do you trust the contents of this directory?\n› 1. Yes, allow Codex to work in this folder without asking for approval\n  2. No, ask me to approve edits and commands\n";
        assert_eq!(trust_keys(codex), Some(vec!["down", "enter"]));
        assert_eq!(trust_keys("nothing to choose"), None);
    }

    #[test]
    fn antigravity_trust_is_answered() {
        // Antigravity CLI 1.4.2, first start in a new folder.
        let agy = "Accessing workspace:\n\n/tmp/x\n\nDo you trust the contents of this project?\n\nAntigravity CLI requires permission to read, edit, and execute files here.\n\n> Yes, I trust this folder\n  No, exit\n\n  ↑/↓ Navigate · enter Confirm\n";
        assert!(is_trust_prompt(agy));
        assert_eq!(trust_keys(agy), Some(vec!["enter"]));
        // Gemini CLI 0.64: in a box, the highlighted row marked `●`.
        let gemini = "╭──\n│ Do you trust the files in this folder?\n│\n│ Trusting a folder allows Gemini CLI to load its local configurations.\n│\n│ ● 1. Trust folder (app)\n│   2. Trust parent folder (src)\n│   3. Don't trust\n╰──\n";
        assert!(is_trust_prompt(gemini));
        assert_eq!(trust_keys(gemini), Some(vec!["enter"]));
        let gemini_no = gemini
            .replace("● 1. Trust folder", "  1. Trust folder")
            .replace("  3. Don't", "● 3. Don't");
        assert_eq!(trust_keys(&gemini_no), Some(vec!["up", "up", "enter"]));
        let answered = format!("{agy}\n  Antigravity CLI 1.4.2\n───\n>\n───\n? for shortcuts      Gemini 3.8 Flash · high\n");
        assert!(!is_trust_prompt(&answered));
    }
}
