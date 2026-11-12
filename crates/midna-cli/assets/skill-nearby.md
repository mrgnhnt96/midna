## Nearby

Midnas on the same network pair with each other (`midna peers`). Everything you send leaves this
Mac only after the human here approves it, and does nothing there until a person on that Mac
accepts it. `midna explain nearby` covers pairing, the inbox and every kind of share.

### Asking another Mac's agent for help

When the human says "open a tab on roon and ask it about X" (or work needs something only that
Mac has: its hardware, a device plugged into it, its own checkout or login):

1. Find the device: `midna peers` lists paired devices, with whether each is nearby now. Use its
   name or id. It must be paired and nearby; you can't pair for the human.
2. Ask, and wait for the answer:

   ```
   midna share send agent-start --to roon --prompt "Run the iOS tests on the plugged-in iPhone and tell me which fail" --wait
   ```

   It starts a new agent there (`--agent claude|codex`; default this terminal's agent) in its
   checkout of this terminal's project (`--session <terminal>` for another one's), with your
   prompt as its first. `--title` names its tab. Write the prompt for someone with no context:
   that agent has never seen this conversation, so say what you need, where, and what the
   answer should contain.
3. First the human here approves sending it (the call waits for that), then a person on that Mac
   accepts it, or it starts at once if they set this Mac to auto-accept agent starts. `--wait`
   then waits (default 30m, `--timeout 2h`) until that agent's first turn ends and prints its
   last message, cut to 4 KiB. The answer also lands in `midna inbox` as a message in reply to
   the agent start.

If the wait runs out (exit 4), the agent is still working or the offer still waits:
`midna share answer <sh_id> --wait` picks it up again. Declined means nothing ran there. For a
longer conversation, the human can carry on in that terminal on the other Mac; another agent
start opens another terminal, it doesn't continue the first.

Over MCP or RPC: `share_send {peer, kind: "agent_start", prompt, agent?, session?, title?,
want_reply: true}`, then `share_answer {id, wait_secs}` (at most 600 per call; call again) until
`answer` is set. Event `share.answered` fires when it comes.
