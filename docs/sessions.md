# Sessions: states, errors and the rules both drivers keep

A box serves the box link calls (`docs/box-link.md`) from herdr or from
its own terminal host. The cloud cannot tell which, so both must mean the
same thing by every state and every error. This page is that meaning. The
scenario in `crates/pitupitu/tests/conformance.rs` runs the same steps
against both drivers and fails on any difference.

## States

`agent_status` is one of five words:

| status | meaning |
|---|---|
| `working` | the agent is running a turn |
| `blocked` | a dialog waits for a person: an approval, a question, the folder trust prompt, a login screen |
| `done` | a turn finished and nobody has looked at it since |
| `idle` | ready for input; any finished turn has been seen |
| `unknown` | an agent is there but its state cannot be classified; this proves nothing, look at the screen |

`done` and `idle` both accept a prompt. What counts as "looked" differs:
herdr marks a session seen when it is focused in the TUI or addressed by
an explicit command; the terminal host marks it seen on `read` and on
`pitupitu attach`. So `done` turns into `idle` at different moments on the
two drivers, and the cloud never keys on `done` itself.

Two counters carry the meaning instead:

- `state_change_seq` advances on every status change. News is anything
  with a higher value than the last one reported on.
- `completion_seq` advances when a turn completes (a `Stop` hook, Codex's
  turn-complete notification, or herdr's detection). A turn is finished
  when this advanced, whatever the status says. `blocked` wins over a
  completion seen in the same look.

`launch_pending` and `interactive_ready` describe start-up only: a session
is silent until `interactive_ready`. `start_agent` returns only when the
agent is ready for a prompt, which means more than a reported `idle`:
right after start, and after the trust dialog when Claude Code reloads,
the status turns idle a moment before the agent reads keys, and a prompt
typed then is lost. Both drivers also require the screen to hold still
for about a second and a half.

`completion_seq` is absent until the first turn completes (herdr) or `0`
(the terminal host); a comparison treats absent as lower than any value.

## Prompts

`prompt` types the text and Enter and returns the state from *before* the
agent reacted. It does not prove the agent started a turn. Delivered means
`state_change_seq` advanced afterwards (the terminal host sets `working`
on Enter; herdr observes it). The cloud waits for that and treats no
change within a few seconds as a prompt that did not land; the box does
not retry on its own.

## Dialogs: idle is not permission

A reported `idle` is never taken as proof that nothing waits on screen.
Before typing a prompt, both drivers look at the bottom of the live screen
(`herdr::detect::dialog`) and refuse with `agent_blocked` when a dialog is
there, whatever the status says. A dialog is answered only with
`send_keys`, and only after the caller has read the screen; `send_keys`
itself is not guarded, because `esc` and `ctrl-c` must always work.

The folder trust prompt is the one dialog a driver answers itself, and
only when `start_agent` carries `accept_trust_prompt`: Claude Code's "Yes,
I trust this folder", Codex's "No, ask me to approve", never the option
that switches approvals off.

## Errors

A failed call answers `{"err": {"kind", "code", "message"}}`. `kind` says
who refused: `api` (the driver, with a `code` below), `not_ready` (an agent
did not reach a state that accepts prompts), `timeout` (the box gave up),
`io` (the driver could not be run or answered something unexpected),
`bad_call` (unknown call or bad arguments). The codes both drivers use,
spelled as herdr spells them:

| code | call | when |
|---|---|---|
| `agent_not_found` | any agent call | no live agent has that name or pane |
| `agent_blocked` | `prompt` | a dialog is on screen (see above) |
| `agent_not_ready` | `start_agent` | the agent never reached a prompt-accepting state; surfaced as kind `not_ready` with the screen in the message |
| `unsupported_agent_kind` | `create_workspace`, `start_agent` | the driver that would serve the session cannot start that kind |
| `agent_name_taken` | `start_agent` | the name is in use |
| `pane_not_found`, `pane_busy` | `start_agent` | no such pane, or it already runs an agent |
| `workspace_not_found`, `tab_not_found` | `create_tab`, `close_*` | no such workspace or tab |
| `no_such_directory` | `create_workspace`, `create_tab` | the folder is not on this box |
| `bad_key` | `send_keys` | not a key name |
| `driver_unavailable` | `create_workspace` | the box is pinned to a driver that does not answer |

Other herdr codes pass through unchanged. The cloud handles a code it does
not know by its `kind`, never by its message text.

## Kinds

`hello` and `inventory` carry `kinds`: the agent kinds the box can start
right now, from the driver that would take a new session (herdr's list
when herdr serves, else `claude` and `codex`). `create_workspace` takes an
optional `kind`, the agent the cloud means to start there, and refuses an
unsupported one before anything is opened. `start_agent` checks again.

## Ids and endings

Ids are opaque strings. Terminal host ids start with `tw`; herdr's do not.
The cloud stores and echoes them and never parses them.

An agent that exits leaves the agent list; the cloud reports that as the
session ending, with what it last did. After a herdr restart, herdr
restores its layout and resumes supported sessions. Terminal host sessions
end with their holder and are not restored.

## Versions

`hello` carries the box version, `inventory` the newest release known
(`latest`, checked against GitHub a few times a day). The box never updates
itself: `pitupitu update` shows what is running, asks, and only then
replaces the binary and restarts the box agent. Sessions are not touched:
herdr is a separate program and the terminal host's holder is a separate
process.
