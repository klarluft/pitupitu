# pitupitu

pitupitu lets you talk to your coding agents (Claude Code, Codex) on any of your machines, by voice or text, from your phone or any browser. You say what you want and where; pitupitu starts the agent session, keeps an eye on it and tells you when it is done or needs you. If you use [herdr](https://herdr.dev), the sessions run there and you see them in herdr too; without it, this agent hosts the terminals itself and `pitupitu attach` shows any session live.

This repository is the part that runs on each of your machines: a small agent that links the machine to pitupitu cloud. The conversation, the voice and the page at [pitupitu.chat](https://pitupitu.chat) are the hosted service.

## How it works

```
phone / browser ──▶ pitupitu.chat (cloud) ◀── box link (outbound WebSocket) ── pitupitu ──▶ herdr, or its own terminals ──▶ your agents
```

- The box agent **dials out** to `wss://api.pitupitu.chat/v1/box`. Nothing connects in: no open port, no VPN, no certificate on your machine.
- The cloud can ask for a fixed set of operations only, served by herdr or by this agent's own terminal host: list and open workspaces and tabs, start an agent in a pane, send it a prompt or keys, read its screen, wait for it. There is no shell and no arbitrary command. The full list is in [docs/box-link.md](docs/box-link.md) and in [`calls.rs`](crates/pitupitu/src/calls.rs).
- Your agents run as they always do, your own unmodified `claude` and `codex` signed in by you, on your own subscriptions or keys. pitupitu never reads, sees or uses their credentials.
- From your files, only `skills/*/SKILL.md` in the pitupitu home folder are sent, plus what the box reports about itself: which agent CLIs are installed (with their versions), and the folders your agents worked in recently (only the folder paths and when, read from the agents' own session history; nothing else from those files).

## Install

```sh
curl -fsSL https://pitupitu.chat/install | sh
```

or with Homebrew (macOS and Linux):

```sh
brew install klarluft/tap/pitupitu
pitupitu setup
```

or build from source with a recent stable Rust:

```sh
cargo install --git https://github.com/klarluft/pitupitu pitupitu
```

You need `claude` and/or `codex` installed and signed in. [herdr](https://herdr.dev) is optional: when it runs, sessions open there (`pitupitu box driver` chooses); without it, pitupitu hosts the terminals itself.

## Set up

```sh
pitupitu setup
```

1. **Sign in**: it shows a code; approve it on any screen at pitupitu.chat. The box gets its own key, stored in `~/.config/pitupitu/pitupitu.toml` (readable only by you). An account can have many boxes, each named (the hostname by default, `--box` to choose); setting up the same box again replaces its old key.
2. **Home folder**: `~/pitupitu` by default. Requests that belong to no project run there, and `skills/<name>/SKILL.md` holds routines you name by voice. What pitupitu should know about you ("client work goes on the work laptop", "I use Codex for reviews") is your preferences, kept in your account: `pitupitu prefs edit`, the page, or just tell pitupitu "remember that …". (A `PITUPITU.md` from 0.1 is no longer read; copy anything useful into `pitupitu prefs`.)
3. **Skill** (offered once): teaches your agents (Claude Code, Codex, Gemini CLI, Antigravity CLI) how to configure pitupitu, so you can ask your own agent to do it.
4. **Service**: a user service starts `pitupitu connect` now and at login (systemd on Linux, launchd on macOS). On a Linux machine you log out of, run `loginctl enable-linger $USER` so it keeps running.

Then open [pitupitu.chat/app](https://pitupitu.chat/app).

## Commands

| | |
|---|---|
| `pitupitu setup` | sign in, home folder, user service (`--home`, `--relink`, `--box`, `--no-service`) |
| `pitupitu connect` | hold the link open in the foreground (what the service runs) |
| `pitupitu status` | account, home folder, herdr, agents found and service as this box sees them |
| `pitupitu settings` | your preferences, default box, agent and models, updates, voice, and every box with its note, driver, on/off and agent |
| `pitupitu prefs` | show your preferences; `prefs edit`, `prefs set <file>` (`-` for stdin), `prefs clear` |
| `pitupitu box note <text>` | one line about this box ("client work, Acme"); `--clear` |
| `pitupitu box driver auto\|herdr\|terminal` | what runs agent sessions here (`auto`: herdr when it runs) |
| `pitupitu box rename <name>` | what you call this machine when you talk; also on the talk page (tap the machine) |
| `pitupitu box on` / `box off` | off: stays linked, but pitupitu starts nothing here and ignores its sessions until on again |
| `pitupitu default <box>` | the box used when you don't say which; `--clear` and pitupitu asks |
| `pitupitu updates mine\|all\|quiet` | what pitupitu brings up between turns unasked: only what it started (default), your own sessions too, or nothing unless asked |
| `pitupitu agent claude\|codex\|auto` | the agent a session gets when you don't say (`auto`: what the machine has, Claude first); `pitupitu box agent …` for this machine only |
| `pitupitu model <agent> <model>` | the model that agent runs on when you don't say (`pitupitu model claude opus`); `--clear` for its own default |
| `pitupitu place add <name> <path>` | a folder on this machine with a name you say ("in acme, run the tests"); `place rm <name>`, `pitupitu places` lists them all |
| `pitupitu --skill` | print the skill for agents; `pitupitu skill install` puts it in `~/.claude/skills` and `~/.codex/skills`, and under `~/.gemini` where Gemini CLI or Antigravity CLI is used |
| `pitupitu sessions` | the sessions in this box's own terminals (when herdr is not used) |
| `pitupitu attach [session]` | show one live here and type into it; `Ctrl-\` detaches, it keeps running |
| `pitupitu uninstall` | stop and remove the service; config and home folder stay |

Every settings command takes `--json` and never prompts without a terminal, so your agents can run them.

Logs: `journalctl --user -u pitupitu -f` on Linux, `~/Library/Logs/pitupitu.log` on macOS. They carry what happened (connected, which call failed and why), never your prompts or agent output. `PITUPITU_LOG=debug` for more.

Environment: `PITUPITU_CONFIG` (config file), `PITUPITU_CLOUD_URL`, `PITUPITU_CLOUD_API_KEY`, `HERDR_BIN` (the herdr binary, default `herdr` on `PATH`).

## Updating

pitupitu never updates itself. Run the install command again (or `brew upgrade pitupitu`), then `pitupitu setup` (or restart the service). The cloud accepts the current and the previous box protocol; an older box is told to update.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Licensed under the [Apache License 2.0](LICENSE).
