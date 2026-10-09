//! The Mac's load, runaway processes under terminals, and git worktrees agents left behind.
//! midnad's `sysload.rs`, `guard.rs` and `worktrees.rs` own the behaviour; these are the wire
//! types and the pure decisions they share with the app.
//!
//! - Busy: the 1-minute load average per core at or above `system.busy_load` percent. A busy
//!   Mac postpones a daemon upgrade (the handoff can time out under load and hang up every
//!   terminal) unless the caller forces it.
//! - Overload: busy for `guard.overload_secs` in a row. midnad emits `system.overloaded` with
//!   the terminals using the most CPU, so the app can offer to pause or stop them.
//! - Busy agents: while busy, what agent terminals start runs in macOS's background mode
//!   (`guard.background_agents`), and agents are told to wait instead of starting heavy work
//!   (`guard.busy_gate`). An agent runs at most `guard.max_subagents` subagents at once.
//! - Runaway loops: background `while`/`until` shell loops an agent started (polling for a
//!   build, a file, a process) that outlive `guard.loop_max_hours` are stopped.
//! - Worktrees: linked git worktrees of repos midna's terminals use are removed after
//!   `worktrees.auto_clean_hours` without activity, when nothing runs in them and they hold no
//!   uncommitted work. Branches are kept.
use crate::types::{Id, Timestamp};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `system.load`: the load average, whether midna counts the Mac as busy, and the terminals
/// using the most CPU.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SystemLoad {
    pub load1: f64,
    pub load5: f64,
    pub load15: f64,
    pub cpus: u32,
    /// load1 per core, in percent (100 = every core busy).
    pub load_percent: u32,
    /// `system.busy_load`, in percent.
    pub busy_at: u32,
    pub busy: bool,
    /// Busy for at least `guard.overload_secs` (seconds busy so far, 0 when not busy).
    #[serde(default)]
    pub busy_for_secs: u64,
    #[serde(default)]
    pub overloaded: bool,
    /// Terminals by CPU use, heaviest first (only those using any).
    #[serde(default)]
    pub top: Vec<HeavyTerminal>,
}

/// A terminal and the CPU its processes use.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HeavyTerminal {
    pub session_id: Id,
    pub name: String,
    pub cwd: String,
    /// Percent of one core, summed over the terminal's processes (800 = eight cores).
    pub cpu_percent: u32,
    pub processes: u32,
    /// The busiest process names with counts: `rustc ×14`, `cargo ×4`.
    #[serde(default)]
    pub busiest: Vec<String>,
    /// Paused with session.pause (its processes are stopped). `cpu_percent`, `processes` and
    /// `busiest` are then what it was using when it was paused.
    #[serde(default)]
    pub paused: bool,
    /// What its agent started runs in macOS's background mode while the Mac is busy
    /// (`guard.background_agents`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub backgrounded: bool,
}

