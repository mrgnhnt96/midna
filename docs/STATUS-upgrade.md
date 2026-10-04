# Upgrade / restart / stop status (midnad)

Goal: shells never die because midnad restarts or upgrades. Decisions and tradeoffs are in `docs/DECISIONS.md` ("Daemon upgrade, restart and stop").

## How to use

```sh
. ./env.sh && cargo build
export MIDNA_HOME=/tmp/midna-dev
target/debug/midnad --foreground &
target/debug/midna daemon restart                  # graceful restart, terminals keep running
target/debug/midna daemon upgrade path/to/midnad   # human only: from a terminal it raises a needs-you approval
target/debug/midnad --upgrade-to path/to/midnad    # same request
kill -TERM <pid>                                   # graceful restart (MIDNA_SIGTERM=stop makes it a stop)
kill -INT <pid>                                    # stop: SIGHUP every terminal's process groups, then exit
target/debug/midnad --install-self                 # $MIDNA_HOME/bin/<ver>-<hash>/, bin/current -> it
```

`daemon.stop` is human only (agents get an approval). Watch `midna events --kind daemon.` for `daemon.upgraded` / `daemon.upgrade_failed` / `daemon.stopping`.

## What works (all tested)

- `daemon.upgrade` / `daemon.restart` / SIGTERM: `--selftest` preflight → pause PTY reads → snapshot every engine → handoff dir → watchdog → same-PID `execv` → restore under the same ids → `daemon.upgraded {from, to, sessions_kept, …}`.
- Shells keep running with no SIGHUP, same pids, still our children (exit codes after an upgrade verified).
- Screens survive exactly: scrollback, alt screen, cursor, title, cursor shape, and typed-but-unsubmitted input. Workarounds for the libghostty export bugs: CSI H, CUP last, NUL strip, title/shape from our own state, half-parsed escape replay. A new export bug was found and fixed: dropped trailing blank rows shifted the screen when there was scrollback.
- No byte lost or duplicated while upgrading twice during `yes | head -c 50M` plus a paced numbered stream.
- A failed selftest aborts with error `1`; nothing changes.
- Watchdog fallback: if the new binary dies or stalls (`MIDNA_UPGRADE_WATCHDOG_SECS`, default 10), the old binary resumes the same handoff from the watchdog process. Terminals survive, the pid changes, and exits are tracked by kqueue without an exit code.
- Stop policy fixes the orphaned-monitor bug: an explicit stop SIGHUPs (then SIGKILLs) every process group in each terminal's session. Upgrade and restart never signal.
- Mutating calls during a handoff get error `4`.
- CLI: `midna daemon info|upgrade|restart|stop`.

Tests:
- `crates/midnad/tests/upgrade.rs`: 7 tests against copies of the real binary.
  - upgrade keeps ids, pids, screens and counting, and the shell stays interactive, twice
  - flood
  - selftest failure
  - agent → needs-you
  - watchdog fallback
  - SIGTERM restart, then a SIGINT stop that kills a HUP-ignoring job-control child
  - restart RPC, with the exit code checked after the exec
- Engine unit tests for the snapshot round-trips and `incomplete_suffix`, and a unit test for `install_self`.

## Not done / limits

- **Unclean death** (SIGKILL, crash, OOM) still loses every PTY. Sessions are marked `exited` ("daemon restarted") on the next start.
- **A new image that resumes and then crashes** is an unclean death too: the watchdog exits once `resumed` is written.
- **The "launchd restarts `bin/current` and resumes the handoff" variant isn't possible** without a process holding the master fds. The watchdog is that process, but only during the handoff.
- **SMAppService phase TODO:** launchd's bootout SIGTERM would now restart rather than stop. Stop with `daemon.stop` first, or set `MIDNA_SIGTERM=stop` in the plist.
- **Not restored:** DECSC saved cursor, origin-mode-relative cursor, half-escapes over 64 KiB.
- **Short leak window during a handoff:** child processes spawned meanwhile by non-PTY code (git, gh, scripts) can inherit the PTY master fds while CLOEXEC is cleared.
- **App:** reconnect already works (events reconnect, then the selected terminal re-attaches). Polish TODOs are in DECISIONS.md: a "restarting" state instead of "not running", re-attach on stream end, and pop-out windows.
- **libghostty `max_scrollback` = 10_000 keeps only about 800 rows** at 100 columns (it's a byte budget). The flood test is sized for that.
- **`tests/seed_insights.rs::seeded_history_feeds_series` fails today.** It's date-dependent seeded data ("api" outranks "midna" this week), it isn't upgrade code, and nothing was changed there.
