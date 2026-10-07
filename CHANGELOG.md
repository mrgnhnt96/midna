# Changelog

Notable changes to midna for users. Each release gets a section here, newest first. The website shows this file at [midna.mrgnhnt.com/changelog](https://midna.mrgnhnt.com/changelog/).

## Unreleased

The first public release.

### New

- **A terminal agents can drive.** Claude Code, Codex and any other tool can open terminals, send input, read screens and raise your attention through the `midna` CLI or its MCP server. `midna capabilities` lists what an agent can do and what only you can.
- **Shells that survive everything.** A background daemon owns every terminal. Quit midna, update it or restart the daemon, and your shells keep running with the same processes and screens.
- **Needs you.** Permission prompts, failed monitors and agent requests from every terminal wait in one queue. Press <kbd>⌘J</kbd> to go through them, and approve once, for a while, for the session or always.
- **Rules.** Allow, ask or deny commands and tools, globally or per project. Agents can add rules; only you can remove them.
- **Triggers.** GitHub and Bitbucket webhooks can start an agent, run a command or raise your attention, delivered over Tailscale Funnel.
- **Insights.** Turns, messages, spend and time spent waiting on you, by day, week or month.
- **Claude plan usage for tools.** `midna usage` (and `usage.get`, `session.get` `agent_info.rate_limits`) shows the 5-hour and weekly limits Claude Code reports, and a `usage.limit_reached` event fires when one runs out. `midna links --kind file --turn last` lists the files an agent edited since your last prompt.
- **Dictation with Kass.** midna opens a real text field as soon as Kass starts listening, so dictation can read and edit what you say in a terminal.
- **Signed updates.** midna checks for updates every few hours, verifies them, and installs them on restart without closing your terminals. Turn on the beta channel in **Settings › Updates** to get new features early.
- **Your own agent arguments.** `midna open --agent claude --resume <id> -- --append-system-prompt "…" --settings board.json` (and `session.open {agent_args, resume}`) start an agent with its own flags and conversation. A `--settings` or `--append-system-prompt` is merged with midna's own, so midna's hooks keep working.
- **Unattended closes.** Turn on **Settings › Agents may force-close terminals** to let an agent close working terminals and use `close --force` without asking you. Off by default.
- **Approvals clean up after themselves.** An approval about a terminal that has since closed, or from an agent that went away, leaves Needs you on its own.
- **An agent that moves on stops needing you.** When an agent raised `midna attention` and then gets a new prompt (your answer, or a message from a tool or another agent), its request leaves Needs you. If it's still stuck, it asks again.
- **Folder trust in Needs you.** When a new Claude asks "Do you trust the files in this folder?", its terminal shows as needs you and you can answer from Needs you.
- **Terminals that close themselves.** `midna open --close-on-exit` (`session.open {close_on_exit: true}`) closes an agent's or command's terminal when it exits normally. A failed exit stays open so you can see why.
- **Unattended updates.** Turn on **Settings › Agents may install updates** (`agents.may_install_updates`) to let an agent run `midna updates install` without asking you, e.g. one that tests each beta as it lands. Signature checks still apply. Off by default.
- **CLI.** `midna get <id>` shows one terminal; `--no-wait` returns a needs-you id instead of waiting for your answer (`midna needs get|wait <id>` follows it); agents may remove a project midna created for their `open --cwd` once its terminals are closed.
- **Closing a terminal moves.** <kbd>⌘W</kbd> folds the closed row away while the rows below slide up, the highlight moves to the terminal shown next, and the pane slides to it the same way. Reduce Motion turns it off.
- **<kbd>⌘T</kbd> opens an agent.** <kbd>⌘T</kbd> starts a new terminal running the agent you last started in that project (Claude, Codex…), and <kbd>⇧⌘T</kbd> opens a plain terminal. At root it's <kbd>⌥⌘T</kbd> and <kbd>⌥⇧⌘T</kbd>. Rebind them in **Settings › Shortcuts**.
- **<kbd>⌘A</kbd> in an agent's input selects your prompt.** In Claude Code's or Codex's input box, <kbd>⌘A</kbd> selects just the text you've typed and moves the cursor to its end, so typing or <kbd>⌫</kbd> replaces it. Elsewhere it still selects the whole screen.
- **<kbd>⌫</kbd> deletes a mouse selection across rows.** Drag over several lines of an agent's input (or a wrapped shell command) and <kbd>⌫</kbd>, <kbd>⌦</kbd> or typing replaces it, as it already did for a selection on the cursor's row. The cursor walks to the selection's edge first.
- **The sidebar folds away.** <kbd>⌘B</kbd> wipes each line back to its status dot, top to bottom, then closes the panel to the rail, where the dots pop in. Expanding opens the panel first, then writes the lines back in. The terminal resizes once, not on every frame. Reduce Motion turns it off.
- **Schedules that run every 55 minutes.** `--cron '@every 55m'` (also `1h30m`, `5h`) runs on a fixed interval cron can't express: `*/55` means minutes 0 and 55, not every 55 minutes. The first run is one interval after you add it (or at `--starts`), then it keeps that pace. The schedule editor picks it automatically when the number doesn't divide an hour or a day, and no longer describes `*/55` as "Every 55 min".
