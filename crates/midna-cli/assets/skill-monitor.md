## Monitor grants

A monitor grant is the human letting one terminal's agent answer other terminals for them while it
watches. Only the human gives one (`midna monitor grant <id> [--scope prompts|approvals|all]
[--until 2am | --for 8h]`) or revokes it, and it ends by itself at its until. `midna info` and
`midna capabilities` say when your terminal has one.

With a grant you may:

- answer another terminal's permission prompt (scope `prompts` or `all`): `midna approve <n_id>`
  or `--deny`, or `midna key <id> enter|escape` once `midna read <id> --screen` shows the prompt.
- approve once, or deny, another terminal's approval item (scope `approvals` or `all`).

Still the human's, grant or not: approvals that confirm a human-only action (giving or revoking a
grant among them), rule removals, folder-trust prompts, agents' questions, and approving for longer
than once (that adds a rule). Your own terminal's items follow the ordinary rules.

Each answer is logged as your terminal's, through its grant (`monitor_grant` in the
`needs_you.resolved` and `session.input_by_agent` events). Approve only what you have read and
judge safe; deny it, or raise attention, when unsure. Once the grant is revoked or expires,
everything is refused as before.
