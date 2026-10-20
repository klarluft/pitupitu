# Box link protocol

The box link is the one connection between this agent and pitupitu cloud. The box dials out; nothing connects in. This is protocol version **3** (box 0.5; the cloud also accepts 2).

## Connecting

`GET wss://api.pitupitu.chat/v1/box` with `Authorization: Bearer ptp_…`, the key `pitupitu setup` got through the device sign-in. TLS only.

Refusals and endings are WebSocket close codes after the upgrade:

| Code | Reason | What the box does |
|---|---|---|
| 4001 | `invalid_key` | logs "run `pitupitu setup --relink`", keeps retrying with backoff |
| 4006 | `replaced` | another box connected with the same key: stops (exit 0) instead of fighting it |
| 4007 | `protocol` | this box is too old; logs how to update, keeps retrying with backoff |
| 4008 | `renamed` | the box was renamed (on the page or with `pitupitu box rename`); it reconnects as after any close and is linked under the new name |

Anything else (a deploy, a network drop) is followed by a reconnect: backoff from 1 s doubling to 30 s, with up to 25 % jitter, forever; a link that stayed up 30 s starts the backoff over.

Liveness: the cloud pings every 20 s. The box treats 60 s without a ping as a dead socket and reconnects.

The cloud accepts the current protocol and the one before it. The box never updates itself: an update runs when the person asks, at the box (`pitupitu update`) or through the cloud (the `update` call).

## Frames

JSON text frames, three shapes:

- **call** (cloud → box): `{"id": 17, "call": "read", "args": {"agent": "notes", "lines": 80}}`
- **reply** (box → cloud): `{"id": 17, "ok": <result>}` or `{"id": 17, "err": {"kind": "…", "code": "…", "message": "…"}}`
- **note** (either way, no reply): `{"note": "<name>", …}`

`err.kind` is one of `api` (herdr answered with an error, or the box refused; `code` is herdr's error code or the box's own), `not_ready` (the agent did not reach a state that accepts prompts), `timeout` (the box gave up on the call), `io` (herdr could not be run or answered something unexpected), `bad_call` (unknown call or bad arguments).

Calls may overlap: each runs on its own task and replies come back in any order. The box times a call out itself after its own `timeout_ms` plus a margin (150 s when it has none).

## The first frames

The box speaks first:

```json
{"note": "hello", "protocol": 3, "version": "0.5.0", "box": "my-laptop", "home": "/home/me/pitupitu",
 "agents": [<AgentInfo>…],
 "install": "installer",
 "latest": null,
 "herdr": {"version": "0.9.3", "protocol": 22, "running": true, "latest": "0.9.5",
           "restart_needed": false, "install": "installer"},
 "kinds": ["claude", "codex", "gemini", "agy", …],
 "available": [{"kind": "claude", "version": "2.3.1 (Claude Code)"}],
 "recent": [{"path": "/home/me/code/app", "agent": "claude", "at": "2026-10-17T20:00:00Z"}]}
```

then a `home` note. Before `hello` the cloud sends nothing.

