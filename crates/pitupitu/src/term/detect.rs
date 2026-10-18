//! What the screen of an agent's terminal says, for the states its hooks
//! cannot report: a dialog waiting for the person (an approval, a
//! question, the folder trust prompt, a login screen). Only the bottom of
//! the screen is looked at, so a dialog quoted in the conversation above
//! does not count.

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

pub use herdr::trust_keys;

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
    // Gemini CLI
    "Apply this change?",
    "Allow execution of",
    "Waiting for user confirmation",
    "Answer Questions",
];

const LOGIN: &[&str] = &[
    "Select login method",
    "Sign in with ChatGPT",
    "Log in with your Anthropic",
    "Please run /login",
    "Invalid API key",
    // Antigravity CLI; shown for a moment at every start too, so a start
    // only fails on it once it stays.
    "You are currently not signed in",
    // Gemini CLI
    "How would you like to authenticate",
    "Waiting for authentication",
];

/// The key hints Antigravity CLI puts under a dialog, in place of its
/// prompt box: an approval ("↑/↓ Navigate · tab Amend · …") or a question
/// ("↑/↓ Navigate · enter Select · esc Skip").
const DIALOG_HINTS: &[&str] = &["tab amend", "esc skip"];

/// The dialog the bottom of `screen` shows, if any. None while the agent's
/// own prompt box is at the bottom ("? for shortcuts"): what is above it is
/// conversation, or a dialog already answered.
pub fn dialog(screen: &str) -> Option<Dialog> {
    let lines: Vec<&str> = screen.lines().collect();
    let filled: Vec<&str> = lines
        .iter()
        .rev()
        .filter(|l| !l.trim().is_empty())
        .take(2)
        .map(|l| l.trim())
        .collect();
    if filled
        .first()
        .is_some_and(|l| l.starts_with("? for shortcuts"))
    {
        return None;
    }
    let bottom = lines[lines.len().saturating_sub(BOTTOM)..].join("\n");
    let has = |set: &[&str]| set.iter().any(|p| bottom.contains(p));
    let hints = filled.iter().any(|l| {
        let l = l.to_lowercase();
        l.contains("navigate") && DIALOG_HINTS.iter().any(|h| l.contains(h))
    });
    if herdr::is_trust_prompt(&bottom) {
        Some(Dialog::Trust)
    } else if has(LOGIN) {
        Some(Dialog::Login)
    } else if has(APPROVAL) || hints {
        Some(Dialog::Approval)
    } else {
        None
    }
}

/// Does the bottom of `screen` show the agent at work: the "esc to cancel"
/// footer Gemini CLI and Antigravity CLI show during a turn, or a line led
/// by a braille spinner? For the agents whose turns are seen only on the
/// screen.
pub fn working(screen: &str) -> bool {
    screen
        .lines()
        .rev()
        .filter(|l| !l.trim().is_empty())
        .take(12)
        .any(|l| {
            let t = l.trim_start();
            l.to_lowercase().contains("esc to cancel")
                || t.chars()
                    .next()
                    .is_some_and(|c| ('\u{2801}'..='\u{28FF}').contains(&c))
                    && t.chars()
                        .skip_while(|c| !c.is_whitespace())
                        .any(|c| !c.is_whitespace())
        })
}

/// A key name, as the cloud's `send_keys` and herdr use them, as bytes.
pub fn key_bytes(key: &str) -> Option<Vec<u8>> {
    let b: &[u8] = match key.to_ascii_lowercase().as_str() {
        "enter" | "return" => b"\r",
        "esc" | "escape" => b"\x1b",
        "tab" => b"\t",
        "backspace" => b"\x7f",
        "space" => b" ",
        "up" => b"\x1b[A",
        "down" => b"\x1b[B",
        "right" => b"\x1b[C",
        "left" => b"\x1b[D",
        "ctrl-c" | "c-c" => b"\x03",
        "ctrl-d" | "c-d" => b"\x04",
        k if k.chars().count() == 1 => return Some(key.as_bytes().to_vec()),
        _ => return None,
    };
    Some(b.to_vec())
}

/// `s` as one word for a POSIX shell.
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:,@%+".contains(c))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
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

    /// Antigravity CLI 1.4: the trust dialog stays above the prompt box
    /// once answered; dialogs replace the box and end with key hints.
    #[test]
    fn antigravity_screens() {
        let trust = "Do you trust the contents of this project?\n\n> Yes, I trust this folder\n  No, exit\n\n  ↑/↓ Navigate · enter Confirm\n";
        assert_eq!(dialog(trust), Some(Dialog::Trust));
        let idle = format!(
            "{trust}\n      ▄▀▀▄        Antigravity CLI 1.4.2\n───────\n>\n───────\n? for shortcuts                 Gemini 3.8 Flash · high\n"
        );
        assert_eq!(dialog(&idle), None);
        assert!(!working(&idle));
        let approval = "  run_command: npm test\n\n> Allow once\n  Always allow\n  Deny\n\n  ↑/↓ Navigate · tab Amend · ctrl+g edit/expand command\n───────── esc to cancel\n";
        assert_eq!(dialog(approval), Some(Dialog::Approval));
        let busy = "> fix the build\n\n⠹ Thinking about the failing test\n───────\n>\n───────── esc to cancel\n";
        assert_eq!(dialog(busy), None);
        assert!(working(busy));
        assert!(working("⠋ Reading files\n───\n>\n───\n"));
    }

    #[test]
    fn keys_and_quoting() {
        assert_eq!(key_bytes("enter"), Some(b"\r".to_vec()));
        assert_eq!(key_bytes("Down"), Some(b"\x1b[B".to_vec()));
        assert_eq!(key_bytes("2"), Some(b"2".to_vec()));
        assert_eq!(key_bytes("f13"), None);
        assert_eq!(shell_quote("--model"), "--model");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote(r#"{"a":1}"#), r#"'{"a":1}'"#);
    }
}
