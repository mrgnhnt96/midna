---
name: midna
description: Drive midna, the AI-managed terminal you are running in, with the `midna` CLI or the `midna` MCP tools. Use it to see other terminals, open shells/monitors/agents the human can watch, get the human's attention, ask for approvals, add policy rules, draft webhook triggers, change settings and move windows. Use it whenever $MIDNA_SESSION is set or `TERM_PROGRAM=midna`.
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
2 bad arguments, 3 daemon unreachable.

## Orient yourself

```
midna info                 # your role (agent) and session
midna list                 # terminals: id, status, kind, name, project
midna projects             # projects (directories that group terminals)
midna projects discover    # folders under the projects.roots setting that can be opened as projects
midna needs                # what is waiting on the human right now
midna explain <id>         # why a terminal/rule/trigger/needs-you item is the way it is
```

## Terminals

- `midna open [--name N] [-- argv...]` opens a shell. Use `--monitor CMD` for a long-running process
  the human should see (a failure raises a needs-you item). Use `--agent claude|codex --prompt TEXT`
  to delegate durable work to a new agent the human can follow.
- `midna send <id> <text>` types text and presses Enter. Add `--no-enter` to skip Enter. In a
  Claude Code or Codex terminal that is a chat message (multi-line text stays one message).
  `--image PATH` (repeatable) attaches an image ahead of the text, e.g. a screenshot you saved:
  `midna send <id> --image /tmp/shot.png -- "Does this layout look right?"`.
  `midna key <id> ctrl-c` presses a key. `midna read <id> [--lines N | --screen]` reads the output.
- `midna rename`, `midna restart` and `midna close <id>` manage a terminal. Closing a busy
  terminal needs `--force` and may ask the human.
- `midna procs <id>` lists a terminal's processes and what its agent has in flight (background
  shells, subagents, scheduled wakeups). `midna restart <id>` reopens an agent in the same
  conversation; it refuses while background work would be lost, so use `--idle` to queue it until
  the agent is idle with nothing in flight (`--cancel` drops it, `--fresh` starts over).
- Prefer opening your own terminal over typing into one someone else is using. Never answer
  another agent's permission prompt unless `midna read --screen` shows that prompt.

## Attention etiquette

- `midna attention "<one line>"` = you are **blocked** and cannot continue without the human.
- `midna attention --note "<one line>"` = FYI, no action needed.
- Keep it to one short line and put details in `--detail`. Don't raise one per step, and don't
  raise one for something you can decide yourself. Check `midna needs` first so you don't duplicate.
- Before you speak up, `midna list` shows which terminals are busy.

## Approvals and policy

- Rules decide `allow` / `ask` / `deny` for `command`, `tool`, `path`, `cli` and `window` actions.
  The narrowest scope wins (session > project > global). Within a scope, deny > ask > allow.
- `midna check <kind> <value>` tests an action without side effects and shows the trace.
- An `ask` decision raises an approval for the human and waits for the answer. Don't approve your
  own requests: agents may approve only when the human turned on `approve.from_cli`, and only for
  their own session.

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

## Human-only, and what to do instead

| You want to | Do this instead |
|---|---|
| remove a rule | `midna rules request-removal <id> --reason ...` |
| set a webhook secret / enable a trigger | `midna triggers enable <id>`, which asks the human |
| change a human-only setting | `midna settings set ...`, which asks the human |
| remove a project, stop, upgrade or reset the daemon, configure webhooks, install an app update | call it; it becomes a needs-you approval |
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