/// A terminal paused with `session.pause` (`Session.paused`): its whole process tree is
/// stopped until `session.resume`. What it was running then, for the app to show.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Paused {
    pub since: Timestamp,
    /// Processes stopped.
    pub processes: u32,
    /// CPU the terminal was using just before (percent of one core; 0 when not sampled yet).
    #[serde(default)]
    pub cpu_percent: u32,
    /// Its busiest processes then: `rustc ×11`, `cargo`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub busiest: Vec<String>,
    /// The terminal's own process when it was paused. A terminal that has started a new one
    /// since (a restart) isn't paused anymore.
    pub pid: i32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct SessionPauseParams {
    pub session_id: Id,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct SessionPauseResult {
    pub session_id: Id,
    pub paused: bool,
    /// Processes signalled.
    pub processes: u32,
}

/// `session.stop_processes`: stop what a terminal started without closing it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct StopProcessesParams {
    pub session_id: Id,
    /// Only these (each must run under the terminal). Omitted: every process under the
    /// terminal's own process (the shell or agent itself keeps running).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pids: Option<Vec<i32>>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct StopProcessesResult {
    pub session_id: Id,
    /// Sent SIGTERM (SIGKILL follows after 3s for any still running).
    pub stopped: Vec<StoppedProcess>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StoppedProcess {
    pub pid: i32,
    pub name: String,
    #[serde(default)]
    pub command: String,
}

/// A linked git worktree midna tracks.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WorktreeInfo {
    pub path: String,
    /// The main checkout it belongs to.
    pub repo: String,
    #[serde(default)]
    pub branch: String,
    /// Last sign of use: a terminal or process in it, a commit, checkout or staged change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_active_at: Option<Timestamp>,
    pub idle_hours: f64,
    /// Terminals whose folder is in it.
    #[serde(default)]
    pub terminals: Vec<Id>,
    /// Processes (any, not only midna's) whose working folder is in it.
    #[serde(default)]
    pub processes: u32,
    #[serde(default)]
    pub locked: bool,
    /// Why it was locked (`git worktree lock --reason`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_reason: Option<String>,
    /// Uncommitted changes to tracked files.
    #[serde(default)]
    pub dirty: bool,
    /// Removable now (with the auto-clean rules); else `keep_because` says why not.
    pub removable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_because: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct WorktreesListParams {
    /// Only worktrees of this repo (any folder inside it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct WorktreesList {
    pub worktrees: Vec<WorktreeInfo>,
    /// `worktrees.auto_clean_hours` (0 = auto-clean off).
    pub auto_clean_hours: u32,
    /// Main checkouts midna watches.
    pub repos: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct WorktreesCleanParams {
    /// Say what would go without removing anything.
    #[serde(default)]
    pub dry_run: bool,
    /// Only this worktree (it still has to be unused and clean).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Ignore the idle time (nothing may run in it, and it must have no uncommitted work).
    #[serde(default)]
    pub ignore_idle: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct WorktreesCleanResult {
    pub dry_run: bool,
    pub removed: Vec<WorktreeInfo>,
    /// Ones that qualified but `git worktree remove` refused (untracked files, …), with why.
    #[serde(default)]
    pub failed: Vec<WorktreeFailure>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WorktreeFailure {
    pub path: String,
    pub error: String,
}

/// load1 per core in percent.
pub fn load_percent(load1: f64, cpus: u32) -> u32 {
    if cpus == 0 {
        return 0;
    }
    (load1 / cpus as f64 * 100.).round().max(0.) as u32
}

/// A background shell loop an agent left polling: a non-interactive shell (`-c`) whose
/// script loops (`while`/`until`) on a sleep. Interactive shells and scripts run from a file
/// never count.
pub fn is_poll_loop(command: &str) -> bool {
    let mut words = command.split_whitespace();
    let Some(prog) = words.next() else { return false };
    let base = prog.rsplit('/').next().unwrap_or(prog).trim_start_matches('-');
    if !matches!(base, "bash" | "zsh" | "sh" | "dash" | "fish") {
        return false;
    }
    let Some(i) = command.find(" -c ").or_else(|| command.find(" -lc ")).or_else(|| command.find(" -ic ")) else { return false };
    let script = &command[i..];
    let loops = ["while ", "while\t", "until ", "until\t"].iter().any(|k| script.contains(k));
    loops && script.contains("sleep")
}

/// A shell command that builds, tests or installs: what `guard.busy_gate` holds back while the
/// Mac is busy. Any part of a compound command counts (`source env.sh && cargo test`).
pub fn is_heavy_command(command: &str) -> bool {
    let words: Vec<&str> = command.split(|c: char| c.is_whitespace() || ";&|()`".contains(c)).filter(|w| !w.is_empty()).collect();
    words.iter().enumerate().any(|(i, w)| {
        let prog = w.rsplit('/').next().unwrap_or(w);
        // The subcommand: the next word that isn't a flag or a toolchain (`cargo +nightly test`).
        let sub = words[i + 1..].iter().find(|n| !n.starts_with('-') && !n.starts_with('+')).copied().unwrap_or("");
        match prog {
            "make" | "gmake" | "ninja" | "xcodebuild" | "gradle" | "gradlew" | "mvn" | "bazel" => true,
            "cargo" => matches!(sub, "build" | "b" | "test" | "t" | "check" | "c" | "clippy" | "run" | "r" | "bench" | "doc" | "install" | "nextest"),
            "npm" | "pnpm" | "yarn" | "bun" => matches!(sub, "install" | "i" | "ci" | "add" | "build" | "test" | "run"),
            "swift" | "go" | "flutter" | "dart" | "docker" => matches!(sub, "build" | "test" | "install" | "compile"),
            _ => false,
        }
    })
}

/// Whether `path` is `dir` or inside it (both absolute, compared component-wise).
pub fn path_within(path: &str, dir: &str) -> bool {
    let dir = dir.trim_end_matches('/');
    !dir.is_empty() && (path == dir || path.strip_prefix(dir).is_some_and(|r| r.starts_with('/')))
}

/// A `git worktree lock` reason written by Claude Code for an agent worktree names the
/// agent's pid (`claude agent agent-a1b2 (pid 1234)`): that lock is stale once the pid is gone.
pub fn lock_pid(reason: &str) -> Option<i32> {
    let i = reason.find("pid ")?;
    reason[i + 4..].chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_percent_per_core() {
        assert_eq!(load_percent(24., 12), 200);
        assert_eq!(load_percent(6., 12), 50);
        assert_eq!(load_percent(1., 0), 0);
    }

    #[test]
    fn poll_loops() {
        assert!(is_poll_loop(r#"bash -c until ! pgrep -f "cargo test --workspace" >/dev/null; do sleep 5; done; cat x"#));
        assert!(is_poll_loop("/bin/zsh -c source snap.sh && eval 'while [ ! -s out ]; do sleep 1; done'"));
        assert!(is_poll_loop("/bin/bash -lc while true; do gh run list; sleep 60; done"));
        assert!(!is_poll_loop("-zsh"));
        assert!(!is_poll_loop("/bin/zsh -l"));
        assert!(!is_poll_loop("bash ./watch.sh"));
        assert!(!is_poll_loop("bash -c cargo build --workspace"));
        assert!(!is_poll_loop("python3 -c while True: sleep(1)"));
        assert!(!is_poll_loop("bash -c until make; do echo retry; done"));
    }

    #[test]
    fn heavy_commands() {
        assert!(is_heavy_command("cargo test --workspace"));
        assert!(is_heavy_command("source ./env.sh && cargo build -p midnad"));
        assert!(is_heavy_command("cargo +nightly clippy"));
        assert!(is_heavy_command("cd app; npm install"));
        assert!(is_heavy_command("/usr/bin/xcodebuild -scheme App"));
        assert!(is_heavy_command("(cd web && pnpm run build)"));
        assert!(!is_heavy_command("cargo --version"));
        assert!(!is_heavy_command("git status"));
        assert!(!is_heavy_command("grep -rn cargo crates"));
        assert!(!is_heavy_command("cmake --version"));
    }

    #[test]
    fn within() {
        assert!(path_within("/a/b", "/a/b"));
        assert!(path_within("/a/b/c", "/a/b/"));
        assert!(!path_within("/a/bc", "/a/b"));
        assert!(!path_within("/a", ""));
    }

    #[test]
    fn lock_pids() {
        assert_eq!(lock_pid("claude agent agent-a39e6d5bc3f5e65e7 (pid 15929)"), Some(15929));
        assert_eq!(lock_pid("on a usb drive"), None);
    }
}
