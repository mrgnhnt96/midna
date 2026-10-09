# midna architecture (build contract)

midna is a macOS-first terminal for working with AI agents. It replaces Saggar.

- Projects contain terminals, and terminals run shells, monitors or agents (Claude Code, Codex).
- **Everything is AI-managed and customizable.** Agents drive midna through the `midna` CLI, JSON-RPC, or MCP.
- The UI exists only where a human must glance, decide, or do something agents may not do.

Read this whole file before you write code. Also read `docs/design/*.dc.html`. Those are the approved screen designs: plain HTML with inline styles, and the JS at the bottom of each file holds sample data.

## Non-negotiable principles

1. **Daemon is the source of truth.** `midnad` owns PTYs, terminal engines, projects, rules, triggers, settings, the event log and the audit trail. The GUI is a client. Closing the GUI never kills shells.
2. **Every capability is an RPC method, and every method is discoverable.**
   - `rpc.discover` returns an OpenRPC document generated from the method catalog in `midna-proto`.
   - The CLI's `midna schema` prints it, and `midna mcp` exposes the same catalog as MCP tools.
   - When you add a feature, you add it to the catalog, with a description written for an agent reader.
3. **Agents may add, humans may remove.**
   - Agents can create rules, but only a human can remove one. An agent may *request* removal, which becomes a needs-you item.
   - Secrets (webhook signing secrets) are human-only.
   - OS permission grants are human-only.
   - Settings marked `human_only` can be read by agents but not written by them.
4. **Everything is logged.** Every state change is an `Event` with a monotonically increasing `seq`, appended to `events.jsonl`. Every RPC call that *acts* (including denials) is also an `audit` event. Clients can `events.subscribe { since_seq }` and replay.
5. **Customizable by prompting.** Settings, keybindings, themes, header/row scripts, triggers and rules all live in daemon state, change through RPC, and are visible in the UI. The UI shows the CLI equivalent instead of building big forms.
6. **Only real signals.** Status comes from hooks, exit codes, the PTY title (OSC 0/2 glyphs), and a screen check. Git data comes from git. Don't invent data.

## Repository layout

```
Cargo.toml            workspace
env.sh                source before building (Zig 0.15.2 + SDK shim + LIBGHOSTTY_VT_SYS_OPTIMIZE=ReleaseFast)
.toolchain/           (gitignored) zig, fakebin/xcrun, ghostty-src
crates/
  midna-proto/        serde types, method catalog, OpenRPC generation, blocking + async client
  midnad/             daemon binary (lib + bin; the lib holds modules so they're testable)
  midna-cli/          `midna` binary: CLI verbs, `midna mcp` (stdio MCP bridge), `midna hook <agent>`
  midna-app/          GPUI app (gpui-kit 0.7), binary `midna-app`, bundled as Midna.app
  midna-relay/        self-hostable webhook relay (later phase)
docs/
  ARCHITECTURE.md     this file
  design/             approved .dc.html boards (Main, CommandBar-A, NeedsYou-C, Rules-B, Triggers-A, Insights-A, Settings-A, Onboarding-B; Settings-C is the older one-table Settings)
spikes/               proven prototypes; reuse code from here freely
```

**Build.**
- Run `. ./env.sh && cargo build`.
- libghostty-vt is pinned at 0.2.2 and needs Zig 0.15.2 exactly. It must be built with `LIBGHOSTTY_VT_SYS_OPTIMIZE=ReleaseFast`.
- Enable grapheme mode 2027 on every terminal.
- The Rust edition is 2024.

**Paths.**
- `MIDNA_HOME` defaults to `~/Library/Application Support/com.mrgnhnt.midna`. Tests must set `MIDNA_HOME` to a temp dir.
- Socket: `$MIDNA_HOME/midnad.sock`.
- State: `$MIDNA_HOME/state.json`, written atomically with write-temp + rename.
- Event log: `$MIDNA_HOME/events.jsonl`.
- Bundle id `com.mrgnhnt.midna`; daemon label `com.mrgnhnt.midna.daemon`.

## Wire protocol

