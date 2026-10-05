---
title: Install
description: Download midna, move it to Applications, and let it set up its background daemon and CLI.
---

## Requirements

- A Mac with Apple Silicon (M1 or later). There's no Intel build yet.
- macOS 12 or later.
- [Claude Code](https://docs.anthropic.com/en/docs/claude-code) or [Codex](https://github.com/openai/codex) if you want to run agents. Midna doesn't install them for you.

## Download and install

1. Go to the [download page](/download/). The newest `.dmg` starts downloading from GitHub on its own. If it doesn't, grab it from the [Releases page](https://github.com/mrgnhnt96/midna/releases/latest).
2. Open the `.dmg` and drag **Midna** into **Applications**.
3. Open Midna from Applications.

## What happens on first launch

Midna sets itself up in the background the first time it opens. It checks the same things on every launch, so there's nothing to redo after an update.

1. **It installs the daemon.** `midnad` is copied to `bin/` inside midna's data folder (`~/Library/Application Support/com.mrgnhnt.midna`).
2. **It registers a login item** so `midnad` starts when you log in and your terminals are there when you need them. macOS may ask you to allow it in **System Settings › General › Login Items**. If it does, **Settings › Permissions › Login item** in midna says "Needs your OK in Login Items" and has a **Fix** button that opens the right pane.
3. **It links the `midna` CLI** into `~/.local/bin`, if that folder is on your shell's `PATH`. If it isn't, **Settings › Agents › midna CLI** offers to install it there and shows the `export PATH=…` line to add to your shell config. Midna never edits your shell files itself. Terminals inside midna always have the CLI on their `PATH`, either way.

## Permissions

Midna doesn't need any special permission to run terminals.

- **Notifications.** macOS asks the first time midna has something to tell you, like an approval or a finished turn. You can also turn notifications on in **System Settings › Notifications**.
- **Accessibility** is optional. It lets [Kass](/docs/kass/) read and edit the text you're typing into a terminal, and lets agents move windows when you allow that. Before you grant it, read [the TCC note on the security page](/docs/security/#accessibility-and-processes-in-your-terminals): processes in midna terminals may be able to borrow the grant.

**Settings › Permissions** shows the state of each one with a button to the right System Settings pane. After you grant Accessibility, midna offers **Relaunch**, because macOS only shows a new grant to a fresh process. Your terminals keep running.

## Updating

Midna updates itself. See [Updates](/docs/updates/).

## Uninstalling

1. Quit midna.
2. In Terminal.app (not a midna terminal), run:

   ```bash
   /Applications/Midna.app/Contents/MacOS/midna-app --uninstall
   ```

   This stops the daemon, which **ends every terminal**, unregisters the login item and removes the CLI link from `~/.local/bin`.
3. Move Midna from Applications to the Trash.

Your data (projects, rules, triggers, settings and the event log) stays in `~/Library/Application Support/com.mrgnhnt.midna`. Delete that folder too if you want it gone.
