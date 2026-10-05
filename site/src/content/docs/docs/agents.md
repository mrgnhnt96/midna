---
title: Claude Code and Codex
description: How midna launches agents, tracks their status and cost, routes their approvals, and lets them drive midna.
---

Midna runs [Claude Code](https://docs.anthropic.com/en/docs/claude-code) and [Codex](https://github.com/openai/codex) in its terminals. Install them the usual way; midna finds them through your login shell.

## Starting an agent

- <kbd>⇧</kbd> <kbd>⌘</kbd> <kbd>T</kbd> opens a new agent in the current project, using the agent you last started there.
- In <kbd>⌘</kbd> <kbd>K</kbd>, type a request and press <kbd>⇧</kbd> <kbd>↩</kbd> to start an agent with it as the first prompt.
- Agents can start other agents for you to follow: `midna open --agent claude --prompt "…"`.
- [Triggers](/docs/triggers/) can start agents from GitHub and Bitbucket events.

If you start `claude` or `codex` yourself in a plain shell, it runs, but without midna's hooks, so midna knows less about it.

## What midna adds

Midna passes everything on the command line when it launches an agent. **It never edits your global Claude or Codex config.**

**Claude Code** gets:

- `--settings` pointing at a file in midna's data folder that registers midna's hooks. They report when a turn starts and ends, when a tool runs and when a permission dialog opens, and they check each tool call against your [rules](/docs/rules/).
- midna's status line, which reports cost to [Insights](/docs/insights/) (setting `agents.claude.statusline`). It replaces your own status line inside midna; turn it off to get yours back, at the cost of spend tracking.
- `--mcp-config` with the [midna MCP server](/docs/mcp/) (setting `agents.mcp`).
- `--append-system-prompt` with a two-line hint that it's running in midna and can run `midna capabilities` (setting `agents.system_hint`).

**Codex** gets `-c notify=…` so midna hears when a turn ends, plus the MCP server and the same hint through `-c` options. Midna doesn't use Codex's hooks, because they need you to trust them first.

Every terminal also gets `MIDNA_SESSION`, `MIDNA_PROJECT` and `MIDNA_SOCKET` in its environment, `TERM_PROGRAM=midna`, the `midna` CLI on its `PATH`, and `MIDNA_SKILL` pointing at a short guide for agents (`midna skill` prints it).

Changes to these settings apply to agents started afterwards.

## Status

Midna shows an agent as **working**, **needs you**, **done** (finished, and you haven't looked yet) or **idle**. It reads that from the hooks, the agent's terminal title and, for permission dialogs, what's on screen. When Esc or <kbd>⌃</kbd> <kbd>C</kbd> interrupts a turn, the status follows.

## Approvals

Two kinds of approval reach [Needs you](/docs/needs-you/):

- **Midna approvals.** A rule said **ask** for a Claude Code tool call. Claude waits until you answer in midna.
- **Permission prompts.** The agent is showing its own permission dialog, because no rule decided and its own settings say ask. Approving or denying in midna presses the answer in the dialog for you.

Codex has no hook before tool calls, so midna rules don't apply to it; its own approval prompts still show up as permission prompts.

Startup dialogs, like Claude's folder trust question or Codex's hooks review, don't become needs-you items yet. Answer them in the terminal.

## Cost

Claude Code's spend shows per turn in Insights and on the **Today** card. Codex doesn't report cost, so its spend shows as zero.

## Sending messages

Type in the terminal as usual, use the [composer](/docs/kass/) (<kbd>⇧</kbd> <kbd>⌘</kbd> <kbd>D</kbd>) for long prompts, or attach images with <kbd>⌘</kbd> <kbd>I</kbd>. Agents can message other agents with `midna send <id> "…"`, which arrives as one message even across lines, and `--image` attaches a picture.

## Restarting into the same conversation

The terminal's restart button, or `midna restart <id>`, reopens the agent in the same conversation (`claude --resume`, `codex resume`). Midna refuses while the agent has background shells or subagents running, because a restart would lose them. In <kbd>⌘</kbd> <kbd>K</kbd>, **Restart … when idle** waits until the agent is idle with nothing in flight.

When a newer Claude Code is installed than a terminal is running, midna restarts that terminal into the same conversation once it's idle. Setting `agents.restart_on_update`: `when_idle` (the default), `ask` to get a needs-you note instead, or `off`.

## Letting agents drive midna

Inside midna, agents use the [`midna` CLI](/docs/cli/) or the [MCP server](/docs/mcp/) to see other terminals, open shells and monitors you can watch, get your attention, add rules, draft triggers and change settings. What they can't do is in the [security model](/docs/security/).
