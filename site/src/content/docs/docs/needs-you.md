---
title: Needs you
description: Everything waiting on you in one place, what each kind of item means, and what approving does.
---

**Needs you** is where midna puts anything only you can answer. When something is waiting, the **N need you** button shows at the top of the sidebar, the terminal's row gets an orange dot with a line saying why, and midna can post a [notification](/docs/settings/#notifications).

## Going through them

Press <kbd>⌘</kbd> <kbd>J</kbd> or click **N need you**. Midna shows one card at a time, oldest first, with:

- what's being asked, who asked and in which terminal
- why, and the exact request
- the last lines of the terminal's screen
- the actions for that kind of item

<kbd>⌘</kbd> <kbd>↩</kbd> is the main action and <kbd>⌘</kbd> <kbd>⌫</kbd> the other one. <kbd>⌘</kbd> <kbd>J</kbd> skips to the next card, and <kbd>⌘</kbd> <kbd>O</kbd> opens the card's terminal. When you're through, you see **All clear** with a summary.

The terminal you're looking at also shows its own approval as a banner along the bottom, so you can answer without leaving it. Needs-you items also show at the top of <kbd>⌘</kbd> <kbd>K</kbd>.

## Kinds of items

| Kind | What it is | <kbd>⌘</kbd> <kbd>↩</kbd> | <kbd>⌘</kbd> <kbd>⌫</kbd> |
| --- | --- | --- | --- |
| Approval | A [rule](/docs/rules/) said **ask**, or an agent asked for a human-only action | Approve | Deny |
| Permission prompt | Claude Code or Codex is showing its own permission dialog | Approve | Deny |
| Blocked | An agent can't go on without you (`midna attention`) | I've done it | Dismiss |
| Note | An agent left you an FYI, or a trigger raised one | Got it | |
| Failed | A monitor or agent exited with an error | Restart | Dismiss |
| Trigger waiting | A webhook trigger is waiting to start | Start | Deny |
| Rule removal | An agent asked you to remove a rule | Remove rule | Keep rule |
| Secret needed | An agent drafted a trigger that needs its webhook secret | Open Triggers | Dismiss |

Answering a permission prompt from midna presses the key in the agent's dialog for you, and only after midna sees the dialog on screen.

## Approving for longer

**Approve** on its own approves once. The arrow next to it (or the split menu on the card) has longer options. Each one adds an **allow** [rule](/docs/rules/) so you aren't asked again:

| Option | Rule it adds |
| --- | --- |
| Once | None |
| 15 minutes, 1 hour | An allow rule for this terminal that expires |
| This session | An allow rule for this terminal |
| Always | An allow rule for the whole project (global when there's no project) |

The rule matches exactly what was asked for, and it's marked as coming from your approval, so you can find and remove it later in **Rules**.

When several permission prompts ask for the same thing, the card offers **Approve all N once** (<kbd>⌘</kbd> <kbd>⇧</kbd> <kbd>↩</kbd>). Approvals from rules always go one at a time.

## Requests for human-only actions

Some actions are yours alone: removing rules, enabling triggers, changing human-only settings, removing projects, stopping or upgrading the daemon, setting up webhooks, installing an app update. When an agent calls one, nothing happens. It becomes an approval instead, and the card shows exactly what will run:

> Approving runs exactly: `settings.set {"key":"approve.from_cli","value":true}`

Approving runs exactly that call, with the parameters it had when the agent asked. If what it points at changed in between (say, the trigger's action was edited), approval fails instead of running the new version.

## Can agents approve?

Not by default. If you turn on `approve.from_cli` (human only) in **Settings › What agents may do without asking**, an agent may approve requests from its own terminal with `midna approve`. It still can't approve anything from another terminal, and never a request for a human-only action. Agents also can't answer another agent's permission prompt by typing into its terminal.

## Waiting and timeouts

An agent blocked on an approval waits up to 5 minutes for you (setting `policy.request_timeout_secs`), then gives up.
