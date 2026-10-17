# pitupitu

pitupitu lets you talk to the AI agents you run in [herdr](https://herdr.dev), by voice or text, from your phone or any browser. You say what you want; pitupitu starts the agent session, keeps an eye on it and tells you when it is done or needs you.

This repository is the part that runs on your machine: a small agent that links herdr to pitupitu cloud. The conversation, the voice and the page at [pitupitu.chat](https://pitupitu.chat) are the hosted service.

## How it works

```
phone / browser ──▶ pitupitu.chat (cloud) ◀── box link (outbound WebSocket) ── pitupitu ──▶ herdr ──▶ your agents
```

- The box agent **dials out** to `wss://api.pitupitu.chat/v1/box`. Nothing connects in: no open port, no VPN, no certificate on your machine.
- The cloud can ask for a fixed set of herdr operations only: list and open workspaces and tabs, start an agent in a pane, send it a prompt or keys, read its screen, wait for it. There is no shell and no arbitrary command. The full list is in [docs/box-link.md](docs/box-link.md) and in [`calls.rs`](crates/pitupitu/src/calls.rs).
- Your agents run as they always do in herdr, with your own subscriptions or keys. pitupitu never sees or uses their credentials.
- From your files, only `PITUPITU.md` and `skills/*/SKILL.md` in the pitupitu home folder are sent (see below).

## Install

```sh
curl -fsSL https://pitupitu.chat/install | sh
```

or build from source with a recent stable Rust:

```sh
cargo install --git https://github.com/klarluft/pitupitu pitupitu
```

You need [herdr](https://herdr.dev) installed and running.

## Set up

```sh
pitupitu setup
```

1. **Sign in**: it shows a code; approve it on any screen at pitupitu.chat. The box gets its own key, stored in `~/.config/pitupitu/pitupitu.toml` (readable only by you). One account has one box: setting up a new machine replaces the old one.
2. **Home folder**: `~/pitupitu` by default. It holds `PITUPITU.md`, your instructions for pitupitu ("start coding sessions with Codex", "P2P2 means my pitupitu workspace"), and `skills/<name>/SKILL.md` for routines you name by voice. Requests that belong to no project run in a herdr workspace there. Edit these files by hand or ask pitupitu to have one of your agents change them.
3. **Service**: a user service starts `pitupitu connect` now and at login (systemd on Linux, launchd on macOS). On a Linux machine you log out of, run `loginctl enable-linger $USER` so it keeps running.

Then open [pitupitu.chat/app](https://pitupitu.chat/app).

## Commands

| | |
|---|---|
| `pitupitu setup` | sign in, home folder, user service (`--home`, `--relink`, `--box`, `--no-service`) |
| `pitupitu connect` | hold the link open in the foreground (what the service runs) |
| `pitupitu status` | account, home folder, herdr and service as this box sees them |
| `pitupitu uninstall` | stop and remove the service; config and home folder stay |

Logs: `journalctl --user -u pitupitu -f` on Linux, `~/Library/Logs/pitupitu.log` on macOS. They carry what happened (connected, which call failed and why), never your prompts or agent output. `PITUPITU_LOG=debug` for more.

Environment: `PITUPITU_CONFIG` (config file), `PITUPITU_CLOUD_URL`, `PITUPITU_CLOUD_API_KEY`, `HERDR_BIN` (the herdr binary, default `herdr` on `PATH`).

## Updating

pitupitu never updates itself. Run the install command again, or `brew upgrade pitupitu`, then `pitupitu setup` (or restart the service). The cloud accepts the current and the previous box protocol; an older box is told to update.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Licensed under the [Apache License 2.0](LICENSE).
