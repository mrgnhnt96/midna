# Agents working on midna

## Never touch the production Midna

The human uses the installed Midna as their daily terminal, and agents usually run inside it. Its daemon owns every live shell, so a bad build, a quit, or a stopped or killed daemon can hang up all of them, this session included.

Production is:

- `/Applications/Midna.app` (bundle id `com.mrgnhnt.midna`)
- its daemon, LaunchAgent `com.mrgnhnt.midna.daemon`
- its home, `~/Library/Application Support/com.mrgnhnt.midna`
- `~/.local/bin/midna` and the Finder Quick Action `~/Library/Services/Open in Midna.workflow`

Agents must never build into it, install over it, launch, quit, restart or signal it, upgrade or stop its daemon, `launchctl` its label, or write its files. Reading its logs and settings is fine. Never run `scripts/reinstall.sh`; it's for the human only. Never `pkill`/`killall` anything named midna; stop test daemons by PID (see `README.md`). Production only changes through real releases, or when the human does it themselves.

Using the `midna` CLI from a production terminal the way any agent would (`midna attention`, `midna open`, `midna read` and so on) is fine. Controlling its daemon (`midna daemon stop|restart|upgrade`) is not.

## Use Midna Dev or a temp home

- **Most work:** `cargo test --workspace`, `scripts/smoke.sh`, or a temp `MIDNA_HOME` (see "Dev mode" in `README.md`).
- **To try a change in a real app:** `scripts/dev-app.sh` (add `--test` to run the tests first). It builds **Midna Dev** and installs it to `~/Applications/Midna Dev.app`. Midna Dev has bundle id `com.mrgnhnt.midna.dev`, its own daemon (`com.mrgnhnt.midna.dev.daemon`), its own home `~/Library/Application Support/com.mrgnhnt.midna.dev`, an inverted icon and the Gruvbox theme, and it never updates itself. Update Midna Dev only when the task needs a packaged app; otherwise leave it alone too.
- To drive Midna Dev's daemon, point the CLI at it: `MIDNA_HOME="$HOME/Library/Application Support/com.mrgnhnt.midna.dev" MIDNA_SOCKET="$MIDNA_HOME/midnad.sock" …/bin/current/midna …`.

Claude Code enforces these rules with `.claude/hooks/protect-production.py`. Other agents must follow them by hand.

## Style

- Never run `cargo fmt`. Long lines are formatted by hand, and there's no `rustfmt.toml`.
