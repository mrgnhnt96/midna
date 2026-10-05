---
title: Settings
description: Where settings live, how to change them, and the ones worth knowing about.
---

Midna's settings live in the daemon, not in a file you edit. You, the Settings window and agents all change the same values, and every change shows up everywhere at once.

## The Settings window

Open it with <kbd>⌘</kbd> <kbd>,</kbd>, the **Settings** button at the bottom of the sidebar, or <kbd>⌘</kbd> <kbd>K</kbd>. Every row shows:

- the control
- what it does
- the CLI command that does the same thing, with a **Copy** button
- who can change it: **human only** (with a lock), **agents too**, or read-only

The toggle in the title bar switches between rows and a read-only `settings.json` view. The **Ask** box opens an agent with your request, for things like "make ⌘T open Claude at root". The footer shows the last change and who made it.

Sections: **Updates**, **Permissions**, **Webhooks**, **Notifications**, **Notification sounds**, **Notification images**, **Look**, **Projects**, **Agents**, **What agents may do without asking**, **Keybindings**, and **Danger zone**.

Switches and choices change right in the window. Numbers and text are changed from the CLI or by asking.

## Settings as code

```bash
midna settings list              # every key: value, default, description, human only or not
midna settings get theme
midna settings set theme light
midna settings reset theme
midna explain policy.default     # what one setting does
```

Values are JSON or bare words: `true`, `42`, `dark`.

**Human-only** settings can be read by anyone but only changed in the Settings window. When an agent, or you from a terminal, sets one, it becomes a [needs-you](/docs/needs-you/) request instead.

## Notable settings

### Look

| Key | Default | What it does |
| --- | --- | --- |
| `theme` | `system` | `dark`, `light` or `system` |
| `density` | `comfortable` | `comfortable` or `compact` sidebar rows |
| `ui.header.script` | `github` | What the terminal header shows: `github` (branch, diff, PR and checks), `github+agent`, `none`, or a path to your own script. Agents may only pick the built-ins. |
| `ui.row.script` | `git-diff-stats` | The second line of each sidebar row: `git-diff-stats`, `none`, or your own script |
| `terminal.option_as_meta` | `true` | <kbd>⌥</kbd> acts as Meta. Off: <kbd>⌥</kbd> types accented characters. |

### What agents may do without asking

| Key | Default | Human only | What it does |
| --- | --- | --- | --- |
| `policy.default` | `auto` | Yes | The decision when no [rule](/docs/rules/) matches |
| `approve.from_cli` | `false` | Yes | Agents may approve their own terminal's requests |
| `agents.may_move_windows` | `false` | Yes | Agents may move, pop out, snap, keep on top or close windows |
| `agents.may_close_idle` | `true` | Yes | Agents may close other terminals that are idle, done or exited |
| `policy.request_timeout_secs` | `300` | | How long an approval waits for you |

### Agents

| Key | Default | What it does |
| --- | --- | --- |
| `agents.mcp` | `true` | Give agents midna launches the [MCP server](/docs/mcp/) |
| `agents.system_hint` | `true` | Tell them in two lines that they run in midna |
| `agents.claude.statusline` | `true` | midna's status line in Claude Code, which tracks cost |
| `agents.restart_on_update` | `when_idle` | Restart a Claude terminal into the same conversation when a newer Claude Code is installed: `when_idle`, `ask` or `off` |
| `agents.restart_idle_secs` | `60` | How long an agent must be quiet before a queued restart runs |
| `ui.ask.agent` | `claude` | The agent <kbd>⌘</kbd> <kbd>K</kbd>'s "Ask an agent" starts |
| `ui.ask.scope` | `project` | Where it starts it: `project` or `root` |
| `kass.auto_send` | `false` | Send [Kass](/docs/kass/) dictations right away instead of keeping them for review |

### Projects

| Key | Default | What it does |
| --- | --- | --- |
| `projects.roots` | none | Folders your projects live in, like `~/Development`. Their subfolders show up in <kbd>⌘</kbd> <kbd>K</kbd>, ready to open. |
| `git.refresh_secs` | `10` | How often git info refreshes |

### Webhooks

| Key | Default | Human only | What it does |
| --- | --- | --- | --- |
| `webhooks.path` | `off` | Yes | How webhooks reach your Mac: `tailscale_funnel` or `off` |
| `webhooks.port` | `7787` | | The local port the receiver listens on |
| `triggers.agent_mode` | `supervised` | Yes | How agents started by [triggers](/docs/triggers/#supervised-agents) run: `supervised` or `inherit` |

### Updates

| Key | Default | Human only | What it does |
| --- | --- | --- | --- |
| `updates.channel` | `stable` | | `stable` or `beta`. See [Updates](/docs/updates/). |
| `updates.feed_url` | GitHub Releases | Yes | Where midna checks for updates |

### Notifications

Midna posts macOS notifications for things you'd want to know while you're elsewhere. `notify.enabled` turns them all on or off, and each kind has its own switch:

| Key | Default | When |
| --- | --- | --- |
| `notify.approval` | on | An agent waits on you: an approval, a permission prompt or a question |
| `notify.attention` | on | An agent raises a note or says it's blocked |
| `notify.failed` | on | A command fails or an agent's turn ends in an error |
| `notify.turn_done` | on | An agent finishes a turn that took at least `notify.turn_done_min_secs` (30) |
| `notify.agent` | on | An agent sends you one on purpose |
| `notify.requests` | off | A trigger to enable, a secret to set, a rule an agent wants removed |
| `notify.background` | off | A background shell an agent started finishes |
| `notify.pr_checks` | off | The checks on a terminal's pull request finish |
| `notify.exited` | off | A terminal's process exits cleanly |
| `notify.triggers` | off | A webhook trigger fires |
| `notify.restarted` | off | midna restarts an agent after an update |

By default, midna doesn't notify about the terminal you're looking at (`notify.when_focused`). Clicking a notification takes you to its terminal.

Each kind has a sound (`notify.sound.<kind>`), a volume (`notify.volume.<kind>`, scaled by `notify.volume`) and an image (`notify.image.<kind>`). Pick them in **Settings › Notification sounds** and **Notification images**, where you can also import your own, or ask an agent ("use ~/Downloads/ding.mp3 for approvals, quieter").

A single terminal can override any of these, or be muted from its header's **…** menu:

```bash
midna notify set turn_done on --session ab12cd34
midna notify mute --session ab12cd34
```

### Keybindings

Every `keys.*` setting is a shortcut. See [Keyboard shortcuts](/docs/shortcuts/).

## Danger zone

- **Reset settings** puts every setting back to its default. Terminals, rules and the event log are kept.
- **Reset midna** closes every terminal and removes projects, triggers and needs-you items, then resets settings. Rules and the event log are kept.

Both ask you to click twice.
