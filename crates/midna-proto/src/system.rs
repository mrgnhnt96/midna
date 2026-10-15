//! The Mac's load, runaway processes under terminals, and git worktrees agents left behind.
//! midnad's `sysload.rs`, `guard.rs` and `worktrees.rs` own the behaviour; these are the wire
//! types and the pure decisions they share with the app.
//!
//! - Busy: the 1-minute load average per core at or above `system.busy_load` percent. A busy
//!   Mac postpones a daemon upgrade (the handoff can time out under load and hang up every
//!   terminal) unless the caller forces it. The load average also counts work waiting on the
//!   disk, so it's only used for that.
//! - CPU busy: the Mac's CPU in use (all cores, from the kernel's tick counters) at or above
//!   `system.busy_cpu` percent. Overload, the busy gate and background mode go by this.
//! - Overload: CPU busy for `guard.overload_secs` in a row. midnad emits `system.overloaded`
//!   with the terminals and the other apps using the most CPU, so the app can say which it is
//!   and offer to pause terminals.
//! - Held: CPU busy and midna's terminals use at least half of the CPU in use
//!   ([`midna_is_the_cause`]). Only then does what agent terminals start run in macOS's
//!   background mode (`guard.background_agents`), and agents are told to wait instead of
//!   starting heavy work (`guard.busy_gate`): when other apps keep the Mac busy, holding
//!   agents back frees nothing. An agent runs at most `guard.max_subagents` subagents at once.
//! - Build slots: at most `guard.build_slots` builds, tests or installs run at once across every
//!   terminal, however calm the Mac is. Another one waits in line, in its PreToolUse hook, and
//!   starts when a slot frees: once the command has returned and nothing it started (a build
//!   sent to the background) still runs.
//! - Runaway loops: background `while`/`until` shell loops an agent started (polling for a
//!   build, a file, a process) that outlive `guard.loop_max_hours` are stopped.
//! - Worktrees: linked git worktrees of repos midna's terminals use are removed after
//!   `worktrees.auto_clean_hours` without activity, when nothing runs in them and they hold no
//!   uncommitted work. Branches are kept.
use crate::types::{Id, Timestamp};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `system.load`: the Mac's CPU use and load average, whether midna counts it as busy, and the
/// terminals and other apps using the most CPU.
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
    /// The load average is at or above `system.busy_load`: daemon upgrades wait.
    pub busy: bool,
    /// The Mac's CPU in use over the last sample, percent of all its cores (100 = all busy).
    #[serde(default)]
    pub cpu_percent: u32,
    /// What midna's terminals use of it, percent of all cores (the rest is other apps).
    #[serde(default)]
    pub midna_percent: u32,
    /// `system.busy_cpu`, in percent.
    #[serde(default)]
    pub busy_cpu_at: u32,
    /// `cpu_percent` is at or above `system.busy_cpu`.
    #[serde(default)]
    pub cpu_busy: bool,
    /// CPU busy because of midna's terminals ([`midna_is_the_cause`]): the busy gate holds agents'
    /// heavy work back, and `midna system wait` waits for this to clear.
    #[serde(default)]
    pub held: bool,
    /// CPU busy for this long (seconds, 0 when not); `overloaded` from `guard.overload_secs`.
    #[serde(default)]
    pub busy_for_secs: u64,
    #[serde(default)]
    pub overloaded: bool,
    /// Terminals by CPU use, heaviest first (only those using any).
    #[serde(default)]
    pub top: Vec<HeavyTerminal>,
    /// Apps and processes outside midna's terminals by CPU use, heaviest first. macOS's own
    /// processes (run as root or other users) can't be read one by one; they're in `system_percent`.
    #[serde(default)]
    pub others: Vec<OtherProcess>,
    /// CPU in use that no listed terminal or app accounts for: macOS and other users' processes,
    /// percent of all cores.
    #[serde(default)]
    pub system_percent: u32,
    /// `guard.build_slots`: builds, tests and installs that run at once (0 = no limit).
    #[serde(default)]
    pub build_slots: u32,
    /// The builds holding a slot, then the ones waiting for one in line order.
    #[serde(default)]
    pub builds: Vec<BuildSlot>,
}

