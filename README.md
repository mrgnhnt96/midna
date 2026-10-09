# midna

midna is a macOS terminal built for working with AI agents. It replaces Saggar.

**[Download](https://midna.mrgnhnt.com/download/) · [Docs](https://midna.mrgnhnt.com/docs/) · [Changelog](https://midna.mrgnhnt.com/changelog/)**

- **Projects** group **terminals**, which run shells, monitors, or agents (Claude Code, Codex).
- **A daemon (`midnad`) is the source of truth.** It owns every PTY, the terminal engines (libghostty-vt), projects, rules, triggers, settings, and the event and audit log. Closing or upgrading the GUI never kills a shell.
- **Everything is AI-managed.** Agents drive midna through the `midna` CLI, JSON-RPC over a Unix socket, or MCP. Every capability is a method in one discoverable catalog (`midna schema`).
- **Agents may add; humans may remove.** Agents can open terminals, add rules, and draft triggers. Some actions belong to the human: approving, removing rules, setting webhook secrets, enabling triggers, and changing human-only settings. When an agent attempts one, it becomes a "needs you" item that the human answers in the GUI (`midna-app`, GPUI).
- **Webhooks** from GitHub and Bitbucket can start agents, run commands, or raise attention. They arrive over Tailscale Funnel.
- **Insights** chart turns, messages, spend, and waiting time, all computed from the event log.

The design contract is `docs/ARCHITECTURE.md`. The reasoning behind it is in `docs/DECISIONS.md`. The security model and its limits are in `docs/SECURITY.md`.

## Build and run

Requirements:
- macOS on Apple silicon
- Rust (edition 2024)
- the pinned toolchain in `.toolchain/` (Zig 0.15.2, an SDK shim, the ghostty source). It's gitignored; `scripts/setup-toolchain.sh` sets it up.

Always source `env.sh` first.

```sh
. ./env.sh
cargo build                      # midnad, midna (CLI), midna-app
cargo test --workspace           # everything uses temp MIDNA_HOMEs
cargo test -p midna-app          # or just the crates you touched (see AGENTS.md)
scripts/smoke.sh                 # end-to-end day in the life against a real temp daemon
```

### Dev mode

Use a temp home with a short path, because Unix socket paths are limited to 104 bytes. Never use your real home.

```sh
export MIDNA_HOME=$(mktemp -d /tmp/mh.XXXX)
MIDNA_APP_PATH=$PWD/target/debug/midna-app target/debug/midnad --foreground &   # GUI = human (debug builds only)
target/debug/midna-app                       # the GUI
target/debug/midna info                      # the CLI (an agent unless it's the GUI)
target/debug/midna open -- /bin/zsh          # prints the new terminal id
MIDNA_BACKEND=fake target/debug/midna-app    # GUI with sample data, no daemon
```

Useful dev environment variables:

| Variable | Effect |
|---|---|
| `MIDNA_SECRETS=file` | File secrets instead of the Keychain |
| `MIDNA_WEBHOOKS_PORT=0` | Webhook receiver on any free port |
| `MIDNA_AGENT_BIN=/bin/echo` | Stub instead of claude / codex |
| `MIDNA_NO_GH=1` | No `gh` lookups |

The `MIDNA_DEBUG_*` UI drivers work in debug builds only. See `docs/STATUS-*.md`.

### Packaged app

```sh
packaging/build-app.sh                       # -> dist/<version>/Midna.app, Developer ID signed if a cert is present
scripts/reinstall.sh [--test] [--no-build]   # rebuild the app and restart it; terminals survive
scripts/dev-app.sh [--test] [--no-build]     # "Midna Dev" beside your real Midna: own home, daemon, CLI link; never updates
packaging/make-update.sh --app dist/<v>/Midna.app --url-base …   # signed update feed
packaging/notarize.sh dist/<v>/Midna.app     # opt-in, uploads to Apple
packaging/e2e/update-e2e.sh                  # full install + auto-update test (test bundle id, temp home)
```

What happens on first launch of the packaged app:
- It installs `midnad` into `MIDNA_HOME/bin/<version>`.
- It registers the LaunchAgent through SMAppService.
- It links the CLI into `~/.local/bin`.
- It checks the update feed every 6 hours.

Updates are verified with sha256, an ed25519 signature, codesign, and the Team ID. The daemon upgrades in place (same pid), and shells survive. Full details are in `docs/RELEASING.md`.

## For agents: the CLI and MCP quickstart

Inside a midna terminal, `MIDNA_SESSION`, `MIDNA_PROJECT` and `MIDNA_SOCKET` are set and `midna` is on `PATH`.

```sh
midna capabilities                 # what you can do, and what only the human can do
midna skill                        # the agent guide (also $MIDNA_SKILL)
midna info                         # your role and terminal
midna open --name tests --monitor 'cargo test'      # a terminal the human can watch
midna send <id> 'ls' ; midna read <id>              # drive and read another terminal
midna attention "need a decision on X"              # get the human's eye
midna rules add deny command 'git push --force*'    # agents may add rules…
midna rules request-removal <rule-id> --reason …    # …but only ask to remove them
midna triggers add --name "Review PRs" --event pull_request.opened --repo me/repo \
  --agent claude --project <p_id> --prompt 'Review PR #{{pr.number}}: {{pr.title}}'
midna explain <id|method|setting>  # why something is the way it is
midna schema [method]              # the OpenRPC catalog; `midna call <method> '<json>'` calls anything
```

MCP: `midna mcp` is a stdio MCP server that exposes every method as a tool. Agents that midna launches get it automatically (`claude --mcp-config …`, `codex -c mcp_servers.midna…`), along with hooks and a short system hint. midna never touches your global agent config.

Exit codes: `0` ok, `1` refused or failed, `2` bad arguments, `3` daemon unreachable.

Refusals say what to do next. A human-only call from an agent returns error `2` and raises a needs-you item instead.

## Layout

```
crates/
  midna-proto/   types, method catalog + OpenRPC, settings catalog, frame codec, blocking client
  midnad/        the daemon (lib + bin): PTYs, engines, policy, triggers/webhooks, insights, upgrade
  midna-cli/     `midna`: CLI verbs, `midna mcp`, `midna hook claude|codex`
  midna-app/     the GPUI app (Midna.app): sidebar, terminal pane, ⌘K, needs-you, Rules,
                 Triggers, Insights, Settings, Kass composer, install + auto-update
docs/
  ARCHITECTURE.md   the build contract        DECISIONS.md   why things are the way they are
  SECURITY.md       human/agent boundary      RELEASING.md   build, sign, publish, install
  STATUS-*.md       per-area status (foundation, app, agents, triggers, upgrade, packaging)
  design/           approved screen boards    screens/       screenshots
packaging/       build-app.sh, make-update.sh, notarize.sh, gen-update-key.sh, e2e/, release-tool/
scripts/        smoke.sh, release.sh, set-version.sh, setup-toolchain.sh, build-dmg.sh
site/           midna.mrgnhnt.com: landing page, docs, changelog, download (Astro + Starlight)
spikes/          proven prototypes the crates grew from
```

## Status

**Working and tested** (details in `docs/STATUS-*.md`):
- The daemon: PTYs, the event and audit log, rules and `policy.request` approvals, needs-you, settings, insights, header and row scripts, and git/PR info.
- In-place upgrade and restart, with shells kept (same pids, screens intact).
- The full GUI: sidebar, terminal pane (kitty keys, scrollback, selection, links, find, split, pop-out), ⌘K, needs-you cards, Rules, Triggers, Insights graphs, Settings, and the Kass composer.
- Claude Code and Codex, verified live: hooks, status, approvals, cost, MCP and the system hint.
- GitHub and Bitbucket triggers, Tailscale Funnel status, and missed-delivery recovery.
- Packaging, install, and signed auto-update (e2e 20/20).
- The security hardening and robustness pass (`docs/SECURITY.md`).

**Known gaps:**
- **Same-user limitation.** The human/agent boundary is a guardrail, not authentication against a hostile local process (`docs/SECURITY.md`).
- **Not run for real:**
  - Codex approval prompts. The user's config sets `approval_policy="never"`.
  - The Kass AX insert.
  - Turning Tailscale Funnel on.
  - The Keychain secret path.
  - `gh` recovery against GitHub.
  - Notarization.
  - The `RequiresApproval` login-item flow.
  - The macOS 12 legacy LaunchAgent path.
- **Startup dialogs aren't surfaced as needs-you:** Claude or Codex folder trust and the hooks review.
- **Relay webhook paths** (`self_relay`, `midna_relay`) aren't built.
- **Trigger filters** are structured only (repo, branch, action, label). There are no expressions.
- **Packaging gaps:**
  - The build is arm64 only (no universal binary).
  - The icon is a placeholder.
- **An unclean daemon death** (SIGKILL, crash) still loses every PTY.
- **Smaller UI gaps:**
  - Keep-on-top can't be toggled off.
  - Chart zoom is missing.
  - The needs-you card stack has static shadows.
  - Link hover has no context menu.
- **Security follow-ups:**
  - Disclaim TCC responsibility for terminal children before granting Accessibility.
  - Consider gating `trigger.replay` for agents.
  - Ship signed builds only.

## What needs the human

These need you (credentials, your screen, or your accounts). Nothing in this list was done automatically.

1. **Releases** are cut with `./scripts/release.sh <version>`; CI signs, notarizes and publishes them (`docs/RELEASING.md`). The release update key lives in `~/.config/midna-release` and the repo's `MIDNA_UPDATE_KEY` secret.
2. **Notarize by hand** only outside CI: `packaging/notarize.sh dist/<v>/Midna.app` needs a notarytool keychain profile.
3. **Run the Kass AX check with midna-app frontmost.** The screen was locked in every session. Steps are in `docs/STATUS-app.md` ("Kass composer"):
   1. Run `kass-probe … kass-sim 2`.
   2. Expect `role=AXTextArea … Inserted{exact:true}`.
   3. Better still, dictate with the real Kass.
4. **Live-test Codex approvals.** Your `~/.codex/config.toml` sets `approval_policy="never"`. Use a temp `CODEX_HOME` with `on-request` (or a webhook-started Codex, which now runs `on-request`), and check that the prompt patterns and approve/deny from midna work.
5. **Enable Tailscale Funnel** for your tailnet. Then run `midna webhooks configure tailscale_funnel` from the GUI (human only), and point a GitHub webhook at the public URL that `midna webhooks status` shows.
6. **Decide on `triggers.agent_mode`.** The default is `supervised`, which forces permission prompts on webhook-started agents even though your global Claude config bypasses them. Switch to `inherit` only if you accept that risk.
7. **Grant Accessibility to Midna.app only after reading `docs/SECURITY.md` → "TCC responsibility".** Agents in midna terminals may inherit that grant.
8. **Replace the placeholder icon.**
9. **Review and commit.** Nothing here was committed.
