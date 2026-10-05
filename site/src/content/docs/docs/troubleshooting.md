---
title: Troubleshooting
description: Fixes for the most common problems.
---

## "midnad not running"

The app can't reach the daemon that runs your terminals. It retries every second and reconnects on its own when the daemon comes back.

- If it says **midnad restarting…**, wait a few seconds. That's normal during an update or `midna daemon restart`.
- Check **Settings › Permissions › Login item**. If it isn't **On**, use **Fix** (below).
- Quit midna and open it again. Every launch checks that the daemon is installed and running, and starts it if it isn't.
- Still stuck? Look at `midnad.log` (see [Logs](#logs)).

If the daemon was killed or crashed, the terminals it was running are gone. They show as exited when it starts again.

## The login item needs approval

macOS may hold back midna's login item until you allow it. **Settings › Permissions › Login item** then says **Needs your OK in Login Items**. Click **Fix**, which opens **System Settings › General › Login Items**, and switch Midna on under **Allow in the Background**. Midna notices on its own.

## `midna: command not found`

Inside a midna terminal, `midna` is always on your `PATH`. Outside one:

- Open **Settings › Agents › midna CLI**. If `~/.local/bin` isn't on your shell's `PATH`, it says so and shows the line to add to your shell config, like `export PATH="$HOME/.local/bin:$PATH"`. Midna never edits your shell files.
- If it says **Not installed**, click **Install to ~/.local/bin**.
- If it says something else is already at `~/.local/bin/midna`, midna leaves it alone. Remove it to let midna link its CLI there.

Open a new shell afterwards.

## macOS says Midna can't be opened

- Make sure you downloaded the DMG from the [download page](/download/) or the [Releases page](https://github.com/mrgnhnt96/midna/releases), and dragged Midna into Applications.
- Midna needs a Mac with Apple Silicon and macOS 12 or later.
- If macOS still blocks it, open **System Settings › Privacy & Security** and look for a message about Midna with an **Open Anyway** button.

## An agent can't do something

That's often on purpose. Ask it what `midna explain <id>` or the refusal said: most refusals name what to do next, and many turned into a [needs-you](/docs/needs-you/) item for you. To see what decides an action, use the **Test** strip in [Rules](/docs/rules/#test-an-action) or `midna check`.

## A terminal is stuck on "needs you"

Press <kbd>⌘</kbd> <kbd>J</kbd> to see what it's waiting on. If an agent's dialog is already gone, **Dismiss** the item. `midna explain <terminal-id>` shows why the terminal has the status it has and the events that led to it.

## Webhooks aren't arriving

- Check the **Delivery path** strip in **Triggers**, or `midna webhooks status`. It shows whether Tailscale Funnel is up and the URL GitHub or Bitbucket should use.
- In GitHub's webhook settings, look at **Recent Deliveries**. A 401 means the secret in GitHub and the one in the trigger don't match.
- Check the trigger's **Recent deliveries** in midna. **Filtered** says which filter said no, and **no trigger** means nothing listens to that event yet or the trigger has no secret.
- A trigger only fires when it's switched on.

See [Triggers and webhooks](/docs/triggers/).

## Logs

Both logs are in midna's data folder, `~/Library/Application Support/com.mrgnhnt.midna`:

- `app.log`: the Midna app, including updates
- `midnad.log`: the daemon

```bash
open ~/Library/Application\ Support/com.mrgnhnt.midna
```

`midna events --follow` shows everything happening in midna as it happens.

## Something else

[Report an issue](https://github.com/mrgnhnt96/midna/issues/new/choose) with your midna version (`midna info` or **Settings › Updates**) and what's in the logs.
