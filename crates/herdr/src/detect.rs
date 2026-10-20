//! What the screen of an agent's pane says (`docs/sessions.md`,
//! "Dialogs"): a dialog waiting for the person (an approval, a question,
//! the folder trust prompt, a login screen), which herdr's own state does
//! not always show. Only the bottom of the screen is looked
//! at, so a dialog quoted in the conversation above does not count.

/// Lines from the bottom of the screen a dialog must be in.
const BOTTOM: usize = 18;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialog {
    /// "Do you trust the files in this folder?"
    Trust,
    /// An approval or a question the agent asks.
    Approval,
    /// The agent is not logged in.
    Login,
}

const TRUST: &[&str] = &[
    "trust this folder",
    "trust the files in this folder",
    "trust the contents of this directory",
];

const APPROVAL: &[&str] = &[
    // Claude Code
    "Do you want to proceed?",
    "Do you want to make this edit",
    "Do you want to create",
    "Do you want to allow",
    "Would you like to proceed?",
    // Codex
    "Would you like to run the following command?",
    "Would you like to make the following edits?",
    "Allow command?",
    "Approve this",
];

const LOGIN: &[&str] = &[
    "Select login method",
    "Sign in with ChatGPT",
    "Log in with your Anthropic",
    "Please run /login",
    "Invalid API key",
];

/// The dialog the bottom of `screen` shows, if any.
pub fn dialog(screen: &str) -> Option<Dialog> {
    let lines: Vec<&str> = screen.lines().collect();
    let bottom = lines[lines.len().saturating_sub(BOTTOM)..].join("\n");
    let has = |set: &[&str]| set.iter().any(|p| bottom.contains(p));
    if has(TRUST) {
        Some(Dialog::Trust)
    } else if has(LOGIN) {
        Some(Dialog::Login)
    } else if has(APPROVAL) {
        Some(Dialog::Approval)
    } else {
        None
    }
}

/// The keys that answer a trust prompt without changing how the agent asks
/// for permission: Claude Code's "Yes, I trust this folder" (older: "Yes,
/// proceed"); for Codex, the option that keeps asking ("No, ask me to
/// approve …"), never the one that switches approvals off. Options are one
/// per line, numbered or not, the highlighted one marked `❯`, `›` or `>`.
/// `None` when the screen offers no such option.
pub fn trust_keys(screen: &str) -> Option<Vec<&'static str>> {
    let lines: Vec<&str> = screen.lines().collect();
    let marked = |l: &str| {
        let t = l.trim_start();
        t.starts_with('❯') || t.starts_with('›') || t.starts_with('>')
    };
    let current = lines.iter().rposition(|l| marked(l))?;
    let wanted = lines.iter().rposition(|l| {
        let l = l.to_lowercase();
        l.contains("ask me")
            || (l.contains("yes") && (l.contains("trust") || l.contains("proceed")))
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
    fn dialogs_at_the_bottom_are_seen_and_quoted_ones_are_not() {
        let claude_trust =
            "\n Do you trust the files in this folder?\n\n ❯ 1. Yes, proceed\n   2. No, exit\n";
        assert_eq!(dialog(claude_trust), Some(Dialog::Trust));
        let approval = "⏺ Bash(rm -rf build)\n\n Do you want to proceed?\n ❯ 1. Yes\n   2. No\n";
        assert_eq!(dialog(approval), Some(Dialog::Approval));
        assert_eq!(
            dialog("Select login method:\n ❯ 1. Claude account"),
            Some(Dialog::Login)
        );
        let mut old = String::from("I asked: Do you want to proceed?\n");
        old.push_str(&"line\n".repeat(30));
        assert_eq!(dialog(&old), None);
        assert_eq!(dialog("> \n? for shortcuts"), None);
    }

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
}
