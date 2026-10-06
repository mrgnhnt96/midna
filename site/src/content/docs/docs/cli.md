---
title: CLI and MCP
description: Drive midna from a shell or an agent with the midna command or the MCP server.
---

The `midna` command does from a shell what the app does with clicks. It's mostly for agents, but it works for you too. Inside a midna terminal it's always on `PATH`.

```bash
midna help              # every verb
midna <verb> --help     # one verb, including which steps are human only
midna capabilities      # what you can do, and what only the human can
```

Add `--json` to any verb for machine-readable output. Exit codes: `0` ok, `1` refused or failed (the message says what to do next), `2` bad arguments, `3` daemon unreachable.

## Roles

Every connection to `midnad` gets a role. The Midna app is the **human**. Everything else, including `midna` typed in any terminal, is an **agent**. So `midna rules remove …` from a shell doesn't remove the rule; it asks you in the app. A process can't prove it's you, so anything only you may do goes through the GUI. `midna info` shows the role you got.

## Common commands

```bash
# Look around
midna info                     # version, role, current terminal
midna list                     # terminals and their status
midna get <id>                 # one terminal: status, reason, cwd, agent info
midna needs                    # what's waiting on the human
midna needs wait <n_id>        # block until the human answers one item
midna explain <id|topic>       # why something is the way it is
midna events --follow          # stream everything that happens

# Terminals
midna open --name tests --monitor 'cargo test --watch'
midna open --agent claude --prompt "Fix the flaky test in api"
midna send <id> 'ls -la'       # type text and press Enter
midna key <id> ctrl-c
midna read <id> --lines 100
midna restart <id> --idle      # restart once the agent is idle
midna close <id>
midna close <id> --force --no-wait   # don't wait for approval: prints the needs-you id, exits 4
midna queue add --after <id> "now run the integration tests"

# Getting the human's attention
midna attention "Need the staging API key to continue"
midna notify send "CI is green" --detail "api#412"

# Rules, triggers, settings
midna check tool 'Bash(rm -rf build)'
midna rules add deny command 'git push --force*'
midna rules request-removal r_9f8e7d --reason "blocks the release script"
midna triggers add …           # see Triggers
midna settings set theme nord
```

## The API underneath

Every verb calls one method in midna's catalog, served as JSON-RPC over a Unix socket. You can list and call methods directly:

```bash
midna schema --list                  # one line per method
midna schema rule.add                # one method's description and JSON Schema
midna schema                         # the whole OpenRPC document
midna call session.scroll '{"id":"ab12cd34","to":"top"}'
```

`midna skill` prints the guide agents get.

## MCP server

`midna mcp` is a stdio [MCP](https://modelcontextprotocol.io) server with the same abilities as the CLI. Both go to the same daemon with the same checks.

Agents midna starts get it automatically, for that agent only (`--mcp-config` for Claude Code, `-c mcp_servers.midna…` for Codex). Turn that off with `agents.mcp`. For any other client:

```json
{
  "mcpServers": {
    "midna": { "type": "stdio", "command": "midna", "args": ["mcp"] }
  }
}
```

Every catalog method is a tool, with `_` instead of `.` (`session_open`, `rule_add`, `settings_set`), plus `capabilities`, `explain` and `guide`. Human-only tools say so in their description; calling one raises a Needs you request instead.

When midna refuses a call, the tool result is an error the model can read, with the next step:

```text
rule.remove is human only; use rule.request_removal to ask the human
```
