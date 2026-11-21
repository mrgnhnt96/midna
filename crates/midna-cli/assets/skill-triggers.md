## Local triggers ("when X happens, do Y")

When the human says "whenever my agent …, do …", write a local trigger. It fires on this Mac
and acts on the terminal that fired (or on `--to`'s, for stream and schedule events). You may add, enable and pause local triggers yourself (no
approval) when the human asked for one: add `--enable` so it is on at once, or
`midna triggers enable|disable <id>` later. `--source local` is inferred for the
events below and for any local-only flag; passing it is always fine.

- **Event** (`--event`, globs ok): `hook.<HookEvent>` (the agent's hook, e.g. `hook.Stop`,
  `hook.UserPromptSubmit`, `hook.Notification`), a midna event kind (`agent.prompt_blocked`,
  `agent.turn_ended`, `agent.turn_started`, `session.status`, …; see `midna events`), `idle`,
  `schedule`, or `stream.<name>.<kind>` (see "Stream triggers" below).
  `agent.prompt_blocked` fires when a hook refuses a prompt; its data is `{hook, message, prompt}`.
- **Filters**: `--session ID` (one terminal), `--in-project P`, `--for-agent claude|codex`,
  `--idle-for 55m` (implies `--event idle`: no turn started or ended for that long),
  `--cron '0 9 * * mon-fri'` (implies `--event schedule`; see below), and
  `--match path=glob` (repeatable; dotted path into the hook payload / event data, glob is
  case-insensitive, every entry must match; the same path twice needs both globs, in any order).
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
- `--cooldown 5m` (default 60s) is per terminal: an event inside it is dropped, not queued.
  `--cooldown 0` fires on every event. Stream triggers count it per stream and default to 0.
  Events a trigger causes never fire triggers, so a `--send` can't loop.
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

## Stream triggers (events from a program on this Mac)

A stream hands midna events from a program on this Mac (a Slack client, a CI watcher, a script),
with no public URL: they go over midna's socket, and nothing listens on the network. Each one is
the midna event `stream.<name>.<kind>`, its data the JSON object the program sent. Local triggers
react to it like any other event. Use one when the human wants something outside midna to start
or nudge work.

- **Publish one**: `midna stream publish ci build_failed --id 4512 --data '{"branch":"master"}'`
  (`--data @file`, or `-`/left out for stdin). Names are 1–32 of `a-z 0-9 -`, kinds 1–64 of
  `a-z 0-9 _ . -`, the data a JSON object of at most 64 KB.
- **Keep a producer running**: `midna stream add <name> [--secret NAME]... [--cwd DIR] [--project P]
  -- <command…>`. midnad runs the command in a login shell's environment (so `node` is on the
  PATH), with stdin from `/dev/null` and `$MIDNA_STREAM` set to the name. It starts it again
  1s after it exits, doubling up to 5 minutes (1s again after a 10-minute run), and when midnad
  starts or the Mac wakes. A stream an agent adds or changes waits, paused, until the human
  approves the needs-you item it raises. Tell them it's waiting.
- **Producer protocol**: each stdout line that is a JSON object
  `{"kind":"message","id":"Ev123","data":{…}}` is published. `kind` is required, `id` (string or
  number) is optional and is what dedupes, and `data` left out is `{}`. Flush each line (Python:
  `print(…, flush=True)`). Other lines, and all of stderr, go to `$MIDNA_HOME/streams/<name>.log`
  (1 MB, one older file kept).
- **Secrets**: `--secret NAME` puts the stored secret in the command's environment as `$NAME`,
  read each time it starts: the project's secret (the `--project`, or the terminal or folder you
  ran `stream add` from), else a global one. Only the names are saved. The values are masked
  (`‹NAME›`) in its output and log.
- **Limits**, per stream: an `id` among its last 1000 in 24 hours is a duplicate and isn't
  published again. Over 60 events a minute (bursts of 20), or with 1000 waiting, a publish is
  dropped (`midna stream publish` exits 1). A stream added with `stream add` only takes its own
  producer's publishes and the human's; anyone may publish to a name that isn't added.
  `midna stream events <name>` lists the last 200 publishes with what midna did with each:
  `accepted`, `duplicate`, `rate_limited`, `too_large`, `busy` or `refused`.
- **Health**: `midna stream list` shows each stream's state (`starting`, `running`, `backoff`,
  `failing`, `paused`, `awaiting_approval`), restarts and last event. `failing` means its
  producer exited 5 times in 10 minutes. midna raises one `stream_failing` needs-you item and
  keeps trying, and the state clears after a 10-minute run, a pause or a remove.
  `midna explain stream <name>` says why, with the log's last lines.
