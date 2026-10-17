# Box link protocol

The box link is the one connection between this agent and pitupitu cloud. The box dials out; nothing connects in. This is protocol version **1**.

## Connecting

`GET wss://api.pitupitu.chat/v1/box` with `Authorization: Bearer ptp_…`, the key `pitupitu setup` got through the device sign-in. TLS only.

Refusals and endings are WebSocket close codes after the upgrade:

| Code | Reason | What the box does |
|---|---|---|
| 4001 | `invalid_key` | logs "run `pitupitu setup --relink`", keeps retrying with backoff |
| 4006 | `replaced` | another box connected with the same key: stops (exit 0) instead of fighting it |
| 4007 | `protocol` | this box is too old; logs how to update, keeps retrying with backoff |

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
 "available": [{"kind": "claude", "version": "2.3.1 (Claude Code)"}],
 "recent": [{"path": "/home/me/code/app", "agent": "claude", "at": "2026-10-17T20:00:00Z"}]}
```

then a `home` note. Before `hello` the cloud sends nothing.

- `driver`: what serves the calls. 0.2 has only `herdr`; the box's own terminal host comes in a later version.
- `herdr`: herdr's version, `""` when it does not answer.
- `available`: the agent CLIs found on `PATH` (`claude`, `codex`, `gemini`, `opencode`) with the first line of `--version`. Whether they are logged in is not checked: that would mean reading their credentials, which the box never does.
- `recent`: up to 30 project folders, newest first, that still exist: the working directory of recent sessions in Claude Code's `~/.claude/projects/*/*.jsonl` and Codex's `~/.codex/sessions/**/*.jsonl`, and when the session file was last written. Only the folder path is read from those files.

Protocol 1 (0.1.x) sent `hello` without `driver`, `available` and `recent`; the cloud accepts both.

## Calls

Names and fields follow the `AgentHost` trait and the types in [`crates/herdr`](../crates/herdr/src/types.rs) (`AgentInfo`, `Workspace`, `Created`).

| call | args | ok |
|---|---|---|
| `list_workspaces` | — | `[Workspace]` |
| `create_workspace` | `cwd`, `label` | `Created` |
| `create_tab` | `workspace`, `cwd?`, `label` | `Created` |
| `close_workspace` | `workspace` | `null` |
| `close_tab` | `tab` | `null` |
| `start_agent` | `name`, `kind`, `pane`, `accept_trust_prompt`, `timeout_ms?`, `agent_args?` | `AgentInfo` |
| `prompt` | `agent`, `text` | `AgentInfo` (the state before the agent reacted) |
| `agent` | `agent` | `AgentInfo` |
| `list_agents` | — | `[AgentInfo]` |
| `wait` | `agent`, `until` (status strings), `timeout_ms` (capped at 120000) | `AgentInfo` |
| `read` | `agent`, `lines` (capped at 2000) | string |
| `send_keys` | `agent`, `keys` | `null` |

An unknown call answers `bad_call`, so calls can be added behind a protocol bump without breaking older boxes. There is no other way for the cloud to act on the box.

## Notes from the box

- `home`: `{"note": "home", "files": [{"path": "skills/deploy/SKILL.md", "text": "…"}]}`. Sent after `hello` and within about a second of any change. Only `skills/*/SKILL.md` from the home folder are ever sent, each cut at 64 KB. (0.1 also sent `PITUPITU.md`; the person's preferences live in the cloud now.)
- `inventory`: `{"note": "inventory", "driver": …, "herdr": …, "available": […], "recent": […]}`, the fields of `hello` again: when herdr starts or stops answering, in reply to the cloud's `settings` note, and every five minutes when `recent` changed.
- `agents`: `{"note": "agents", "agents": [<AgentInfo>…]}`, herdr's full agent list, sent whenever the set of panes or any pane's `(agent_status, state_change_seq, completion_seq)` differs from the last list sent. The box polls herdr once a second for it.

## Notes from the cloud

- `settings`: `{"note": "settings", "driver": "auto" | "herdr" | "terminal"}`, sent after `hello` and when the person changes it. 0.2 answers with an `inventory` note and stays on herdr (a box set to `terminal` logs that this version has no terminal host yet).

Nothing is buffered on the box while the link is down; the `hello` after a reconnect carries a fresh agent list.
