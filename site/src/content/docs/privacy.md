---
title: Privacy
description: What midna and this website keep, what leaves your Mac and where it goes. No account, no analytics, no cookies.
---

Last updated November 6, 2026.

midna has no account, and neither the app nor this website has analytics, crash reporting or tracking. What you do in midna stays on your Mac unless one of the things listed below sends it somewhere.

midna is made by Morgan Hunt. If you have a question about this page, [open an issue](https://github.com/mrgnhnt96/midna/issues/new/choose).

## What stays on your Mac

Projects, rules, triggers, settings, logs and the event log are kept in `~/Library/Application Support/com.mrgnhnt.midna`. Secrets are kept in the macOS Keychain. Nothing in that folder is uploaded by midna.

midna keeps this until you delete it, with two limits: a project's Summary keeps closed terminals for 7 days (`summary.keep_days`), and cleanup keeps its last 100 runs.

To remove all of it, follow [Uninstall](/docs/install/#uninstall) and delete the folder.

## What the app sends

| What | Where it goes | When | What's sent |
| --- | --- | --- | --- |
| Update check | GitHub | Every 6 hours | A request for the channel's update file, with midna's version in the user agent |
| Link preview | The site you're pointing at | When the pointer rests on a web link in a terminal | A normal request for that page, to show its title and description. Never for local or private addresses |
| Pull request details | GitHub, through your own `gh` | When a terminal's branch has a pull request, or the pointer rests on a pull request link | What `gh` sends when you run it yourself |
| Network check | `api.anthropic.com` | While an agent is waiting for the network to come back | A connection attempt only. No data |
| Cleanup after a terminal closes | Anthropic, through your own Claude account | When an agent terminal closes, if `cleanup.enabled` is on (it is by default) | The terminal's name and folder, the repo, the names of the branches and worktrees to remove, your cleanup rules and your own cleanup items, in a prompt to `cleanup.model`. Nothing runs when there's nothing to clean up. Turn it off in Settings |
| Webhooks | Whichever path you set up | Only if you turn webhooks on (they're off by default) | Events arrive from GitHub or Bitbucket through the path you chose, such as Tailscale Funnel |
| Report an issue | GitHub, in your browser | Only when you choose it | A prefilled issue with versions and hardware. Nothing is sent until you submit it. No paths, project names, terminal output or logs |

## Agents

Agents such as Claude Code and Codex are separate programs that you install and sign in to yourself. What you send them goes to their makers under those companies' own terms. midna doesn't send your prompts anywhere else.

Triggers and scripts you add run as you, and they can reach whatever you tell them to.

## This website

This site is hosted on GitHub Pages, so GitHub handles every visit and may keep technical logs, such as IP addresses. See [GitHub's privacy statement](https://docs.github.com/en/site-policy/privacy-policies/github-general-privacy-statement).

The [download page](/download/) asks GitHub's API for the newest release from your browser, and the download itself comes from GitHub.

The site sets no cookies, loads no analytics and serves its own fonts.
