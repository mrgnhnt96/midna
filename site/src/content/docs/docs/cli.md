---
title: The midna CLI
description: Drive midna from a shell or an agent with the midna command.
---

The `midna` command does from a shell what the app does with clicks. It's mostly for agents, but it works for you too. Inside a midna terminal it's always on your `PATH`; elsewhere, see [Install](/docs/install/#what-happens-on-first-launch).

```bash
midna help             # every verb
midna <verb> --help    # one verb, and which steps are human only
midna capabilities     # what you can do, and what only the human can do
```

Add `--json` to any verb for machine-readable output. Text with flags in it goes after `--`, like `midna check command -- git push --force`.

## Who you are

Every connection to `midnad` gets a role.

- **Human**: the Midna app.
- **Agent**: everything else, including the `midna` command in any terminal, yours too.

So when you run `midna rules remove …` in a shell, it doesn't remove the rule; it asks you in the app. That's on purpose: a process can't prove it's you, so anything only you may do goes through the GUI. `midna info` shows the role you got. See the [security model](/docs/security/).

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | OK |
| 1 | Refused or failed. The message says what to do next, and names the needs-you item if one was raised. |
| 2 | Bad arguments |
| 3 | The daemon isn't reachable |

## Orientation

```bash
midna info                  # version, your role and terminal
midna list                  # terminals with their status
midna projects              # projects; `projects discover` lists folders you can open
midna needs                 # what's waiting on the human
midna explain <id|topic>    # why a terminal, rule, trigger or item is the way it is
midna events --follow       # stream everything that happens
```

## Terminals

```bash
midna open                                  # a shell in this directory's project
midna open --name tests --monitor 'cargo test --watch'
midna open --agent claude --prompt "Fix the flaky test in api"
midna send <id> 'ls -la'                    # type text and press Enter
midna send <id> --image /tmp/shot.png -- "Does this look right?"
midna key <id> ctrl-c                       # press keys
midna read <id> --lines 100                 # read output (--screen for just the screen)
midna rename <id> "dev server"
midna restart <id> --idle                   # restart when the agent is idle
midna close <id>                            # --force for a busy terminal
midna focus <id>                            # bring it to the front in the app
midna procs <id>                            # its processes and background work
midna prompts <id> --jump prev              # scroll an agent to an earlier prompt
midna links                                 # links from an agent's conversation
```

## Getting the human's attention

```bash
midna attention "Need the staging API key to continue"   # blocked
midna attention --note "Migration done, 3 warnings" --detail "see the log"
midna notify send "CI is green" --detail "api#412"
```

`attention` raises a [needs-you](/docs/needs-you/) item. `notify send` posts a macOS notification, at most 6 a minute.

## Rules, approvals and triggers

```bash
midna check tool 'Bash(rm -rf build)'       # test an action, no side effects
midna rules add deny command 'git push --force*'
midna rules request-removal r_9f8e7d --reason "blocks the release script"
midna approve <n-id> --scope 15m            # human, or agents with approve.from_cli
midna triggers add --name "Review PRs" --event pull_request.opened --repo me/api \
  --project p_1a2b3c --agent claude --prompt 'Review PR #{{pr.number}}'
midna triggers test <t-id> --payload event.json
midna webhooks status
```

See [Rules](/docs/rules/) and [Triggers](/docs/triggers/).

## Settings, windows and the palette

```bash
midna settings list                          # every key, value, default, human only or not
midna settings set theme light
midna window open_screen insights
midna window split <id> stacked
midna commands add --title "Deploy staging" --rpc session.open \
  --params '{"kind":"monitor","command":["./deploy.sh"]}'
midna projects add-command <p-id> --name test --pinned -- cargo test
midna updates status
midna permissions status
```

`commands add` puts a command in your <kbd>⌘</kbd> <kbd>K</kbd> palette. Commands that would do something sensitive always need two presses of <kbd>↩</kbd>, and the confirmation shows the exact call.

## Daemon

```bash
midna daemon info
midna daemon restart       # restart midnad; terminals keep running
midna daemon stop          # human only: ends every terminal
```

## The API underneath

Every verb is a call to one method in midna's catalog. You can see and call the methods directly:

```bash
midna schema --list               # one line per method
midna schema rule.add             # a method's description and JSON Schema
midna schema                      # the whole OpenRPC document
midna call session.scroll '{"id":"ab12cd34","to":"top"}'
```

`midna explain` covers ids, methods, setting keys and topics: `status`, `rules`, `approvals`, `triggers`, `needs-you`, `settings`, `windows`, `human-only` and `mcp`.

```bash
midna explain human-only
midna explain approve.from_cli
midna explain command -- git push --force
```

`midna skill` prints the guide agents get, with the etiquette midna expects from them.
