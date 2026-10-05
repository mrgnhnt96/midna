---
title: Security model
description: What agents can and can't do in midna, and the limits of that boundary.
---

Midna lets agents do almost everything, and keeps a few things for you. This page explains how that boundary works and, just as important, what it doesn't protect against.

## The boundary

**Agents may add; humans may remove.** An agent working through midna's own interfaces (the CLI, MCP, JSON-RPC, the app binary or the command palette's file) can't act as you, even when it's confused or prompt-injected. It can't:

- approve its own requests, or anyone else's (unless you turn on `approve.from_cli`, and then only its own terminal's)
- answer another agent's permission prompt by typing into its terminal
- remove rules
- set webhook secrets or switch triggers on
- change human-only settings
- remove projects, stop, upgrade or reset the daemon, set up webhooks, or install an app update

When an agent tries one of these, nothing happens. It becomes a [needs-you](/docs/needs-you/) item, and you decide in the app. The card shows the exact call that will run, and if what it points at changes before you answer, approving fails instead of running the new version.

## Who counts as you

Midna decides who's calling from the process on the other end of its socket:

- **The Midna app is you.** In release builds, midna checks the caller's code signature: it must be Midna, signed by the same Apple team as the daemon. Renaming another program doesn't pass.
- **Anything running inside a midna terminal is an agent**, whatever it is, including a copy of the Midna app started from a shell.
- **Everything else is an agent too.**

A connection can lower its role, never raise it. A terminal can only speak for itself: a process that claims to be another terminal is refused.

## The same-user limitation

**Read this before relying on the boundary.** `midnad`, the app, every terminal and every agent run as the same macOS user, and macOS doesn't wall one user's processes off from each other. An agent in a midna terminal could, outside midna's interfaces:

- read and write midna's state files and event log
- kill `midnad` or unload its login item
- start its own `midnad`
- get at webhook secrets: they're in your Keychain, which protects them only with its own access prompt

So the boundary is a **guardrail, not a security boundary** against a malicious local process. It keeps a well-meaning or confused agent from acting as you through midna. It doesn't stop code that's actively trying to escape. If you run untrusted code with agents, run them as a different macOS user.

The guide midna gives agents tells them never to route around a refusal: no other tools, no editing midna's files, no going to the socket directly. That's an instruction, not an enforcement.

## Accessibility and processes in your terminals

macOS may treat a process in a midna terminal as belonging to Midna for privacy permissions. If you grant Midna **Accessibility**, a program running in one of its terminals could inherit that grant and use it to click buttons in midna's own window, including **Approve**. Terminal apps usually prevent this; midna doesn't yet.

Accessibility is optional. It's used for [Kass](/docs/kass/) and for letting agents move windows. If you don't need those, leave it off. If you do, keep this in mind.

## Webhooks

- Every delivery must be signed with the trigger's secret (HMAC-SHA256, checked in constant time). Unsigned or wrongly signed requests never run anything.
- A delivery, or the same signed body sent again under a new id, runs once.
- The receiver limits how much it accepts: request size, requests in flight, and how many rejected requests it records.
- Secrets live in your Keychain and are never shown again, returned to agents, or written to logs.
- Text from webhooks is marked as untrusted in agent prompts, and agents started by triggers run supervised. See [Triggers](/docs/triggers/#supervised-agents) for why that lowers the risk of prompt injection without removing it.

Two limits: replaying a stored delivery doesn't re-check its signature (it was checked when it arrived), and agents are allowed to replay. Duplicate detection covers the last 500 deliveries.

## Files

Midna's data folder and its socket are private to your user (`0700` and `0600`).

## Signed builds only

The checks above rely on code signing. Builds you make yourself without a Developer ID certificate fall back to recognizing the app by name, which another program can imitate. Use the signed releases from the [download page](/download/) for real work.

## Reporting a problem

If you find a way around the boundary, please [open an issue](https://github.com/mrgnhnt96/midna/issues/new/choose).
