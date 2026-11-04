# Box link protocol

The box link is the one connection between this agent and pitupitu cloud. The box dials out; nothing connects in. This is protocol version **7** (the cloud also accepts 6).

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
{"note": "hello", "protocol": 8, "version": "0.11.0", "box": "my-laptop", "home": "/home/me/pitupitu",
 "agents": [<AgentInfo>…],
 "install": "installer",
 "latest": null,
 "herdr": {"version": "0.9.3", "protocol": 22, "running": true, "latest": "0.9.5",
           "restart_needed": false, "install": "installer"},
 "kinds": ["claude", "codex", "gemini", "agy", …],
 "available": [{"kind": "claude", "version": "2.3.1 (Claude Code)", "command": "claude",
                "signed_in": null, "models": null, "models_at": "2026-11-03T10:00:00Z", "models_error": null},
               {"kind": "kiro", "version": "kiro-cli 2.37.1", "command": "kiro-cli", "signed_in": true,
                "models": [{"id": "auto", "name": null, "provider": null, "note": "1.0x credits", "default": true}, …],
                "models_at": "2026-11-03T10:00:00Z", "models_error": null}],
 "recent": [{"path": "/home/me/code/app", "agent": "claude", "at": "2026-10-17T20:00:00Z"}]}
```

then a `home` note. Before `hello` the cloud sends nothing; right after it, the cloud sends `settings` (below).

- `install`: how this box agent was installed, so an update runs the same way: `brew` when the binary lives under a Homebrew `Cellar`, else `installer` (the release tarball, as `install.sh` and `pitupitu update` put it).
- `latest`: a newer release of the box agent than this one, as `"0.5.1"`, or null. The box asks GitHub (`klarluft/pitupitu` releases) when it starts and every six hours.
- `herdr`: null when herdr is not installed. `version` and `protocol` are the installed herdr's version and socket protocol (`herdr status`); `running` whether its server answers right now (the box starts one when it does not, below); `latest` a newer herdr release (`herdrdev/herdr` on GitHub, asked on the same schedule) or null; `restart_needed` whether the running server is older than the binary after an update (herdr says `restart_needed` or `server_binary_stale`, or the server reports another version than the binary; it keeps working, new server-side features wait for a restart); `install` is `brew` (under a Cellar), `installer` (where herdr's own installer puts it: `~/.local/bin`, `$HERDR_INSTALL_DIR`) or `other` (mise, Nix, a build of one's own), which the box does not update.
- `kinds`: the agent kinds herdr can start (`herdr agent`'s list). Empty while herdr does not answer. The cloud refuses a kind not in it before anything is opened (`sessions.md`, "Kinds").
- `available`: the coding agents ("harnesses") found on `PATH`, one per profile ("Harnesses", below) whose `command` answers `--version`, with the first line it printed, and what the harness's lister last said: `signed_in` (`null` when the harness cannot say), `models` (`null` when not listed: no lister, signed out, or the lister failed, `models_error` saying which, in plain words), and `models_at`, when the lister ran. The box never reads an agent's credentials to tell whether it is signed in; only a lister that asks the agent itself (Kiro CLI's `whoami`) says so.
- `recent`: up to 30 project folders, newest first, that still exist: the working directory of recent sessions in Claude Code's `~/.claude/projects/*/*.jsonl` and Codex's `~/.codex/sessions/**/*.jsonl`, and when the session file was last written. Only the folder path is read from those files.

Protocol 7 had the same `hello` with `available` holding `kind` and `version` only, for five kinds (`claude`, `codex`, `gemini`, `agy`, `opencode`), no `harnesses` note and no `refresh_models`. Protocol 6 had the same `hello` and every call but `last_message`. Protocol 5 (0.7 and 0.8) had neither `last_message` nor `focus_session`. Protocol 4 (0.6) had neither `focus_session` nor `close_session`. Protocol 3 (0.5) had the same `hello`; its cloud sent no `history` setting and the box sent no `session` or `git` notes. Protocol 2 (0.2 to 0.4) had `driver` (`herdr` or `terminal`, for the terminal host those versions had), `herdr` as a version string, and `latest` in `inventory` only. Protocol 1 (0.1) sent `hello` without `driver`, `available` and `recent`.

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
| `focus_session` | `pane` | `null`; protocol 6, below |
| `last_message` | `agent` | `{"text", "at", "turn", "cut"}`; protocol 7, below |
| `refresh_models` | `kind?` (every harness when absent) | `[Available]`, the entries refreshed: the listers run now (below). Errors: `api` with code `unknown_kind`. The box gives itself 15 s; protocol 8 |
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

### `focus_session`

Brings a session to the front in herdr on this machine, when the person asks for it ("show it on my desk"). The box focuses the pane itself (`agent focus`), which brings up its workspace and tab and, in a split tab, puts what the person types in that pane. When that fails, as for a pane with no agent, the box asks herdr where `pane` is (`pane get`) and focuses its workspace and its tab (`workspace focus`, `tab focus`), leaving the focus on whichever pane of the tab had it. Nothing is opened, closed or typed. It is the one call that moves the view: everything else pitupitu opens is created with `--no-focus`.

- Only a herdr window open on this machine (or attached with `herdr --remote`) shows the change. With none open, herdr's focus still moves, and the next `herdr` opened here starts on that tab. With several windows attached, which ones follow is herdr's to decide; herdr does not tell the box either way.
- herdr counts the focused pane as looked at, with or without a window attached: a turn that finishes there goes to `idle` without `done`, while its `completion_seq` still advances (`sessions.md`). The same was already true of whichever pane herdr had in front.
- `api` with code `not_found`: herdr has no pane with that id (or its tab or workspace went away on the way).
- Any other herdr error keeps herdr's own code, as for every call.

### `last_message`

The agent's whole last reply, read from its own session log, for when the end of the screen is not enough (a long report, an answer that scrolled away). Only while history is on for this box (below): the same switch, the same files, the same redaction.

```json
{"text": "# Findings\n\n…\n\nIn short: …", "at": "2026-10-20T09:40:00.000Z", "turn": 4, "cut": false}
```

- Which log: herdr's `agent get` for `agent` (a name or a pane, as for the other calls) carries `agent_session`, the agent's own session as herdr's integration reports it. By `id`: Claude Code's `~/.claude/projects/*/<id>.jsonl`, Codex's `~/.codex/sessions/YYYY/MM/DD/rollout-…-<id>.jsonl`, Kiro's `sess_<id>` folder, pi's `…_<id>.jsonl`, Qwen Code's `chats/<id>.jsonl`, Cline's `<id>.messages.json`, OpenCode's and Kilo Code's session `<id>` (of the logs under "What is read"). By `path`: only when the path is one of those files (pi's integration reports its path). Without a reported session: for Claude Code and Codex, no log (their integrations report it). For the others (herdr has no integration for Kiro and Cline; pi's, OpenCode's, Kilo Code's and Qwen Code's may not be installed), the log of that agent whose folder is the pane's folder and which was written in the last 10 minutes, but only when exactly one fits: a wrong log is worse than none. `agent_session` itself is not sent to the cloud, in this reply or in `AgentInfo`.
- `text`: the `text` blocks of the agent's answer to the person's last prompt (what counts as a prompt is what a `session` note counts as a `you` turn), in order, each trimmed, joined by a blank line. A reply interleaved with tool calls is several log entries; all of their text is in it. Never tool calls or their results, thinking, Claude Code's own additions, notifications or subagents' transcripts. While the agent has written nothing yet for the newest prompt, the reply before it is given. Redacted as `session` notes are.
- `cut`: the text was longer than 64 KB. It is then its first 16 KB, a line `[… N characters left out …]`, and its end, 64 KB in all; the end is kept because that is where a reply concludes.
- `at`: when the reply's last text block was written, as the log has it. `turn`: how many prompts the person had typed in that session by then (the first is 1), so the reply answers prompt `turn`.
- `api` with code `history_off`: history is off on this box, or the cloud has not said yet on this link. Nothing is read, herdr is not asked.
- `api` with code `no_log`: no log found: an agent whose logs the box does not read (any but those under "What is read"), herdr reports no session for the pane (for Claude Code and Codex: their integration is missing; `pitupitu setup` installs it) and none or several recent logs of the agent are in its folder, no such file, or nothing replied yet. The message says which. Cline's log is written at checkpoints, so its last reply can be an older one for a while.
- `agent_not_found` and other herdr codes as for every agent call. The box gives the call 8 seconds; the cloud should wait about as long.

### `update`

Runs the update the way the thing was installed, exactly as `pitupitu update [box|herdr] --yes` does at the keyboard, and only because the cloud asked on the person's word; the box never starts one itself.

- `box`: `brew upgrade pitupitu` under a Cellar; otherwise the release tarball for this machine, checked against its `.sha256`, put in place of the running binary. The reply goes out first; then the service restarts, the link drops, and the next `hello` carries the new `version`. `restart` is `done` when a service runs the box agent, `needed` when there is none (a `pitupitu connect` started by hand).
- `herdr`: `brew upgrade herdr` under a Cellar, `herdr update` (`--handoff` when asked) for herdr's own installer. Afterwards `herdr status` is read again and an `inventory` note follows. `restart` is `needed` when the running server is older than the new binary, `done` after a handoff, `none` otherwise.
- Errors are `api` with a code: `update_failed` (it ran and failed; the message is the command's last line), `update_unavailable` (herdr installed some other way, herdr not installed, no release binary for this machine; the message says what to do instead), `nothing_to_update` (already the newest).

## Notes from the box

- `home`: `{"note": "home", "files": [{"path": "skills/deploy/SKILL.md", "text": "…"}]}`. Sent after `hello` and within about a second of any change. Only `skills/*/SKILL.md` from the home folder are ever sent, each cut at 64 KB. (0.1 also sent `PITUPITU.md`; the person's preferences live in the cloud now.)
- `inventory`: `{"note": "inventory", "install": …, "latest": …, "herdr": …, "kinds": […], "available": […], "recent": […]}`, the fields of `hello` again, sent when any of them changed: herdr starts or stops answering, `herdr status` reads differently (looked at every minute and after an update), a newer release appears (asked every six hours), or `recent` or `available` changed (both looked at every five minutes; `available` also on every connect).
- `agents`: `{"note": "agents", "agents": [<AgentInfo>…]}`, herdr's full agent list, each entry with the `agent_session` herdr reports when that is an id (which of the agent's own sessions the pane holds, `{"agent", "kind": "id", "value"}`; a log's path is never sent), sent whenever the set of panes or any pane's `(agent_status, state_change_seq, completion_seq)` differs from the last list sent. The box looks once a second.

- `session` and `git`: the box's history, only while the cloud's `settings` say `history: true` (below). In a worktree, `git` and `repo` carry `main`, the repository's main folder. A session whose folder is gone keeps the `git` last seen for it, for as long as the history window. The `last_message` call answers only then too.

## Notes from the cloud

- `settings`: `{"note": "settings", "history": true | false, "resend": true}` (`resend` optional), right after `hello` and whenever the person changes it. `history: true` means: read the agents' own session logs on this box, send `session` and `git` notes, and answer `last_message`. `false` means: send nothing of the kind, refuse `last_message` (`history_off`) and remove the reader's state. The default is the cloud's, and it is on: a box that never switched history off is sent `true`. Until the note comes, the box reads nothing. `resend: true` (with `history: true`, after the person asked the cloud to rebuild what it knows) sends everything again from the start. The box keeps nothing about the setting itself; the cloud says it again on every link. Other fields are ignored, so a note with protocol 2's `driver` and no `history` is no setting at all.

- `harnesses`: `{"note": "harnesses", "revision": "…", "profiles": [<Profile>…]}`, right after `settings` and whenever the cloud's profiles change (protocol 8). The box uses them at once instead of the copy built into its release, keeps them for its next start, and runs the listers again (below).

No other note from the cloud is acted on.

## Harnesses

Every kind herdr can start is a harness the box looks for. What the box knows about each is a profile; the cloud sends the current ones, and the release carries a copy for when it has none:

```json
{"kind": "kiro", "name": "Kiro CLI", "command": "kiro-cli", "lister": "kiro", "integration": null,
 "screens": {"trust": ["Trust this workspace?"], "trust_yes": ["Trust, continue"],
             "approval": ["requires approval"], "login": ["let's get you signed in"],
             "model_unavailable": ["is not available"],
             "dismiss": [{"phrases": ["ClinePass"], "keys": ["esc"]}]}}
