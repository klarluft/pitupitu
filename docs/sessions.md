# Sessions: states, errors and the rules the box keeps

A box serves the box link calls (`docs/box-link.md`) through herdr. This
page is what every state and every error means to the cloud. The scenario
in `crates/pitupitu/tests/conformance.rs` runs these steps against a live
herdr and a real Claude Code (`PITUPITU_LIVE_HERDR=1`) and fails on any
difference.

## States

`agent_status` is one of five words, herdr's own detection:

| status | meaning |
|---|---|
| `working` | the agent is running a turn |
| `blocked` | a dialog waits for a person: an approval, a question, the folder trust prompt, a login screen |
| `done` | a turn finished and nobody has looked at it since |
| `idle` | ready for input; any finished turn has been seen |
| `unknown` | an agent is there but its state cannot be classified; this proves nothing, look at the screen |

`done` and `idle` both accept a prompt. herdr marks a session seen when
it is focused in the TUI or addressed by an explicit command, so when
`done` turns into `idle` depends on what the person does at the desk; the
cloud never keys on `done` itself.

Two counters carry the meaning instead:

- `state_change_seq` advances on every status change. News is anything
  with a higher value than the last one reported on.
- `completion_seq` advances when a turn completes. A turn is finished
  when this advanced, whatever the status says. `blocked` wins over a
  completion seen in the same look.

`launch_pending` and `interactive_ready` describe start-up only: a session
is silent until `interactive_ready`. `start_agent` returns only when the
agent is ready for a prompt, which means more than a reported `idle`:
right after start, and after the trust dialog when Claude Code reloads,
the status turns idle a moment before the agent reads keys, and a prompt
typed then is lost. So the screen must also hold still for about a second
and a half.

`completion_seq` is absent until the first turn completes; a comparison
treats absent as lower than any value.

## Prompts

`prompt` types the text and Enter and returns the state from *before* the
agent reacted. It does not prove the agent started a turn. Delivered means
`state_change_seq` advanced afterwards. The cloud waits for that and
treats no change within a few seconds as a prompt that did not land; the
box does not retry on its own.

## Dialogs: idle is not permission

A reported `idle` is never taken as proof that nothing waits on screen.
Before typing a prompt, the box looks at the bottom of the live screen
(`herdr::detect::dialog`) and refuses with `agent_blocked` when a dialog is
there, whatever the status says. A dialog is answered only with
`send_keys`, and only after the caller has read the screen; `send_keys`
itself is not guarded, because `esc` and `ctrl-c` must always work.

The folder trust prompt is the one dialog the box answers itself, and
only when `start_agent` carries `accept_trust_prompt`: Claude Code's "Yes,
I trust this folder", Codex's "No, ask me to approve", never the option
that switches approvals off.

## Errors

A failed call answers `{"err": {"kind", "code", "message"}}`. `kind` says
who refused: `api` (herdr or the box, with a `code` below), `not_ready`
(an agent did not reach a state that accepts prompts), `timeout` (the box
gave up), `io` (herdr could not be run or answered something unexpected),
`bad_call` (unknown call or bad arguments). The codes, spelled as herdr
spells them:

| code | call | when |
|---|---|---|
| `server_not_running` | any | no herdr server answers; the box is starting one |
| `agent_not_found` | any agent call | no live agent has that name or pane |
| `agent_blocked` | `prompt` | a dialog is on screen (see above) |
| `agent_not_ready` | `start_agent` | the agent never reached a prompt-accepting state; surfaced as kind `not_ready` with the screen in the message |
| `unsupported_agent_kind` | `create_workspace`, `start_agent` | herdr cannot start that kind |
| `agent_name_taken` | `start_agent` | the name is in use |
| `pane_not_found`, `pane_busy` | `start_agent` | no such pane, or it already runs an agent |
| `workspace_not_found`, `tab_not_found` | `create_tab`, `close_*` | no such workspace or tab |
| `no_such_directory` | `create_workspace`, `create_tab` | the folder is not on this box |
| `bad_key` | `send_keys` | not a key name |
| `update_failed`, `update_unavailable`, `nothing_to_update` | `update` | the box's own (`box-link.md`, "`update`") |

Other herdr codes pass through unchanged. The cloud handles a code it does
not know by its `kind`, never by its message text.

## Kinds

`hello` and `inventory` carry `kinds`: the agent kinds herdr can start,
empty while it does not answer. `create_workspace` takes an optional
`kind`, the agent the cloud means to start there, and refuses an
unsupported one before anything is opened. `start_agent` checks again.

## Ids and endings

Ids are herdr's, opaque to the cloud: it stores and echoes them and never
parses them.

An agent that exits leaves the agent list; the cloud reports that as the
session ending, with what it last did. After a herdr restart, herdr
restores its layout and, with its integrations installed (`pitupitu
setup` does), resumes the agents' sessions.

## Versions

`hello` carries the box version and herdr's, and `inventory` follows when
either changes or a newer release appears (checked against GitHub a few
times a day). The box never updates itself or herdr unasked: `pitupitu
update [box|herdr|all]` and the `update` call show or say what changes,
and run only on the person's yes. Sessions are not touched: the box
agent's restart leaves herdr running, and `herdr update` leaves the
running server and its panes as they are until it restarts.
