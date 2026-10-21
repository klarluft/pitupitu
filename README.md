# pitupitu

pitupitu lets you talk to your coding agents (Claude Code, Codex and the others) on any of your machines, by voice or text, from your phone or any browser. You say what you want and where; pitupitu starts the agent session, keeps an eye on it and tells you when it is done or needs you.

pitupitu runs your agents in [herdr](https://herdr.dev), a terminal workspace manager for coding agents. Open `herdr` on that machine and you see exactly what your phone sees: the same sessions, live, to watch or type into at the desk. If herdr is not installed, pitupitu installs it.

This repository is the part that runs on each of your machines: a small agent that links the machine to pitupitu cloud. The conversation, the voice and the page at [pitupitu.chat](https://pitupitu.chat) are the hosted service.

## How it works

```
phone / browser ──▶ pitupitu.chat (cloud) ◀── box link (outbound WebSocket) ── pitupitu ──▶ herdr ──▶ your agents
                                                                                              ▲
                                                                   you, at the desk: `herdr` ─┘
```

- The box agent **dials out** to `wss://api.pitupitu.chat/v1/box`. Nothing connects in: no open port, no VPN, no certificate on your machine.
- The cloud can ask for a fixed set of operations only, served through herdr: list and open workspaces and tabs, start an agent in a pane, send it a prompt or keys, read its screen, wait for it; and update this agent or herdr when you say so. There is no shell and no arbitrary command. The full list is in [docs/box-link.md](docs/box-link.md) and in [`calls.rs`](crates/pitupitu/src/calls.rs).
- Your agents run as they always do, your own unmodified `claude` and `codex` (or any agent herdr supports) signed in by you, on your own subscriptions or keys, in your own permission mode. pitupitu never reads, sees or uses their credentials.
- herdr's server: the box agent starts `herdr server` when none is running (at its own start, and whenever herdr stops answering) and never stops it. It runs on its own, so restarting or updating the box agent leaves your sessions running. Open `herdr` and you attach to that same server; if you opened `herdr` first, pitupitu uses yours.
- Where sessions go: a project gets a herdr workspace labelled after it, as you would make one; quick work in the pitupitu home folder (a lookup, a question) goes into tabs of one `pitupitu` workspace, so yours are not littered with them.
- From your files, only `skills/*/SKILL.md` in the pitupitu home folder are sent, plus what the box reports about itself: its version and herdr's (and whether newer ones exist), which agent CLIs are installed (with their versions), and the folders your agents worked in recently (only the folder paths and when, read from the agents' own session history; nothing else from those files). More of that history is sent only if you switch it on: see [History](#history).

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

The install script and `pitupitu setup` install [herdr](https://herdr.dev) with its own installer when it is not on your `PATH`. A herdr you installed with Homebrew, mise or Nix is used as it is. You need `claude` and/or `codex` (or another agent herdr knows) installed and signed in.

## Set up

```sh
pitupitu setup
```

It asks a few plain questions; pressing Enter takes the suggested answer.

1. **Sign in**: it shows a code; approve it on any screen at pitupitu.chat. The box gets its own key, stored in `~/.config/pitupitu/pitupitu.toml` (readable only by you). An account can have many boxes, each named (the hostname by default, `--box` to choose); setting up the same box again replaces its old key.
2. **pitupitu's folder** (the home folder): `~/pitupitu` by default. Requests that belong to no project run there, and `skills/<name>/SKILL.md` holds routines you name by voice. What pitupitu should know about you ("client work goes on the work laptop", "I use Codex for reviews") is your preferences, kept in your account: `pitupitu prefs edit`, the page, or just tell pitupitu "remember that …". (A `PITUPITU.md` from 0.1 is no longer read; copy anything useful into `pitupitu prefs`.)
3. **Your agents can change pitupitu's settings** (asked once, yes by default): installs a skill that teaches Claude Code and Codex how, so you can ask your own agent to do it.
4. **History** (asked once, no by default): whether pitupitu may read what your agents did on this machine, so it can summarise your work. See [History](#history).
5. **herdr**, the small program your agents run inside: installed if missing (said in one line, not asked). Then, for the agents it finds, their sessions are set to come back after herdr restarts (`herdr integration install claude` and `codex`).
6. **Starting by itself**: pitupitu starts now and whenever you log in (a user service running `pitupitu connect`: systemd on Linux, launchd on macOS). On Linux, setup asks once whether to keep it running while you are logged out, so your phone still reaches the machine and it comes back by itself after a restart; yes by default (`loginctl enable-linger`). `--linger` / `--no-linger` answer for a script.

Then open [pitupitu.chat/app](https://pitupitu.chat/app).

## Commands

| | |
|---|---|
| `pitupitu setup` | sign in, pitupitu's folder, herdr, starting by itself (`--home`, `--relink`, `--box`, `--no-service`, `--linger`, `--no-linger`) |
| `pitupitu connect` | hold the link open in the foreground (what the service runs) |
| `pitupitu status` | account, home folder, herdr (version, server, newer release), agents found, service and version as this box sees them |
| `pitupitu settings` | your preferences, default box, agent and models, updates, voice, and every box with its note, on/off, agent and history |
| `pitupitu prefs` | show your preferences; `prefs edit`, `prefs set <file>` (`-` for stdin), `prefs clear` |
| `pitupitu box note <text>` | one line about this box ("client work, Acme"); `--clear` |
| `pitupitu box rename <name>` | what you call this machine when you talk; also on the talk page (tap the machine) |
| `pitupitu box history on` / `off` | let pitupitu read what your agents did on this machine, or stop it ([History](#history)); off by default |
| `pitupitu box on` / `box off` | off: stays linked, but pitupitu starts nothing here and ignores its sessions until on again |
| `pitupitu default <box>` | the box used when you don't say which; `--clear` and pitupitu asks |
| `pitupitu updates mine\|all\|quiet` | what pitupitu brings up between turns unasked: only what it started (default), your own sessions too, or nothing unless asked |
| `pitupitu agent claude\|codex\|auto` | the agent a session gets when you don't say (`auto`: what the machine has, Claude first); `pitupitu box agent …` for this machine only |
| `pitupitu model <agent> <model>` | the model that agent runs on when you don't say (`pitupitu model claude opus`); `--clear` for its own default |
| `pitupitu place add <name> <path>` | a folder on this machine with a name you say ("in acme, run the tests"); `place rm <name>`, `pitupitu places` lists them all |
| `pitupitu --skill` | print the skill for agents; `pitupitu skill install` puts it in `~/.claude/skills` and `~/.codex/skills` |
| `pitupitu update [box\|herdr\|all]` | show the newer release and what is running, ask, then install it the way it was installed; sessions keep running (`--check` only looks, `--yes` for a script, `--handoff` restarts herdr's server too, `--no-restart`) |
| `pitupitu uninstall` | stop and remove the service; config, home folder and herdr stay |

Every settings command takes `--json` and never prompts without a terminal, so your agents can run them.

Logs: `journalctl --user -u pitupitu -f` on Linux, `~/Library/Logs/pitupitu.log` on macOS. They carry what happened (connected, which call failed and why), never your prompts or agent output. `PITUPITU_LOG=debug` for more.

To see a session at the desk, open `herdr` on that machine (or `herdr --remote <machine>` over SSH): pitupitu's sessions are there with yours.

Environment: `PITUPITU_CONFIG` (config file), `PITUPITU_CLOUD_URL`, `PITUPITU_CLOUD_API_KEY`, `HERDR_BIN` (the herdr binary, default `herdr` on `PATH`). `PITUPITU_HERDR_SESSION` points the box agent at a named herdr session instead of the default one; it is for tests.

## History

Off unless you switch it on, for each machine on its own: `pitupitu box history on`, the question `pitupitu setup` asks, or the page. With it on, pitupitu can tell you what you were doing on the laptop yesterday, or what is left on a project, including the sessions you started yourself.

What is sent, from Claude Code's and Codex's own session logs on this machine (the last 30 days):

- each session's folder, its repository (root, `origin` remote without any password in it, branch), when it ran and its title;
- what you typed and what the agent answered, each cut to 2000 characters;
- for each tool the agent used, its name and one target: the file it edited or read, or the first 120 characters of the command it ran;
- for the repositories those sessions worked in: the branch, HEAD, and the current branch's commits of the last 30 days, with the subject line and how many files each changed.

Never sent: file contents, edits' text, command output and other tool results, the agents' thinking, subagents' transcripts, anything else in `~/.claude` or `~/.codex`, credentials, author names or emails, diffs. Anything that looks like a key or password in what is sent becomes `[redacted]` first (`sk-…`, `ghp_…`, `Bearer …`, `password=…` and the like).

`pitupitu box history off` stops it at once, and the box removes its small record of what it already sent (`history.json` in its state folder). What pitupitu already stored is yours to delete on pitupitu.chat. The exact format is in [docs/box-link.md](docs/box-link.md#history).

## Updating

pitupitu never updates itself, or herdr. `pitupitu status` and the page at pitupitu.chat say when a newer release of either exists (the box asks GitHub a few times a day), and pitupitu can offer it when you talk. Then:

```sh
pitupitu update          # this box agent
pitupitu update herdr    # herdr
pitupitu update all      # both
```

Each shows the version it would install, what is running right now, and asks before doing anything. It updates the way the thing was installed: `brew upgrade` for a Homebrew install; for the box agent otherwise the release binary (checked against its SHA-256) in place of this one; for herdr otherwise herdr's own `herdr update`. A herdr installed some other way (mise, Nix) is left to that way.

Your sessions are not touched. The box agent's update restarts only the box agent; its link to the cloud drops for a few seconds. herdr's update replaces the binary and leaves the running server and its sessions as they are; the server runs the new version after it next restarts (`pitupitu status` says when one is waiting). `pitupitu update herdr --handoff` restarts it now, keeping the sessions (herdr's live handoff, best effort).

`--check` only looks; `--yes` answers for you, for a script; `--no-restart` leaves the running service on the old version until you restart it. Updating from the page or by voice runs the same, after you said yes. The cloud accepts the current and the previous box protocol; an older box is told to update.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Licensed under the [Apache License 2.0](LICENSE).
