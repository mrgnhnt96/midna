---
name: midna
description: Drive midna, the AI-managed terminal you are running in, with the `midna` CLI or the `midna` MCP tools. Use it to see other terminals, open shells/monitors/agents the human can watch, get the human's attention, ask for approvals, add policy rules, draft webhook triggers, add local triggers ("when X happens in a terminal, do Y"), change settings and move windows. Use it whenever $MIDNA_SESSION is set or `TERM_PROGRAM=midna`.
---

# midna

You are inside a midna terminal when `MIDNA_SESSION` is set. midna is a terminal for AI agents:
a daemon (`midnad`) owns every terminal, rule, trigger and setting, and a human watches through
the GUI. Almost everything is yours to manage. A few things are the human's alone, and those
become *requests* the human sees.

Three ways in, all equivalent (same daemon, same checks, same audit log):

- CLI: `midna <verb>`. Run `midna help`, `midna <verb> --help` or `midna capabilities`.
- MCP: the `midna` server's tools (`session_open`, `rule_add`, ...), plus `capabilities`,
  `explain` and `guide`.
- Raw RPC: `midna call <method> '<json>'`. `midna schema <method>` shows the params.

Add `--json` to any verb for machine-readable output. Exit codes: 0 ok, 1 refused/failed,
2 bad arguments, 3 daemon unreachable, 4 waiting on the human (`--no-wait`).

## Orient yourself

```
midna info                 # your role (agent) and session
midna list                 # terminals: id, status, kind, name, project
midna get <id>             # one terminal: status and reason, cwd, command, agent info
midna projects             # projects (directories that group terminals)
midna projects discover    # folders under the projects.roots setting that can be opened as projects
midna needs                # what is waiting on the human right now
midna explain <id>         # why a terminal/rule/trigger/needs-you item is the way it is
```

## Script it

When a job takes several calls, do them in one shell command instead of one MCP call each: list,
filter with `jq`, act. `--json` output is stable, and `midna call` reaches any method, even one
with no verb.

```
# the screen of every idle agent
for id in $(midna list --json | jq -r '.[] | select(.kind == "agent" and .status.state == "idle") | .id'); do
  echo "== $id"; midna read "$id" --screen
done

# a method with no verb: look up its params, then call it
midna schema session.list | jq .params.properties
midna call session.list '{"project_id":"midna"}' | jq -r '.[].name'

# ask without blocking (the needs-you id is printed, exit 4), then wait for the human's answer
id=$(midna close "$t" --force --no-wait); [ $? -eq 4 ] && midna needs wait "$id" --timeout 600
```

## Topics

The rest of the guide comes on request. Read one with `midna skill <topic>`, the `guide` tool's
`topic`, or `midna://skill/<topic>`; the files sit beside `$MIDNA_SKILL`.

- `triggers`: the human says "whenever X, do Y" or wants something on a schedule (local triggers).
- `keep-awake`: work has to run while the human is away or the Mac would sleep.
- `load`: before heavy work, when a command is held because the Mac is busy, before you wait on a
  process, and for worktrees and what midna cleans up when a terminal closes.
- `secrets`: you see `[secret:NAME]`, a command needs a token, or a command produced one.
- `queue`: a follow-up you can't send mid-turn, or work sequenced across terminals.
- `terminals`: an agent's prompts, processes and subagents, `restart --idle`, `replace`, plan
  usage, Claude's question picker.
- `links`: pinning links for the human, link patterns, hover cards and link menus.

## Terminals

- `midna open [--name N] [-- argv...]` opens a shell. Use `--monitor CMD` for a long-running process
  the human should see (a failure raises a needs-you item). Use `--agent claude|codex --prompt TEXT`
  to delegate durable work to a new agent the human can follow. `--resume ID` reopens a
  conversation, and words after `--` are the agent's own arguments
  (`midna open --agent claude --resume ID -- --append-system-prompt "…" --settings s.json`).
  midna merges a `--settings` / `--append-system-prompt` with its own, so its hooks still run.
- Add `--background` to `open` for something that should run out of the way (a dev server, a
  watcher). It sits in the sidebar's folded Background group but is still listed, readable and
  able to raise needs-you. `midna background <id> [--off]` moves an existing terminal.
- Add `--close-on-exit` to `open` (`session.open {close_on_exit: true}`) for a terminal that does
  one job: it closes when its agent or command exits normally, so you don't have to watch for
  `exited`. A failed exit stays open with its needs-you item.
