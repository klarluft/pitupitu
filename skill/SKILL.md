---
name: pitupitu
description: Install, sign in and configure pitupitu, the voice app that lets a person talk to their coding agents (Claude Code, Codex) on this and their other machines. Use when the person asks to set up pitupitu, link this machine, change what pitupitu knows about them, name or describe this machine, choose the default machine, or check pitupitu's status.
---

# pitupitu

pitupitu lets the person talk (on https://pitupitu.chat, phone or desktop) to a voice foreman that starts, watches and answers agent sessions on their machines. Each machine runs the small open-source `pitupitu` box agent, which links it to pitupitu cloud. The agents it starts are the person's own `claude` and `codex`, signed in by the person; pitupitu never handles their credentials, and neither should you.

## Install and link this machine

1. Install: `curl -fsSL https://pitupitu.chat/install | sh` (Linux, macOS). It puts `pitupitu` in `~/.local/bin` and runs setup when there is a terminal; without one, run step 2 yourself.
2. Link: `pitupitu setup --home ~/pitupitu`. It prints a short code and a URL. **The person must approve the code themselves** in their browser while signed in to pitupitu.chat: show them the code and URL and wait. Never try to approve it for them. `--box <name>` sets how this machine is named (default: its hostname); `--no-browser` only prints the URL.
3. Setup installs a user service that keeps the link open. Check with `pitupitu status`.

If the person's agents are not logged in yet, tell them to run `claude` (or `codex`) once and log in. Do not do it for them and do not touch credential files.

## Settings

Every command takes `--json` and never prompts when there is no terminal.

- `pitupitu settings`: everything at once (preferences, default machine, voice, every machine with its note and driver).
- `pitupitu prefs` / `pitupitu prefs show`: the preferences, the text the voice foreman reads in every conversation: who the person is, what runs where, how they like to work. Keep it short, one fact per line.
- `pitupitu prefs set <file>` (or `-` for stdin), `pitupitu prefs clear`. `pitupitu prefs edit` opens an editor (a person at a terminal only).
- `pitupitu box note "<one line>"`: describe this machine ("client work, Acme"). `pitupitu box note --clear`.
- `pitupitu box driver auto|herdr|terminal`: what runs agent sessions here. `auto` (default) uses herdr when it runs.
- `pitupitu box on` / `pitupitu box off`: a box switched off stays linked, but pitupitu starts nothing there and ignores its sessions until it is on again ("ignore the laptop for now").
- `pitupitu default <machine>` / `pitupitu default --clear`: the machine used when a request does not say which.
- `pitupitu updates mine|all|quiet`: what the voice foreman brings up between turns unasked. `mine` (default): only what it started; `all`: the person's own sessions too; `quiet`: nothing unless asked.
- `pitupitu agent claude|codex|auto`: the agent a session gets when the person does not say (`auto`: whatever the machine has, Claude first). `pitupitu box agent claude|codex|auto`: this machine's own default over the account's.
- `pitupitu model claude opus` / `pitupitu model codex gpt-5` / `pitupitu model claude --clear`: the model that agent runs on when the person does not say, as the CLI names it.

Change preferences by reading them first (`pitupitu prefs --json`), editing the text, and writing it back whole with `pitupitu prefs set -`; keep what the person did not ask to change.

## Not here

The conversation, voice, minutes and billing are on https://pitupitu.chat (the page, and `/app/account`). Removing the service: `pitupitu uninstall`.
