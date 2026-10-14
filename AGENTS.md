# Agents working on midna

## Never touch the production Midna

The human uses the installed Midna as their daily terminal, and agents usually run inside it. Its daemon owns every live shell, so a bad build, a quit, or a stopped or killed daemon can hang up all of them, this session included.

Production is:

- `/Applications/Midna.app` (bundle id `com.mrgnhnt.midna`)
- its daemon, LaunchAgent `com.mrgnhnt.midna.daemon`
- its home, `~/Library/Application Support/com.mrgnhnt.midna`
- `~/.local/bin/midna` and the Finder Quick Action `~/Library/Services/Open in Midna.workflow`
- its Finder extension `com.mrgnhnt.midna.finder-sync` (never `pluginkit -e` it; Midna Dev's is `com.mrgnhnt.midna.dev.finder-sync`)

Agents must never build into it, install over it, launch, quit, restart or signal it, upgrade or stop its daemon, `launchctl` its label, or write its files. Reading its logs and settings is fine. Never run `scripts/reinstall.sh`; it's for the human only. Never `pkill`/`killall` anything named midna; stop test daemons by PID (see `README.md`). Production only changes through real releases, or when the human does it themselves.

Using the `midna` CLI from a production terminal the way any agent would (`midna attention`, `midna open`, `midna read` and so on) is fine. Controlling its daemon (`midna daemon stop|restart|upgrade`) is not.

## Use Midna Dev or a temp home

- **Most work:** the tests for the code you touched (see "Test what you touched" below), `scripts/smoke.sh`, or a temp `MIDNA_HOME` (see "Dev mode" in `README.md`).
- **To try a change in a real app:** `scripts/dev-app.sh` (add `--test=app` etc. to run the tests for what you touched first, or `--test` for all of them). It builds **Midna Dev** and installs it to `~/Applications/Midna Dev.app`. Midna Dev has bundle id `com.mrgnhnt.midna.dev`, its own daemon (`com.mrgnhnt.midna.dev.daemon`), its own home `~/Library/Application Support/com.mrgnhnt.midna.dev`, an inverted icon and the Gruvbox theme, and it never updates itself. Update Midna Dev only when the task needs a packaged app; otherwise leave it alone too.
- To drive Midna Dev's daemon, point the CLI at it: `MIDNA_HOME="$HOME/Library/Application Support/com.mrgnhnt.midna.dev" MIDNA_SOCKET="$MIDNA_HOME/midnad.sock" …/bin/current/midna …`.

Claude Code enforces these rules with `.claude/hooks/protect-production.py`. Other agents must follow them by hand.

## Test what you touched

The full suite is slow (the midnad and CLI integration tests start real temp daemons), so run only the tests that cover the code you changed. Start from the modules you touched: their unit tests (`scripts/test.sh -p midnad --lib agent_work::`) and the integration tests that exercise them (`scripts/test.sh -p midnad --test guard`, `scripts/test.sh -p midna-app insights::`). That is enough when the change stays inside a module's internals, such as a private helper, a fix-up when `state.json` loads, or a parsing detail with no other callers.

Run whole crate suites only when the change reaches what other code depends on: an RPC method's params or result, a type in `midna-proto`, the hook payloads or events midnad handles, or anything the CLI or the app calls into. Then run the crates that change can affect. The crates depend on each other like this: `midna-proto` ← `midnad` ← `midna-cli`, and `midna-proto` ← `midna-app`.

| Your change reaches the surface of | Run |
|---|---|
| only `midna-app` | `scripts/test.sh -p midna-app` |
| only `midna-cli` | `scripts/test.sh -p midna-cli` |
| `midnad` | `scripts/test.sh -p midnad -p midna-cli` |
| `midna-proto` | `scripts/test.sh` (the whole workspace) |

`scripts/test.sh` takes `cargo test`'s arguments. It builds at low priority, then waits its turn: one checkout's tests run at a time across the machine, so parallel agents don't push each other's integration tests past their timeouts. Add `-p` flags when a change spans crates. Keep the whole workspace for proto changes, cross-cutting refactors and releases.

Each checkout builds into its own `target/`. Never point a worktree's `CARGO_TARGET_DIR` (or cargo's build dir) at another checkout's `target/`: cargo gives the same crate the same artifact names in every checkout, so they overwrite each other and a build can link another checkout's stale midna crate.

## Releases never skip a version

If a release's workflow fails or is cancelled before its GitHub release is published, cut that **same** version again. Never move on to the next number. Fix the cause, delete the tag (`git push origin :refs/tags/vX && git tag -d vX`) and run `./scripts/release.sh X` again. Before you pick a number, check that the newest tag has a release (`gh release view v<newest>`). `release.sh` refuses to go past one that doesn't. Details are in `docs/RELEASING.md` ("A release failed").

## Style

- Never run `cargo fmt`. Long lines are formatted by hand, and there's no `rustfmt.toml`.
