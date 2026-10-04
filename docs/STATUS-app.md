# midna-app status (main window phase)

## How to run

```sh
. ./env.sh
cargo build -p midna-app

# against midnad (default). A temp home is safest for dev:
export MIDNA_HOME=$(mktemp -d /tmp/mh.XXXX)       # keep the path short (Unix socket limit)
MIDNA_APP_PATH=$PWD/target/debug/midna-app target/debug/midnad --foreground &   # GUI = human
target/debug/midna-app

# without a daemon: the design's sample data, with real PTYs per terminal
MIDNA_BACKEND=fake target/debug/midna-app
MIDNA_BACKEND=fake MIDNA_FAKE_SETTINGS=theme=light,density=compact,ui.row.script=git-diff-stats target/debug/midna-app
```

Dev env (the `MIDNA_DEBUG_*`, `MIDNA_SELECT` and updater-override variables work in debug builds, or release builds with `--features dev-drivers`, only; see `src/dev.rs` and docs/SECURITY.md):

| Variable | Effect |
|---|---|
| `MIDNA_SELECT=<session>` | Initial selection |
| `MIDNA_W` / `MIDNA_H` | Window size |
| `MIDNA_DEBUG_APPROVE_MENU=1` | Opens the approve dropdown |
| `MIDNA_DEBUG_SCREEN=rules\|triggers\|insights\|commands\|needs` | Opens that screen or overlay |
| `MIDNA_DEBUG_KEYS=cmd-t,e,c,h,o,enter` | Sends keystrokes through GPUI's own dispatch: bindings, then key handlers, then the IME input handler |
| `MIDNA_NO_ACTIVATE=1` | Doesn't take focus on launch |
| `MIDNA_DEBUG=1` | Logs backend events |

Offscreen screenshot (works while the screen is locked):

```sh
cargo build -p midna-app --features snapshot
MIDNA_SNAPSHOT=out.png MIDNA_BACKEND=fake target/debug/midna-app
```

## What works

Everything below was verified against the fake backend and against a real midnad with a temp `MIDNA_HOME`.

- **Window chrome.** Transparent titlebar with the native traffic lights over the sidebar. The 44px sidebar strip and the empty header space drag the window, and a double-click zooms it.
- **Sidebar (264px):**
  - "N need you ⌘J" button (shows "Nothing needs you" when empty).
  - Project groups: uppercase name, need badge, and a "…" menu (new terminal / new agent here).
  - Rows: status dot (needs-you ring, working, done, failed, idle outline), name, agent icon (Claude / Codex / monitor / shell SVGs from the board), the attention line for needs-you/failed (reason · age), and optional `ui.row.script` segments.
  - Compact density.
  - Footer: the Today card (turns / messages / $ from `insights.summary`; it opens Insights) and the Triggers / Rules / Settings buttons.
- **Header:** dot, name, agent icon, then `script.run` header segments (branch, diff stats, PR + checks; links open in the browser). The toolbar has split (toast: not built), pop out / keep on top (a separate `WindowKind::PopUp` window with its own stream), restart (`session.restart` + re-attach), and more (copy id, close terminal).
- **Terminal pane.** Port of the spike's glyph-cache renderer inside `paint_layer`, fed by `stream.attach` with credit-based `want`.
  - Resize: the grid recalculates from layout and sends tag 0x02.
  - Keyboard input, including modifiers, arrows, ctrl and alt-meta.
  - IME marked text.
  - ⌘V paste (bracketed).
  - Drag selection, ⌘C and ⌘A.
  - Cursor styles and blink.
  - Default colors follow the theme.
  - Verified live: typing `echo Hi` ran in a daemon shell.
