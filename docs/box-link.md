# Box link protocol

The box link is the one connection between this agent and pitupitu cloud. The box dials out; nothing connects in. This is protocol version **5** (box 0.7; the cloud also accepts 4).

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
{"note": "hello", "protocol": 5, "version": "0.7.0", "box": "my-laptop", "home": "/home/me/pitupitu",
 "agents": [<AgentInfo>…],
 "install": "installer",
 "latest": null,
 "herdr": {"version": "0.9.3", "protocol": 22, "running": true, "latest": "0.9.5",
           "restart_needed": false, "install": "installer"},
 "kinds": ["claude", "codex", "gemini", "agy", …],
 "available": [{"kind": "claude", "version": "2.3.1 (Claude Code)"}],
 "recent": [{"path": "/home/me/code/app", "agent": "claude", "at": "2026-10-17T20:00:00Z"}]}
```

then a `home` note. Before `hello` the cloud sends nothing; right after it, the cloud sends `settings` (below).

- `install`: how this box agent was installed, so an update runs the same way: `brew` when the binary lives under a Homebrew `Cellar`, else `installer` (the release tarball, as `install.sh` and `pitupitu update` put it).
- `latest`: a newer release of the box agent than this one, as `"0.5.1"`, or null. The box asks GitHub (`klarluft/pitupitu` releases) when it starts and every six hours.
- `herdr`: null when herdr is not installed. `version` and `protocol` are the installed herdr's version and socket protocol (`herdr status`); `running` whether its server answers right now (the box starts one when it does not, below); `latest` a newer herdr release (`herdrdev/herdr` on GitHub, asked on the same schedule) or null; `restart_needed` whether the running server is older than the binary after an update (herdr says `restart_needed` or `server_binary_stale`, or the server reports another version than the binary; it keeps working, new server-side features wait for a restart); `install` is `brew` (under a Cellar), `installer` (where herdr's own installer puts it: `~/.local/bin`, `$HERDR_INSTALL_DIR`) or `other` (mise, Nix, a build of one's own), which the box does not update.
- `kinds`: the agent kinds herdr can start (`herdr agent`'s list). Empty while herdr does not answer. The cloud refuses a kind not in it before anything is opened (`sessions.md`, "Kinds").
- `available`: the agent CLIs found on `PATH` (`claude`, `codex`, `gemini`, `agy`, `opencode`) with the first line of `--version`. Whether they are logged in is not checked: that would mean reading their credentials, which the box never does.
- `recent`: up to 30 project folders, newest first, that still exist: the working directory of recent sessions in Claude Code's `~/.claude/projects/*/*.jsonl` and Codex's `~/.codex/sessions/**/*.jsonl`, and when the session file was last written. Only the folder path is read from those files.

Protocol 4 (0.6) had the same `hello` and every call but `close_session`. Protocol 3 (0.5) had the same `hello`; its cloud sent no `history` setting and the box sent no `session` or `git` notes. Protocol 2 (0.2 to 0.4) had `driver` (`herdr` or `terminal`, for the terminal host those versions had), `herdr` as a version string, and `latest` in `inventory` only. Protocol 1 (0.1) sent `hello` without `driver`, `available` and `recent`.

## Calls

Names and fields follow the `AgentHost` trait and the types in [`crates/herdr`](../crates/herdr/src/types.rs) (`AgentInfo`, `Workspace`, `Created`).

| call | args | ok |
|---|---|---|
| `list_workspaces` | — | `[Workspace]` |
| `create_workspace` | `cwd`, `label`, `kind?` (the agent the cloud means to start there; refused as `unsupported_agent_kind` before the workspace opens when herdr cannot start it; since 0.4), `scratch?` (default false; since 0.5: work in the pitupitu home folder, such as a lookup. The box opens a tab in its one `pitupitu` workspace instead of a workspace of its own, so the person's are not littered with lookups. That workspace is found by its label, or opened with `cwd` when there is none. `Created` names the `pitupitu` workspace, the new tab and its pane; the cloud closes the task with `close_tab`, leaving the other scratch tabs, and herdr closes the workspace with its last tab). A `driver` argument from a protocol 2 cloud is ignored | `Created` |
| `create_tab` | `workspace`, `cwd?`, `label` | `Created` |
| `close_workspace` | `workspace` | `null` |
| `close_tab` | `tab` | `null` |
| `close_session` | `pane` | `null`; since 0.7, below |
| `start_agent` | `name`, `kind`, `pane`, `accept_trust_prompt`, `timeout_ms?`, `agent_args?` | `AgentInfo` |
| `prompt` | `agent`, `text` | `AgentInfo` (the state before the agent reacted) |
| `agent` | `agent` | `AgentInfo` |
| `list_agents` | — | `[AgentInfo]` |
| `wait` | `agent`, `until` (status strings), `timeout_ms` (capped at 120000) | `AgentInfo` |
| `read` | `agent`, `lines` (capped at 2000), `color` (optional, default false) | string; with `color`, colours and attributes come as SGR codes (`ESC [ … m`) and no other escapes, for a page showing the screen |
| `send_keys` | `agent`, `keys` | `null` |
| `update` | `what` (`box` or `herdr`), `handoff?` (herdr only, default false: restart herdr's server on the new version, the sessions kept, best effort; without it the running server stays as it is) | `{"what", "from", "to", "install", "restart"}`; since 0.5 |

An unknown call answers `bad_call`, so calls can be added behind a protocol bump without breaking older boxes. There is no other way for the cloud to act on the box.

### `close_session`

Stops a session when the person asks, including one they started by hand in herdr, which the cloud knows only by its pane. The box asks herdr which tab holds `pane` (`pane get`) and closes that tab (`tab close`), with everything running in it. When it was the only tab of its workspace, herdr closes the workspace too, as it does whenever the last tab goes. Other tabs and workspaces are left alone.

- `api` with code `not_found`: herdr has no pane with that id (or it went away before its tab closed).
- Any other herdr error keeps herdr's own code, as for every call.

### `update`

Runs the update the way the thing was installed, exactly as `pitupitu update [box|herdr] --yes` does at the keyboard, and only because the cloud asked on the person's word; the box never starts one itself.

- `box`: `brew upgrade pitupitu` under a Cellar; otherwise the release tarball for this machine, checked against its `.sha256`, put in place of the running binary. The reply goes out first; then the service restarts, the link drops, and the next `hello` carries the new `version`. `restart` is `done` when a service runs the box agent, `needed` when there is none (a `pitupitu connect` started by hand).
- `herdr`: `brew upgrade herdr` under a Cellar, `herdr update` (`--handoff` when asked) for herdr's own installer. Afterwards `herdr status` is read again and an `inventory` note follows. `restart` is `needed` when the running server is older than the new binary, `done` after a handoff, `none` otherwise.
- Errors are `api` with a code: `update_failed` (it ran and failed; the message is the command's last line), `update_unavailable` (herdr installed some other way, herdr not installed, no release binary for this machine; the message says what to do instead), `nothing_to_update` (already the newest).

## Notes from the box

- `home`: `{"note": "home", "files": [{"path": "skills/deploy/SKILL.md", "text": "…"}]}`. Sent after `hello` and within about a second of any change. Only `skills/*/SKILL.md` from the home folder are ever sent, each cut at 64 KB. (0.1 also sent `PITUPITU.md`; the person's preferences live in the cloud now.)
- `inventory`: `{"note": "inventory", "install": …, "latest": …, "herdr": …, "kinds": […], "available": […], "recent": […]}`, the fields of `hello` again, sent when any of them changed: herdr starts or stops answering, `herdr status` reads differently (looked at every minute and after an update), a newer release appears (asked every six hours), or `recent` or `available` changed (both looked at every five minutes; `available` also on every connect).
- `agents`: `{"note": "agents", "agents": [<AgentInfo>…]}`, herdr's full agent list, sent whenever the set of panes or any pane's `(agent_status, state_change_seq, completion_seq)` differs from the last list sent. The box looks once a second.

- `session` and `git`: the box's history, only while the cloud's `settings` say `history: true` (below).

## Notes from the cloud

- `settings`: `{"note": "settings", "history": true | false, "resend": true}` (`resend` optional), right after `hello` and whenever the person changes it. `history: true` means: read the agents' own session logs on this box and send `session` and `git` notes. `false` means: send nothing of the kind and remove the reader's state. The default is the cloud's, and it is on: a box that never switched history off is sent `true`. Until the note comes, the box reads nothing. `resend: true` (with `history: true`, after the person asked the cloud to rebuild what it knows) sends everything again from the start. The box keeps nothing about the setting itself; the cloud says it again on every link. Other fields are ignored, so a note with protocol 2's `driver` and no `history` is no setting at all.

No other note from the cloud is acted on.

## History

On unless the person switches it off for this box: `pitupitu box history off` or the page; `pitupitu box history on` switches it back on. `pitupitu setup` asks nothing about it; it says in one line what is read and links the privacy page. What the cloud already stored is deleted on pitupitu.chat.

### What is read

- Claude Code: `~/.claude/projects/*/*.jsonl`, one file per session. Entries of `type` `user` and `assistant` that are not a subagent's (`isSidechain` true) give the turns; each carries `sessionId`, `cwd` and `timestamp`. A user entry is what the person typed when its `message.content` is a string or `text` blocks, and it is not Claude Code's own (`isMeta`, an `origin` other than `human`, a command's output, a notification, an interruption; `<system-reminder>` parts are dropped). A slash command is sent as `/name args`, a shell command run with `!` as `! command` without its output. Assistant entries give `text` blocks and `tool_use` blocks (`name`, `input`); thinking is not read. The last `ai-title` entry gives the title.
- Codex: `~/.codex/sessions/YYYY/MM/DD/*.jsonl`. `session_meta` gives `payload.id`, `payload.cwd` and `payload.timestamp`; then `response_item` messages with role `user` (`input_text`, without the instructions and environment Codex adds as user messages) and `assistant` (`output_text`), `function_call` and `local_shell_call`, `custom_tool_call` `apply_patch` (only the first file it names), and finished command items (`event_msg` `item_completed`, `CommandExecution`). The same command logged twice in a row is one turn.
- The sessions of the last 7 days, at most the newest 300.

Never read, never sent: tool results and command output, file contents (an edit's old and new text, a written file, a patch's body), thinking, subagents' transcripts, anything else under `~/.claude` or `~/.codex`, credentials of any kind.

### `session`

```json
{"note": "session", "session": {
  "agent": "claude", "id": "<the agent's session id>", "cwd": "/home/me/code/app",
  "git": {"root": "/home/me/code/app", "remote": "git@github.com:me/app.git", "branch": "main"},
  "started_at": "2026-10-20T09:00:00.000Z", "last_at": "2026-10-20T09:40:00.000Z",
  "title": "Greeting in the README",
  "turns": [
    {"at": "…", "role": "you", "text": "Add a greeting to the README"},
    {"at": "…", "role": "agent", "text": "I'll read it first."},
    {"at": "…", "role": "tool", "name": "Edit", "target": "/home/me/code/app/README.md"},
    {"at": "…", "role": "tool", "name": "Bash", "target": "cargo test -p app"}
  ],
  "turns_total": 57, "live": false}}
```

- `agent`: `claude` or `codex`. `id`: the agent's own session id (the file name when the log has none). `cwd`: the session's folder, sent even when it no longer exists.
- `git`: the repository `cwd` is in (`git rev-parse --show-toplevel`, `remote get-url origin` without any user name or password in it, `rev-parse --abbrev-ref HEAD`; `branch` null when detached), looked up at most once a minute per folder; null when the folder is not in a repository.
- `started_at`, `last_at`: the first and last time the log records (RFC 3339 as the agent wrote it). `title`: Claude Code's title, else the first thing the person typed, cut to 80 characters; null when neither exists.
- `turns`, in order: `you` and `agent` carry `text`, a `you` turn's cut to 1000 characters and an `agent` turn's to 600 (then `…`); `tool` carries `name` and `target`: the file for file tools (`Edit`, `Write`, `Read`, `MultiEdit`, `NotebookEdit`, Codex's `apply_patch`), the start of the command for shell tools (`Bash`, Codex's commands, sent as `shell`), else the tool's first string argument that is not content; one line, at most 120 characters; null when there is none.
- Redaction: in every text, target, title and commit subject, anything that looks like a secret becomes `[redacted]`: `sk-…`, `ptp_…`, `ghp_…`, `gho_…` (and GitHub's other token prefixes), `xox…-`, `AKIA…`, `Bearer <token>`, the value of `password=`, `token=`, `key=` and the like, and runs of 32 or more base64 or hex characters with a 20-character stretch of letters and digits (so paths and UUIDs stay).
- Size: a record is at most 32 KB of JSON. When there is more, the oldest turns are left out, except the first `you` turn, which always stays; `turns_total` is how many turns there were.
- `live`: the log was written in the last 5 minutes.
- When: logs are looked at every 10 seconds (their size and time). A changed session's whole record is sent again, as a replacement, at most every 30 seconds; so is one that stopped being live. When history is switched on, or with `resend`, the sessions are sent oldest first. Each session is its own note, and the box sends no more than 10 notes a second.
- The box remembers in `history.json` in its state folder (`$XDG_STATE_HOME/pitupitu`, `~/.local/state/pitupitu` on Linux; `~/Library/Application Support/pitupitu` on macOS) the size and time of each log and each repository's HEAD as last sent, so a restart sends only what changed. The file is removed when history is switched off; `resend` starts it over.

### `git`

```json
{"note": "git", "repo": {
  "root": "/home/me/code/app", "remote": "git@github.com:me/app.git", "branch": "main",
  "head": "<sha>",
  "commits": [{"sha": "<sha>", "at": "2026-10-20T11:00:00+02:00", "subject": "Add a greeting", "files": 3, "branch": "main"}]}}
```

For the repository of every session of the last 7 days: `git rev-parse HEAD` and the branch, looked at every minute and when a new repository turns up. When either changed since the last note (or there was none), the note is sent with the current branch's commits of the last 7 days, newest first, at most 50 (`git log --since=7.days --max-count=50 --shortstat`): the hash, the committer's time, the subject only (redacted) and how many files it changed (0 for a merge). No diffs, no file contents, no names or emails. `git` runs with a 10-second timeout, never prompts, takes no locks and does not run a file-system monitor.

Nothing is buffered on the box while the link is down; the `hello` after a reconnect carries a fresh agent list.

## herdr on the box

What the states, counters and error codes mean is in [`sessions.md`](sessions.md).

- **Your own agents, unmodified.** herdr starts them (`herdr agent start --kind`) from your `PATH`, as you, signed in by you, in your own permission mode. The box never reads, copies or passes on their credentials and adds no permission flags. The folder trust prompt is answered only when the cloud asks (`accept_trust_prompt`), with the option that keeps approvals on.
- **The server.** herdr starts no server by itself: with none running, every call fails with `server_not_running`. The box starts `herdr server` before `hello` and whenever its once-a-second `agent list` finds none running, detached (a session and process group of its own, no terminal, not part of the user service), with a pause between tries that grows from 5 s to 5 min. It never stops a server. A herdr you opened is used as it is; one the box started is what `herdr` attaches to later. Everything is in herdr's default session.
- **Workspaces and tabs.** A place is a herdr workspace (`workspace create --cwd --label --no-focus`); a task in it is a tab. Scratch work goes into tabs of one `pitupitu` workspace (`create_workspace` with `scratch`). pitupitu closes only what it created, and a session the person asked it to stop (`close_session`).
- **Seeing a session.** `herdr` on that machine, or `herdr --remote <machine>` over SSH.
- **Installed and updated by pitupitu, when asked.** `install.sh` and `pitupitu setup` install herdr with its own installer when it is missing, and `setup` runs `herdr integration install` for Claude Code and Codex so herdr resumes their sessions after a restart. Updates run only through `pitupitu update herdr` or the `update` call.
