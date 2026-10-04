# Foundation status (midna-proto, midnad, midna-cli)

## How to run

```sh
. ./env.sh && cargo build
export MIDNA_HOME=/tmp/midna-dev        # never use the real home while developing
target/debug/midnad --foreground        # or `midnad` alone: detaches and logs to $MIDNA_HOME/midnad.log
target/debug/midna info                 # in another shell
target/debug/midna open -- /bin/zsh     # prints the new terminal id
target/debug/midna send <id> 'ls'
target/debug/midna read <id>
```

**Daemon flags:** `midnad [--foreground|-f] [--home DIR] [--socket PATH]`. SIGINT stops (hangs up every terminal); SIGTERM is a graceful restart that keeps terminals (see `docs/STATUS-upgrade.md`).

**Socket resolution**, used by both the daemon and the clients:
1. `MIDNA_SOCKET`
2. else `$MIDNA_HOME/midnad.sock`
3. else `~/Library/Application Support/com.mrgnhnt.midna/midnad.sock`

**Other environment variables:**
- `MIDNA_APP_PATH` makes that executable the human GUI. By default, an executable named `midna-app` or one inside `Midna.app` is the GUI.
- `MIDNA_CLI_PATH` overrides the CLI path used for hooks and `PATH`.
- `MIDNA_NO_GH=1` disables the `gh` lookups.

**Tests:**

```sh
cargo test -p midna-proto -p midnad -p midna-cli
```

- 7 proto unit tests
- 7 daemon unit tests and 19 daemon integration tests
- 3 CLI unit tests and 4 CLI integration tests

Each test starts an in-process daemon on `/tmp/midna-{t,c}-<pid>-<n>` and cleans it up afterwards.

## What works

### midna-proto

- Every domain type in the contract, with serde and JSON Schema.
- The method catalog (50 methods), each with a description, params and result schemas, and the `mutating`, `human_only` and `stub` flags.
- `openrpc()`, the frame codec, the settings catalog, path resolution, and RFC 3339 helpers.
- A blocking `Client`:
  - `call` and `call_value`
  - `subscribe`, which returns an iterator of `Event`; `next_notification` also yields `window.command`
  - `attach_stream`, returning a struct with `want`, `resize`, `input`, `next_frame` and `writer()`

### midnad

**Core**
- Newline JSON-RPC over the Unix socket.
- Caller roles from `LOCAL_PEERPID` + `proc_pidpath`, plus the `caller.session` / `caller.role` downgrade.
- `state.json` is written atomically by a debounced saver.
- `events.jsonl` is append-only with a monotonic `seq`.
- Every mutating call produces an `audit` event, denials included.
- `events.list` and `events.subscribe`, with gap-free replay.

**Sessions**
- PTY spawn with the contract's environment variables.
- One engine thread per session (libghostty, grapheme mode 2027).
- `stream.attach` with credit-based B-snap frames; multiple clients per session are fine.
- `session.input`, `read` (lines or the exact screen), `resize`, `rename`, `close`, `restart` and `focus`.
- Exit codes set `exited` or `failed`. A failed monitor or agent raises a needs-you item.
- The OSC title is tracked, and the title-glyph status heuristics run with a screen check.

**Policy and needs-you**
- The settings catalog, plus `settings.*`. `human_only` keys become needs-you confirmations when an agent tries to set them.
- `needs_you.*`: list, raise (blocked/note) and resolve. Resolve handles approve with a scope, deny, dismiss, done and restart, with the permission rules from the contract.
- `rule.add`, `rule.list`, `rule.request_removal` and `rule.remove` (human only), with expiry.
- `policy.check`, which returns a trace.
- `policy.request`, which blocks with a timeout and raises an approval item. Approval scopes create rules with `origin`.
- Agent CLI and window verbs are policy-gated.

**Agents, insights and scripts**
- `agent.hook` uses the spike's state machine to drive status, turns and prompt counts, raise and auto-clear permission prompts, and record cost from Claude's status line.
- `insights.summary` (today, yesterday or week, optionally grouped by project, agent, terminal or day, with `vs_previous`) and `insights.activity`, both computed from events only.
- `window.command` is forwarded to subscribed GUI connections and gated by `agents.may_move_windows`.
- `script.run` supports:
  - the built-ins `github`, `github+agent`, `git-diff-stats` and `none`
  - custom executables, with a 5s timeout, the session JSON on stdin, and JSON segments on stdout
