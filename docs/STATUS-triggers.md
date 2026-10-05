# Triggers and webhooks status

GitHub/Bitbucket webhooks become agent terminals, monitor commands or needs-you notes. The decisions behind this are in `docs/DECISIONS.md` under "Triggers and webhooks".

## How to try it

```sh
. ./env.sh && cargo build
export MIDNA_HOME=/tmp/midna-dev MIDNA_SECRETS=file MIDNA_WEBHOOKS_PORT=7787
target/debug/midnad --foreground
# another shell (the CLI is an "agent", so the human-only steps go through `midna call` from the GUI
# or a needs-you approval; for local poking, MIDNA_APP_PATH=$(pwd)/target/debug/midna makes the CLI human)
target/debug/midna triggers add --name "Review new PRs" --event pull_request.opened --repo me/repo \
  --agent claude --project p_xxxxxx --prompt 'Review PR #{{pr.number}} “{{pr.title}}”'
target/debug/midna triggers set-secret t_xxxxxx      # reads stdin, hidden
target/debug/midna triggers enable t_xxxxxx
target/debug/midna webhooks status
target/debug/midna triggers deliveries
```

## Environment variables

| Variable | Effect |
|---|---|
| `MIDNA_SECRETS=file` | Secrets go to `$MIDNA_HOME/secrets/<id>` (0600) instead of the Keychain. Tests always use this. |
| `MIDNA_WEBHOOKS_PORT=N` | Always run the receiver on N, whatever `webhooks.path` says. `0` picks any free port. |
| `MIDNA_TAILSCALE=path` | Tailscale CLI override. |
| `MIDNA_AGENT_BIN=path` | Replace the agent executable (tests use `/bin/echo`). |
| `MIDNA_NO_GH=1` | Disables the `gh` lookups, including missed-delivery recovery. |

## What works

**Methods.** All of these are implemented (no more stubs in the group):
- `trigger.list`, `add`, `update`, `set_enabled`, `set_secret`, `remove`, `deliveries`, `replay`, `test`
- `webhooks.status`, `webhooks.configure`
- the new `webhooks.reconcile`

**Human-only enforcement:**
- Agent drafts start as `needs_secret` and raise one `secret_needed` item.
- An agent calling `set_secret` gets error 2 plus that item, and its secret is discarded, never persisted.
- An agent enabling a trigger gets a deferred approval, or `secret_needed` when there's no secret yet.
- Agents can pause.
- An agent editing an enabled trigger's action or source sends it back to draft.
- An agent removing an enabled trigger is asked of the human.
- `webhooks.path` and `webhooks.relay_url` are now human-only settings.

**Secrets.**
- They live in the Keychain (`com.mrgnhnt.midna.webhook` / trigger id), or in a file when `MIDNA_SECRETS=file`.
- They're never returned or logged. A test greps `state.json` and `events.jsonl` for both secrets.

**Receiver** (`tiny_http`, 127.0.0.1):
- **Routes.** `POST /hooks/github` (`X-Hub-Signature-256`, `X-GitHub-Event`, `X-GitHub-Delivery`), `POST /hooks/bitbucket` (`X-Hub-Signature` sha256, `X-Event-Key`, `X-Request-UUID`), and `GET /hooks/health`.
- **Signatures.** HMAC-SHA256 with a constant-time compare, checked against every candidate trigger's secret. Only the verified triggers are evaluated.
- **Matching.** Event plus action (`pull_request.opened`, `pullrequest:created`, globs). Filters cover repo, branch, action and label, all as case-insensitive globs.
- **Recording.** Every request is recorded as a Delivery with an `eval` trace: verified, bad_signature, filtered, no_trigger, replayed or recovered.
- **Dedupe.** Repeats of a GUID are deduped (200, not re-run).
- **Response.** It answers 202 before running actions.
- **Ping.** A verified `ping` fills in `github_hook_id`.

**Actions:**
- `start_agent` opens an agent session through the normal `session.open` path, with actor `trigger`.
- `run_command` opens a monitor; template values are shell-quoted.
- `attention` raises a needs-you note.
- They emit `trigger.fired` (counted by Insights) and `trigger.delivery`.

**Replay and dry run:**
- `trigger.replay` re-runs a stored payload through the same filters.
- `trigger.test` is a dry run that shows the rendered prompt.

**Tailscale Funnel:**
- **Detection.** It finds the CLI (app bundle or PATH) and parses `tailscale status --json` and `tailscale funnel status --json`.
- **Status.** `webhooks.status` reports the public URL (`https://<dns>:8443/hooks/github`), the Bitbucket URL, health, the receiver, the last delivery and recovery.
- **Configure.** `webhooks.configure{path:tailscale_funnel}` runs `tailscale funnel --bg --https=8443 http://127.0.0.1:<port>` in the foreground with a 20s timeout. It returns `enable_url` when Funnel isn't enabled for the tailnet.

**Missed GitHub deliveries:**
- **When.** Recovery runs on startup, on wake, and on `webhooks.reconcile`.
- **How.** It uses `gh api repos/{repo}/hooks/{id}/deliveries`, looks back 3 days, never reaches before the trigger's `enabled_at`, and redelivers locally as `recovered`.

**CLI:**
- `midna triggers list|show|add|update|enable|disable|set-secret|remove|deliveries|replay|test`
- `midna webhooks status|configure|reconcile`
- All verbs take `--json`. `set-secret` reads stdin with echo off when it's a TTY.

## Tests