```

Fields the box does not use are ignored. What a profile can and cannot make the box do:

- `command`: looked for on `PATH` and run with `--version`, nothing else. Only a bare name (`^[a-z0-9][a-z0-9._-]{0,31}$`) is taken; a profile with a path is dropped.
- `lister`: which of the box's own listers applies, by name; an unknown name lists nothing. The listers are fixed in the box's code (`crates/pitupitu/src/harness.rs`), each a fixed command, run with nothing on standard input, no terminal, `NO_COLOR=1`, a 10 s limit and at most 1 MB of output, when the box starts, every six hours, after a `harnesses` note and on `refresh_models`:

  | lister | runs or reads | gives |
  |---|---|---|
  | `claude` | nothing (the cloud knows Claude Code's aliases and ids) | — |
  | `codex` | `codex debug models` | the catalog's ids and names, hidden ones left out |
  | `kiro` | `kiro-cli whoami`, then only when signed in `kiro-cli chat --list-models --format json` | `signed_in`; ids, credit rates as `note`, the default |
  | `opencode`, `kilo` | `opencode models`, `kilo models` | one `provider/model` per line |
  | `pi` | `pi --list-models` | the table's provider and model |
  | `agy` | `agy models` | ids and names |
  | `qwen_settings` | `~/.qwen/settings.json` | `modelProviders.*[].id` and `name`, and the default; nothing else in the file is read |
  | `cline_settings` | `~/.cline/data/settings/models.json` | `providers.*.models.*` ids; the provider settings and tokens are in other files, never read |

  A lister that would need a sign-in says `signed_in: false` and never starts one. At most 200 models per harness.
- `screens`: phrases that add to the box's own dialog detection (`sessions.md`, "Dialogs"), matched in the bottom 18 lines: `trust` a folder trust question, answered (when `start_agent` says `accept_trust_prompt`) by the option containing a `trust_yes` phrase, never one that also says "without asking", "don't ask", "auto-approve", "yolo", "bypass", "dangerously" or "always allow"; `approval` a question for the person; `login` not signed in; `dismiss` a first-run screen that is neither (an advert), closed during `start_agent` with Esc, the only key a profile can name. `model_unavailable` is the cloud's.
- `integration`: the name `pitupitu setup` passes to `herdr integration install` for a harness it finds, so herdr resumes its sessions after a restart and reports its session id.

## History

On unless the person switches it off for this box: `pitupitu box history off` or the page; `pitupitu box history on` switches it back on. `pitupitu setup` asks nothing about it; it says in one line what is read and links the privacy page. What the cloud already stored is deleted on pitupitu.chat.

### What is read

- Claude Code: `~/.claude/projects/*/*.jsonl`, one file per session. Entries of `type` `user` and `assistant` that are not a subagent's (`isSidechain` true) give the turns; each carries `sessionId`, `cwd` and `timestamp`. A user entry is what the person typed when its `message.content` is a string or `text` blocks, and it is not Claude Code's own (`isMeta`, an `origin` other than `human`, a command's output, a notification, an interruption; `<system-reminder>` parts are dropped). A slash command is sent as `/name args`, a shell command run with `!` as `! command` without its output. Assistant entries give `text` blocks and `tool_use` blocks (`name`, `input`); thinking is not read. The last `ai-title` entry gives the title.
- Codex: `~/.codex/sessions/YYYY/MM/DD/*.jsonl`. `session_meta` gives `payload.id`, `payload.cwd` and `payload.timestamp`; then `response_item` messages with role `user` (`input_text`, without the instructions and environment Codex adds as user messages) and `assistant` (`output_text`), `function_call` and `local_shell_call`, `custom_tool_call` `apply_patch` (only the first file it names), and finished command items (`event_msg` `item_completed`, `CommandExecution`). The same command logged twice in a row is one turn.
- Kiro CLI: `~/.kiro/sessions/<hash>/sess_<id>/messages.jsonl`, with `id`, `title` and the first of `workspacePaths` (the folder) from the `session.json` beside it. Records of `type` `user` (`content`), `assistant` with `operationType` `Say` (not `Reasoning`) and `tool_call` (`toolName`; the target is `filePath` or `args.path` for `kind` `edit` and `read`, `args.command` for `execute`); each carries `timestamp`. `tool_result`, approvals, `session_start` (the system prompt) and Kiro's other records are skipped. Not read: `~/.kiro/sessions/cli/*.history`, Kiro's settings and its `data.sqlite3`.
- pi: `~/.pi/agent/sessions/--<folder>--/<time>_<id>.jsonl`. The `session` header gives `id`, `cwd` and `timestamp`; then `message` entries with role `user` (a string or `text` blocks), `bashExecution` (sent as `! command`, without its output) and `assistant` (`text` and `toolCall` blocks with `name` and `arguments`; not `thinking`). `toolResult` and `system` messages and the other entry types are skipped. A branched session's entries are read in file order. Not read: `auth.json`, `models.json` and the rest of `~/.pi`.
- Qwen Code: `~/.qwen/projects/<folder>/chats/<id>.jsonl`. Records of `type` `user` with `provenance` `real_user` and `assistant`, each with `sessionId`, `cwd` and `timestamp`; their `message.parts`: `text` (not those with `thought` true) and the assistant's `functionCall` (`name`, `args`). `tool_result` and `system` records (telemetry, snapshots) and the background memory extractor's records (a `promptId` with `#managed`) are skipped. Not read: `settings.json`, `sessions/*.json`, `*.runtime.json`, credentials.
- Cline: `~/.cline/data/sessions/<id>/<id>.messages.json`, one JSON document Cline rewrites, read whole; its folder (`cwd`) and title (`metadata.title`) come from `<id>.json` beside it. Messages with role `user` give the `text` blocks wrapped in `<user_input …>` (unwrapped; other user text is Cline's own), role `assistant` the `text` and `tool_use` blocks (`run_commands`' first command, `editor`'s `path`); not `thinking`, not `tool_result`; `ts` (ms) is the time. Cline writes the file at checkpoints, so a session can show up later than it happened. Not read: `~/.cline/data/settings`, `globalState.json`, its databases.
- OpenCode and Kilo Code: `~/.local/share/opencode/opencode.db` and `~/.local/share/kilo/kilo.db`, SQLite, opened read-only, waiting at most 250 ms for a writer. Only the `session`, `message` and `part` tables are queried, never another (those databases also hold the accounts and credentials). Sessions without `parent_id` (not a subagent's) and not archived; `directory` is the folder, `title` the title (not OpenCode's placeholder `New session - …`). Parts in order: a user message's `text` parts, an assistant message's `text` parts and `tool` parts (`tool`, `state.input`); parts marked `synthetic` or `ignored`, `reasoning` and step parts are skipped, and a tool's output is not even taken out of the database. A session is named `<database>#<id>` in `history.json`. The database is queried again only when it or its WAL changed (size or time); a changed session is read whole.
- The sessions of the last 7 days, at most the newest 300.

Never read, never sent: tool results and command output, file contents (an edit's old and new text, a written file, a patch's body), thinking, subagents' transcripts, anything else in those agents' folders (settings, process registries, other tables of a database), credentials of any kind.

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

- `agent`: `claude`, `codex`, `kiro`, `pi`, `qwen`, `cline`, `opencode` or `kilo` (herdr's names). `id`: the agent's own session id (the file name when the log has none). `cwd`: the session's folder, sent even when it no longer exists.
- `git`: the repository `cwd` is in (`git rev-parse --show-toplevel`, `remote get-url origin` without any user name or password in it, `rev-parse --abbrev-ref HEAD`; `branch` null when detached), looked up at most once a minute per folder; null when the folder is not in a repository.
- `started_at`, `last_at`: the first and last time the log records (RFC 3339 as the agent wrote it). `title`: the agent's own title (Claude Code's, Kiro's, Cline's, OpenCode's and Kilo Code's), else the first thing the person typed, cut to 80 characters; null when neither exists.
- `turns`, in order: `you` and `agent` carry `text`, a `you` turn's cut to 1000 characters and an `agent` turn's to 600 (then `…`); `tool` carries `name` (the agent's own tool name) and `target`: the file for file tools (`Edit`, `Write`, `Read`, `MultiEdit`, `NotebookEdit`, Codex's `apply_patch`; for the other agents any `file_path` or `filePath` argument, and `path` for pi's `read`, `edit`, `write`, Cline's `editor` and Kiro's edit and read tools), the start of the command for shell tools (`Bash`, Codex's commands, sent as `shell`; for the other agents a `command` argument, Cline's first of `commands`), else the tool's first string argument that is not content; one line, at most 120 characters; null when there is none.
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
- **The server.** herdr starts no server by itself: with none running, every call fails with `server_not_running`. The box starts `herdr server` before `hello` and whenever its once-a-second `agent list` finds none running, detached (a session and process group of its own, no terminal, not part of the user service) and with the environment a terminal would give it (`terminal_env.rs`: the login shell's variables the box lacks, the login shell's `PATH` entries added to its own, on WSL the interop socket, distro name, Windows `PATH` and WSLg variables when missing; the service manager's and herdr's per-pane variables left out), with a pause between tries that grows from 5 s to 5 min. It never stops a server. A herdr you opened is used as it is; one the box started is what `herdr` attaches to later. Everything is in herdr's default session.
- **Workspaces and tabs.** A place is a herdr workspace (`workspace create --cwd --label --no-focus`); a task in it is a tab. Scratch work goes into tabs of one `pitupitu` workspace (`create_workspace` with `scratch`). pitupitu closes only what it created, and a session the person asked it to stop (`close_session`). It moves the focus only when the person asks to see a session (`focus_session`).
- **Seeing a session.** `herdr` on that machine, or `herdr --remote <machine>` over SSH.
- **Installed and updated by pitupitu, when asked.** `install.sh` and `pitupitu setup` install herdr with its own installer when it is missing, and `setup` runs `herdr integration install` for every harness found that herdr has an integration for (the profile's `integration`), so herdr resumes their sessions after a restart. Updates run only through `pitupitu update herdr` or the `update` call.