/// A build, test or install holding a build slot or waiting in line for one.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BuildSlot {
    pub session_id: Id,
    /// The terminal's name.
    pub name: String,
    /// The command, cut to 200 characters.
    pub command: String,
    /// Waiting for a slot (else holding one).
    #[serde(default)]
    pub waiting: bool,
    /// Seconds since it got its slot, or since it started waiting.
    #[serde(default)]
    pub for_secs: u64,
}

/// An app (its processes grouped by the `.app` they run from) or a lone process outside midna's
/// terminals, and the CPU it uses.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OtherProcess {
    /// The app's name (`Xcode`), else the process name (`fseventsd`).
    pub name: String,
    /// Percent of one core, summed over its processes, like `HeavyTerminal.cpu_percent`.
    pub cpu_percent: u32,
    pub processes: u32,
    /// An app's busiest process names with counts: `clang ×22`, `XCBBuildService`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub busiest: Vec<String>,
}

/// Whether midna's terminals are what keeps the CPU busy: they use at least half of what's in use.
pub fn midna_is_the_cause(cpu_percent: u32, midna_percent: u32) -> bool {
    cpu_percent > 0 && midna_percent * 2 >= cpu_percent
}

/// A per-core percent (800 = eight cores) as a share of the whole Mac, in percent.
pub fn share_of_mac(cpu_percent: u32, cpus: u32) -> f64 {
    cpu_percent as f64 / cpus.max(1) as f64
}

