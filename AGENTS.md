# pitupitu (box agent)

The open-source agent that runs on the user's machine and links herdr to pitupitu cloud over one outbound WebSocket. Read `README.md` for what it does and `docs/box-link.md` for the protocol.

- Keep it small: it serves the box link calls over herdr, sends the home folder's pitupitu files, and installs itself as a user service. Conversation, voice and everything else is the cloud's.
- `crates/herdr` is the only crate that runs `herdr` (`herdr --skill`, `herdr api schema` for exact payloads). Key "a turn finished" on `completion_seq`, never on the `done` status.
- `crates/pitupitu`: `calls.rs` (call → `AgentHost` method), `link.rs` (socket, hello, notes, liveness, reconnect), `home.rs`, `device.rs` (sign-in), `config.rs`, `service.rs`.
- The cloud may only do what a named call allows; no shell, no arbitrary herdr command.
- Logs: metadata only, never prompts, agent output or file contents.
- Live herdr tests: create your own workspace or tab and close it; don't touch other panes.
- `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test` clean before committing. Commits are signed off (DCO).

`CLAUDE.md` only imports this file.