**Control connection.**
- Newline-delimited JSON-RPC 2.0 over the Unix socket: one JSON object per line. Requests, responses and notifications all use this format.
- Server-pushed notifications use `method: "event"` (params = `Event`) and `method: "window.command"` (to GUI clients).

**Frame stream connection (terminal rendering, "B-snap", proven in `spikes/gpui-terminal/src/transport.rs`).**
- A client opens a second connection and sends one JSON line: `{"jsonrpc":"2.0","id":1,"method":"stream.attach","params":{"session":"<id>","cols":..,"rows":..,"cell_w":..,"cell_h":..}}`.
- The daemon answers with one JSON line (result `{ok:true}`). From then on the connection is binary in both directions:
  - Daemon → client: length-prefixed frames, `u32 LE len` + payload = `Frame::encode()` (the dirty-rows frame format from the spike). Moving that encoder into `midna-proto` is fine.
  - Client → daemon: 1-byte tags.
    - `0x01` = want (credit for the next frame).
    - `0x02` + u16 cols + u16 rows + u32 cw + u32 ch = resize.
    - `0x03` + u32 len + bytes = input. Input may also go through `session.input` on the control connection.
- The daemon only sends a frame when the client has credit, and merges dirty rows meanwhile. That's what makes 120fps on an 80MB `cat` possible.

**Caller identity.**
- **GUI = human.** The daemon reads the peer pid (`LOCAL_PEERPID`) and resolves its executable with `proc_pidpath`. If that is the midna-app binary (`MIDNA_APP_PATH` env override for dev), the connection's role is `human`.
- **CLI inside a midna terminal = agent.** Calls carrying `MIDNA_SESSION` (sent as the `caller.session` field in params, see below) are `agent` with that session id.
- **`caller.no_wait: true`** (CLI `--no-wait`, MCP `no_wait: true`) never changes the role: a mutating call runs on its own thread, and if it raises an approval the caller gets error `6` with `data.needs_you_id` at once while the call finishes when the human answers (the approval then stays open for at least a day).
- **Anything else** is `agent` with no session.
- **Document the limitation honestly.** Same-user processes can't be fully authenticated, so human-only actions requested over the CLI always become a needs-you confirmation in the GUI and never execute directly.
- **Hardening (see `docs/SECURITY.md`).** Signed builds check the GUI's code signature (Team ID + identifier) instead of its name; `MIDNA_APP_PATH` is debug-only; a GUI binary running inside a midna terminal is an agent; `caller.session` must match the terminal the calling process runs in; the app's `MIDNA_DEBUG_*` drivers compile out of release builds.

**Errors.** JSON-RPC error codes: `-32601` unknown method, `-32602` bad params, `1` refused by policy, `2` human-only, `3` not found, `4` conflict, `6` pending (a `caller.no_wait` call is waiting on the human: `data.needs_you_id`; the call carries on when they answer and `needs_you.get` reports how it ended).

## Domain model (midna-proto)

All ids are short lowercase strings: 8 hex chars for sessions, `p_xxxxxx` for projects, `r_…` for rules, `t_…` for triggers, `n_…` for needs-you items, `d_…` for deliveries. Timestamps are RFC 3339 UTC strings. The domain types use `#[serde(rename_all = "snake_case")]`.

- **Project** `{ id, name, path, icon?: string, order: u32, commands: [ProjectCommand{name, run, pinned}], last_opened_at?, auto_created: bool }`. `auto_created`: midna added it for an agent's `session.open` with a `cwd` no project covered; agents may `project.remove` such a project once none of its terminals runs (the human adding or editing it clears the flag).
- **Session** (a terminal)
  - Fields: `{ id, project_id, name, kind: shell|agent|monitor, agent?: claude|codex, cwd, command: [string], pid?, title, status: Status, created_at, last_activity_at, keep_on_top: bool, git?: GitInfo }`
  - **Status**: `{ state: idle|working|needs_you|done|failed|exited, reason?: string, exit_code?: i32, since }`
  - **GitInfo**: `{ branch, ahead, behind, added, removed, files, pr?: {number, url, checks: none|pending|passing|failing, failing_count} }`