- `install`: how this box agent was installed, so an update runs the same way: `brew` when the binary lives under a Homebrew `Cellar`, else `installer` (the release tarball, as `install.sh` and `pitupitu update` put it).
- `latest`: a newer release of the box agent than this one, as `"0.5.1"`, or null. The box asks GitHub (`klarluft/pitupitu` releases) when it starts and every six hours.
- `herdr`: null when herdr is not installed. `version` and `protocol` are the installed herdr's version and socket protocol (`herdr status`); `running` whether its server answers right now (the box starts one when it does not, below); `latest` a newer herdr release (`herdrdev/herdr` on GitHub, asked on the same schedule) or null; `restart_needed` whether the running server is older than the binary after an update (it keeps working; new server-side features wait for a restart); `install` is `brew` (under a Cellar), `installer` (where herdr's own installer puts it: `~/.local/bin`, `$HERDR_INSTALL_DIR`) or `other` (mise, Nix, a build of one's own), which the box does not update.
- `kinds`: the agent kinds herdr can start (`herdr agent`'s list). Empty while herdr does not answer. The cloud refuses a kind not in it before anything is opened (`sessions.md`, "Kinds").
- `available`: the agent CLIs found on `PATH` (`claude`, `codex`, `gemini`, `agy`, `opencode`) with the first line of `--version`. Whether they are logged in is not checked: that would mean reading their credentials, which the box never does.
- `recent`: up to 30 project folders, newest first, that still exist: the working directory of recent sessions in Claude Code's `~/.claude/projects/*/*.jsonl` and Codex's `~/.codex/sessions/**/*.jsonl`, and when the session file was last written. Only the folder path is read from those files.

Protocol 2 (0.2 to 0.4) had `driver` (`herdr` or `terminal`, for the terminal host those versions had), `herdr` as a version string, and `latest` in `inventory` only. Protocol 1 (0.1) sent `hello` without `driver`, `available` and `recent`.

## Calls

Names and fields follow the `AgentHost` trait and the types in [`crates/herdr`](../crates/herdr/src/types.rs) (`AgentInfo`, `Workspace`, `Created`).

| call | args | ok |
|---|---|---|
| `list_workspaces` | — | `[Workspace]` |
| `create_workspace` | `cwd`, `label`, `kind?` (the agent the cloud means to start there; refused as `unsupported_agent_kind` before the workspace opens when herdr cannot start it; since 0.4), `scratch?` (default false; since 0.5: work in the pitupitu home folder, such as a lookup. The box opens a tab in its one `pitupitu` workspace instead of a workspace of its own, so the person's are not littered with lookups. That workspace is found by its label, or opened with `cwd` when there is none. `Created` names the `pitupitu` workspace, the new tab and its pane; the cloud closes the task with `close_tab`, leaving the other scratch tabs, and herdr closes the workspace with its last tab). A `driver` argument from a protocol 2 cloud is ignored | `Created` |
| `create_tab` | `workspace`, `cwd?`, `label` | `Created` |
| `close_workspace` | `workspace` | `null` |
| `close_tab` | `tab` | `null` |
| `start_agent` | `name`, `kind`, `pane`, `accept_trust_prompt`, `timeout_ms?`, `agent_args?` | `AgentInfo` |
| `prompt` | `agent`, `text` | `AgentInfo` (the state before the agent reacted) |
| `agent` | `agent` | `AgentInfo` |
| `list_agents` | — | `[AgentInfo]` |
| `wait` | `agent`, `until` (status strings), `timeout_ms` (capped at 120000) | `AgentInfo` |
| `read` | `agent`, `lines` (capped at 2000), `color` (optional, default false) | string; with `color`, colours and attributes come as SGR codes (`ESC [ … m`) and no other escapes, for a page showing the screen |
| `send_keys` | `agent`, `keys` | `null` |
| `update` | `what` (`box` or `herdr`), `handoff?` (herdr only, default false: restart herdr's server on the new version, the sessions kept, best effort; without it the running server stays as it is) | `{"what", "from", "to", "install", "restart"}`; since 0.5 |

An unknown call answers `bad_call`, so calls can be added behind a protocol bump without breaking older boxes. There is no other way for the cloud to act on the box.

### `update`

Runs the update the way the thing was installed, exactly as `pitupitu update [box|herdr] --yes` does at the keyboard, and only because the cloud asked on the person's word; the box never starts one itself.

- `box`: `brew upgrade pitupitu` under a Cellar; otherwise the release tarball for this machine, checked against its `.sha256`, put in place of the running binary. The reply goes out first; then the service restarts, the link drops, and the next `hello` carries the new `version`. `restart` is `done` when a service runs the box agent, `needed` when there is none (a `pitupitu connect` started by hand).
- `herdr`: `brew upgrade herdr` under a Cellar, `herdr update` (`--handoff` when asked) for herdr's own installer. Afterwards `herdr status` is read again and an `inventory` note follows. `restart` is `needed` when the running server is older than the new binary, `done` after a handoff, `none` otherwise.
- Errors are `api` with a code: `update_failed` (it ran and failed; the message is the command's last line), `update_unavailable` (herdr installed some other way, herdr not installed, no release binary for this machine; the message says what to do instead), `nothing_to_update` (already the newest).

## Notes from the box

- `home`: `{"note": "home", "files": [{"path": "skills/deploy/SKILL.md", "text": "…"}]}`. Sent after `hello` and within about a second of any change. Only `skills/*/SKILL.md` from the home folder are ever sent, each cut at 64 KB. (0.1 also sent `PITUPITU.md`; the person's preferences live in the cloud now.)
- `inventory`: `{"note": "inventory", "install": …, "latest": …, "herdr": …, "kinds": […], "available": […], "recent": […]}`, the fields of `hello` again, sent when any of them changed: herdr starts or stops answering, `herdr status` reads differently (looked at every minute and after an update), a newer release appears (asked every six hours), or `recent` changed (looked at every five minutes).
- `agents`: `{"note": "agents", "agents": [<AgentInfo>…]}`, herdr's full agent list, sent whenever the set of panes or any pane's `(agent_status, state_change_seq, completion_seq)` differs from the last list sent. The box looks once a second.

## Notes from the cloud

None are acted on. Protocol 2's `settings` note (the driver: herdr or the box's own terminals) is ignored: herdr serves every call.

Nothing is buffered on the box while the link is down; the `hello` after a reconnect carries a fresh agent list.

## herdr on the box

What the states, counters and error codes mean is in [`sessions.md`](sessions.md).

- **Your own agents, unmodified.** herdr starts them (`herdr agent start --kind`) from your `PATH`, as you, signed in by you, in your own permission mode. The box never reads, copies or passes on their credentials and adds no permission flags. The folder trust prompt is answered only when the cloud asks (`accept_trust_prompt`), with the option that keeps approvals on.
- **The server.** herdr starts no server by itself: with none running, every call fails with `server_not_running`. The box starts `herdr server` before `hello` and whenever its once-a-second `agent list` finds none running, detached (a session and process group of its own, no terminal, not part of the user service), with a pause between tries that grows from 5 s to 5 min. It never stops a server. A herdr you opened is used as it is; one the box started is what `herdr` attaches to later. Everything is in herdr's default session.
- **Workspaces and tabs.** A place is a herdr workspace (`workspace create --cwd --label --no-focus`); a task in it is a tab. Scratch work goes into tabs of one `pitupitu` workspace (`create_workspace` with `scratch`). pitupitu closes only what it created.
- **Seeing a session.** `herdr` on that machine, or `herdr --remote <machine>` over SSH.
- **Installed and updated by pitupitu, when asked.** `install.sh` and `pitupitu setup` install herdr with its own installer when it is missing, and `setup` runs `herdr integration install` for Claude Code and Codex so herdr resumes their sessions after a restart. Updates run only through `pitupitu update herdr` or the `update` call.
