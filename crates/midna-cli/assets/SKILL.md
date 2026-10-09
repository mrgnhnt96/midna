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
- `midna prompts <id>` lists the prompts the human sent an agent terminal (numbered, ▸ = where its
  view is). `midna prompts <id> --jump N|prev|next|latest|live` scrolls the agent's own view there,
  e.g. to show the human the answer to an earlier question.
- `midna rename`, `midna restart` and `midna close <id>` manage a terminal (a rename also stops midna
  naming it automatically from the agent's summary, `terminal.auto_name`). Closing a busy
  terminal needs `--force` and may ask the human (unless they turned on `agents.may_force_close`).
  If you shouldn't sit waiting for the answer, add `--no-wait`: it prints the needs-you id and exits 4
  at once, the close happens if the human approves, and `midna needs get <id>` (or
  `midna needs wait <id>`) tells you how it went. An approval about a terminal that closes in the
  meantime is withdrawn.
- `midna procs <id>` lists a terminal's processes and what its agent has in flight (background
  shells, subagents, scheduled wakeups). `midna restart <id>` reopens an agent in the same
  conversation; it refuses while background work would be lost, so use `--idle` to queue it until
  the agent is idle with nothing in flight (`--cancel` drops it, `--fresh` starts over).
  `midna replace <id>` swaps a terminal for a brand-new one in the same place (new id, name and
  conversation; the old one closes) and prints the new id.
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
  `@monthly`, `@yearly`. For an interval that doesn't divide the hour or day use `@every 55m`
  (`1h30m`, `5h`): `*/55` means minutes 0 and 55, not every 55 minutes. `@every` runs one
  interval after it's added (or from `--starts`), then keeps that pace. With no
  `--session`/`--in-project`/`--for-agent` it fires once, about no terminal: use `--notify`,
  `--attention`, `--run CMD --project P` or `--agent claude --prompt T --project P` (a fresh
  agent each run). With one of those it acts on
  every running terminal that matches, so `--send` and `--set-status` work. Templates add
  `{{local_time}}` (`09:00`) and `{{scheduled_for}}`. Runs missed while the Mac slept fire late
  only within 10 minutes. `midna triggers show <id>` and `test` list the next runs.
  Narrow one with `--between 13:00-17:00` (local time of day; the end isn't included, and
  `22:00-06:00` wraps midnight), `--starts '2026-10-06 13:00'` / `--ends 2026-10-31` (local), and
  `--max-runs N` (`1` runs once; setting a new limit starts the count again). Every 5 minutes
  from 1 to 5 PM on weekdays until Friday:
  `--cron '*/5 * * * mon-fri' --between 13:00-17:00 --ends 2026-10-10`.
  An empty value clears one on update.

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

## Keeping the Mac awake

The human's Mac idle-sleeps after a minute. Keep-awake holds it awake during their work hours
while there is work (an agent working, queued input, an agent waiting to resume, a schedule
trigger or wakeup due), so your queued and scheduled work still runs when they step away. It only
stops idle sleep: the display sleeps, the screen locks, closing the lid still sleeps the Mac.
It is off until the human (or an agent they asked) turns it on.

- `midna keep-awake` says whether it is held now, why or why not, and when the hours open or close.
- `midna keep-awake on|off`, `hours 8am 6pm weekdays`, `day fri 9am-3pm`, `day sat off`,
  `today off|on|until 5pm|for 5h|clear` (`for 5h` / `until 1am` alone too), `mode with_work|always`, `battery 20`, `linger 5`.
