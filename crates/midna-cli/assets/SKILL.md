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
- Add `--background` to `open` for something that should run out of the way (a dev server, a
  watcher). It sits in the sidebar's folded Background group but is still listed, readable and
  able to raise needs-you. `midna background <id> [--off]` moves an existing terminal.
- `midna send <id> <text>` types text and presses Enter. Add `--no-enter` to skip Enter. In a
  Claude Code or Codex terminal that is a chat message (multi-line text stays one message).
  `--image PATH` (repeatable) attaches an image ahead of the text, e.g. a screenshot you saved:
  `midna send <id> --image /tmp/shot.png -- "Does this layout look right?"`.
  `midna key <id> ctrl-c` presses a key. `midna read <id> [--lines N | --screen]` reads the output.
- `midna prompts <id>` lists the prompts the human sent an agent terminal (numbered, ▸ = where its
  view is). `midna prompts <id> --jump N|prev|next|latest|live` scrolls the agent's own view there,
  e.g. to show the human the answer to an earlier question.
- `midna rename`, `midna restart` and `midna close <id>` manage a terminal. Closing a busy
  terminal needs `--force` and may ask the human.
- `midna procs <id>` lists a terminal's processes and what its agent has in flight (background
  shells, subagents, scheduled wakeups). `midna restart <id>` reopens an agent in the same
  conversation; it refuses while background work would be lost, so use `--idle` to queue it until
  the agent is idle with nothing in flight (`--cancel` drops it, `--fresh` starts over).
- `midna subagent <terminal-id> <agent-id>` reads one of a terminal's subagents (its prompt, tool
  calls and replies) from its transcript; the ids are in `midna procs`.
- `midna links` lists the links, PRs, artifacts and files that came up in your conversation;
  the human sees them under the header's links button (⌘L). Pin what they will want to come back
  to (`midna links pin <id|url>`: the PR you opened, the design you published, the doc they sent),
  and `midna links add <url|path> --title T --why "note"` for something important that never
  showed up as a link. Don't pin everything: a few pins are useful, twenty aren't.
  Links remember the prompts they came up in: `midna links --kind file --turn last` lists the
  files edited since the human's last prompt (`--turn N` for prompt N of `midna prompts`).
- `midna usage` shows Claude's plan usage (5-hour and weekly windows, account-wide) and whether
  it is limited until a reset. Check it before starting long work for someone else.
- Prefer opening your own terminal over typing into one someone else is using. Never answer
  another agent's permission prompt unless `midna read --screen` shows that prompt.

## Attention etiquette

- `midna attention "<one line>"` = you are **blocked** and cannot continue without the human.
- `midna attention --note "<one line>"` = FYI, no action needed.
- Keep it to one short line and put details in `--detail`. Don't raise one per step, and don't
  raise one for something you can decide yourself. Check `midna needs` first so you don't duplicate.
- Before you speak up, `midna list` shows which terminals are busy.
- `midna notify send "<title>" --detail "<line>"` posts a macOS notification (click = your
  terminal). Use it when the human asked to be told ("ping me when CI is green") or a result
  needs them while they may be away. Approvals, failures and long finished turns already notify:
  don't duplicate them, and don't send progress updates.
- Which notifications the human gets is a setting: `midna notify` shows them for your terminal,
  `midna notify set turn_done on` (or `--global`, or `midna notify mute`) changes them. Do it
  when they ask; don't turn off something they rely on.