- **NeedsYou** item: `{ id, session_id?, project_id?, kind, title, detail, screen_excerpt?: [string], asked_by: Actor, created_at, bulk_safe: bool, approval?: ApprovalRequest }`
  - `kind` is one of: `approval`, `permission_prompt`, `blocked`, `note`, `failed`, `trigger_waiting`, `rule_removal`, `secret_needed`.
  - **ApprovalRequest** `{ action: PolicyAction, matched_rule?: rule_id, target_session?: session_id }`. An open approval is **withdrawn** (resolution `{kind: withdrawn, reason}`; a caller blocked on it gets deny with source `withdrawn`) when the terminal it is about (`target_session`) or the asker's terminal closes, or the asker's connection drops.
  - An agent's folder-trust startup dialog ("Do you trust the files in this folder?") is a `permission_prompt` titled `Trust this folder? <cwd>`, raised from the screen (no hook fires while it is up); approve picks Yes, deny picks No.
  - **Resolutions**:
    - `approve{ scope: once|minutes(u32)|session|always }`
    - `deny`
    - `dismiss` (human only)
    - `done` ("I've done it")
    - `restart` (failed)
- **Rule**
  - Fields: `{ id, effect: allow|ask|deny, matcher: Matcher, scope: global|project(id)|session(id), expires_at?, added_by: Actor, added_at, origin?: {needs_you_id, approval_scope}, fired: u64, last_fired_at?, removal_request?: RemovalRequest }`
  - **Matcher** `{ kind: command|tool|path|cli|window, pattern: string }`. Patterns are globs, e.g. `git push --force*`, `Bash(rm -rf*)`, `close --force`.
  - **RemovalRequest** `{ requested_by: Actor, reason, at, needs_you_id }`
- **Actor** `{ kind: human|agent|system|trigger, session?: id, name?: string }`
- **PolicyAction** `{ kind: command|tool|path|cli|window, value: string, session?: id, project?: id }`
  - **Evaluation**:
    - The most specific scope wins (session > project > global).
    - Within a scope, deny beats ask beats allow.
    - With no match, the default comes from the setting `policy.default` (default `ask` for `cli` destructive verbs, `allow` otherwise; keep the defaults table in code).
    - `policy.check` returns `{ decision, rule?: Rule, trace: [{rule_id, matched, reason}] }`. The trace powers the "test a command" view.
- **Trigger**
  - Fields: `{ id, name, source: github|bitbucket, event: string, filter: {repo?, branch?, action?, label?}, action: TriggerAction, enabled: bool, state: draft|needs_secret|active|paused, secret_set: bool, created_by: Actor, created_at, last_fired_at?, fired: u64 }`
  - **TriggerAction** is one of:
    - `start_agent{ project_id, agent, prompt_template }`, where templates use `{{pr.number}}`, `{{pr.title}}`, `{{repo}}`, `{{branch}}`, `{{sender}}`, `{{url}}`
    - `run_command{ project_id, command, background?, headless?, timeout_secs? }`: a monitor terminal as a tab, or in the sidebar's Background group (`background`), or no terminal at all (`headless`, stopped after `timeout_secs`, default 1800; its run goes on the delivery's `command_runs`)
    - `attention{ message }`
