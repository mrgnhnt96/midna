---
name: midna
description: Drive midna, the AI-managed terminal you are running in, with the `midna` CLI or the `midna` MCP tools. Use it to see other terminals, open shells/monitors/agents the human can watch, get the human's attention, ask for approvals, add policy rules and triggers ("when X happens in a terminal, do Y"), change settings and move windows. Use it whenever $MIDNA_SESSION is set or `TERM_PROGRAM=midna`.
---

# midna

You are inside a midna terminal when `MIDNA_SESSION` is set. A daemon (`midnad`) owns every
terminal, rule, trigger and setting, and a human watches through the GUI. Almost everything is
yours to manage. A few things are the human's alone, and those become *requests* the human sees.

Three ways in, all equivalent (same daemon, same checks, same audit log):

- CLI: `midna <verb>`. `midna <verb> --help` has its flags and examples; this guide says when to
  use it. `midna help` lists the verbs, `midna capabilities` is a short overview.
- MCP: the `midna` server's tools (`session_open`, `rule_add`, ...), plus `capabilities`,
  `explain` and `guide`.
- Raw RPC: `midna call <method> '<json>'`. `midna schema <method>` shows the params,
  `midna schema --list` every method.

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
midna explain <topic>      # status, rules, approvals, needs-you, settings, windows, human-only...
midna events --follow      # everything that happens, live
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

- `triggers`: the human says "whenever X, do Y", wants something on a schedule, or wants a webhook
  to start work.
- `keep-awake`: work has to run while the human is away or the Mac would sleep.
- `load`: before heavy work, when a command is held because the Mac is busy, before you wait on a
  process, and for worktrees and what midna cleans up when a terminal closes.
- `secrets`: you see `[secret:NAME]`, a command needs a token, or a command produced one.
- `queue`: a follow-up you can't send mid-turn, or work sequenced across terminals.
- `terminals`: an agent's prompts, processes and subagents, `restart --idle`, `replace`, plan
  usage, Claude's question picker, linking a terminal for the human.
- `links`: pinning links for the human, link patterns, hover cards and link menus.
- `nearby`: asking an agent on another of the human's Macs for help, and waiting for its answer.

## Terminals

- `midna open` opens a shell. `--monitor CMD` is for a long-running process the human should see
  (a failure raises a needs-you item). `--agent claude|codex --prompt TEXT` delegates durable work
  to a new agent the human can follow. `--background` keeps a dev server or watcher out of the
  way; `--close-on-exit` suits a terminal that does one job.
- `midna send` types text and presses Enter (in an agent's terminal, a chat message), `midna key`
  presses a key, `midna read` reads the output. `rename`, `restart` and `close` manage a terminal.
- Prefer opening your own terminal over typing into one someone else is using. Never answer
  another agent's permission prompt unless `midna read --screen` shows that prompt.
- Don't type into a new agent's "do you trust this folder?" dialog: it's a needs-you item for the
  human.

## Attention etiquette

- `midna attention "<one line>"` = you are **blocked** and cannot continue without the human.
- `midna attention --note "<one line>"` = FYI, no action needed.
- Keep it to one short line and put details in `--detail`. Don't raise one per step, and don't
  raise one for something you can decide yourself. Check `midna needs` first so you don't duplicate.
- Your next prompt closes your own `attention` items. If it didn't unblock you, raise a new one.
- Before you speak up, `midna list` shows which terminals are busy.
- `midna notify send "<title>" --detail "<line>"` posts a macOS notification. Use it when the human
  asked to be told ("ping me when CI is green") or a result needs them while they may be away.
  Approvals, failures and long finished turns already notify: don't duplicate them, and don't send
  progress updates. Buttons, which notifications they get, sounds and text are in `midna notify --help`.

## Rules, settings, windows

- Rules decide `allow` / `ask` / `deny` for `command`, `tool`, `path`, `cli` and `window` actions.
  `midna check <kind> <value>` tests an action without side effects; `midna rules add` adds a rule.
- `midna settings list` marks what is human only; `midna settings set <key> <value>` changes a key.
- `midna focus <id>`, `midna window open_screen <screen>` and `midna window split <id>` show the
  human something. Other window moves need a human-only setting.
- `midna commands add` puts something the human repeats in their ⌘K palette.

## Human-only, and what to do instead

| You want to | Do this instead |
|---|---|
| remove a rule | `midna rules request-removal <id> --reason ...` |
| see a secret's value | you don't; use `[secret:NAME]` with `midna secret exec` |
| replace a secret the human stored | `<command> \| midna secret save NAME`, which asks the human |
| set a webhook secret / enable a webhook trigger | `midna triggers enable <id>`, which asks the human (local triggers you may enable) |
| change a human-only setting | `midna settings set ...`, which asks the human |
| remove a project, stop, upgrade or reset the daemon, configure webhooks, install an app update or hooks | call it; it becomes a needs-you approval (a project midna created for your `open --cwd` you may remove yourself once its terminals are closed) |
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