- Sounds and images are theirs to pick. When they hand you one ("use ~/Downloads/ding.mp3 for
  approvals, quieter"), `midna notify import <file> --for approval` copies it in and uses it,
  `midna settings set notify.volume.approval 40` sets how loud, and `midna notify test approval`
  lets them hear it.

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

## Local triggers ("when X happens, do Y")

When the human says "whenever my agent …, do …", write a local trigger. It fires on this Mac
and acts on the terminal that fired. You may add, enable and pause local triggers yourself (no
approval) when the human asked for one: add `--enable` so it is on at once, or
`midna triggers enable|disable <id>` later. `--source local` is inferred for the
events below and for any local-only flag; passing it is always fine.

- **Event** (`--event`, globs ok): `hook.<HookEvent>` (the agent's hook, e.g. `hook.Stop`,
  `hook.UserPromptSubmit`, `hook.Notification`), a midna event kind (`agent.prompt_blocked`,
  `agent.turn_ended`, `agent.turn_started`, `session.status`, …; see `midna events`), `idle`, or
  `schedule`.
  `agent.prompt_blocked` fires when a hook refuses a prompt; its data is `{hook, message, prompt}`.
- **Filters**: `--session ID` (one terminal), `--in-project P`, `--for-agent claude|codex`,
  `--idle-for 55m` (implies `--event idle`: no turn started or ended for that long),
  `--cron '0 9 * * mon-fri'` (implies `--event schedule`; see below), and
  `--match path=glob` (repeatable; dotted path into the hook payload / event data, glob is
  case-insensitive, every entry must match).
- **Actions** (pick one):
  - `--send TEXT` (repeat for more steps; `--send-no-enter TEXT` types without Enter). Steps run
    in order, each waiting until the agent is ready again.
  - `--set-status LABEL --color C --base B [--clear-on prompt|turn|status|never] [--icon I]`.
    Colors: red, orange, amber, yellow, green, teal, blue, purple, pink, gray or `#rrggbb`. `base`
    (idle|working|needs_you|done|failed) is the built-in state underneath; it still drives
    sorting, notifications and Needs You. `clear_on` defaults to `prompt`.
  - `--clear-status`, or the usual `--attention MSG`, `--run CMD --project P`,
    `--agent claude|codex --prompt T --project P`.
  - `--notify TITLE [--notify-body BODY] [--silent]`: a macOS notification (category
    `from_trigger`, on by default, its own sound and mute switch); clicking it selects the
    terminal that fired. Works on webhook triggers too.
- **Templates** in sent text, messages and commands: `{{last_prompt}}` (the terminal's most recent
  prompt, in full), `{{event}}`, `{{session.id}}`, `{{session.name}}`, `{{session.project_id}}`,
  `{{session.agent}}`, `{{session.status}}`, and `{{data.<path>}}` or bare `{{<path>}}` (hook
  payload / event data, e.g. `{{message}}`). In `--run` each value is shell-quoted.
- `--cooldown 5m` (default 60s) is per terminal. Events a trigger causes never fire triggers, so
  a `--send` can't loop.
- Durations: `90s`, `55m`, `1h30m`. A bare number means minutes for `--idle-for`, seconds for
  `--cooldown`.
- Escape hatch: `--action-json '{"kind":"send_to_session","steps":[{"text":"/compact"}]}'` and
  `--filter-json '{"match":{"message":"*Compact first*"}}'` take the raw objects (`midna schema trigger.add`).
- Built in: "Prompt blocked" (`prompt_blocked_status`) shows a "Prompt blocked" status (needs_you) when
  a hook refuses a prompt. Edit, pause or remove it like any other trigger.
- `midna list` shows a custom status after the state (`needs_you · Prompt blocked`).
- Check one with `midna triggers test <id> --session <terminal> --payload '{"message":"…"}'`.
- **Schedules** (cron jobs): `--cron` takes `minute hour day-of-month month day-of-week` in local
  time (`*/30 * * * *`, `0 9 * * mon-fri`, `0 18 1 * *`) or `@hourly`, `@daily`, `@weekly`,
  `@monthly`, `@yearly`. With no `--session`/`--in-project`/`--for-agent` it fires once, about no
  terminal: use `--notify`, `--attention`, `--run CMD --project P` or
  `--agent claude --prompt T --project P` (a fresh agent each run). With one of those it acts on
  every running terminal that matches, so `--send` and `--set-status` work. Templates add
  `{{local_time}}` (`09:00`) and `{{scheduled_for}}`. Runs missed while the Mac slept fire late
  only within 10 minutes. `midna triggers show <id>` and `test` list the next runs.

Auto-compact when a hook blocks a prompt for context, then resend the prompt:

```
midna triggers add --name "Auto-compact" --event agent.prompt_blocked \
  --match 'message=*Compact first*' --send /compact --send '{{last_prompt}}' --enable
```

Keep a terminal's prompt cache warm while it sits idle:

```
midna triggers add --name "Keep cache warm" --idle-for 55m --session $MIDNA_SESSION \
  --send "Still there? Reply with one word." --enable
```

Every weekday at 9, start a Claude terminal that summarizes overnight PRs:

```
midna triggers add --name "Morning PRs" --cron '0 9 * * mon-fri' --project <project-id> \
  --agent claude --prompt "Summarize PRs opened since yesterday 9am" --enable
```

## Queued messages

`midna queue add <text>` leaves a message for a terminal (yours by default, `--session ID` for
another). midna types it, and presses Enter unless `--no-enter`, once it is first in line and the
agent is ready for input: not working or waiting on the human, no dialog on screen, nothing typed
in its input box. Then the next one goes. The human sees the queue on the terminal and can edit,
reorder or remove it.

- Use it for a follow-up you can't send mid-turn: queue `/compact`, then the next step, and both
  go in order once you stop. Or hand another terminal its next step without interrupting it.
- Sequence work across terminals with `--after ID`: the message waits until that terminal is idle
  with an empty queue (`midna queue add --session B --after A "Review what A just pushed"`).
- Other conditions: `--idle 10m|1h|90` (idle that long; bare number = minutes), `--at 18:00`
  (local, today or tomorrow) or `--at <RFC 3339>`. Default is as soon as the agent is ready.
- Text is the words after `add`, everything after `--`, or `-` for stdin. `--image PATH` attaches
  an image. `--first` or `--position N` puts it ahead of others (1 = next).
- `midna queue` lists it: state, when, who queued it, and what the first one is waiting for. A
  failed message holds the queue until `midna queue edit <id> --retry`, `send-now` or `rm`.
- `midna queue edit|rm|mv <id> <to>|send-now|clear|pause|resume` manage it. Positions are 1-based.
- Not for something to send right now (`midna send`) or a reaction that should repeat (a local
  trigger).

## Secrets (`[secret:NAME]`)

When the human pastes a secret into your terminal (a token, an API key, a `.env` block), midna
stores it in the Keychain and types `[secret:NAME]` in its place, so the value never enters your
context. Use it without seeing it:

- `midna secret exec NAME -- <command…>` runs the command with `$NAME` set. `VAR=NAME` sets
  `$VAR` instead (`midna secret exec GH_TOKEN=GITHUB_TOKEN -- gh api user`). Write `$NAME`
  inside the command (`-- sh -c 'curl -H "Authorization: Bearer $API_KEY" …'`), never the value.
  The value is masked as `‹NAME›` in the output, including base64 and URL-encoded forms.
- `midna secret write NAME .env [--as KEY]` sets it in a `.env`-style file for a dev server.
  Don't read that file back afterwards, because that would put the value in your context.
- `midna secret list` shows what's stored for this project.
- Never ask the human to paste a value into the chat. If you need a secret, ask them to paste it
  into the terminal; midna offers to store it.

You can save secrets too. When a command produces a token (a login, an API that issues keys),
pipe it straight into midna so it never enters your context:

```sh
gh auth token | midna secret save GH_TOKEN
curl -s … | jq -r .access_token | midna secret save API_TOKEN --label "Acme API token"
```

Then use `[secret:GH_TOKEN]` like any other. The value comes from stdin only, never as an
argument. A token you already saw (in an MCP tool result, say) is worth saving too, so it stops
spreading into commands and files. midna marks that one *exposed* so the human knows to rotate it.
A secret belongs to your terminal's project (`--global` for every project). Replacing a secret
the human stored asks them first. Only the human removes secrets.

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