- **Triggers on a stream**: `--event stream.slack.message` (globs: `stream.slack.*`). A stream
  event has no terminal, so `--send`, `--set-status` and `--clear-status` need `--to NAME|ID`, the
  terminal to act on. It is an exact terminal name, else a session id, looked up when the trigger
  fires (within `--in-project` when set; of several, the most recently active). When none is
  running, nothing happens and the delivery says why. `--notify` and `--attention` take `--to`
  too. `--session` and `--for-agent` are refused. Templates add `{{data.<path>}}` and
  `{{stream.name}}`, `{{stream.kind}}`, `{{stream.id}}`. There is no cooldown by default, so
  every message fires; `--cooldown 1m` fires at most once a minute per stream, and each event it
  holds back is a `cooldown` delivery in `midna triggers deliveries` that says so.
- `--match` takes the same path more than once to need several words in one field, in any
  order: `--match 'text=*failed*' --match 'text=*master*'`. Its paths start inside the data
  (`text`, not `data.text`).
- **Test it**: `midna triggers test <id> --stream slack --kind message --data '{"text":"…"}'`
  says whether the trigger would fire and what it would do. Nothing is published or run. To run
  a real event through the triggers again, `midna stream events slack` shows its seq, then
  `midna stream events slack --replay <seq|id>` (past dedupe and cooldown).

### Example: a Slack channel's messages into a terminal

`docs/examples/slack-stream/` in midna's repository holds a Slack Socket Mode client. It is one
file, needs Node 22 or later and no packages, and forwards each message in one channel as
`stream.slack.message`. CI bots that post an empty text with the words in attachments or blocks
are covered: `data.text` holds all of them as plain text. Its README has the details.

1. **The Slack app** (the human does this at api.slack.com/apps: Create New App, From scratch):
   turn Socket Mode on and make an app-level token with `connections:write` (`xapp-…`). Under
   Event Subscriptions, subscribe to the bot event `message.channels` (`message.groups` for a
   private channel). Under OAuth & Permissions add `channels:history` (`groups:history`), then
   install the app to get the bot token (`xoxb-…`). Invite the app into the channel
   (`/invite @app`). The channel ID (`C0…`) is at the bottom of the channel's details.
2. **The tokens**: ask the human to paste each one into this terminal (never into the chat) and
   store them as `SLACK_APP_TOKEN` and `SLACK_BOT_TOKEN`. `midna secret list` shows them.
3. **The script**:
   ```
   mkdir -p ~/.midna-streams && cd ~/.midna-streams && \
     for f in slack-stream.mjs bot-message.json; do curl -fsSLO \
       https://raw.githubusercontent.com/mrgnhnt96/midna/main/docs/examples/slack-stream/$f; done
   node --version   # v22 or later
   ```
4. **The stream**, from the same terminal, with the channel's ID:
   ```
   midna stream add slack --secret SLACK_APP_TOKEN --secret SLACK_BOT_TOKEN \
     -- env CHANNEL=C0123ABCD node ~/.midna-streams/slack-stream.mjs
   ```
   Once the human approves it, `midna stream list` shows it `running`, and its log says
   `connected`. The script exits, so the stream ends up `failing`, when a token is wrong or the
   app isn't in the channel.
5. **The trigger**: when a CI failure for `roon_mobile` on `master` is posted, type a line into
   the terminal named "build angel":
   ```
   midna triggers add --name "roon_mobile master failed" --event stream.slack.message \
     --match 'text=*failed to build on pipeline roon_mobile*' --match 'text=*master*' --to "build angel" \
     --send 'CI failed on master: {{data.text}}' --enable
   ```
   A bot that posts "building" and edits it to "failed" sends the edit as `message_changed`:
   use `--event 'stream.slack.message*'` to catch both.
6. **Check it**:
   ```
   node ~/.midna-streams/slack-stream.mjs --data < ~/.midna-streams/bot-message.json \
     | midna triggers test <id> --stream slack --kind message --data -
   ```
   `--data` turns a saved Slack message event (`bot-message.json` is a CI bot's post with an
   empty `text` and the failure in an attachment) into what the script publishes. Then post a
   matching line in the channel. `midna stream events slack` shows it `accepted`, and
   `midna triggers deliveries` shows the firing.

## Webhook triggers

- `midna triggers add --name N --event pull_request.opened --repo owner/name --project P --agent claude --prompt "Review PR {{pr.number}}"`
  drafts a trigger. Other actions are `--run CMD` and `--attention MSG`.
- Agent-made webhook triggers start as `needs_secret`. Only a human can paste the signing secret
  and enable the trigger. Tell the human; don't try to set the secret yourself.
- `midna triggers test <id> --payload file.json` dry-runs a trigger. `midna explain <trigger-id>`
  says what it does and what it is waiting for.
