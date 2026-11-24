## Monitor grants

A monitor grant is the human letting one terminal's agent answer other terminals for them while it
watches. Only the human gives one (`midna monitor grant <id> [--scope prompts|approvals|all]
[--until 2am | --for 8h]`) or revokes it, and it ends by itself at its until. `midna info` and
`midna capabilities` say when your terminal has one.

midna drives the monitoring agent by itself: on the grant it types a briefing into the terminal
(once the agent is ready), then a note whenever another tab needs it: a prompt or approval in
the grant's scope, or an agent tab sitting idle, done, exited or failed (finished: close it with
`midna close <id>`; stopped short: nudge it with `midna queue add --session <id> "…"`). It checks
every `monitor.check_secs` (default 30). Don't run your own watch loop or Monitor: handle the
note, end your turn, and the next one comes. When the grant ends, a note says so. At a Claude
usage limit, notes wait until it resets (a note would only fail your turn again), and tabs the
limit stopped are left out: midna resumes them itself (`agents.resume_after_limit`).

The human's own limits come first: the monitor rules, a markdown file in midna's home
(`monitor-rules.md`) saying what a monitor may and may not do ("everything except Slack messages
sent straight to a person"). It is in the briefing and in every note. Leave what it rules out for
the human: don't answer it, don't deny it. When the human asks, keep the file for them:
`midna monitor rules` lists it, `midna monitor rules add "<rule>"` appends one, `midna monitor
rules remove <n|text>` takes one out, and `midna monitor rules path` prints the file to edit.

With a grant you may:

- answer another terminal's permission prompt (scope `prompts` or `all`): `midna approve <n_id>`
  or `--deny`, or `midna key <id> enter|escape` once `midna read <id> --screen` shows the prompt.
- approve once, or deny, another terminal's approval item (scope `approvals` or `all`).

While the grant lasts, midna tells your agent's own permission check to let these through, so
it never stops you halfway: a Bash command made only of `midna approve`, `key`, `needs`, `read`,
`get`, `list`, `procs`, `queue add|list`, `close` and `attention` (optionally piped into `grep`,
`head`, `tail`, `wc`, `sort` or `jq`), and the same verbs' MCP tools. Anything else in the command
(another program, `$(…)`, a redirect) leaves it to the ordinary check, so keep these commands to
themselves. midna still checks the grant's scope on each one. This needs a Claude whose hooks
report to midna; when the human grants a tab where it can't (a Codex tab, or a Claude that never
reported to midna's hooks), midna tells them at once.

Still the human's, grant or not: approvals that confirm a human-only action (unless the human
turned on `monitor.approve_human_only`, and even then never one about monitor grants or a
`monitor.*` setting), rule removals, folder-trust prompts, agents' questions, and approving for
longer than once (that adds a rule). Your own terminal's items follow the ordinary rules.

Each answer is logged as your terminal's, through its grant (`monitor_grant` in the
`needs_you.resolved` and `session.input_by_agent` events). Approve only what you have read and
judge safe; deny it, or raise attention, when unsure. Once the grant is revoked or expires,
everything is refused as before.
