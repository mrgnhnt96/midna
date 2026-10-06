---
title: Install and update
description: Install midna, what it sets up on first launch, how updates work, and how to uninstall.
---

## Requirements

- A Mac with Apple Silicon. There's no Intel build.
- macOS 12 or later.
- [Claude Code](https://docs.anthropic.com/en/docs/claude-code) or [Codex](https://github.com/openai/codex) if you want to run agents. midna finds them through your login shell; it doesn't install them.

## Install

1. Go to the [download page](/download/). The newest `.dmg` downloads from GitHub. You can also get it from the [Releases page](https://github.com/mrgnhnt96/midna/releases/latest).
2. Open the `.dmg` and drag **Midna** into **Applications**.
3. Open Midna.

## What the first launch sets up

midna checks these on every launch, so nothing needs redoing after an update.

- **The daemon.** `midnad` is installed into `~/Library/Application Support/com.mrgnhnt.midna/bin/` and registered as a login item, so your terminals are running when you log in. If macOS holds the login item back, **Settings › Permissions › Login item** shows **Fix**, which opens **System Settings › General › Login Items**.
- **The CLI.** `midna` is linked into `~/.local/bin` if that folder is on your `PATH`. If it isn't, **Settings › Agents › midna CLI** shows the line to add to your shell config. midna never edits your shell files. Inside midna terminals the CLI is always on `PATH`.

A short setup screen walks through notifications, a first project, webhooks and a theme. Every step can be skipped and done later from Settings.

**Permissions.** midna needs nothing special to run terminals. macOS asks about notifications the first time midna has one to show. Accessibility is optional and only asked for when [Kass](https://kass.mrgnhnt.com) dictation needs it; read [the note on the security page](/docs/security/#accessibility) before granting it.

## Updates

midna checks for updates a few seconds after launch and every 6 hours after that. A new version is downloaded in the background and verified before anything happens:

- its SHA-256 matches the update feed
- the feed entry carries midna's ed25519 signature
- the app's code signature is valid and from the same Apple Team ID

When it's ready, the status bar shows **Update ready · restart to apply**. midna swaps the app in one step and relaunches, then upgrades `midnad` in place: same process, same terminals, same scrollback. Updates never go to an older version.

**Channels.** `updates.channel` is `stable` (the default) or `beta`, which also gets prereleases. Switch in **Settings › Updates** or with:

```bash
midna settings set updates.channel beta
```

Going back to stable doesn't downgrade you; you stay on the beta until a newer stable release comes out.

```bash
midna updates status    # running and available versions
midna updates check     # check now
```

`midna updates install` is yours to run. An agent that asks gets a Needs you item, unless you turn on **Settings › Agents may install updates** (`agents.may_install_updates`) for one that should install and test each beta as it lands.

## Uninstall

1. Quit midna.
2. In Terminal.app (not a midna terminal), run:

   ```bash
   /Applications/Midna.app/Contents/MacOS/midna-app --uninstall
   ```

   This stops the daemon, which **ends every terminal**, removes the login item and removes the CLI link.
3. Move Midna to the Trash.

Your projects, rules, triggers, settings and event log stay in `~/Library/Application Support/com.mrgnhnt.midna`. Delete that folder to remove them too.