- GitInfo (branch, ahead/behind, `git diff --shortstat HEAD`, and the PR plus checks via `gh`, cached for 30s) refreshes every `git.refresh_secs` and emits `session.git`.

**Agent launching**
- `$MIDNA_HOME/hooks/claude-settings.json` is written at startup. It holds all 8 hook events and the status line.
- Claude launches as `claude --settings <file> [prompt]`.
- Codex launches as `codex -c notify=["<midna>","hook","codex","notify"] [prompt]`.
- Neither touches the user's global config.

### midna-cli (`midna`)

- **Verbs:**
  - `info`, `schema`, `list`, `open`, `close`, `rename`, `read`, `send`, `focus`
  - `attention [--note]`, `needs`, `approve [--scope …|--deny]`, `check`
  - `rules list|add|request-removal|remove`
  - `settings list|get|set|reset`
  - `events [--follow] [--since N] [--kind P]`, `insights`, `window …`
  - `hook <claude|codex>`, `mcp`, and `call <method> [json]`
- **Output.** Human-readable by default; `--json` for machine output.
- **Exit codes.** 0 ok, 1 refused or failed, 2 bad args, 3 daemon unreachable.
- **`midna hook claude`:**
  - For PreToolUse, it prints Claude's `permissionDecision` JSON (allow, deny or ask) when a midna rule or a human decided. It prints nothing when midna has no opinion.
  - It is always silent and exits 0 outside midna or when the daemon is down.
- **`midna mcp`.** A stdio MCP server: `initialize`, `tools/list` (48 tools, with the catalog's descriptions and schemas) and `tools/call`.

## Stubbed (later phases)

These methods are in the catalog but return error `5` ("not implemented yet"):
- ~~`daemon.upgrade`~~ (implemented, see `docs/STATUS-upgrade.md`)
- `trigger.add`, `trigger.update`, `trigger.set_enabled`, `trigger.set_secret`, `trigger.remove`, `trigger.replay`, `trigger.test`
- `webhooks.status`, `webhooks.configure`

`trigger.list` and `trigger.deliveries` already work; they read the empty state lists. There's no async client yet; the blocking client on a thread works with GPUI.

## Known issues and limitations

- **Not verified with real agents.**
  - I didn't launch real `claude` or `codex`, because that would write to `~/.claude` and `~/.codex`.
  - The hook wiring and payload handling are tested with recorded payload shapes from `spikes/agent-status`. Still unconfirmed:
    - that `claude --settings` accepts the generated file
    - that the status line still renders
    - that the Codex `notify` override works
  - The prompt patterns behind the title glyphs and the screen check are best-effort and need tuning against real sessions.
- **Same-user processes can't really be authenticated.** The human role is a naming convention: an executable named `midna-app` or set by `MIDNA_APP_PATH`. That's why human-only requests from the CLI turn into needs-you items and never run directly.
- **Agent policy gates can block for a long time.** An `ask` on a gated agent verb, or from `policy.request`, holds that connection's thread for up to `policy.request_timeout_secs` (300s).
- **Terminals don't survive a daemon restart.** The re-exec upgrade is a later phase; leftover sessions are marked `exited`.
- **The event log stays fully in memory**, with no compaction. `session.status` fires on every hook reason change (for example `tool Bash` → `after Bash`), so the log grows about one event per tool call.
- **Input sent over `stream.attach` isn't logged.** It doesn't produce `session.input_by_agent` the way `session.input` does.
- **Multiple GUIs fight over size.** If several GUI clients attach to the same session with different sizes, the last resize wins.
- **Git and `gh` only refresh live sessions.** The `gh` lookup runs on the git refresh thread; a slow network can delay git updates for other sessions by up to 10s per lookup.

## Added in the closing pass

- Methods: `daemon.reset`, `rule.restore`, `ui.commands.list|add|remove`, `updates.status|check|install|report`, `permissions.status`, `session.clear`; `window.command` action `split`. Settings `ui.ask.agent`, `ui.ask.scope`.
- Events: `policy.decided`, `rule.restored`, `ui.commands_changed`, `updates.status`, `updates.requested`, `daemon.reset`.
- CLI: `midna commands`, `midna updates`, `midna permissions`, `midna daemon reset`, `midna rules restore`, `midna window split`.
- Event log: monthly files under `$MIDNA_HOME/events/`, 62-day in-memory window, old reads from disk.
- Tests: `crates/midnad/tests/ui_updates_reset.rs`, `eventlog` and `rpc::ui` unit tests. `cargo test --workspace` passes.