- `midna send <id> <text>` types text and presses Enter. Add `--no-enter` to skip Enter. In a
  Claude Code or Codex terminal that is a chat message (multi-line text stays one message).
  `--image PATH` (repeatable) attaches an image ahead of the text, e.g. a screenshot you saved:
  `midna send <id> --image /tmp/shot.png -- "Does this layout look right?"`.
  `midna key <id> ctrl-c` presses a key. `midna read <id> [--lines N | --screen]` reads the output.
- `midna rename`, `midna restart` and `midna close <id>` manage a terminal (a rename also stops midna
  naming it automatically from the agent's summary, `terminal.auto_name`). Closing a busy
  terminal needs `--force` and may ask the human (unless they turned on `agents.may_force_close`).
  If the human is looking at the terminal, `close` returns at once but the close waits on a
  5-second countdown they can stop (`session.close_kept`) or put off until they look away.
  If you shouldn't sit waiting for the answer, add `--no-wait`: it prints the needs-you id and exits 4
  at once, the close happens if the human approves, and `midna needs get <id>` (or
  `midna needs wait <id>`) tells you how it went. An approval about a terminal that closes in the
  meantime is withdrawn.
- When you tell the human about another terminal, name it by its id (`midna list`) or link it as
  `[name](midna://session/<id>)`: ⌘-clicking a terminal's id, an agent's conversation id (its
  first 8+ characters do) or a `midna://session/<id>` link brings that terminal to the front.
- Prefer opening your own terminal over typing into one someone else is using. Never answer
  another agent's permission prompt unless `midna read --screen` shows that prompt.

## Attention etiquette

- `midna attention "<one line>"` = you are **blocked** and cannot continue without the human.
- `midna attention --note "<one line>"` = FYI, no action needed.
- Keep it to one short line and put details in `--detail`. Don't raise one per step, and don't
  raise one for something you can decide yourself. Check `midna needs` first so you don't duplicate.
- Your next prompt closes your own `attention` items. If it didn't unblock you, raise a new one.
- Before you speak up, `midna list` shows which terminals are busy.
- `midna notify send "<title>" --detail "<line>"` posts a macOS notification (click = your
  terminal). Use it when the human asked to be told ("ping me when CI is green") or a result
  needs them while they may be away. Approvals, failures and long finished turns already notify:
  don't duplicate them, and don't send progress updates.
- `--action <label>` adds buttons (up to 6, 24 characters each). Ones sharing a first word
  (`Snooze 15 min`, `Snooze 1 hour`) or the text before `": "` (`Later: 1 hour`, `Later: tomorrow`)
  fold into one split button: its face is the first of them, the rest are in its menu. Put the
  usual pick first. The response is always the full label.
- Which notifications the human gets is a setting: `midna notify` shows them for your terminal,
  `midna notify set turn_done on` (or `--global`, or `midna notify mute`) changes them. Do it
  when they ask; don't turn off something they rely on.
- Sounds and images are theirs to pick. When they hand you one ("use ~/Downloads/ding.mp3 for
  approvals, quieter"), `midna notify import <file> --for approval` copies it in and uses it,
  `midna settings set notify.volume.approval 40` sets how loud, and `midna notify test approval`
  lets them hear it.
- A notification's title and text are templates too: `midna settings set notify.body.turn_done
  "{{elapsed}} · {{reply}}"`, `notify.title.<kind>` likewise (empty = midna's own; a body of
  `none` shows the title only; a title can't be empty). Each kind has
  {{text}} and {{heading}} (midna's own body and title), {{project}}, {{session.name}} and its own variables (in the setting's
  description, `midna settings list --json`); `midna notify test <kind>` shows it with sample values.

## Approvals and policy

- Rules decide `allow` / `ask` / `deny` for `command`, `tool`, `path`, `cli` and `window` actions.
  The narrowest scope wins (session > project > global). Within a scope, deny > ask > allow.
- `midna check <kind> <value>` tests an action without side effects and shows the trace.
- An `ask` decision raises an approval for the human and waits for the answer. Don't approve your
  own requests: agents may approve only when the human turned on `approve.from_cli`, and only for
  their own session. `--no-wait` on any verb returns the needs-you id instead of waiting.
- A new Claude or Codex in a folder it hasn't seen asks "do you trust this folder?" before it starts;
  that shows as a needs-you item (its terminal is needs_you) the human answers. Don't type into it.
  Folders the human listed in `agents.trust_folders` (human only) are answered Yes automatically.

## Rules: agents add, humans remove

- `midna rules add <allow|ask|deny> <kind> <pattern> [--scope global|project:ID|session:ID] [--expires SECS]`
  Patterns are globs, e.g. `git push --force*` or `Bash(rm -rf*)`.
- You can't remove a rule. Ask instead with `midna rules request-removal <id> --reason "..."`. The
  human sees it as a needs-you item and decides.

## Triggers (webhooks)

- `midna triggers add --name N --event pull_request.opened --repo owner/name --project P --agent claude --prompt "Review PR {{pr.number}}"`
  drafts a trigger. Other actions are `--run CMD` and `--attention MSG`.
- Agent-made triggers start as `needs_secret`. Only a human can paste the signing secret and enable
  the trigger. Tell the human; don't try to set the secret yourself.
- `midna triggers test <id> --payload file.json` dry-runs a trigger. `midna explain <trigger-id>`
  says what it does and what it is waiting for.

## Settings

- `midna settings list` shows every key, its value, its default and whether it is human only.
  `midna settings set <key> <value>` changes one.
- Human-only keys (for example `approve.from_cli`, `agents.may_move_windows` and `policy.default`)
  can be read but not written. Setting one asks the human instead.
- Shortcuts are `keys.*` settings. Where each one works is its when clause (a GPUI context
  predicate); `keys.when` overrides it, one `<keys.* key> = <when clause>` rule per shortcut
  (`*` = anywhere). Settings has no row for it, so it's yours to set when the human asks for a
  shortcut to work somewhere else. Setting it replaces the whole list, so `midna settings get
  keys.when` first; `midna explain keys.when` has the syntax. midnad rejects a clause that
  doesn't parse.

## Windows

- `midna focus <id>` and `midna window open_screen rules|triggers|insights|settings|needs_you` are
  always allowed.
- `midna window split <id> [side|stacked]` shows a terminal beside the selected one (always
  allowed; `midna window split close` folds it, the terminal keeps running).
- Moving, popping out, snapping, keeping on top or closing windows needs the human-only setting
  `agents.may_move_windows`.

## The human's palette, updates, permissions

- `midna commands add --title "Deploy staging" --rpc session.open --params '{"kind":"monitor","command":["./deploy.sh"]}'`
  adds a ⌘K command for something the human repeats; `midna commands list|remove <id>`.
- `midna updates status|check` reads or refreshes the app's updater; `install` asks the human.
- `midna permissions status` shows what midna can see; `midna permissions open accessibility`
  opens the pane, but only the human can grant it.
- `midna hooks status` says whether midna's hooks are in Claude Code's / Codex's global config
  (so a hand-typed `claude` in a midna terminal reports status). `midna hooks preview` shows the
  change; `install` / `uninstall` ask the human.

## Human-only, and what to do instead

| You want to | Do this instead |
|---|---|
| remove a rule | `midna rules request-removal <id> --reason ...` |
| see a secret's value | you don't; use `[secret:NAME]` with `midna secret exec` |
| replace a secret the human stored | `<command> \| midna secret save NAME`, which asks the human |
| set a webhook secret / enable a webhook trigger | `midna triggers enable <id>`, which asks the human (local triggers you may enable) |
| change a human-only setting | `midna settings set ...`, which asks the human |
| remove a project, stop, upgrade or reset the daemon, configure webhooks, install an app update | call it; it becomes a needs-you approval (a project midna created for your `open --cwd` you may remove yourself once its terminals are closed) |
| approve your own request | wait; the human answers it |
| grant macOS permissions | `midna attention` and say what you need |

When a call is refused (exit 1, error code 1 or 2), the message says what happened and what to
do next. A needs-you item may already exist, and its id is in the message.

## Never route around a denial

- A deny or a refusal is the human's answer. Don't retry the same thing another way: no other
  tool, no editing midna's state files, no calling midnad's socket directly, no asking another
  agent to do it.
- Don't edit `$MIDNA_HOME` (state.json, events.jsonl, hooks/) or your own hook settings.
- If you think a decision is wrong, say so in one line, using `midna attention` or your reply, and
  carry on with something else.

## Learn more

- `midna capabilities` prints a short overview.
- `midna schema [method]` prints the API; `midna schema --list` lists every method.
- `midna explain <id|method|setting|topic>` explains one thing (topics: status, rules, triggers,
  needs-you, approvals, settings, windows, human-only).
- `midna events --follow` streams everything that happens.
