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

/// Phrases for harnesses beyond the built-in ones, from the cloud's
/// profiles (`docs/box-link.md`, "Harnesses"); they add to the lists
/// above, never replace them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Screens {
    pub trust: Vec<String>,
    /// The option that answers a trust dialog.
    pub trust_yes: Vec<String>,
    pub approval: Vec<String>,
    pub login: Vec<String>,
    /// A first-run screen that is neither a question nor an approval (an
    /// advert, a tip), with the phrases that show it. Only Esc closes one.
    pub dismiss: Vec<String>,
}

static EXTRA: std::sync::RwLock<Option<Screens>> = std::sync::RwLock::new(None);

/// Words that make an option one that switches approvals off: such an
/// option is never chosen to answer a trust dialog.
const NEVER: &[&str] = &[
    "without asking",
    "don't ask",
    "do not ask",
    "auto-approve",
    "yolo",
    "bypass",
    "dangerously",
    "always allow",
];

/// Replace the extra phrases (a `harnesses` note arrived).
pub fn set_screens(screens: Screens) {
    *EXTRA.write().unwrap_or_else(|e| e.into_inner()) = Some(screens);
}

fn extra() -> Screens {
    EXTRA
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default()
}

fn bottom(screen: &str) -> String {
    let lines: Vec<&str> = screen.lines().collect();
    lines[lines.len().saturating_sub(BOTTOM)..].join("\n")
}

/// The dialog the bottom of `screen` shows, if any.
pub fn dialog(screen: &str) -> Option<Dialog> {
    let bottom = bottom(screen);
    let extra = extra();
    let has = |set: &[&str], more: &[String]| {
        set.iter().any(|p| bottom.contains(p))
            || more
                .iter()
                .any(|p| !p.is_empty() && bottom.contains(p.as_str()))
    };
    if has(TRUST, &extra.trust) {
        Some(Dialog::Trust)
    } else if has(LOGIN, &extra.login) {
        Some(Dialog::Login)
    } else if has(APPROVAL, &extra.approval) {
        Some(Dialog::Approval)
    } else {
        None
    }
}

/// The keys that close a first-run screen the bottom of `screen` shows
/// (only ever Esc), or `None`.
pub fn dismiss_keys(screen: &str) -> Option<Vec<&'static str>> {
    let bottom = bottom(screen);
    extra()
        .dismiss
        .iter()
        .any(|p| !p.is_empty() && bottom.contains(p.as_str()))
        .then(|| vec!["esc"])
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
    let yes: Vec<String> = extra().trust_yes.iter().map(|p| p.to_lowercase()).collect();
    let wanted = lines.iter().rposition(|l| {
        let l = l.to_lowercase();
        if NEVER.iter().any(|w| l.contains(w)) {
            return false;
        }
        l.contains("ask me")
            || (l.contains("yes") && (l.contains("trust") || l.contains("proceed")))
            || yes.iter().any(|y| !y.is_empty() && l.contains(y.as_str()))
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
    fn profile_phrases_add_dialogs_and_never_switch_approvals_off() {
        set_screens(Screens {
            trust: vec!["Trust this workspace?".into()],
            trust_yes: vec!["Trust, continue".into()],
            approval: vec!["requires approval".into()],
            login: vec!["let's get you signed in".into()],
            dismiss: vec!["Open ClinePass".into()],
        });
        let kiro = " Trust this workspace?\n ❯ Don't trust, exit\n   Trust, continue\n";
        assert_eq!(dialog(kiro), Some(Dialog::Trust));
        assert_eq!(trust_keys(kiro), Some(vec!["down", "enter"]));
        let sneaky = " Trust this workspace?\n ❯ Exit\n   Trust, continue and don't ask again\n";
        assert_eq!(trust_keys(sneaky), None);
        assert_eq!(
            dialog(" Write File requires approval\n ❯ Allow once\n"),
            Some(Dialog::Approval)
        );
        assert_eq!(
            dialog("Welcome to Kiro CLI, let's get you signed in!"),
            Some(Dialog::Login)
        );
        assert_eq!(dismiss_keys("…\n  Open ClinePass  \n"), Some(vec!["esc"]));
        assert_eq!(dismiss_keys("a normal screen"), None);
        // The built-in phrases stay.
        assert_eq!(
            dialog(" Do you trust the files in this folder?\n ❯ 1. Yes, proceed\n"),
            Some(Dialog::Trust)
        );
        set_screens(Screens::default());
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
