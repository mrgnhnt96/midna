---
title: Security model
description: What agents can and can't do in midna, and the limits of that boundary.
---

midna lets agents do almost everything and keeps a few decisions for you. This page covers how that works and what it doesn't protect against.

## The boundary

An agent working through midna's interfaces (the CLI, MCP, JSON-RPC, or the app binary) can't act as you, even when it's confused or prompt-injected. It can't:

- approve requests, its own or anyone else's (unless you turn on `approve.from_cli`, and then only its own terminal's)
- answer another agent's permission prompt by typing into its terminal
- remove rules
- set webhook secrets or switch webhook triggers on
- change human-only settings
- remove projects, stop or reset the daemon, set up webhooks, install global hooks or install an update

When an agent tries one of these, it becomes a [Needs you](/docs/agents/#needs-you) request. The card shows the exact call approving will run, and approving fails if its target changed in the meantime.

## Who counts as you

midna decides from the process on the other end of its socket:

- **The Midna app is you.** Release builds check the caller's code signature: Midna, signed by the same Apple team as the daemon.
- **Anything running inside a midna terminal is an agent**, including a copy of the app started from a shell.
- **Everything else is an agent too.**

A connection can lower its role, never raise it, and a terminal can only speak for itself.

## The same-user limitation

`midnad`, the app and every agent run as the same macOS user, and macOS doesn't isolate one user's processes from each other. Outside midna's interfaces, an agent could read and write midna's state files, kill `midnad`, or reach webhook secrets in your Keychain behind its own access prompt.

So the boundary is a **guardrail, not a security boundary** against a malicious local process. It keeps a well-meaning or confused agent from acting as you through midna. If you run untrusted code with agents, run them as a different macOS user.

## Accessibility

macOS may treat a process in a midna terminal as part of Midna for privacy permissions. If you grant Midna **Accessibility**, a program in one of its terminals could inherit it and click buttons in midna's window, including **Approve**. Accessibility is only needed for [Kass](https://kass.mrgnhnt.com) dictation and for letting agents move windows. Leave it off if you don't use those.

## Webhooks

- Every delivery must be signed with the trigger's secret (HMAC-SHA256, compared in constant time). Unsigned or wrongly signed requests run nothing.
- A delivery runs once, even if the same signed body is sent again under a new id.
- The receiver limits request size and requests in flight.
- Secrets live in your Keychain and are never shown again, returned to agents or logged.
- Webhook text is marked as untrusted in prompts, and agents started by webhooks run supervised. See [Triggers](/docs/triggers/#untrusted-input).

Replaying a stored delivery doesn't re-check its signature, and agents may replay.

## Files and builds

midna's data folder and socket are private to your user (`0700` and `0600`).

The checks above rely on code signing. A build you make yourself without a Developer ID certificate recognizes the app by name, which another program can imitate. Use the signed releases for real work.

If you find a way around the boundary, please [open an issue](https://github.com/mrgnhnt96/midna/issues/new/choose).
