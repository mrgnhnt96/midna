## Load, runaway work and worktrees

- `midna system` shows the Mac's CPU in use, what midna's terminals and other apps use of it, and
  the terminals and apps using the most. Before starting heavy work (a workspace build, many
  worktree subagents at once), check it; on a busy Mac run fewer at a time. While midna's
  terminals keep the CPU busy (`held`), midna denies new subagents and builds/tests/installs
  (`guard.busy_gate`, reason "the Mac is busy"); when other apps keep it busy, nothing is held.
  Then tell the human you're waiting, run `midna system wait` as a background command (your
  tool's background mode, so its completion brings you back) and retry the held command once it
  finishes. It returns as soon as the gate would let the command through (`held` is false;
  `overloaded` stays on longer, so don't wait on that), and midnad never stops it as a poll loop.
  Don't write your own wait loop, and don't just end your turn: nothing would wake you.
  `--timeout S` gives up after S seconds (exit 4).
- If the human tells you to go ahead anyway, run `midna system run-anyway`, then retry: the gate
  lets your terminal through until their next prompt. Only on the human's word, never on your own.
- An agent runs at most `guard.max_subagents` subagents at once; start the next when one
  finishes. What you start while held may run in macOS's background mode (slower); that's
  expected.
- Don't wait on a process with `until ! pgrep -f "<pattern>"; do sleep N; done`: your own loop's
  command line contains the pattern, so two such loops see each other and never end. Wait for
  your background task's completion notice, or on a file the command writes. midnad stops poll
  loops under agent terminals after `guard.loop_max_hours`.
- `midna system pause|resume|stop <terminal>` are human only (you get an approval); the app
  offers them when the Mac stays overloaded (`system.overloaded`).
- `midna daemon upgrade|restart` wait while the load average is high (`busy`, error 7); `--force`
  is the human's call.
- `midna worktrees` lists the git worktrees of repos midna watches and why each is kept;
  idle ones (24h, nothing in them, no uncommitted changes) are removed by midnad, branches kept.
  Clean up the worktrees you create when you're done with them.
- When your terminal closes, midna cleans up after it (`cleanup.enabled`): a cheap headless model
  removes the linked worktree you worked in or made, the branches you made and their remote
  branches, once merged and with nothing uncommitted, plus the human's own `cleanup.items`.
  `midna cleanup preview` shows what closing this terminal would clean up; `midna cleanup runs`
  what earlier ones did. If the human asks you to clean up something every time, add it with
  `midna cleanup add "<instruction>"`. Don't rely on it for unmerged work: it keeps that.
