---
title: Updates
description: How midna updates itself without closing your terminals, and the stable and beta channels.
---

Midna updates itself. Your terminals keep running through it.

## How it works

1. Midna checks for a new version about 5 seconds after it opens, then every 6 hours. **Settings › Updates** has **Check now**.
2. When there's a newer version, midna downloads it in the background and checks it before anything else happens:
   - the download's SHA-256 matches the one in the update feed
   - the feed entry carries midna's ed25519 signature
   - the new app's code signature is valid
   - it's signed by the same Apple Team ID as the app you're running
   
   If any check fails, the update is thrown away.
3. The status bar shows **Update ready · restart to apply**. Click it, or **Restart to apply** in **Settings › Updates**. Quitting midna with an update ready also installs it.
4. Midna swaps the old app for the new one in a single step, so `Midna.app` is never half old and half new, then relaunches.
5. The new app upgrades `midnad` in place. Same process, same terminals, same screens and scrollback.

Updates only go forward: midna never installs an older version than the one you have.

## Stable and beta

The setting `updates.channel` picks which releases you get:

- **stable** (the default): public releases.
- **beta**: prereleases too, with versions like `0.3.0-beta.1`. You get new things earlier, and they may be rougher.

Switch it in **Settings › Updates**, or:

```bash
midna settings set updates.channel beta
```

Switching back to stable doesn't downgrade you. You stay on your beta until a stable release comes out that's newer than it.

## Where updates come from

Midna reads its update feed from GitHub Releases:

```text
https://github.com/mrgnhnt96/midna/releases/download/channels/{channel}.json
```

`{channel}` is `stable` or `beta`. The setting `updates.feed_url` changes it, and it's human only: agents can't point midna somewhere else. Whatever the URL, an update must still carry midna's signature.

## From the CLI

```bash
midna updates status    # running and available versions
midna updates check     # check now
midna updates install   # install a downloaded update (asks you, from a terminal)
```

Installing is human only. From a terminal or an agent, `install` becomes a [needs-you](/docs/needs-you/) request.

## Updating by hand

You can also download the newest DMG from the [download page](/download/) and replace the copy in Applications. Midna's data and your terminals are kept: the next launch upgrades the daemon in place.
