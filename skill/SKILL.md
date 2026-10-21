---
name: pitupitu
description: Install, sign in and configure pitupitu, the voice app that lets a person talk to their coding agents (Claude Code, Codex) on this and their other machines. Use when the person asks to set up pitupitu, link this machine, change what pitupitu knows about them, name or describe this machine, choose the default machine, follow a session (be told when it finishes or needs them), or check pitupitu's status.
---

# pitupitu

pitupitu lets the person talk (on https://pitupitu.chat, phone or desktop) to a voice foreman that starts, watches and answers agent sessions on their machines. Each machine runs the small open-source `pitupitu` box agent, which links it to pitupitu cloud. The agents run in herdr (a terminal workspace manager for coding agents): the person opens `herdr` on that machine to see the same sessions their phone sees. The agents are the person's own `claude`, `codex` and others, signed in by the person; pitupitu never handles their credentials, and neither should you.

## Install and link this machine

1. Install: `curl -fsSL https://pitupitu.chat/install | sh` (Linux, macOS). It puts `pitupitu` in `~/.local/bin`, installs herdr when it is missing, and runs setup when there is a terminal; without one, run step 2 yourself. Where Homebrew is the person's way of installing tools, `brew install klarluft/tap/pitupitu` instead, then step 2.
2. Link: `pitupitu setup --home ~/pitupitu`. It prints a short code and a URL. **The person must approve the code themselves** in their browser while signed in to pitupitu.chat: show them the code and URL and wait. Never try to approve it for them. `--box <name>` sets how this machine is named (default: its hostname); `--no-browser` only prints the URL.
3. Setup installs herdr if it is still missing and herdr's integrations for the agents it finds, then a user service that keeps the link open. On Linux it asks once whether to keep pitupitu running while the person is logged out (lingering; yes by default); without a terminal pass `--linger` or `--no-linger` only if the person said which. Check with `pitupitu status`.

If the person's agents are not logged in yet, tell them to run `claude` (or `codex`) once and log in. Do not do it for them and do not touch credential files.

## Settings

Every command takes `--json` and never prompts when there is no terminal.

- `pitupitu settings`: everything at once (preferences, default machine, voice, every machine with its note, agent and history).
- `pitupitu prefs` / `pitupitu prefs show`: the preferences, the text the voice foreman reads in every conversation: who the person is, what runs where, how they like to work. Keep it short, one fact per line.
- `pitupitu prefs set <file>` (or `-` for stdin), `pitupitu prefs clear`. `pitupitu prefs edit` opens an editor (a person at a terminal only).
- `pitupitu box note "<one line>"`: describe this machine ("client work, Acme"). `pitupitu box note --clear`.
- `pitupitu box history on|off`: whether pitupitu reads what the person's agents did on this machine (their prompts, the gist of the agents' replies, which files and commands were touched, commits; never file contents, command output or keys) to summarise their work. On by default. Switching it off is always fine.
- `pitupitu box rename <name>`: the name the person uses for this machine when they talk ("on the mac, …"); it relinks under the new name by itself.
- `pitupitu box on` / `pitupitu box off`: a box switched off stays linked, but pitupitu starts nothing there and ignores its sessions until it is on again ("ignore the laptop for now").
- `pitupitu default <machine>` / `pitupitu default --clear`: the machine used when a request does not say which.
- `pitupitu updates mine|all|quiet`: what the voice foreman brings up between turns unasked. `mine` (default): the followed sessions (its own tasks, and the person's own once followed); `all`: every session; `quiet`: nothing unless asked.
- `pitupitu agent claude|codex|auto`: the agent a session gets when the person does not say (`auto`: whatever the machine has, Claude first). `pitupitu box agent claude|codex|auto`: this machine's own default over the account's.
- `pitupitu model claude opus` / `pitupitu model codex gpt-5` / `pitupitu model claude --clear`: the model that agent runs on when the person does not say, as the CLI names it.
- `pitupitu follow` / `pitupitu unfollow`: follow a session, so pitupitu tells the person when it finishes or needs them, or stop. With nothing after it, the session the command runs in (it must run inside herdr); or name one by words from its title or by its id (`pitupitu follow login page`). `pitupitu follow --off` is the same as `unfollow`.
- `pitupitu sessions`: this machine's sessions with their status, which are followed (●) and which pitupitu started itself.
- `pitupitu place add <name> <path>` / `pitupitu place rm <name>` / `pitupitu places`: folders on this machine with a name the person says ("in acme, run the tests"). Use it when they ask to name a project folder for pitupitu; the path is made absolute.

When the person says to keep them posted on this session, to let pitupitu report on it, to tell them when it is done, or the like, run `pitupitu follow` with nothing after it, from inside this session: it follows the session you are in. Tell them it is done in one line. Use `pitupitu sessions` to see what is followed already.

Change preferences by reading them first (`pitupitu prefs --json`), editing the text, and writing it back whole with `pitupitu prefs set -`; keep what the person did not ask to change.

## Updating

`pitupitu update --check` says whether a newer release of the box agent exists (`--json` for `{"current","latest","update_available"}`); `pitupitu update herdr --check` the same for herdr, `pitupitu update all --check` for both. `pitupitu update [herdr|all]` installs it the way it was installed (Homebrew, or the installer): it shows what would change and what is running, then asks. **Run it only when the person asks to update, and let them answer the question themselves.** Pass `--yes` only when they said, in so many words, to update without asking, and `--handoff` (restart herdr's server on the new version now) only when they asked for that. Sessions keep running through an update: only the box agent restarts, and herdr's running server stays as it is unless handed off.

## Not here

The conversation, voice, minutes and billing are on https://pitupitu.chat (the page, and `/app/account`). Removing the service: `pitupitu uninstall` (herdr stays). Driving herdr itself (its panes and agents) is herdr's own skill, `herdr --skill`, not this one.
