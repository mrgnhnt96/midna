# Agent status, hooks and approvals against the real CLIs

**Verified against** Claude Code 2.1.288 (Haiku via `ANTHROPIC_MODEL=haiku`) and Codex CLI 0.160.0.

**Test setup.**
- A temp `MIDNA_HOME` daemon, with `MIDNA_APP_PATH` set to the CLI so that my CLI calls counted as human.
- `MIDNA_CLI_PATH` pointed at a wrapper that logged every raw hook payload before passing it to `midna hook`.
- Agents were opened with `midna open --agent claude|codex` and driven only with `midna send` / `read` / `needs` / `approve` / `rules`.
- Everything was killed and removed afterwards.

## Verified end to end

### Claude Code

**Launch**
- `claude --settings $MIDNA_HOME/hooks/claude-settings.json` is accepted.
- These hooks all reached `agent.hook`: SessionStart, UserPromptSubmit, PreToolUse, PermissionRequest, PostToolUse, Notification, Stop, SubagentStop and SessionEnd.
- The payload schema matches what the code reads: `hook_event_name`, `tool_name`, `tool_input.command`, `prompt`, `notification_type`, and `cost.total_cost_usd` / `model.display_name` in the status line.
- The midna status line renders `Haiku 4.5 · $0.03`, and `agent.cost` deltas add up to Claude's total.

**Status transitions**
- idle → working on `midna send`.
- → needs_you when the native permission dialog opens.
- → working after approving in midna, which sends Enter only after the screen check sees the dialog.
- → done on Stop.
- → idle on SessionEnd, then exited on `/exit` and on ctrl-d ×2.

**Gaps filled by the title + screen logic, all observed live**
- Approval answered with no hook yet: `prompt answered (title)`.
- Esc during a running tool: `stopped (title)`, which sets idle and ends the turn.
- ctrl-c while generating: idle.
- Esc on the permission dialog: no hook fires and the title doesn't change, so the screen watcher catches it as `prompt dismissed (screen)`.

**Native prompt, resolved through midna**
- Approve and deny both work.
- Resolving after the dialog had already gone sends no key. I checked this by leaving typed text in the input box: it was not submitted.

**Policy (`ask` rule `Bash(echo midna-test*)`)**
- A midna `approval` item appears, the hook blocks, and the session shows `needs_you`.
- Approve-once lets Claude run the command.
- Deny makes Claude receive a deny and reply "denied".
- Always adds the rule `allow Bash(echo midna-test three)` at project scope, and the next run passes via `rule` with no item.

**Insights**
- `insights.summary` shows turns, messages, spend and approvals for both agents.

### Codex

**Launch**
- `codex -c notify=[...]` works on 0.160.0.
- The notify payload arrives as the last argv: `{"type":"agent-turn-complete","input-messages":[…],"last-assistant-message":…}`.
- Hooks were never trusted. At the "Hooks need review" screen I chose "Continue without trusting".

**Status and turns**
- Working comes from the title spinner, done comes from notify, and idle comes from the title stopping.
- Esc mid-turn gives idle and ends the turn.
- ctrl-c ×2 gives exited.

## Fixed

1. **Enter didn't submit to Claude.**
   - Cause: Claude enables the kitty keyboard protocol, after which a bare CR only inserts a newline, and text plus CR in one write reads as a paste.
   - Fix: the daemon now tracks the kitty flags. It sends Enter as `CSI 13 u` (Esc as `CSI 27 u`), and puts it 150ms after the text.
   - This applies to `session.input` and to prompt answers.
2. **Native permission prompts never became needs_you.**
   - Cause: 2.1.288 fires `PermissionRequest` when the dialog opens, not `Notification`.
   - Fix: midna now registers PermissionRequest, plus PostToolUseFailure, PermissionDenied and StopFailure.
3. **Premature idle.** The `✳` title arrives before the dialog is drawn, so a stopped title now only counts after 1.5s of settling plus the screen check.
4. **Esc on the dialog was missed.** A watcher now polls the screen while the prompt is visible.
5. **Approvals looked like work.** A midna approval kept the session `working`, and the spinner would have cleared needs_you. The session is now `needs_you` while the approval blocks.
6. **Inherited Claude/Saggar env leaked into agents.** This turned off Claude's transcript saving and risked Saggar hooks claiming the agent. The scrub list is wider now.
7. **Codex problems:**
   - The spinner wasn't recognised as status, and the title flooded the log with roughly 40 events per turn.
   - The task-title-generation notify was counted as a turn and a message.
   - A double ctrl-c quit was reported as `failed` with a needs-you item.
8. **Screen check.** It is tightened to real dialogs. Claude's folder-trust dialog and Codex's hooks-review dialog no longer match; Enter there would exit or open the review.
9. **CLI.** `midna <verb> --help` used to run the verb.

**Regression tests**
- `crates/midnad/tests/agents_real.rs` holds 4 tests:
  - recorded Claude native-permission replay
  - recorded ask-rule replay with deny / always / rule
  - Codex notify replay
  - Enter encoding in kitty vs legacy mode
- `agent_state` unit tests run against real screens and titles.
- The fixtures are in `crates/midnad/tests/fixtures/`, with paths sanitized.

