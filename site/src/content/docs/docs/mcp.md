---
title: MCP server
description: midna mcp exposes every midna method as an MCP tool.
---

`midna mcp` is an [MCP](https://modelcontextprotocol.io) server over stdio. It gives an agent the same abilities as the [`midna` CLI](/docs/cli/), as tools. Both go to the same daemon, with the same checks and the same audit log.

## Agents midna launches get it automatically

When midna starts Claude Code or Codex, it adds the server for that agent only:

- Claude Code: `--mcp-config` pointing at `hooks/mcp.json` in midna's data folder
- Codex: `-c mcp_servers.midna.…` options

Nothing is written to your global agent config. Turn it off with the setting `agents.mcp`; the change applies to agents started afterwards.

For Claude Code, midna also allows the read-only tools (listing terminals, reading output, explaining) without a permission prompt. Tools that act still go through Claude's permission flow and midna's [rules](/docs/rules/).

## Adding it to another client

Any MCP client can run it:

```json
{
  "mcpServers": {
    "midna": { "type": "stdio", "command": "midna", "args": ["mcp"] }
  }
}
```

Outside a midna terminal, the server connects as an agent that isn't in any terminal.

## Tools

- Every catalog method is a tool, named with `_` instead of `.`: `session_open`, `session_read`, `rule_add`, `trigger_add`, `settings_set` and so on. `midna schema --list` lists them all.
- Three more tools help an agent find its way: `capabilities` (an overview, and every method's flags), `explain` (the same as `midna explain`) and `guide` (the agent guide). The guide is also the resource `midna://skill`.

Human-only tools say so in their description. Calling one doesn't do it: it becomes a [needs-you](/docs/needs-you/) request, or tells the agent the request path (for example, `rule_remove` points at `rule_request_removal`).

## Refusals

When midna refuses a call, the tool result is an error the model can read, not a protocol error. Its text says what happened and what to do next:

```text
rule.remove is human only; use rule.request_removal to ask the human
Next: rule.remove is human-only; use the `rule_request_removal` tool with {"id":"r_9f8e7d","reason":"…"} and the human will see it.
```

The result's structured content holds the error code and, when one was raised, the needs-you item's id.