- **Waking a sleeping Mac** (`keep_awake.wake`, `midna keep-awake wake on|off`): midnad asks
  macOS to wake the Mac 2 minutes before the next work due inside the hours (a schedule
  trigger, a message queued `--at` a time, an agent's wakeup) and holds it until that work runs,
  so a 7 AM trigger runs on a Mac that slept all night. It needs a one-time admin grant,
  `midna keep-awake wake setup` (human only: macOS asks for their password). A closed lid still
  keeps a laptop asleep. To start work at a time while they're away: a schedule trigger at that
  time, hours that include it, and wake on.
- The same through RPC (`midna call`, MCP `keep_awake_status` / `keep_awake_set`), the contract
  other tools build on:

```
keep_awake.status {}  ->
  { held, reason: work|always|disabled|outside_hours|day_off|today_off|battery_low|no_work|failed,
    line, window_open, next_on?, next_off? (RFC 3339), work: ["2 agents working", …],
    held_since?, battery?: {percent, on_ac, low}, schedule: "9 AM–6 PM weekdays; Fri 9 AM–3 PM",
    settings: {enabled, mode: with_work|always, start: "HH:MM", end: "HH:MM", days: ["mon", …],
               hours: {"fri": "09:00-15:00", "sat": "off", "sun": "all day"}, min_battery, linger_mins, wake},
    today?: {date: "YYYY-MM-DD", on, until?: "HH:MM", until_date?: "YYYY-MM-DD", line},
    wake: {ready, next? (RFC 3339), reason?: "Morning kickoff at 7 AM", line, error?} }

keep_awake.set { enabled? (alias on), mode?, start?, end?, days?, hours?, min_battery?, linger_mins?, wake?, today? }
  -> keep_awake.status
  start/end: 8am | 8:30 PM | 17:30 (end at or before start runs past midnight; equal = all day)
  days:      weekdays | weekends | daily | mon-fri | "mon,wed,fri" | ["sat","sun"]
  hours:     {"fri": "9am-3pm", "sat": "off", "sun": "all day", "mon": null}  merges by day
             (null = back to the schedule); a list or string of `day = hours` rules replaces them all
  today:     off | on | until 5pm | for 5h | {"on": true, "until": "17:00"} | {"on": true, "for": "5h"} | clear
             (off = the rest of today, on = until midnight; on until a time, even past midnight
             (`for 5h` at 8 PM, `until 1am`; 24 h at most), then the schedule again. Needs
             keep-awake enabled)

keep_awake.wake_setup { remove? }  -> keep_awake.status   (human only: installs or removes the grant)
```

Only the fields given change, and one bad field saves none (error -32602 names it). The settings
are also ordinary `keep_awake.*` settings. Event `keep_awake.changed` (data = the status) fires
when it is taken or released.

## Load, runaway work and worktrees

- `midna system` shows the load per core and the terminals using the most CPU. Before starting
  heavy work (a workspace build, many worktree subagents at once), check it; on a busy Mac run
  fewer at a time.
- Don't wait on a process with `until ! pgrep -f "<pattern>"; do sleep N; done`: your own loop's
  command line contains the pattern, so two such loops see each other and never end. Wait for
  your background task's completion notice, or on a file the command writes. midnad stops poll
  loops under agent terminals after `guard.loop_max_hours`.
- `midna system pause|resume|stop <terminal>` are human only (you get an approval); the app
  offers them when the Mac stays overloaded (`system.overloaded`).
- `midna daemon upgrade|restart` wait while the Mac is busy (error 7); `--force` is the human's call.
- `midna worktrees` lists the git worktrees of repos midna watches and why each is kept;
  idle ones (24h, nothing in them, no uncommitted changes) are removed by midnad, branches kept.
  Clean up the worktrees you create when you're done with them.
- When your terminal closes, midna cleans up after it (`cleanup.enabled`): a cheap headless model
  removes the linked worktree you worked in or made, the branches you made and their remote
  branches, once merged and with nothing uncommitted, plus the human's own `cleanup.items`.
  `midna cleanup preview` shows what closing this terminal would clean up; `midna cleanup runs`
  what earlier ones did. If the human asks you to clean up something every time, add it with
  `midna cleanup add "<instruction>"`. Don't rely on it for unmerged work: it keeps that.

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
