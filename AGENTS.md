# pitupitu (box agent)

The open-source agent that runs on the user's machine and links their coding agents (in herdr, or in terminals it hosts itself) to pitupitu cloud over one outbound WebSocket. Read `README.md` for what it does, `docs/box-link.md` for the protocol and `docs/sessions.md` for what every state and error means on both drivers.

- Keep it small: it serves the box link calls over herdr or its own terminal host, reports its inventory and the home folder's skills, and installs itself as a user service. Conversation, voice and everything else is the cloud's.
- `crates/herdr` is the only crate that runs `herdr` (`herdr --skill`, `herdr api schema` for exact payloads). Key "a turn finished" on `completion_seq`, never on the `done` status.
- `crates/pitupitu`: `calls.rs` (call → `AgentHost` method), `link.rs` (socket, hello, notes, liveness, reconnect), `router.rs` (herdr and the terminal host side by side), `term/` (the terminal host: `holder.rs` the PTYs, `client.rs` its `AgentHost` and the hook entry, `detect.rs` what a screen shows, `attach.rs`), `inventory.rs`, `settings.rs`, `skill.rs`, `home.rs`, `device.rs` (sign-in), `config.rs`, `service.rs`.
- The terminal host runs the person's own, unmodified agent CLIs and never reads their credentials or adds permission flags (`docs/box-link.md`, "Terminal host"). Keep it that way.
- The cloud may only do what a named call allows; no shell, no arbitrary herdr command.
- Logs: metadata only, never prompts, agent output or file contents.
- Live herdr tests: create your own workspace or tab and close it; don't touch other panes.
- `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test` clean before committing. Commits are signed off (DCO).

`CLAUDE.md` only imports this file.
