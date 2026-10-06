---
title: Troubleshooting
description: Fixes for the most common problems.
---

## "midnad not running"

The app can't reach the daemon. It retries every second and reconnects on its own.

- **midnad restarting…** is normal for a few seconds during an update or `midna daemon restart`.
- Check **Settings › Permissions › Login item**. If it isn't on, click **Fix**, which opens **System Settings › General › Login Items**, and allow Midna under **Allow in the Background**.
- Quit and reopen midna. Every launch makes sure the daemon is installed and running.
- Still stuck? Check `midnad.log` (see [Logs](#logs)).

If the daemon crashed or was killed, its terminals are gone and show as exited.

## `midna: command not found`

Inside a midna terminal, `midna` is always on `PATH`. Outside one, open **Settings › Agents › midna CLI**:

- If `~/.local/bin` isn't on your `PATH`, it shows the line to add to your shell config, like `export PATH="$HOME/.local/bin:$PATH"`.
- If it says **Not installed**, click **Install to ~/.local/bin**.
- If something else is already at `~/.local/bin/midna`, midna leaves it alone. Remove it first.

## macOS won't open Midna

- Download it from the [download page](/download/) and drag it into Applications.
- midna needs Apple Silicon and macOS 12 or later.
- If macOS still blocks it, look in **System Settings › Privacy & Security** for an **Open Anyway** button.

## An agent can't do something

That's often on purpose. Most refusals say what to do next, and many raised a Needs you item. Use the **Test** field in [Rules](/docs/rules/#testing-an-action) or `midna check` to see what decides an action.

## A terminal is stuck on "needs you"

Press <kbd>⌘</kbd> <kbd>J</kbd> to see what it's waiting on. If the agent's dialog is already gone, **Dismiss** the item. `midna explain <terminal-id>` shows why the terminal has its status.

## A typed `claude` doesn't show its status

Agents typed into a shell are picked up through a small wrapper on `PATH`. If your shell config replaces `PATH` in an unusual way, or the **Hooks** item in the status bar is amber, open it and reinstall. `midna hooks status` shows the state per agent.

## Webhooks aren't arriving

- Check the delivery path strip in **Triggers**, or `midna webhooks status`, for whether Tailscale Funnel is up and which URL to use.
- In GitHub's webhook settings, check **Recent Deliveries**. A 401 means the secrets don't match.
- In midna, check the trigger's deliveries. **Filtered** says which filter said no; **no trigger** means nothing listens to that event or the trigger has no secret.
- A trigger only fires when it's switched on.

## Logs

Both logs are in `~/Library/Application Support/com.mrgnhnt.midna`:

- `app.log`: the app, including updates
- `midnad.log`: the daemon

`midna events --follow` shows everything happening as it happens.

## Something else

[Report an issue](https://github.com/mrgnhnt96/midna/issues/new/choose) with your version (`midna info`) and what's in the logs. <kbd>⌘</kbd> <kbd>K</kbd> › **Report an issue…** fills in the details for you.
