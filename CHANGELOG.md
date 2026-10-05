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
- **Dictation with Kass.** midna opens a real text field as soon as Kass starts listening, so dictation can read and edit what you say in a terminal.
- **Signed updates.** midna checks for updates every few hours, verifies them, and installs them on restart without closing your terminals. Turn on the beta channel in **Settings › Updates** to get new features early.