/// A share of the Mac for people: `51%`, `<1%` (above 0 but under 1), `0%`.
pub fn share_label(share: f64) -> String {
    if share > 0. && share < 1. { "<1%".into() } else { format!("{}%", share.round() as u32) }
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

/// `system.run_anyway`: which terminal's agent goes ahead (default: the caller's own).
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct RunAnywayParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Id>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct RunAnywayResult {
    pub session_id: Id,
    pub run_anyway: bool,
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
/// Mac is busy. Any simple command in a compound one counts (`source env.sh && cargo test`), but
/// only by the program it runs: words in quotes, heredoc bodies and comments are text, not
/// commands (`tb note "ran make"` and `cat > x.md <<EOF … cargo build … EOF` aren't builds).
pub fn is_heavy_command(command: &str) -> bool {
    heavy_program(command).is_some()
}

/// The program of the first build, test or install in a shell command (`cargo` for
/// `source env.sh && cargo test`): what holds a build slot (`guard.build_slots`).
pub fn heavy_program(command: &str) -> Option<String> {
    shell_commands(command).iter().find_map(|words| heavy_words(words))
}

/// The program one simple command, as words, runs when it's a build, test or install.
fn heavy_words(words: &[String]) -> Option<String> {
    let mut i = 0;
    // Look past assignments, shell keywords and wrappers to the program they run.
    loop {
        let w = words.get(i)?;
        let base = w.rsplit('/').next().unwrap_or(w);
        // The index after the flags starting at `j`; flags in `with_arg` take the next word.
        let flags = |mut j: usize, with_arg: &[&str]| {
            while let Some(f) = words.get(j).filter(|f| f.starts_with('-') && f.len() > 1) {
                j += if with_arg.contains(&f.as_str()) { 2 } else { 1 };
            }
            j
        };
        i = match base {
            _ if is_assignment(w) => i + 1,
            "!" | "{" | "if" | "then" | "else" | "elif" | "do" | "while" | "until" | "time" | "nohup" | "exec" | "command" | "builtin" => {
                flags(i + 1, &[])
            }
            "env" => {
                let mut j = flags(i + 1, &["-u", "-C", "-S"]);
                while words.get(j).is_some_and(|a| is_assignment(a)) {
                    j += 1;
                }
                j
            }
            "nice" => flags(i + 1, &["-n"]),
            "sudo" => flags(i + 1, &["-u", "-g", "-C", "-D"]),
            "caffeinate" => flags(i + 1, &["-t", "-w"]),
            "timeout" | "gtimeout" => flags(i + 1, &["-s", "-k", "--signal", "--kill-after"]) + 1,
            "bash" | "zsh" | "sh" | "dash" | "fish" => {
                // `bash -c '<script>'`: the script is the command. `bash x.sh` runs a file, never heavy.
                let j = flags(i + 1, &[]);
                let c = words[i + 1..j].iter().any(|f| !f.starts_with("--") && f.contains('c'));
                return words.get(j).filter(|_| c).and_then(|s| heavy_program(s));
            }
            "eval" => return heavy_program(&words[i + 1..].join(" ")),
            _ => break,
        };
    }
    let prog = words[i].rsplit('/').next().unwrap_or(&words[i]);
    // The subcommand: the next word that isn't a flag or a toolchain (`cargo +nightly test`).
    let sub = words[i + 1..].iter().find(|n| !n.starts_with('-') && !n.starts_with('+')).map_or("", |s| s.as_str());
    let heavy = match prog {
        "make" | "gmake" | "ninja" | "xcodebuild" | "gradle" | "gradlew" | "mvn" | "bazel" => true,
        "cargo" => matches!(sub, "build" | "b" | "test" | "t" | "check" | "c" | "clippy" | "run" | "r" | "bench" | "doc" | "install" | "nextest"),
        "npm" | "pnpm" | "yarn" | "bun" => matches!(sub, "install" | "i" | "ci" | "add" | "build" | "test" | "run"),
        "swift" | "go" | "flutter" | "dart" | "docker" => matches!(sub, "build" | "test" | "install" | "compile"),
        _ => false,
    };
    heavy.then(|| prog.to_string())
}

/// `NAME=value` before a command.
fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        name.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// Splits a shell command into its simple commands, each a list of words, about the way a shell
/// would: quoted text stays inside its word (its `;` and `|` don't split), and heredoc bodies and
/// comments are dropped. `;` `&&` `||` `|` `&` `(` `)` backticks and newlines end a command.
fn shell_commands(command: &str) -> Vec<Vec<String>> {
    let chars: Vec<char> = command.chars().collect();
    let mut cmds = vec![];
    let mut words = vec![];
    let mut word = String::new();
    let mut in_word = false;
    // Heredoc terminators waiting for the end of the line their bodies start after.
    let mut heredocs: Vec<String> = vec![];
    let flush = |word: &mut String, in_word: &mut bool, words: &mut Vec<String>| {
        if *in_word {
            words.push(std::mem::take(word));
            *in_word = false;
        }
    };
    let end = |words: &mut Vec<String>, cmds: &mut Vec<Vec<String>>| {
        if !words.is_empty() {
            cmds.push(std::mem::take(words));
        }
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match c {
            '\'' => {
                in_word = true;
                i += 1;
                while i < chars.len() && chars[i] != '\'' {
                    word.push(chars[i]);
                    i += 1;
                }
            }
            '"' => {
                in_word = true;
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        i += 1;
                    }
                    word.push(chars[i]);
                    i += 1;
                }
            }
            '\\' => {
                in_word = true;
                if let Some(n) = next.filter(|n| *n != '\n') {
                    word.push(n);
                }
                i += 1;
            }
            '#' if !in_word => {
                while i + 1 < chars.len() && chars[i + 1] != '\n' {
                    i += 1;
                }
            }
            '<' if next == Some('<') && chars.get(i + 2) == Some(&'<') => {
                // A here-string (`<<<`): its word is an argument like any other.
                in_word = true;
                word.push_str("<<<");
                i += 2;
            }
            '<' if next == Some('<') => {
                flush(&mut word, &mut in_word, &mut words);
                i += 2;
                if chars.get(i) == Some(&'-') {
                    i += 1;
                }
                while chars.get(i).is_some_and(|c| *c == ' ' || *c == '\t') {
                    i += 1;
                }
                let mut delim = String::new();
                while let Some(&d) = chars.get(i).filter(|d| !d.is_whitespace() && !";&|()<>".contains(**d)) {
                    if !"'\"\\".contains(d) {
                        delim.push(d);
                    }
                    i += 1;
                }
                heredocs.push(delim);
                continue;
            }
            '\n' => {
                flush(&mut word, &mut in_word, &mut words);
                end(&mut words, &mut cmds);
                // Each pending heredoc's body runs up to a line that is just its terminator.
                for delim in heredocs.drain(..) {
                    loop {
                        let start = i + 1;
                        if start >= chars.len() {
                            i = chars.len();
                            break;
                        }
                        let stop = chars[start..].iter().position(|c| *c == '\n').map_or(chars.len(), |p| start + p);
                        i = stop;
                        if chars[start..stop].iter().collect::<String>().trim() == delim {
                            break;
                        }
                    }
                }
            }
            ';' | '|' | '(' | ')' | '`' => {
                flush(&mut word, &mut in_word, &mut words);
                end(&mut words, &mut cmds);
            }
            // `2>&1` and `&>log` are redirections, not a background `&`.
            '&' if word.ends_with(['>', '<']) || next == Some('>') => {
                in_word = true;
                word.push(c);
            }
            '&' => {
                flush(&mut word, &mut in_word, &mut words);
                end(&mut words, &mut cmds);
            }
            c if c.is_whitespace() => flush(&mut word, &mut in_word, &mut words),
            c => {
                in_word = true;
                word.push(c);
            }
        }
        i += 1;
    }
    flush(&mut word, &mut in_word, &mut words);
    end(&mut words, &mut cmds);
    cmds
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
    fn cause_and_shares() {
        assert!(midna_is_the_cause(90, 80));
        assert!(midna_is_the_cause(90, 45));
        assert!(!midna_is_the_cause(88, 6));
        assert!(!midna_is_the_cause(0, 0));
        assert_eq!(share_of_mac(940, 12).round(), 78.);
        assert_eq!((share_label(0.3), share_label(5.4), share_label(0.)), ("<1%".into(), "5%".into(), "0%".into()));
    }

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
        assert!(is_heavy_command("FOO=1 RUST_LOG=debug cargo test"));
        assert!(is_heavy_command("env -u X CARGO_INCREMENTAL=0 nice -n 10 time cargo build"));
        assert!(is_heavy_command("timeout 600 make -j8"));
        assert!(is_heavy_command("bash -lc 'source env.sh && cargo test -p midnad'"));
        assert!(is_heavy_command("cargo build 2>&1 | tail -20"));
        assert!(is_heavy_command("echo `make`"));
        assert!(is_heavy_command("for d in a b; do make -C $d; done"));
        assert!(is_heavy_command("cat > x <<EOF\nhi\nEOF\ncargo test"));
        assert!(is_heavy_command("echo hi # note\nmake"));
        assert_eq!(heavy_program("source ./env.sh && /usr/bin/cargo test -p midnad").as_deref(), Some("cargo"));
        assert_eq!(heavy_program("bash -lc 'cd app && flutter test'").as_deref(), Some("flutter"));
        assert_eq!(heavy_program("git status; npm ci && xcodebuild").as_deref(), Some("npm"));
        assert_eq!(heavy_program("cargo --version"), None);
    }

    #[test]
    fn heavy_commands_ignore_text() {
        // Real false positives from `midna events --kind guard.held`.
        assert!(!is_heavy_command(r#"tb checkpoint --done "split the make target; ran cargo build once" --next "cargo test""#));
        assert!(!is_heavy_command("tb found 'flutter build fails on CI && make hangs' --kind bug"));
        assert!(!is_heavy_command(r#"tb task new "Busy guard" --detail "it holds cargo build and make | dart test""#));
        assert!(!is_heavy_command("cat >> x.py <<'EOF'\nRE = re.compile(r\"make tha\")\ncargo build\nEOF"));
        assert!(!is_heavy_command("python3 - <<'EOF'\nopen('a').write(s.replace('cargo build', 'make'))\nEOF\ngit diff --stat"));
        assert!(!is_heavy_command("cat > pr.md <<EOF\n## Why\nmake\n  cargo test --workspace\nEOF"));
        assert!(!is_heavy_command("cat <<-EOF\n\tmake\n\tEOF"));
        assert!(!is_heavy_command("cat > a <<A && cat > b <<\"B\"\nmake\nA\ncargo build\nB\ngit status"));
        assert!(!is_heavy_command("git commit -m \"fix: don't run make; cargo build is enough\""));
        assert!(!is_heavy_command("echo make cargo build"));
        assert!(!is_heavy_command("grep <<< 'make all' make"));
        assert!(!is_heavy_command("bash scripts/build.sh"));
        assert!(!is_heavy_command("# cargo build\ngit status"));
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
