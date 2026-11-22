//! Holding new agent terminals while the Mac is overloaded (`guard.hold_agents_at`).
//!
//! With dozens of agents on a dozen cores, every new one makes all of them slower (a stress test
//! ran 59 at load ~199 on 12 cores: a grep took 7s, a tab took minutes to start). So while the
//! load average is far above the core count (load1 per core at or above `guard.hold_agents_at`
//! percent) and the CPU is busy (`system.busy_cpu`), an agent terminal opened by anything but the
//! human (an agent, a trigger, the task board) waits its turn instead of starting:
//!
//! - It never fails. `session.open` returns the terminal at once, so callers (and the task
//!   board's terminal cap, which counts open terminals) see it and don't retry. Its process is a
//!   small `/bin/sh` that prints why it waits and execs the agent once its go file
//!   (`<home>/admit/<id>`) appears. Ctrl-C in the terminal starts it now.
//! - It shows: the custom status [`WAITING`], with the load in its detail, until it starts.
//! - Its queued messages wait too (`queue::readiness`): nothing is typed into the wrapper.
//! - The line is `State.admit_line`, oldest first, so it outlives a daemon upgrade (the wrappers
//!   do too: they're ordinary terminal processes).
//! - Every TICK, once the load is calm (under CALM_SHARE of the setting, or the CPU no longer
//!   busy), the first in line starts, at most one per GAP: the load average lags by a minute,
//!   and letting them all go at once would rebuild the pile they waited out.
//! - While anyone waits, new agent terminals join the line behind them.
//! - The human's own opens always start; while overloaded they print a one-line warning first.
//! - Turning the setting off (0) lets the whole line go.
use crate::daemon::Daemon;
use crate::rpc::Ctx;
use midna_proto::system::SystemLoad;
use midna_proto::*;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The custom status a held terminal shows.
pub const WAITING: &str = "Waiting for the Mac";
/// The wrapper's `$0`: how a terminal still waiting is told from one whose agent runs.
const WRAPPER: &str = "midna-wait";
const TICK: Duration = Duration::from_secs(2);
/// Least time between two terminals starting from the line.
const GAP: Duration = Duration::from_secs(10);
/// The load counts as calm below this share of `guard.hold_agents_at` (no flapping).
const CALM_SHARE: f64 = 0.8;
/// The wrapper: `$1` is the go file, `$2` the message, the rest the agent's argv. A trapped
/// SIGINT (Ctrl-C) starts it now; exec puts the trap back to the default for the agent.
const SCRIPT: &str = r#"f=$1; trap 'go=1' INT; printf '%s\n' "$2"; shift 2; while [ -z "$go" ] && [ ! -e "$f" ]; do sleep 1; done; rm -f "$f"; exec "$@""#;

#[derive(Default)]
pub struct Runtime {
    last: Mutex<Option<Instant>>,
}

fn log(msg: &str) {
    eprintln!("midnad[{}]: {msg}", std::process::id());
}

fn hold_at(d: &Daemon) -> u32 {
    d.setting("guard.hold_agents_at").as_i64().unwrap_or(300).max(0) as u32
}

/// Far above the core count: new agent terminals wait.
fn over(l: &SystemLoad, at: u32) -> bool {
    at > 0 && l.cpu_busy && l.load_percent >= at
}

/// Calm enough to let the next one in line go.
fn calm(l: &SystemLoad, at: u32) -> bool {
    at == 0 || !l.cpu_busy || (l.load_percent as f64) < at as f64 * CALM_SHARE
}

fn go_file(d: &Daemon, sid: &str) -> std::path::PathBuf {
    d.cfg.home.join("admit").join(sid)
}

fn describe(l: &SystemLoad) -> String {
    format!("load {:.0} on {} cores, {}% of the CPU in use", l.load1, l.cpus, l.cpu_percent)
}

/// What a new agent terminal opened by `ctx` does now.
pub enum Admission {
    Start,
    /// The human's own: it starts, with this warning shown first.
    Warn(String),
    /// It waits in line.
    Hold(SystemLoad),
}

pub fn admission(d: &Daemon, ctx: &Ctx) -> Admission {
    let at = hold_at(d);
    if at == 0 {
        return Admission::Start;
    }
    let l = crate::guard::load(d);
    if ctx.is_human() {
        return match over(&l, at) {
            true => Admission::Warn(format!(
                "\x1b[33mmidna: the Mac is overloaded ({}). This agent starts anyway because you opened it; agents others start wait their turn (guard.hold_agents_at).\x1b[0m\r\n",
                describe(&l)
            )),
            false => Admission::Start,
        };
    }
    let waiting = !d.core().state.admit_line.is_empty();
    if waiting || over(&l, at) { Admission::Hold(l) } else { Admission::Start }
}

/// The argv a held terminal runs: wait for its go file, then exec `argv`.
pub fn wrap(d: &Daemon, sid: &str, l: &SystemLoad, argv: Vec<String>) -> Vec<String> {
    let go = go_file(d, sid);
    if let Some(dir) = go.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::remove_file(&go);
    let msg = format!(
        "\x1b[33mmidna: the Mac is overloaded ({}), so this agent waits its turn and starts once it calms down (guard.hold_agents_at). Press Ctrl-C to start it now.\x1b[0m",
        describe(l)
    );
    let mut v = vec!["/bin/sh".into(), "-c".into(), SCRIPT.into(), WRAPPER.into(), go.to_string_lossy().into_owned(), msg];
    v.extend(argv);
    v
}

