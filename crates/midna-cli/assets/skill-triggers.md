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
    in order, each waiting until the agent is ready again. Steps never type over text in
    Claude's input box, except on `agent.prompt_blocked`: Claude puts the refused prompt back
    in the box, and midna clears it (ctrl-c) when the box holds exactly that prompt.
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
- Who did it: an event's `actor` is `{kind: trigger, name, trigger: <id>}` when a trigger acted on
  its own. While a headless `--run` command is still going, the midna calls it makes (`midna open
  --agent …`, `midna attention …`) get `{kind: trigger, name, trigger: <id>, via: "run"}`, with agent
  privileges; midna works this out from the process tree, never from `MIDNA_TRIGGER`. A run's own
  `--allowedTools` isn't widened: only the tools set on the trigger are.
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

## Webhook triggers

- `midna triggers add --name N --event pull_request.opened --repo owner/name --project P --agent claude --prompt "Review PR {{pr.number}}"`
  drafts a trigger. Other actions are `--run CMD` and `--attention MSG`.
- Agent-made webhook triggers start as `needs_secret`. Only a human can paste the signing secret
  and enable the trigger. Tell the human; don't try to set the secret yourself.
- `midna triggers test <id> --payload file.json` dry-runs a trigger. `midna explain <trigger-id>`
  says what it does and what it is waiting for.