The suites:
- **`crates/midnad/tests/triggers.rs`.** 9 integration tests. Each uses a temp `MIDNA_HOME`, `MIDNA_SECRETS=file` mode, a random free port, and `/bin/echo` as the agent. They cover:
  - a signed delivery starting an agent with the rendered prompt
  - bad and missing signatures
  - filtered, no_trigger and paused cases
  - dedupe
  - replay
  - the agent secret and enable rules, including the leak check
  - Bitbucket signatures with an attention action
  - `webhooks.status` and `configure`
  - the dry run and ping linking the hook id
  - recovered deliveries
- **`crates/midna-cli/tests/triggers_cli.rs`.** The CLI flow as an agent.
- **Unit tests (14) in `midnad::webhooks`:**
  - HMAC against GitHub's documented test vector
  - payload facts, matching and templates (including the shell-injection case)
  - Tailscale status and funnel-status fixtures, plus the command construction and enable-URL parsing
  - `gh` deliveries list/detail fixtures and the missing-delivery selection
  - the file secret store (0600)
  - form-encoded bodies

I also changed `tests/daemon.rs::discover_lists_every_method`: it asserted that `trigger.add` was a stub, so it now uses `daemon.upgrade`.

## Not done / caveats

- **Never exercised for real:**
  - The Keychain path; tests must not touch it. The calls are `security_framework::passwords::{set,get,delete}_generic_password`.
  - Turning Funnel on. I only ran the read-only `tailscale status --json` and `tailscale funnel status --json`.
  - Recovery against GitHub. The `gh` calls are best effort, covered by fixtures only.
- **Relay clients** (`self_relay`, `midna_relay`) aren't built. They report `health: down`, "relay not available yet".
- **Prompt injection.** PR titles and bodies flow into agent prompts verbatim (shell-quoted only for `run_command`).
- **Filters are structured only** (repo, branch, action, label). The design's expression filters (`draft == false`, `conclusion == "failure"`) aren't supported. Neither is sharing one secret between triggers.
- **Replay and recovery evaluate against all current triggers.** They don't re-check signatures.
- **Health has no "degraded" state yet.** It is `healthy|down|off`. The status doesn't track GitHub-side 502s.
- **Workspace build and test, at the time of writing:**
  - `cargo build` passes for the whole workspace.
  - `cargo test` passes for `midna-proto`, `midnad` and `midna-cli`.
  - `midna-app`'s test target didn't compile ("recursion limit reached while expanding `#[test]`"). That code belongs to the app agent, who was mid-edit.

## Local triggers

Triggers can also fire on what midnad sees on this Mac (`source: local`, code in `crates/midnad/src/local.rs`, tests in `crates/midnad/tests/local_triggers.rs`):

- **Events.** `hook.<HookEvent>` (the hook payload is the data), any midna event kind (its data), or `idle` (`filter.idle_minutes` without a prompt or turn; fires once per idle stretch, so a keep-warm prompt restarts the clock). Filters: `session`, `project`, `agent`, and `match` (dotted path → case-insensitive glob).
- **Actions.** The webhook ones, plus three that act on the terminal that fired: `send_to_session` (its steps go into the terminal's message queue, see DECISIONS.md "Queued messages", so they're typed in order once the agent is ready; a firing whose earlier steps are still queued is refused; `{{last_prompt}}` is the full latest prompt), `set_status` (label + color + optional icon over a built-in `base` state that still drives sorting, notifications and Needs You; clears on prompt, turn, status change or never) and `clear_status`.
- **Rules.** No secret; agents may add, enable and pause them (the human asks an agent for them). Default cooldown 60s per terminal. Events a trigger caused, and `trigger.*` events, never fire triggers. Firings are recorded as deliveries with `source: local`.
- **Refused prompts.** When a UserPromptSubmit hook refuses a prompt, Claude writes a `system`/`informational` warning to its transcript (`tests/fixtures/claude-2.1.289-prompt-blocked.jsonl`; interactive sessions append `Original prompt: …`) and never starts a turn. midnad ends the turn it opened, sets the terminal idle and emits `agent.prompt_blocked {hook, message, prompt}`. A refused *first* prompt leaves no transcript at all; then a fallback ends the turn after 15s of silence with a stopped title (no event, since a slow hook looks the same).
- **Built-in.** "Prompt blocked by a hook" (`builtin: prompt_blocked_status`) sets an amber "Prompt blocked" status with a needs_you base. It is seeded once (removing it keeps it gone) and comes back on `daemon.reset`.
- **Full prompts.** `agent.prompt_submitted` now keeps the whole prompt (it was cut at 200 characters).
- **Schedules (cron jobs).** `event: schedule` with `filter.cron`, a five-field expression in local time (`crates/midna-proto/src/cron.rs`: lists, ranges, steps, month/weekday names, `@hourly`…`@yearly`, Vixie's either-day rule when both day fields are set). The local worker checks once per minute boundary (on its 15s tick). After sleep or a stall it fires a missed run once, and only if it was within the last 10 minutes; older ones are skipped. Without a session/project/agent filter a schedule fires once, about no terminal (notify, attention, run_command, start_agent). With one it acts on every running terminal that matches, which is how `send_to_session`/`set_status` get a target. Validation refuses session actions with neither `session` nor `project`. Data is `{cron, scheduled_for, local_time}`. `trigger.test` without a payload tests against the next run and lists the next three. Cooldown doesn't apply. The schedule's last checked minute isn't persisted, so a daemon restart inside a minute it already fired can fire that minute again.