/// Put a just-opened terminal running [`wrap`] in line.
pub fn hold(d: &Daemon, sid: &str, l: &SystemLoad) {
    let ahead = {
        let mut core = d.core();
        let ahead = core.state.admit_line.len();
        core.state.admit_line.push(sid.to_string());
        ahead
    };
    d.mark_dirty();
    log(&format!("admit: {sid} waits for the Mac ({}, {ahead} ahead)", describe(l)));
    let cs = CustomStatus {
        label: WAITING.into(),
        color: "amber".into(),
        icon: None,
        base: StatusState::Idle,
        clear_on: StatusClear::Never,
        trigger_id: None,
        detail: Some(format!("The Mac is overloaded ({}). This agent starts once it calms down; Ctrl-C in the terminal starts it now.", describe(l))),
        needs_you_id: None,
        since: time::now_rfc3339(),
    };
    crate::local::set_custom(d, sid, cs, Actor::system());
}

/// Whether `sid` waits in line.
pub fn is_waiting(d: &Daemon, sid: &str) -> bool {
    d.core().state.admit_line.iter().any(|s| s == sid)
}

pub fn start(d: &Arc<Daemon>) {
    let w = Arc::downgrade(d);
    let _ = std::thread::Builder::new().name("admit".into()).spawn(move || {
        loop {
            let Some(d) = w.upgrade() else { return };
            if d.shutting_down.load(Ordering::Relaxed) {
                return;
            }
            tick(&d);
            drop(d);
            std::thread::sleep(TICK);
        }
    });
}

/// Whether the terminal's process is still the wrapper waiting for its go file.
fn wrapper_runs(pid: Option<i32>) -> bool {
    pid.and_then(crate::procs::command_line).is_some_and(|c| c.split(' ').any(|a| a == WRAPPER))
}

fn tick(d: &Daemon) {
    let line = d.core().state.admit_line.clone();
    if line.is_empty() {
        return;
    }
    // Closed, restarted, or started by Ctrl-C: out of the line.
    let pids: Vec<(String, Option<Option<i32>>)> = {
        let core = d.core();
        line.iter().map(|sid| (sid.clone(), core.state.session(sid).map(|s| s.pid))).collect()
    };
    for (sid, pid) in pids {
        match pid {
            None => leave(d, &sid, None),
            Some(pid) if !wrapper_runs(pid) => leave(d, &sid, Some("started")),
            _ => {}
        }
    }
    let Some(first) = d.core().state.admit_line.first().cloned() else { return };
    let at = hold_at(d);
    let l = crate::guard::load(d);
    if at > 0 && !calm(&l, at) {
        return;
    }
    let mut last = d.admit.last.lock().unwrap_or_else(|e| e.into_inner());
    if at > 0 && last.is_some_and(|t| t.elapsed() < GAP) {
        return;
    }
    *last = Some(Instant::now());
    drop(last);
    let waited = d.core().state.session(&first).and_then(|s| s.custom_status.as_ref().map(|c| c.since.clone()));
    if let Err(e) = std::fs::write(go_file(d, &first), b"") {
        log(&format!("admit: can't start {first}: {e}"));
        return;
    }
    log(&format!("admit: {first} starts ({}; waiting since {})", describe(&l), waited.unwrap_or_default()));
    leave(d, &first, Some("admitted"));
}

/// Take `sid` out of the line, and its status off when it's still there (`why`).
fn leave(d: &Daemon, sid: &str, why: Option<&str>) {
    d.core().state.admit_line.retain(|s| s != sid);
    d.mark_dirty();
    let Some(why) = why else {
        let _ = std::fs::remove_file(go_file(d, sid));
        return;
    };
    let (ours, holds) = {
        let core = d.core();
        let s = core.state.session(sid);
        let ours = s.and_then(|s| s.custom_status.as_ref()).is_some_and(|c| c.label == WAITING);
        (ours, s.is_some_and(|s| s.status.reason.as_deref() == Some(WAITING)))
    };
    if ours {
        crate::local::clear_custom(d, sid, Actor::system(), why, true);
        // Its base is idle, so clearing leaves the label as the reason; the agent's hooks take
        // over from here, unless they already have.
        if holds {
            d.set_status(sid, StatusState::Idle, Some(why.into()), None, Actor::system());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(load_percent: u32, cpu_busy: bool) -> SystemLoad {
        SystemLoad { load_percent, cpu_busy, ..Default::default() }
    }

    #[test]
    fn holds_only_far_above_the_cores_with_a_busy_cpu() {
        assert!(over(&load(300, true), 300));
        assert!(!over(&load(299, true), 300));
        // A high load average with idle cores is work waiting on the disk: nothing to free.
        assert!(!over(&load(900, false), 300));
        assert!(!over(&load(900, true), 0), "0 = off");
    }

    #[test]
    fn lets_the_line_go_once_well_under() {
        assert!(!calm(&load(250, true), 300), "between the marks it keeps holding");
        assert!(calm(&load(239, true), 300));
        assert!(calm(&load(900, false), 300));
        assert!(calm(&load(900, true), 0));
    }

    #[test]
    fn the_wrapper_waits_for_its_go_file_then_execs() {
        let dir = std::env::temp_dir().join(format!("midna-admit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let go = dir.join("go");
        let out = dir.join("out");
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", SCRIPT, WRAPPER, &go.to_string_lossy(), "waiting", "/bin/sh", "-c", &format!("echo ran \"$@\" > {}", out.display()), "x", "a b"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(child.try_wait().unwrap().is_none(), "it waits");
        assert!(wrapper_runs(Some(child.id() as i32)));
        std::fs::write(&go, b"").unwrap();
        assert!(child.wait().unwrap().success());
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "ran a b\n");
        assert!(!go.exists(), "the go file is gone");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