## Remaining gaps

- **midna-app has to encode keys for the kitty mode too.**
  - `Frame` carries `decckm` / `bracketed_paste` but not the kitty flags, and Enter from the GUI as a bare `\r` won't submit to Claude.
  - Either add the flags to `Frame`, or use libghostty's key encoder with `Terminal::kitty_keyboard_flags`.
  - I didn't touch proto or the app.
- **Codex approvals weren't exercised live.**
  - The user's `~/.codex/config.toml` sets `approval_policy="never"`, and midna doesn't override it.
  - The Codex prompt patterns come from the spike.
  - Codex has no cost signal, and interrupted Codex turns count no message because no notify arrives.
- **Startup dialogs aren't surfaced as needs-you.** Claude's folder trust and Codex's folder trust or hooks review leave the session `idle`.
- **Native prompts won't appear for this user in normal use.** Their global `defaultMode` is `bypassPermissions`, so in practice only midna rules gate tools. I forced `default` for testing through the temp project's `.claude/settings.local.json`.
- **midna's `statusLine` replaces the user's** (Saggar bridge + combined script) inside midna terminals. `agents.claude.statusline=false` restores the user's line, but then cost isn't tracked.
- **Resolving a `permission_prompt` whose dialog is already gone** reports `approved` even though no key was sent.
- **`idle_prompt` Notification (~60s after Stop) wasn't observed live.**
- **Side effects of testing:**
  - Claude saved its transcripts and folder trust for the two temp dirs in its own config.
  - Codex was run in `$HOME`, which is already trusted, with echo-only prompts, so `~/.codex/config.toml` wasn't modified.
- **Workspace `cargo test` doesn't pass yet**, all because of other agents' in-progress work:
  - The `midna-app` test target doesn't compile ("recursion limit reached while expanding `#[test]`").
  - `engine::snapshot_tests::incomplete_suffix_shapes` fails. That test is new and not mine.
  - `seed_insights::seeded_history_feeds_series` fails.
  - `cargo build` passes, and so does everything this work touches (`agents_real`, `daemon`, the `agent_state` unit tests, `triggers`, and the CLI tests).

## Discoverability pass: MCP, skill, help, explain

**State:** an agent midna launches now gets three things without any global config: the `midna` MCP server, a short system hint ("run `midna capabilities`…"), and `MIDNA_SKILL`. Every refusal it hits says what to do next.

**Verified live**
- Claude Code 2.1.288 accepts `--mcp-config <hooks/mcp.json> --settings <…> --append-system-prompt <hint>` together. A trivial `claude -p` run against a temp daemon showed:
  - the `midna` server `connected`, with 61 midna tools
  - Claude calling `mcp__midna__daemon_info`, which returned the terminal's `MIDNA_SESSION` (so the env reaches the MCP server)
  - the read-only allow list working in `-p` mode
- Codex 0.160.0 accepts `-c mcp_servers.midna.{command,args,env}` and `-c developer_instructions=…`. I checked this with a temp `CODEX_HOME` (`codex mcp get`, `codex debug prompt-input`). I didn't run a full Codex session with tool calls.

**Tests** (all against temp homes; agent binaries are `/bin/echo`)
- `crates/midna-cli/tests/mcp.rs`:
  - `protocol_conformance`: initialize/version negotiation, ping, tools/list (name charset, schemas, annotations, human-only wording), the JSON-RPC error codes, notifications and stray responses, tool errors with `Next:`, resources, and the guide/capabilities tools.
  - `agent_workflow_through_mcp_only`: the agent is in a midna terminal and works through MCP only.
    - It lists projects, opens a shell, runs `echo` and reads `mcp-5`.
    - It adds a rule, gets refused on removal (pointed at `rule_request_removal`), then requests removal. The rule stays until the human acts.
    - It explains the rule and an action, then drafts a trigger, which ends up `needs_secret`.
    - It sets `theme`, then tries `approve.from_cli`; that is refused and deferred, and the value is unchanged.
    - It raises `blocked`. The human sees `rule_removal`, `secret_needed`, `approval` and `blocked` items, and `explain` on the agent's own terminal shows what it's waiting on.
- `crates/midna-cli/tests/cli.rs`:
  - `discoverability`: every verb's `--help` and `help <verb>`; `capabilities`, `skill` and `schema` offline.
  - `explain_and_guidance_against_a_daemon`
  - `agents_get_mcp_skill_and_hint_without_global_config`: covers the argv order, the Codex `-c` overrides, both settings off, and `MIDNA_SKILL`.
- Unit tests in `help.rs` (every method is reachable), `guide.rs` and `mcp.rs` (names, toolified descriptions, `$defs` refs).

**Workspace:** `cargo build` and `cargo test` pass, including `seed_insights`, which no longer depends on the date.

**Open**
- Codex may ask before it runs MCP tools, depending on the user's approval policy; not exercised.
- Claude defers MCP tools behind ToolSearch, so in practice the agent finds midna through the system hint and `midna capabilities`, not through the tool list. That's why the hint is on by default.
- The parity gaps that remain are listed in DECISIONS: split pane, `commands.json`, app updates, ask-bar memory, and rule restore.

