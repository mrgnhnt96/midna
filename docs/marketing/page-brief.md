# Page brief

- Type and state: Product site for a free, open source Mac app, live on a public beta. Stable hasn't shipped (newest release v0.1.0-beta.46, checked 2026-11-06).
- Audience: Developers on Apple Silicon Macs who already run Claude Code or Codex in a terminal, often several at once, and know what an approval prompt is.
- The page's one job: Get that developer to download midna and try it on a real project.
- Primary CTA: "Download for Mac", to `/download/`. Wording and target wait on the download decision below.
- Secondary action: "Read the docs", to `/docs/`.
- Search phrases: terminal for Claude Code; macOS terminal for AI agents; run multiple Claude Code sessions; approve Claude Code prompts in one place; Claude Code terminal app Mac.
- Objections: Does it work with my agent? Will my shells die when it updates? Is it safe to let agents drive my terminal? Is it signed? What does it send, and to whom? Is it free?
- Analytics and cookies: none. Fonts are served from the site. The download page calls `api.github.com` from the browser.

## Claims

Checked 2026-11-06.

| Claim on the page | Evidence |
| --- | --- |
| Native macOS terminal for Claude Code and Codex | `crates/midna-app`, `README.md` |
| Agents open and read terminals through a CLI and MCP server | `midna capabilities`; `site/src/content/docs/docs/agents.md` |
| Claude Code approvals land in one queue, approve and deny from midna | Real dialog fixture `crates/midnad/tests/fixtures/claude-2.1.288-permission.screen.txt`; `docs/DECISIONS.md` |
| Codex's approval prompts stay in its terminal | Live run with codex-cli 0.163.0, `approval_policy="on-request"`, temp daemon: prompt shown, status stayed `working`, `midna needs` empty for over a minute. Backlog B774 |
| Approving for 15 minutes, 1 hour, the session or always adds a rule | `approve-menu.png`, a real render; `docs/DECISIONS.md` |
| Shells keep running when the app restarts or updates | Daemon owns the shells: `AGENTS.md`, `README.md` |
| Free and open source (MIT) | `LICENSE`, Copyright 2026 Morgan Hunt |
| Apple Silicon | arm64-only DMG: `.github/workflows/release.yml:155` |
| macOS 12 or later | `LSMinimumSystemVersion` 12.0 in `packaging/build-app.sh` and the shipped `Info.plist`. Never run on macOS 12: see below |
| Signed, notarized, updates itself | beta.46 DMG: `spctl` accepted as Notarized Developer ID (Developer ID Application: Morgan Hunt, U2G2XV3688); `stapler validate` passes on the DMG and `Midna.app`. Updates: `crates/midna-app/src/updater.rs` |
| Eight built-in themes | `crates/midna-proto/src/themes.rs` `BUILTIN_IDS` |
| Triggers can arrive by webhook over Tailscale Funnel | `crates/midnad/src/webhooks/`, but turning Funnel on hasn't been run for real: see below |
| Privacy page: no account, analytics or crash reporting | Source search of `crates/` for sentry, telemetry, posthog, amplitude, crash reporting: nothing. `ureq` only in `updater.rs` |
| Privacy page: update check every 6 hours | `CHECK_EVERY` in `crates/midna-app/src/updater.rs` |
| Privacy page: what link previews, the network check, cleanup and webhooks send | `crates/midna-app/src/terminal/link_preview.rs`; `PROBE_HOST` in `crates/midnad/src/resume.rs`; `crates/midna-proto/src/cleanup.rs`; `webhooks.path` in `crates/midna-proto/src/settings.rs` (off by default). Cleanup is on by default |
| Privacy page: how long data is kept | `summary.keep_days` default 7 in `crates/midna-proto/src/settings.rs`; `RUNS` 100 in `crates/midnad/src/cleanup.rs`; the event log's archives are never deleted (`crates/midnad/src/eventlog.rs`) |
| Privacy page: what cleanup sends | `prompt` in `crates/midnad/src/cleanup.rs` |
| Screenshots | Real renders of midna with the fake backend's sample data, made with the `snapshot` feature (`docs/DECISIONS.md:150`), not recreations. The hero is labelled "Example data". `main`, `main-light`, `approve-menu`, `command-bar`, `needs-you` and `composer` were rendered 2026-11-06 at 1280x800 @2x with `MIDNA_BACKEND=fake MIDNA_SELECT=a1f00002` |

Structured data uses `Organization` for midna, because naming Morgan as a `Person` is their call.

## Waiting on a person

- Morgan: decide what Download does. Every release is a prerelease, so `/releases/latest` is a 404 and the page ends at "midna isn't out yet". Offer the beta, cut a stable release, or call the site pre-launch.
- Morgan: run midna once on macOS 12, or raise the stated minimum to a version it has run on.
- Morgan: turn Tailscale Funnel on for real and check a webhook arrives, or drop Funnel from the home page.
- Morgan: read `/privacy/` and confirm it's what you want to publish under your name.
