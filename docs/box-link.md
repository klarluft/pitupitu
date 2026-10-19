# Box link protocol

The box link is the one connection between this agent and pitupitu cloud. The box dials out; nothing connects in. This is protocol version **2** (the cloud also accepts 1).

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

The cloud accepts the current protocol and the one before it. The box never updates itself.

## Frames

JSON text frames, three shapes:

- **call** (cloud → box): `{"id": 17, "call": "read", "args": {"agent": "notes", "lines": 80}}`
- **reply** (box → cloud): `{"id": 17, "ok": <result>}` or `{"id": 17, "err": {"kind": "…", "code": "…", "message": "…"}}`
- **note** (either way, no reply): `{"note": "<name>", …}`

`err.kind` is one of `api` (herdr answered with an error; `code` is herdr's error code), `not_ready` (the agent did not reach a state that accepts prompts), `timeout` (the box gave up on the call), `io` (herdr could not be run or answered something unexpected), `bad_call` (unknown call or bad arguments).

Calls may overlap: each runs on its own task and replies come back in any order. The box times a call out itself after its own `timeout_ms` plus a margin (150 s when it has none).

## The first frames

The box speaks first:

```json
{"note": "hello", "protocol": 2, "version": "0.2.0", "box": "my-laptop", "home": "/home/me/pitupitu",
 "agents": [<AgentInfo>…],
 "driver": "herdr", "herdr": "herdr 0.9.3",
 "kinds": ["claude", "codex", "gemini", "agy"],
 "available": [{"kind": "claude", "version": "2.3.1 (Claude Code)"}],
 "recent": [{"path": "/home/me/code/app", "agent": "claude", "at": "2026-10-17T20:00:00Z"}]}
```

then a `home` note. Before `hello` the cloud sends nothing.

- `driver`: where new sessions start: `herdr`, or `terminal` for the box's own terminal host (below).
- `herdr`: herdr's version, `""` when it does not answer.
- `latest` (in `inventory`): a newer release than this box runs, as `"0.4.0"`, or absent; the box asks GitHub when it starts and every six hours. The person updates with `pitupitu update`; the box never updates itself. Since 0.4.
- `kinds`: the agent kinds the driver taking new sessions can start right now (herdr's own list, or `claude` and `codex` for the terminal host). Since 0.4; the cloud refuses a kind not in it before anything is opened (`sessions.md`, "Kinds").
- `available`: the agent CLIs found on `PATH` (`claude`, `codex`, `gemini`, `opencode`) with the first line of `--version`. Whether they are logged in is not checked: that would mean reading their credentials, which the box never does.
- `recent`: up to 30 project folders, newest first, that still exist: the working directory of recent sessions in Claude Code's `~/.claude/projects/*/*.jsonl` and Codex's `~/.codex/sessions/**/*.jsonl`, and when the session file was last written. Only the folder path is read from those files.

Protocol 1 (0.1.x) sent `hello` without `driver`, `available` and `recent`; the cloud accepts both.

## Calls

Names and fields follow the `AgentHost` trait and the types in [`crates/herdr`](../crates/herdr/src/types.rs) (`AgentInfo`, `Workspace`, `Created`).

| call | args | ok |
|---|---|---|
| `list_workspaces` | — | `[Workspace]` |
| `create_workspace` | `cwd`, `label`, `driver?` (`terminal` or `herdr`: where the cloud would like the session, e.g. `terminal` for scratch work such as lookups so herdr is not littered with them; honoured only while the box's driver setting is `auto`; since 0.3.2), `kind?` (the agent the cloud means to start there; refused as `unsupported_agent_kind` before the workspace opens when the driver cannot start it; a box pinned to herdr while herdr does not answer refuses with `driver_unavailable`; since 0.4) | `Created` |
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

An unknown call answers `bad_call`, so calls can be added behind a protocol bump without breaking older boxes. There is no other way for the cloud to act on the box.

## Notes from the box

- `home`: `{"note": "home", "files": [{"path": "skills/deploy/SKILL.md", "text": "…"}]}`. Sent after `hello` and within about a second of any change. Only `skills/*/SKILL.md` from the home folder are ever sent, each cut at 64 KB. (0.1 also sent `PITUPITU.md`; the person's preferences live in the cloud now.)
- `inventory`: `{"note": "inventory", "driver": …, "herdr": …, "kinds": […], "latest": …, "available": […], "recent": […]}`, the fields of `hello` again: when herdr starts or stops answering, in reply to the cloud's `settings` note, every five minutes when `recent` changed, and when the newest release known changes.
- `agents`: `{"note": "agents", "agents": [<AgentInfo>…]}`, the full agent list (herdr's and the terminal host's), sent whenever the set of panes or any pane's `(agent_status, state_change_seq, completion_seq)` differs from the last list sent. The box looks once a second.

## Notes from the cloud

- `settings`: `{"note": "settings", "driver": "auto" | "herdr" | "terminal"}`, sent after `hello` and when the person changes it: where new sessions start. `auto` is herdr when it answers, else the terminal host. Answered with an `inventory` note. Sessions already running on the other side keep being served there until they end. (0.2 only had herdr.)

Nothing is buffered on the box while the link is down; the `hello` after a reconnect carries a fresh agent list.

## Terminal host

What the states, counters and error codes mean, identically on both drivers, is in [`sessions.md`](sessions.md).

Since 0.3, a box without herdr (or set to `terminal`) serves the same calls from terminals it hosts itself. The cloud cannot tell the difference, except that the terminal host's ids start with `tw` (`tw3`, `tw3:t1`, `tw3:p1`), so they never collide with herdr's.

- **Your own agents, unmodified.** It runs `claude` and `codex` from your `PATH`, as you, in your login shell, signed in by you. It never reads, copies or passes on their credentials, and adds no permission flags: they run in your own permission mode. If an agent is not logged in, its session reports blocked and pitupitu tells you to run it once and log in.
- **Holder.** The terminals live in `pitupitu terminals`, a process the box agent starts when it first needs it and leaves running in a session of its own; the user service stops only the box agent (`KillMode=process` on Linux, `AbandonProcessGroup` on macOS), so updating or restarting it never ends a session. It listens on a Unix socket (`$XDG_RUNTIME_DIR/pitupitu/terms.sock`, else `~/.local/state/pitupitu/terms.sock`, mode 0600) and exits when its last terminal closes. Each terminal is a PTY running your login shell in the folder, with a screen emulator holding what it shows and 5000 lines of scrollback.
- **Starting an agent** types its command into the shell: the agent, the cloud's arguments (a model), and how it reports its state for that session only: for Claude Code `--settings` with hooks (`SessionStart` → ready, `UserPromptSubmit` → working, `Stop` → done, `Notification` of a permission prompt → blocked, `PostToolUse` → working again), each running `pitupitu hook <pane> <event>`; for Codex `-c notify=[…]` (turn complete → done). Your own settings files are not touched. Claude Code's trust prompt is answered with "Yes, I trust this folder" only when the cloud asks (`accept_trust_prompt`); Codex's with "No, ask me to approve", never the option that switches approvals off.
- **The screen** covers what hooks do not: an approval, a question, the trust prompt or a login screen at the bottom of the screen is `blocked`; the shell in front again means the agent exited, and it leaves the list.
- **Calls**: `prompt` types the text (pasted, so new lines stay in it) and Enter; `send_keys` the named keys; `read` the last lines of scrollback and screen, and counts as having looked (`done` → `idle`); `wait` polls.
- **You can look**: `pitupitu sessions` lists them; `pitupitu attach [session]` shows one live in your terminal and lets you type into it; `Ctrl-\` detaches and leaves it running. A Claude Code session can also be continued anywhere with `claude --resume`.
