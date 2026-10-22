# pitupitu (box agent)

The open-source agent that runs on the user's machine and links their coding agents, running in herdr, to pitupitu cloud over one outbound WebSocket. Read `README.md` for what it does, `docs/box-link.md` for the protocol and `docs/sessions.md` for what every state and error means.

- This repo is the box agent only: its code, the box link protocol docs, `README.md`, `CONTRIBUTING.md` and the skill. Product plans, ideas, notes and to-do lists do not live here, not even as a draft. A commit message or pull request describes its own change and nothing more: no links to other repositories' work and no quoted conversations.
- Keep it small: it serves the box link calls through herdr, keeps herdr's server running, reports its inventory and the home folder's skills, sends the agents' session history unless the person switched it off, updates itself and herdr when asked, and installs itself as a user service. Conversation, voice and everything else is the cloud's.
- `crates/herdr` is the only crate that runs `herdr` (`herdr --skill`, `herdr api schema` for exact payloads): `cli.rs` the `AgentHost` over herdr's CLI, `admin.rs` herdr itself (status, starting its server, updating, integrations), `detect.rs` what a screen shows. Key "a turn finished" on `completion_seq`, never on the `done` status.
- `crates/pitupitu`: `calls.rs` (call → `AgentHost` method, scratch tabs), `link.rs` (socket, hello, notes, the `update` call, liveness, reconnect), `herdr_box.rs` (herdr on this box: the inventory's `herdr`, keeping its server up, updating it), `inventory.rs`, `history.rs` (the session reader, on unless switched off, and git facts: what it may read and send is in `docs/box-link.md`, "History"; keep its never-sent list), `settings.rs`, `list.rs` (the person's list, reminders and schedules, over the cloud's REST API), `skill.rs`, `home.rs`, `device.rs` (sign-in), `config.rs`, `service.rs`, `update.rs` (the box agent's own update and the release checks: shown and asked, never silent; sessions untouched). `lib.rs` exposes these to the tests under `tests/`.
- herdr runs the person's own, unmodified agent CLIs; the box never reads their credentials or adds permission flags. Keep it that way.
- The box starts herdr's server when none answers and never stops one. It never updates itself or herdr unasked.
- The cloud may only do what a named call allows; no shell, no arbitrary herdr command.
- The skill (`skill/SKILL.md`) is the agents' manual for the CLI: a CLI change updates it in the same change. The tests in `crates/pitupitu/src/skill_drift.rs` fail when a command it shows does not parse or a command is missing from it.
- Logs: metadata only, never prompts, agent output or file contents.
- Live herdr tests: a throwaway named session (`herdr --session`), started and then stopped and deleted by the test; never the person's own session. Inside it, create your own workspace or tab and close it.
- `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test` clean before committing. Commits are signed off (DCO).

`CLAUDE.md` only imports this file.