- **Delivery** `{ id, source, event, delivery_guid, received_at, verdict: verified|bad_signature|filtered|replayed|recovered|no_trigger, trigger_id?, session_started?: id, summary, command_runs?: [{ trigger_id, command, started_at, finished_at?, exit_code?, signal?, timed_out?, output?, error? }] }`. A headless run finishing emits `trigger.command_finished { trigger_id, delivery_id, run }`.
- **Setting**
  - The settings catalog is in code. Each entry is `{ key, type: bool|string|enum([..])|int|keybinding, default, description, human_only: bool, section }`.
  - Values are stored in state. `settings.list` returns key, value, default, description, human_only, and `cli: "midna settings set <key> <value>"`.
  - Keys include:
    - `theme` (dark|light|system)
    - `density` (comfortable|compact)
    - `ui.header.script` (built-in parts joined with `+`, or a custom path; default `github`)
    - `ui.row.script` (same; default `diff`)
    - `ui.status.script` (same, run for the selected terminal; default `worktree+branch`)
    - `ui.status.items` (ordered list: daemon, usage, cache, policy (off by default), webhooks, triggers, hooks, accessibility, awake, script, spacer, update, keys)
    - `updates.channel` (stable|beta)
    - `webhooks.path` (tailscale_funnel|self_relay|midna_relay|off)
    - `webhooks.port` (int, default 7787)
    - `webhooks.relay_url`
    - `agents.may_move_windows` (human_only)
    - `agents.may_close_idle` (human_only)
    - `agents.may_force_close` (human_only, default false): agents close any terminal (working ones, `close --force`, someone else's) with no default ask; rules on `close …` still apply
    - `agents.may_install_updates` (human_only, default false): an agent's `updates.install` goes to the GUI without a needs-you approval; the app's signature checks still apply
    - `approve.from_cli` (human_only)
    - `keys.*` keybindings: `keys.command_bar` = cmd-k, `keys.next_needs_you` = cmd-j, `keys.new_terminal` = cmd-t, `keys.new_agent` = cmd-shift-t, `keys.new_terminal_root` = cmd-alt-t, `keys.new_agent_root` = cmd-alt-shift-t, `keys.approve` = cmd-enter, `keys.deny` = cmd-backspace, `keys.settings` = cmd-comma, `keys.rules`, `keys.triggers`, `keys.insights`. ⌘1–9 are reserved for projects.
- **Event** `{ seq: u64, at, kind: string, actor: Actor, project_id?, session_id?, data: serde_json::Value }`
  - Kinds, dotted: `project.added|updated|removed`, `session.opened|renamed|closed|status|title|exited|input_by_agent`, `needs_you.raised|resolved`, `rule.added|fired|removal_requested|removed|expired`, `trigger.added|updated|delivery|fired`, `settings.changed`, `window.command`, `agent.turn_started|turn_ended|prompt_submitted|cost`, `daemon.started|upgraded`, `audit` (data: `{method, params_summary, outcome: ok|denied|error}`).
- **Insights** are computed in the daemon from events: `insights.summary{ range: today|yesterday|week, by?: project|agent|terminal|day }`.
  - It returns totals and rows: `turns`, `messages` (human prompt submits), `spend_usd`, `working_secs`, `waiting_secs`, `approvals`, `triggers_fired`, plus `vs_previous` deltas.
  - It must come from events only. The Insights UI is graph-based (the user wants graphs, not a daily text report); add time-bucketed series such as `insights.series{range, metric, bucket: hour|day, by?}`.

## Method catalog (initial; extend additively)

Each method has a name, a description written for agents, params/result types, `mutating: bool`, and `human_only: bool`.

| Group | Methods |
|---|---|
| `rpc` | `rpc.discover` |
| `daemon` | `daemon.info` (version, pid, uptime, home, socket), `daemon.upgrade{binary_path}` (human_only), `daemon.restart`, `daemon.stop` (human_only), `daemon.reset{keep_rules?}` (human_only) |
| `project` | `project.list`, `project.add{path,name?}`, `project.update{id,name?,icon?,commands?}`, `project.remove{id}` (human_only, except an `auto_created` project with no running terminal) |
| `session` | `session.list{project_id?}`, `session.get{id}`, `session.open{project_id, kind, agent?, name?, cwd?, command?, prompt?, agent_args?, resume?}`, `session.close{id, force?}`, `session.rename{id,name}`, `session.input{id, text, enter?: bool}`, `session.read{id, lines?: u32, screen?: bool}` (plain text), `session.resize`, `session.restart{id}`, `session.focus{id}` (emits `window.command`), `session.prompts{id}`, `session.jump_prompt{id, n | to}` (prompt fast travel) |
| `stream` | `stream.attach` (see wire protocol) |
| `events` | `events.list{since_seq?, limit?, filter?}`, `events.subscribe{since_seq?}` (connection then receives `event` notifications) |
| `needs_you` | `needs_you.list`, `needs_you.get{id, wait_secs?}` (an open item, or how it ended: resolved/withdrawn/timeout, plus a no-wait call's result or error), `needs_you.raise{kind:blocked|note, message}` (agents: "attention"), `needs_you.resolve{id, resolution}` (human for approvals; agents get `approve` only when `approve.from_cli` allows it, and only for their own session) |
| `policy` | `policy.check{action}` (no side effects), `policy.request{action}` (check + raise approval if ask + block up to `timeout_secs` waiting for the decision; used by hooks) |
| `rule` | `rule.list`, `rule.add{effect, matcher, scope, expires_in_secs?}`, `rule.request_removal{id, reason}`, `rule.remove{id}` (human_only), `rule.restore{rule}` (human_only) |
| `trigger` | `trigger.list`, `trigger.add` (agents create drafts), `trigger.update`, `trigger.set_enabled` (human_only to enable), `trigger.set_secret{id, secret}` (human_only), `trigger.remove`, `trigger.deliveries{trigger_id?, limit?}`, `trigger.replay{delivery_id}`, `trigger.test{trigger_id, payload}` |
| `webhooks` | `webhooks.status` (path, public URL, health, last delivery), `webhooks.configure{path}` (human_only) |
| `settings` | `settings.list`, `settings.get{key}`, `settings.set{key,value}` (refused for human_only keys from agents), `settings.reset{key}` |
| `insights` | `insights.summary`, `insights.activity{since?, filter?}` |
| `usage` | `usage.get` (Claude's plan usage limits, the latest any terminal's status line reported: `claude{five_hour, seven_day: {used_percentage, resets_at, expired?}, observed_at, session, limited, limited_until}`); per terminal in `session.get` `agent_info.rate_limits`; event `usage.limit_reached` |
| `keep_awake` | `keep_awake.status` (held, reason, line, window_open, next_on/next_off, work, battery, schedule, settings, today), `keep_awake.set{enabled|on?, mode?, start?, end?, days?, hours?, min_battery?, linger_mins?, today?}` (returns the status); event `keep_awake.changed` |
| `clock` | `clock.sleeps{since?}` (when the Mac slept, the last 14 days: `sleeps[{from, to, secs}]`; idle triggers, `idle_for` messages, needs-you expiry and restart grace count awake time only) |
| `window` | `window.list`, `window.command{action: front|keep_on_top|pop_out|snap|close|open_screen|split, target?, value?}` (daemon forwards to the GUI; policy-checked; `agents.may_move_windows` gates all but front, open_screen and split) |
| `notify` | `notify.list{session?, global?}`, `notify.set{session? \| global, key, value: bool\|null}` (per-terminal overrides in `Session.notify`), `notify.send{title, body?, session?, sound?, category?, id?, open?, actions?, wait_secs?}` (agents: dedupe + 6/min; returns `id`, `via`, `response`), `notify.withdraw{id}` (event `notify.withdrawn`), `notify.response{id, wait_secs?}`, `notify.respond{id, response}` (app only; event `notify.responded`), `notify.media{kind?}`, `notify.import{path, use_for?}` (sound/image into MIDNA_HOME/notify), `notify.remove{name}`, `notify.test{category?, session?}`, `notify.play{sound, volume?, session?}` (a kind's or a named sound, no banner; event `notify.sound`; agents 6/min) |
| `ui` | `ui.commands.list`, `ui.commands.add{command, replace?}`, `ui.commands.remove{id}` (palette user commands, `$MIDNA_HOME/commands.json`) |
| `updates` | `updates.status`, `updates.check`, `updates.install` (human_only), `updates.report` (GUI only); the app owns the updater, midnad proxies |
| `permissions` | `permissions.status` (what midnad can see; `midna permissions open <name>`) |
| `agent` | `agent.hook{agent, event, payload}` (called by `midna hook`; drives status, turns and approvals) |
| `script` | `script.run{session_id, slot: header|row|status, script?}` (executes the configured script, or with `script` a script path listed in `ui.status.items`; returns segments `[{text, tone?: dim|ok|err|need|work|accent, icon?, tooltip?, link?, mono?, join?}]`; contract in `midna explain scripts`) |

## Agent status and hooks (proven in `spikes/agent-status`)

**Launching agents.** midna launches agents itself so it never touches the user's global config.
- Claude: `claude --settings <MIDNA_HOME/hooks/claude-settings.json>`. That file registers `midna hook claude` for these hook events:
  - SessionStart
  - UserPromptSubmit
  - PreToolUse
  - PostToolUse
  - Notification
  - Stop
  - SubagentStop
  - SessionEnd
- Codex: `codex -c notify=[...]` (Codex hooks need trust, so use notify and the screen/title instead).
- Both also get the `midna` MCP server (`claude --mcp-config <MIDNA_HOME/hooks/mcp.json>`, `codex -c mcp_servers.midna.*`; setting `agents.mcp`) and a 2–3 line system hint pointing at `midna capabilities` (`--append-system-prompt` / `-c developer_instructions`; setting `agents.system_hint`). Every terminal gets `MIDNA_SKILL` = `$MIDNA_HOME/hooks/SKILL.md` (source `crates/midna-cli/assets/SKILL.md`, also `midna skill`).

**Hook to daemon.** `midna hook claude` reads the hook JSON from stdin and calls `agent.hook`. For PreToolUse it calls `policy.request` and prints Claude's hook decision JSON (`permissionDecision` allow / deny / ask).

**Every terminal's environment:**
- `MIDNA_SESSION=<id>`
- `MIDNA_PROJECT=<id>`
- `MIDNA_SOCKET=<path>`
- `TERM_PROGRAM=midna`
- `PATH` with the midna CLI dir prepended

**Gaps no hook covers.** These are approval answered, Esc during a tool, and Esc or ctrl-c at a prompt. Fill them from the OSC title glyph (`◐◑` = working, `✳` = stopped) plus a screen check.
- Only answer a prompt by sending keys after the screen check confirms a prompt is visible.

## GUI (midna-app)

Build it with GPUI via `gpui-kit` 0.7 (see `spikes/gpui-terminal` for a working renderer that paints per-cell glyphs from a cache inside `window.paint_layer`).

**Windows.**
- **Main window**: the left sidebar (projects → terminals; Today card; Triggers/Rules/Settings buttons), the terminal header, the terminal pane, the approval banner and the bottom status bar. Exactly as `docs/design/Main.dc.html`.
- **Overlays in the main window:**
  - the ⌘K palette (CommandBar-A)
  - the needs-you card stack (NeedsYou-C)
  - Rules (Rules-B), Triggers (Triggers-A) and Insights (graph-based; Insights-A is NOT the target — design fresh graphs), which replace the terminal pane; the sidebar stays
- **Settings is its own separate window** (Settings-A: sidebar sections, search over both panes).
- **Pop-out / keep-on-top** terminals are separate windows (`NSWindow` level). Satellites use `WindowKind::PopUp`.

**⌘K fallback.** Free text that doesn't match a command opens a new agent terminal in the current project with that text as the prompt. Pick Claude or Codex and this project or root.

**Theme.** The tokens are in the `<helmet>` of every design board (`.t-dark` / `.t-light`). Fonts are Atkinson Hyperlegible Next for the UI and JetBrains Mono for terminal and code. Bundle the font files or fall back to system fonts.

**Keybindings.** Read them from settings (`keys.*`) and live-update them on `settings.changed`.

**Kass composer.** This is a later phase. A native `NSTextView` appears only when dictation starts (the distributed-notification handshake `com.mrgnhnt.kass.dictationWillBegin` / `dictationReady` / `dictationDidEnd`; Info.plist key `KassDictationHandshake`). See `spikes/kass-composer`.

## Later phases (do not block MVP on these)

- **Daemon in-place upgrade:** same-PID `execv`, PTY fds inherited, VT state saved with the libghostty formatter (`spikes/daemon-reexec`).
- **SMAppService install and auto-update** (`spikes/install-update`).
- **`midna-relay`.**
- **Kass composer.**
- **SSH.**

## Testing rules (for every agent)

- Tests use a temp `MIDNA_HOME`. Never touch the real one, `~/.claude`, `~/.codex`, Saggar's config, or any global config.
- GUI checks:
  - Screenshot only your own window, with `screencapture -l <windowid>`. Never take a full-screen screenshot.
  - Never send keystrokes or clicks to other apps' windows.
- Clean up after yourself: background processes you started, temp dirs, and test windows.
- Don't run `git commit` or push. The user reviews the work.
