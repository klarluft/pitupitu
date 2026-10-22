//! Keeps `skill/SKILL.md` true to the CLI: every `pitupitu …` command it
//! shows must parse, and every command must be in it. A change to the CLI
//! updates the skill in the same change.

use super::Cli;
use clap::{CommandFactory, Parser};
use pitupitu::skill;

/// Commands the skill leaves out on purpose.
const NOT_IN_SKILL: &[&str] = &[
    // What the user service runs; never something an agent should start.
    "connect",
    // The same as `pitupitu --skill`, which the skill does not need to name.
    "skill print",
];

/// Every `pitupitu …` command in the skill: inline code spans and the
/// lines of fenced blocks that start with `pitupitu `. Anything else in
/// backticks (an install pipe, a path, another tool) does not start that
/// way and is not a pitupitu command.
fn commands(md: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut fenced = false;
    for line in md.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            out.push(line.trim().to_string());
            continue;
        }
        let mut parts = line.split('`');
        parts.next();
        while let Some(span) = parts.next() {
            out.push(span.trim().to_string());
            parts.next();
        }
    }
    out.retain(|c| c.starts_with("pitupitu "));
    out
}

/// Words, with double-quoted text as one word.
fn words(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    for ch in cmd.chars() {
        match ch {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !word.is_empty() {
                    out.push(std::mem::take(&mut word));
                }
            }
            c => word.push(c),
        }
    }
    if !word.is_empty() {
        out.push(word);
    }
    out
}

/// Every concrete command a documented one stands for: `<placeholder>`
/// becomes a word, `a|b` each of its choices, `[a|b]` nothing or each
/// choice.
fn expand(cmd: &str) -> Vec<Vec<String>> {
    let mut lines: Vec<Vec<String>> = vec![vec![]];
    for word in words(cmd) {
        let (optional, word) = match word.strip_prefix('[').and_then(|w| w.strip_suffix(']')) {
            Some(inner) => (true, inner.to_string()),
            None => (false, word),
        };
        let mut choices: Vec<Option<String>> = word
            .split('|')
            .map(|w| {
                // `<name>` alone, or inside a quoted word ("<one line>").
                let mut w = w.to_string();
                while let (Some(a), Some(b)) = (w.find('<'), w.find('>')) {
                    if b < a {
                        break;
                    }
                    w.replace_range(a..=b, "word");
                }
                Some(w)
            })
            .collect();
        if optional {
            choices.push(None);
        }
        lines = lines
            .into_iter()
            .flat_map(|line| {
                choices.iter().map(move |c| {
                    let mut l = line.clone();
                    l.extend(c.clone());
                    l
                })
            })
            .collect();
    }
    lines
}

#[test]
fn the_rules_read_commands_as_meant() {
    let md = "Run `pitupitu box history on|off` or `herdr --skill`.\n\
              `curl -fsSL https://x | sh`\n```sh\npitupitu update [herdr|all]\n```\n\
              `pitupitu box note \"<one line>\"`";
    let all: Vec<_> = commands(md).iter().flat_map(|c| expand(c)).collect();
    let join: Vec<_> = all.iter().map(|w| w.join(" ")).collect();
    assert_eq!(
        join,
        [
            "pitupitu box history on",
            "pitupitu box history off",
            "pitupitu update herdr",
            "pitupitu update all",
            "pitupitu update",
            "pitupitu box note word",
        ]
    );
    // The quoted note stays one word.
    assert_eq!(all.last().unwrap().len(), 4);
}

#[test]
fn every_command_in_the_skill_parses() {
    let found = commands(skill::SOURCE);
    assert!(found.len() > 20, "found only {found:?}");
    for cmd in found {
        for argv in expand(&cmd) {
            if let Err(e) = Cli::try_parse_from(&argv) {
                panic!("skill/SKILL.md shows `{cmd}`, which does not parse ({argv:?}):\n{e}");
            }
        }
    }
}

#[test]
fn every_command_is_in_the_skill() {
    let shown: Vec<Vec<String>> = commands(skill::SOURCE)
        .iter()
        .flat_map(|c| expand(c))
        .collect();
    let is_shown = |path: &[&str]| {
        shown
            .iter()
            .any(|argv| argv.len() > path.len() && argv[1..=path.len()] == *path)
    };
    let mut missing = Vec::new();
    for cmd in Cli::command().get_subcommands() {
        let mut paths = vec![vec![cmd.get_name()]];
        for sub in cmd.get_subcommands() {
            paths.push(vec![cmd.get_name(), sub.get_name()]);
        }
        for path in paths {
            let name = path.join(" ");
            if !NOT_IN_SKILL.contains(&name.as_str()) && !is_shown(&path) {
                missing.push(name);
            }
        }
    }
    assert!(
        missing.is_empty(),
        "not in skill/SKILL.md (add them, or to NOT_IN_SKILL with the reason): {missing:?}"
    );
    for name in NOT_IN_SKILL {
        let path: Vec<_> = name.split(' ').collect();
        assert!(
            !is_shown(&path),
            "`{name}` is in the skill now; drop it from NOT_IN_SKILL"
        );
    }
}