- **Approval banner.** Shows for the selected session's approval item: an Approve split button with a dropdown (once ⌘↩ / 15 min / 1 h / this session / always = adds a rule) and Deny. Every option calls `needs_you.resolve`. Verified live: ⌘↩ resolved a pending `policy.request` as **human** (`decision: allow, source: human`).
- **Status bar:** midnad state and terminal count, policy (`policy.default`) and rule count, webhooks path and health, triggers today, and live key hints.
- **Shortcuts from `keys.*`**, rebuilt live on `settings.changed`: new terminal / new agent (same project, and at root), ⌘J next needs-you, ⌘1–9 projects, ⌘↩ / ⌘⌫ approve/deny, ⌘K, ⌘, and Escape. Verified: ⌘T opened and selected a new shell in the same project, and changing `keys.next_needs_you` to `cmd-u` updated the labels live.
- **Theme.** `dark`, `light` and `system` (follows the macOS appearance), switched live on `settings.changed` (verified with `midna settings set theme light`).
- **Fonts.** Atkinson Hyperlegible Next (Regular, Medium, Bold) and JetBrains Mono (Regular, SemiBold, Bold, Italic, BoldItalic) are embedded from `crates/midna-app/assets/fonts` (OFL texts included), falling back to Helvetica Neue and Menlo.
- **"midnad not running" state.** Shows the start command, socket path and error, and reconnects every second. After a reconnect the app refetches everything and re-attaches the terminal.
- **Hooks for the next agents:**
  - `app::Screen` {Terminal, Rules, Triggers, Insights}, which replaces the terminal pane.
  - `app::Overlay` {CommandBar, NeedsYou}.
  - `app::Menu`.
  - Actions in `actions.rs`: `ToggleCommandBar`, `OpenNeedsYou`, `OpenRules`, `OpenTriggers`, `OpenInsights`, `OpenSettings`, `Dismiss`, `SplitRight`, `PopOut`, …
  - Placeholder views in `ui/screens.rs` (each shows the CLI equivalent) and a separate Settings window in `ui/settings.rs` (lists `settings.list` with each key's CLI).
  - `window.command` handling: front, open_screen, pop_out / keep_on_top.

## Screenshots (`docs/screens/`)

- `fake-dark.png`, `fake-light.png`: the design's sample data in both themes.
- `fake-dark-approve-menu.png`: the approve dropdown.
- `fake-light-compact-rowscript.png`: compact density with `git-diff-stats` row segments.
- `fake-dark-insights-placeholder.png`, `fake-dark-commandbar-placeholder.png`: placeholder hooks.
- `real-daemon-dark.png`: live midnad, with a shell waiting in `policy.request` showing the banner and its attention line.
- `not-running.png`: no daemon.

The screen was locked during this session, so these are offscreen renders (`--features snapshot`) of the window only, without the native traffic lights. An earlier `screencapture -l` of the live window, taken while unlocked, matched them.

## Gaps / next

- **Scrollback.** The stream protocol has no scroll message, and the frames carry no mouse-reporting mode. Wheel scrolling and mouse reporting to TUIs need a protocol addition (for example a tag `0x04` with a scroll delta, and mouse modes in the frame).
- **Selection** is limited to the visible screen: no word or line double-click and no auto-scroll.
- **Header segments from the real daemon** have no icons or join hints (see DECISIONS: GUI). Diff stats render as separate segments spaced 14px apart, not "+N −N ×N" as one group.
- **Split** panes and the "…" menu contents beyond copy and close are not built. The need-dot pulse animation is not implemented.
- **Pop-out** windows always float; keep-on-top isn't toggled per session or persisted (`session.keep_on_top` is unused).
- The ⌘K palette, the needs-you card stack, the Rules / Triggers / Insights screens and Settings-C are placeholders.
- **Webhooks:** `webhooks.status` returns code 5 (not implemented) on the daemon today, so the status bar shows "Webhooks off".
- **Observed in midnad (not mine to fix):** after killing midnad, a `--monitor "while true; …"` child survived as an orphan, while plain shells exited.

## Rules and Triggers screens

Both replace the terminal pane (sidebar stays) and open from the sidebar buttons, ⌘K, `keys.rules` / `keys.triggers`, and `window.command open_screen`. Code: `ui/rules.rs`, `ui/triggers.rs`, `ui/screen_kit.rs`. Decisions are in DECISIONS.md under "GUI: Rules and Triggers screens".

**Rules (Rules-B)**
- **Removal requests band** on top, hidden when empty: the rule (effect pill, pattern, scope), the asking terminal with its agent icon, “reason”, “asked 4m ago”, and Remove (human) / Keep. The header shows "N removals proposed".
- **Three lanes** (Global / Project / Terminal), grouped by project or terminal and sorted deny → ask → allow, then by last fired. Each card shows:
  - effect, pattern, and a "fired in the last 2 min" dot
  - kind · who added it (agent icon + terminal, or "you via approval") · when
  - N× · last fired
  - the expiry bar with its countdown
  - "↳ from approval · Approve for 15 minutes · terminal"
  - "removal requested ↑", which flashes the request
  - otherwise Remove → "Remove for every … terminal?" → `rule.remove` → an Undo toast (re-adds).
- **Test strip:** kind chip + field. Enter runs `policy.check` as the selected terminal and shows the decision, the winning rule and the trace.
- **Last fired feed:** live `rule.fired` rows (time, terminal, input, verdict, → rule, outcome). An ask's answer is filled in from `needs_you.resolved`. Click highlights and scrolls to the rule. Pause/Resume holds new rows, and Clear highlight resets.
- **Empty state:** "No rules yet" with "Ask: deny force pushes in kass"-style suggestions, which open ⌘K prefilled.

**Triggers (Triggers-A)**
- **Path strip** from `webhooks.status`: dot, path name, free/paid chip, host, health, detail. "Change path" menu → `webhooks.configure` (human only). Funnel-down alert with "Ask: <fix>", and a recovered-deliveries banner.
- **List:** "Waiting on you" drafts (Needs secret / Ready to enable, drafted by …), then triggers with switches (`trigger.set_enabled`), event · repo, last fired, and an "Edit by asking" hint.
- **Detail:** name, project and source chips, last fired, switch.
  - Secret callout with a bullets-only paste field → `trigger.set_secret`, or Enable callout. Discard confirms.
  - When / Only if / Then (icon, "Start Claude in midna as …", template) / Secret (••••, "Set today 15:00 · Keychain", Replace…).
  - Recent deliveries with verdict badges, subject, outcome, a recovered chip and Replay (`trigger.replay`). "Not matched by any trigger" rows (bad signature: can't replay).
- **Empty state** with "Ask: Start Claude on every PR opened in kass" suggestions.
- **Error 5** shows "Not available yet" and rechecks every 20s.

**Verified against a real midnad** (temp `MIDNA_HOME`, `MIDNA_SECRETS=file`, `MIDNA_AGENT_BIN=/bin/echo`, receiver on a random port):
- 8 rules seeded by agent terminals (`midna rules add`), including a 15-min session rule.
- Two `rules request-removal` from agent terminals.
- `policy.request` fires (deny/allow/ask), with one ask left waiting.
- Remove (with Undo toast), Keep, and the test strip.
- 4 agent-drafted triggers. Secrets set and triggers enabled through the GUI code path; the secrets are absent from state.json, events.jsonl and the log.
- Signed GitHub deliveries posted to the receiver: verified (started a terminal), filtered, no_trigger and bad_signature, plus a GUI Replay.

**Screenshots** (`docs/screens/`, offscreen `--features snapshot`, 1440×900):
- `rules-dark.png`, `rules-dark-confirm.png`, `rules-light.png`, `rules-dark-empty.png`
- `triggers-dark.png`, `triggers-dark-secret-pathmenu.png`, `triggers-light-ready.png`, `triggers-dark-empty.png`

**Gaps**
- No "no rule · default" feed rows; the daemon only emits `rule.fired` when a rule decided (TODO for daemon).
- The trigger filter shows only structured filters (repo, branch, action, label); the board's expression filters don't exist in the daemon.
- The "Ask" → ⌘K prefill wasn't exercised in a screenshot. Click-driven flows were exercised through the `MIDNA_DEBUG_RULES` / `MIDNA_DEBUG_TRIGGERS` hooks, not real clicks.
- `cargo test -p midna-app` fails to compile because of the gpui `test` macro shadowing in `charts.rs` / `insights.rs` / `commands.rs` / `model.rs` (see DECISIONS). With those imports fixed, all 29 tests pass, including the Rules/Triggers ones.

## Insights (graphs) and the Settings window

**Insights** (`ui/insights.rs`, charts in `ui/charts.rs`) replaces the terminal pane; open it from the Today card, ⌘K, `keys.insights` (⇧⌘I) or `window.command{open_screen: insights}`. Esc goes back.
- Today / Week / Month switcher; 7 headline tiles with deltas vs the previous period.
- Agent turns over time, stacked by project (hourly today, daily week/month), with legend totals; hover a column for exact per-project values; click a segment or legend chip to filter the log by that project.
- Spend over time (line + area, crosshair tooltip), working vs waiting on you per agent terminal (horizontal stacked bars, hover for exact times, click to filter the log), approvals and triggers columns.
- Activity log filtered by project, terminal, who and what, plus "While you were away"; grouped by day for week/month; click a live terminal's row to open it.
- Friendly empty state; looks right in dark and light.
- Data: new daemon method `insights.series` (+ `range: month` for `insights.summary`), all from events.

**Settings** (`settings_window.rs`) is its own ~900×640 window: ⌘, (`keys.settings`), the sidebar Settings button, the app menu, or ⌘K.
- Sections: Updates (channel, versions, daemon uptime), Permissions (Accessibility via `AXIsProcessTrusted`, Notifications, Login item, each with a System Settings button), Webhooks, Look, Agents (Claude hooks, Codex, status line, Kass "handshake not detected"), What agents may do without asking (human-only switches + policy), Keybindings (read-only), Danger zone (reset settings, two-click confirm).
- Each row: control, description, the CLI equivalent with a Copy button, and who can set it (human only with a lock / agents too / read-only).
- Title-bar toggle Rows ↔ annotated read-only `settings.json`; Ask box opens a Claude agent with the request; live footer shows each change and who made it (verified: `midna settings set density compact` from a shell shows "an agent (CLI) · just now").

**Verify:** `cargo test -p midna-proto -p midnad -p midna-cli -p midna-app` passes except `daemon.rs::discover_lists_every_method`, which fails on the `daemon.upgrade` assertion (another agent's in-progress daemon work, not insights). New tests: 2 `insights::series_*` unit tests, `tests/seed_insights.rs` (seeded history + live `agent.hook` through the RPC), chart scale/format tests, series wire-shape and key-normalization tests.

Run against seeded data:
```sh
MIDNA_SEED_HOME=/tmp/mh-seed cargo test -p midnad --test seed_insights -- --ignored seed_dev_home
MIDNA_HOME=/tmp/mh-seed MIDNA_APP_PATH=$PWD/target/debug/midna-app target/debug/midnad --foreground &
MIDNA_HOME=/tmp/mh-seed target/debug/midna-app     # the sidebar needs one terminal for MIDNA_DEBUG_SCREEN
```

Screenshots (offscreen `snapshot` renders, no native traffic lights): `insights-dark-today`, `insights-dark-month`, `insights-dark-hover` (column + row tooltips), `insights-dark-week-log`, `insights-light-week`, `insights-light-hover-today`, `insights-light-away-log`, `insights-light-empty`, `settings-dark-rows`, `settings-dark-json`, `settings-light-rows`, `settings-light-rows-agents`. The old `fake-dark-insights-placeholder.png` was removed.

**Gaps:** no zoom/brush on charts; the log shows the newest 1000 events of a range (daemon cap); "while you were away" only tracks focus once Insights has been opened in this run; Notifications/Login-item status are best-effort (no notifications are posted yet; SMAppService is a later phase); full daemon reset needs a daemon method; the Ask box has no IME (shares `LineInput`); ints/strings in Settings are changed via CLI or asking.

## ⌘K command bar and needs-you card stack

Code: `src/commands.rs` (registry + fuzzy matcher, pure), `src/ui/command_bar.rs`, `src/ui/needs_you.rs`. Small additive hooks in `app.rs` (state fields, `set_overlay` focus/reset, ⌘J), `ui/mod.rs` (routing), `ui/sidebar.rs` (button → `OpenNeedsYou`), `icons.rs` (7 icons), `main.rs` (`mod commands`). The placeholder overlay in `ui/screens.rs` is gone.

**⌘K (CommandBar-A).** A 720px palette over a scrim, with a context chip (`project › terminal`) and esc.
- **Rows** are built from live state:
  - needs-you items (approve with scope chips / deny, remove / keep rule, restart / dismiss, I've done it, got it, set secret)
  - go to terminal / project (⌘1–9)
  - new terminal / Claude / Codex here or at root (with their `keys.*` shortcuts)
  - project commands
  - Rules / Triggers / Insights / Settings
  - add a rule… (prefills an ask)
  - keep on top / pop out / restart / close per terminal
  - theme and density
  - `$MIDNA_HOME/commands.json`
- **Display**: matched characters are highlighted, badges are colored, shortcut kbds sit on each row, and the empty query shows "Needs you" / "Suggested" groups plus "Try asking" chips.
- **Keyboard**: ↑↓, ↩, ⇧↩ ask, ⇥ agent, ⇧⇥ scope, esc, ⌘↩ / ⌘⌫, ⌘V.
- **Destructive commands** arm on the first ↩ (red confirm panel) and run on the second.
- **Free text** opens "Ask an agent": a new Claude/Codex tab in the project or root with the text as `prompt`, shown with its `midna open …` CLI equivalent.

**Needs you (NeedsYou-C).** ⌘J or the sidebar button.
- **Header**: "N left", a progress bar (green handled, red denied, accent current), and back with esc.
- **Card**: kind chip, age, who · where, title, Asked by / Why / Request, the screen excerpt (item's own or the live terminal tail), Open terminal ⌘O, approve-all-N when bulk-safe, then the actions for the item's kind.
- **Up next** lists 3 items. Footer key hints follow the card's actions. The empty state is "Nothing needs you", or "All clear" with a summary of the pass. The bottom approval banner on the terminal is unchanged.

**Verified** against a temp-home midnad, all as `human` in the audit log:
- ⌘↩ approved and ⌘⌫ denied two `policy.request` gates from an ask rule. The waiting shells printed "approved by the human (once)" / "denied by the human".
- ⌘J skip; Keep rule on a `rules request-removal`; Dismiss and Restart on a failed monitor (exit 101); I've done it on `midna attention`; Got it on `attention --note` → "All clear".
- From ⌘K: ran a project command (new monitor session), closed a terminal (↩↩), and set the theme to light; the `needs approval` badge came from the real `git push*` ask rule.
- Ask-an-agent was checked on the fake backend only (Codex at root), because a real agent would touch `~/.codex`.

Tests: 10 new in `commands.rs` and 1 in `needs_you.rs`; `cargo test -p midna-app` 29 passed. The rest of the workspace passes except `midnad/tests/seed_insights.rs` (Insights work in progress, not touched here).

Screenshots: `commandbar-empty-dark`, `commandbar-destructive-armed`, `commandbar-ask-agent`, `commandbar-ask-started`, `commandbar-live-light-policy-badge`, `needsyou-live-failed`, `needsyou-live-pass-progress`, `needsyou-live-all-clear`, `needsyou-approve-menu`.

**Gaps:**
- ~~No IME or cursor movement in the bar's input.~~ done (shared `TextField`)
- ~~The last Ask agent/scope choice is in memory only (needs a settings key).~~ done (`ui.ask.*`)
- ~~No `ui.commands` RPC for agents.~~ done
- Keep-on-top can't be toggled off (pop-outs always float).
- The behind-card shadows are static.
- The board's radial accent glow is a linear gradient.

## Kass composer (dictation and long prompts)

Code: `src/kass.rs` (handshake listener), `src/composer.rs` (native `NSTextView` composer). Hooks: `app.rs` (field + init), `ui/mod.rs` (sync, render, actions), `actions.rs` (`keys.composer` + composer bindings), `terminal.rs` (`bracketed_paste()` getter), `settings_window.rs` (Kass row + `kass.auto_send`), `backend/fake.rs`; proto settings `keys.composer` (⇧⌘D) and `kass.auto_send`. New deps: objc2 0.6, objc2-foundation/app-kit 0.3, block2, core-foundation-sys, raw-window-handle (all already in Cargo.lock).

- Kass's WillBegin shows the composer under the selected terminal, makes the text view first responder and replies `dictationReady` (0.4–9 ms measured). DidEnd: empty → hides; text → stays for review (↩ sends with `session.input`, Esc cancels) or sends at once with `kass.auto_send`.
- ⇧⌘D opens it by hand for typing or pasting long prompts. ⇧↩ adds a newline; multi-line text goes as one bracketed paste.
- Settings shows "Handshake detected" after the first Kass notification.

**Verified** (temp `MIDNA_HOME`, a `/bin/cat` terminal, the spike's `kass-probe` plus a small Swift poster for DidEnd):
- `kass-sim`: WillBegin → Ready received every run (rtt 2–9 ms); notifications arrive off the main thread.
- Text typed into the open composer (CGEvents to midna-app's own pid) did not reach the terminal; DidEnd(inserted) kept it open; ↩ delivered `hello from kass` to the session (`midna read`). Esc discarded a draft (nothing sent). With `kass.auto_send=true` DidEnd sent `auto sent line` with no key press. DidEnd(cancelled) kept the text. Empty DidEnd hid it. ⇧⌘D (via `MIDNA_DEBUG_KEYS`) + typing + ↩ sent `typed by hand`.
- Tests: `kass::classify_filters_by_pid`, `kass::delivers_off_main`, `composer::encode_single_and_multi_line`; `cargo test -p midna-proto -p midna-app` pass, midnad/CLI pass except `seed_insights::seeded_history_feeds_series` (date-dependent insights test, untouched).

**Not verified: the AX insert itself.** The screen was locked the whole session, so midna-app was never the frontmost app; its AX tree exposes no windows, and the app-level focused element is `AXApplication`. AX permission was available (`AXIsProcessTrusted = true`). To finish, with the screen unlocked:
```sh
MIDNA_HOME=/tmp/mk midnad --foreground &   # then: midna open -- /bin/cat
MIDNA_HOME=/tmp/mk MIDNA_DEBUG=1 target/debug/midna-app &   # click its window so it's frontmost
spikes/kass-composer/target/debug/kass-probe $(pgrep -n midna-app) kass-sim 2
```
Expect `ready=true … role=Some("AXTextArea") … insert=err=0 Inserted{exact:true}`; run 0 ends with ↩ (the text should appear in the cat terminal), run 1 with Esc. Better still, dictate with the real Kass.

Screenshots (offscreen `snapshot` renders, where the field's text is drawn by GPUI; a live `screencapture -l` showed a stale frame because the screen was locked): `composer-dark-dictating.png`, `composer-dark-review-multiline.png`, `composer-light-dictating.png`.

**Gaps:** no pre-fill from the shell line; the composer only lives in the main window (not pop-outs); WillBegin is ignored when the main window isn't active or another screen/overlay is showing; Info.plist key waits for bundling.

## Terminal pane: keys, scrollback, selection, links, split, find

Decisions: DECISIONS.md "Terminal pane as a daily driver". Code: `midnad/src/engine_input.rs` (encoders, selection, links, find), `term.rs`/`stream.rs` (new tags), `midna-proto/src/frame.rs` (`ClientMsg`, `FrameExt`), `midna-app/src/terminal.rs`, `ui/split.rs`, `term_debug.rs`.

**What works (verified live against a temp-home midnad, release builds):**
- **Keys** encoded by libghostty for the app's mode. Test program `keys.py` (raw tty, pushes kitty flags) received: kitty `>1u` → ctrl-a `CSI 97;5u`, alt-b `CSI 98;3u`, esc `CSI 27u`, shift-enter `CSI 13;2u`, shift-tab `CSI 9;2u`; kitty `>15u` → `a` `CSI 97u`, shift-z `CSI 122:90;2u`, enter `CSI 13u`; legacy → `^A`, `ESC b`, `ESC CR`, `CSI 1;5D`, `CSI Z`, `CSI 15~`, `CSI 5~`, `CSI H`, `CSI 1;3D`; text incl. `é`. Option-as-meta via `terminal.option_as_meta`.
- **Paste** (⌘V) bracketed when the app asked; **focus** `CSI I`/`CSI O` on pane/window focus changes (mode 1004).
- **Scrollback:** trackpad/wheel (fractional rows accumulated), shift-PgUp/PgDn, shift-Home/End, ⌘↑/⌘↓ (and ⌘PgUp/PgDn). Fading scrollbar thumb plus a "↓ N lines below · ⌘↓" chip while scrolled back; the cursor hides while scrolled; typing snaps to the bottom. `less`: wheel scrolls the pager (arrow keys). `vim` with `mouse=a`: wheel and click/drag (visual mode) via SGR 1006.
- **Selection:** drag, double-click word, triple-click line, option-drag rectangle, autoscroll past the edges, ⌘C / ⌘A from the engine (wide chars `世界` copied correctly, rectangles, wrapped lines joined). Selection stays on its text as output arrives.
- **Links:** ⌘-click OSC 8 hyperlinks, `https://example.com/a_(b)` (trailing `).` trimmed), `src/main.rs:2:5` (opened in `$EDITOR +2` in a new terminal, or `open`).
- **Find** ⌘F with count (`10/11`), ↩/⇧↩, esc.
- **Split** ⌘D / header button: side by side or stacked, own streams, focus edge, close / move to main.
- **Performance** (release, 1180×720 window, 30×~150 grid; screen locked, so the dev driver drew at a 120 Hz pace instead of the display link): `cat` of a 105 MB colored file with wide chars finished in **0.73 s** while the pane ran **86–100 frames/s** (capped by the 8 ms pacing), draw **mean 1.2 ms / max 4.8 ms**; `yes | head -c 200M` took **4.1 s** at **~100 frames/s**, draw mean 0.75 ms / max 1.4 ms; typing during/after stayed immediate. Headroom for the spike's 120 fps.

**Tests:** 20 engine unit tests (legacy/DECCKM/kitty disambiguate/report-all/events/modifyOtherKeys sequences, paste, focus, SGR mouse, viewport, alt-screen wheel, selection word/line/rect/wide/wrap, links, find), `tests/daemon.rs::stream_keys_scroll_and_selection` (end to end over the stream + RPCs), proto codec tests, key routing test in the app. `cargo test` passes except `midna-cli` `mcp_exposes_catalog_as_tools` (another agent's 3 extra MCP tools; see DECISIONS TODO).

**Screenshots** (offscreen renders via `--features snapshot` + `MIDNA_DEBUG_TERM … shot:`; the screen was locked): `terminal-keys-kitty`, `terminal-keys-legacy`, `terminal-select-wide`, `terminal-rect`, `terminal-scrolled`, `terminal-find2`, `terminal-less-wheel`, `terminal-vim-mouse`, `terminal-split`, `terminal-split-stacked`.

**Gaps:** htop not installed (not tested); no link hover underline or context menu; selection/viewport shared by every client of a session; split pane doesn't re-attach after a daemon reconnect; the fake backend has no mouse/selection; zsh -f doesn't redraw its prompt after the split resize (the prompt row scrolls into history).

## Closing pass: rules feed defaults, palette, updates, reset, permissions, reconnect, terminal menu, IME field

Decisions: DECISIONS.md "Closing the open TODOs". Verified against a temp `MIDNA_HOME` daemon with private copies of the binaries (`MIDNA_APP_PATH` pointing at the copy).

**Works (seen live):**
- An agent's `midna window split <id> stacked` opened the stacked split without `agents.may_move_windows`.
- `midna daemon restart`: the status bar and card said "midnad restarting…", then both panes of the split re-attached by themselves and took input; the one control call caught at the exec (`ui.commands.list`) was retried and succeeded.
- Rules feed shows "no rule · default" and "no rule · agent's own prompt decides / unsupervised" rows next to rule rows; removing a rule and pressing Undo restored it with its original id (`rule.restored` in the log).
- ⌘K: user commands from `ui.commands.add` ("Deploy staging") match; the Ask toggles reflect `ui.ask.agent=codex`; the query field edits mid-word (typed, ←←⌫).
- Terminal: ⌘-hover underlines `https://…` and `src/main.rs:1`; right-click shows Copy/Paste/Open link/Select All/Clear with Open link enabled over a link.
- Settings: Permissions and Login item rows show `midna permissions open …`; Update rows `midna updates check|install`; Danger zone has "Reset midna" (`midna daemon reset`); Agents has the two ⌘K Ask settings.

**Not verified:** the Kass AX insert (`kass-probe … kass-sim`): the screen locked again before it could run, and it needs midna-app frontmost. The commands in the Kass section above still apply. `updates.install` end to end needs a packaged build with a feed (dev builds report `disabled`; the proxy path is covered by `tests/ui_updates_reset.rs`).

Screens: `docs/screens/live-*.png` (see DECISIONS.md for which are live captures and which are offscreen renders).
