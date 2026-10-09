//! Keeps the Mac usable when agents get carried away (see midna_proto::system).
//!
//! Every TICK: read the load average, sample CPU time for every process under a terminal, and
//! - after `guard.overload_secs` busy, emit `system.overloaded` with the heaviest terminals
//!   (the app offers Pause / Stop processes), and `system.calm` once the load drops back;
//! - stop background poll loops under agent terminals older than `guard.loop_max_hours`
//!   (`until ! pgrep -f X; do sleep 5; done` never ends when two agents wait on each other).
//!
//! `session.pause` stops (SIGSTOP) a terminal's whole process tree, agent included, and
//! `session.resume` continues it; `session.stop_processes` ends what a terminal started
//! (SIGTERM, SIGKILL after 3s) while its shell or agent keeps running.
//!
//! Dev: `MIDNA_DEBUG_LOAD=<load1>` reports that load average instead of the real one.
use crate::daemon::Daemon;
use crate::procs;
use midna_proto::system::{self as sys, HeavyTerminal, SystemLoad};
use midna_proto::*;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_secs(10);
/// The load counts as calm again below this share of `system.busy_load` (no flapping).
const CALM_SHARE: f64 = 0.8;
const KILL_GRACE: Duration = Duration::from_secs(3);
/// Terminals listed in `system.overloaded` / `system.load`.
const TOP: usize = 5;

#[derive(Default)]
pub struct Runtime(Mutex<Inner>);

#[derive(Default)]
struct Inner {
    busy_since: Option<Instant>,
    /// `system.overloaded` went out for this busy spell.
    alerted: bool,
    /// (cpu_ns, process start) per pid at the last sample, and when it was taken.
    last: HashMap<i32, (u64, i64)>,
    last_at: Option<Instant>,
    top: Vec<HeavyTerminal>,
    paused: HashSet<Id>,
}

fn log(msg: &str) {
    eprintln!("midnad[{}]: {msg}", std::process::id());
}

impl Runtime {
    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub fn start(d: &Arc<Daemon>) {
    let w = Arc::downgrade(d);
    let _ = std::thread::Builder::new().name("guard".into()).spawn(move || {
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

// ------------------------------------------------------------------ load

/// (load1, load5, load15).
pub fn loadavg() -> (f64, f64, f64) {
    if let Some(l) = std::env::var("MIDNA_DEBUG_LOAD").ok().and_then(|v| v.trim().parse::<f64>().ok()) {
        return (l, l, l);
    }
    let mut a = [0f64; 3];
    let n = unsafe { libc::getloadavg(a.as_mut_ptr(), 3) };
    if n < 3 { (0., 0., 0.) } else { (a[0], a[1], a[2]) }
}

pub fn cpus() -> u32 {
    std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(1)
}

fn busy_at(d: &Daemon) -> u32 {
    d.setting("system.busy_load").as_i64().unwrap_or(150).max(1) as u32
}

/// The load right now, with the heaviest terminals from the last sample.
pub fn load(d: &Daemon) -> SystemLoad {
    let (load1, load5, load15) = loadavg();
    let cpus = cpus();
    let busy_at = busy_at(d);
    let pct = sys::load_percent(load1, cpus);
    let overload_secs = d.setting("guard.overload_secs").as_i64().unwrap_or(120).max(0) as u64;
    let g = d.guard.inner();
    let busy_for = g.busy_since.map(|t| t.elapsed().as_secs()).unwrap_or(0);
    let mut top = g.top.clone();
    for t in &mut top {
        t.paused = g.paused.contains(&t.session_id);
    }
    SystemLoad {
        load1: round2(load1),
        load5: round2(load5),
        load15: round2(load15),
        cpus,
        load_percent: pct,
        busy_at,
        busy: pct >= busy_at,
        busy_for_secs: busy_for,
        overloaded: g.busy_since.is_some() && busy_for >= overload_secs,
        top,
    }
}

/// Whether the Mac is busy right now (for postponing an upgrade).
pub fn busy(d: &Daemon) -> Option<SystemLoad> {
    let l = load(d);
    l.busy.then_some(l)
}

fn round2(v: f64) -> f64 {
    (v * 100.).round() / 100.
}

// ------------------------------------------------------------------ tick

fn tick(d: &Arc<Daemon>) {
    let table = procs::table();
    sample(d, &table);
    overload(d);
    stop_old_loops(d, &table);
}

/// Live terminals: (id, name, cwd, root pid, is an agent).
fn terminals(d: &Daemon) -> Vec<(Id, String, String, i32, bool)> {
    let pids: HashMap<Id, i32> = d.terminal_pids().into_iter().collect();
    let core = d.core();
    core.state
        .sessions
        .iter()
        .filter_map(|s| pids.get(&s.id).map(|&pid| (s.id.clone(), s.name.clone(), s.cwd.clone(), pid, s.agent.is_some())))
        .collect()
}

/// CPU per terminal since the last sample, heaviest first.
fn sample(d: &Daemon, table: &HashMap<i32, procs::Row>) {
    let now = Instant::now();
    let (prev, prev_at) = {
        let g = d.guard.inner();
        (g.last.clone(), g.last_at)
    };
    let secs = prev_at.map(|t| now.duration_since(t).as_secs_f64()).filter(|s| *s > 0.5);
    let mut top = vec![];
    for (id, name, cwd, root, _) in terminals(d) {
        let pids = procs::descendants(table, root);
        let (mut ns, mut busiest): (u64, HashMap<String, (u32, u64)>) = (0, HashMap::new());
        for pid in &pids {
            let r = &table[pid];
            // A pid reused since the last sample (other start time) counts from zero.
            let before = prev.get(pid).filter(|(_, start)| *start == r.start).map(|(c, _)| *c).unwrap_or(0);
            let used = r.cpu_ns.saturating_sub(before);
            ns += used;
            let e = busiest.entry(r.name.clone()).or_default();
            e.0 += 1;
            e.1 += used;
        }
        let Some(secs) = secs else { continue };
        let cpu_percent = (ns as f64 / 1e9 / secs * 100.).round() as u32;
        if cpu_percent == 0 {
            continue;
        }
        let mut names: Vec<(String, (u32, u64))> = busiest.into_iter().filter(|(_, (_, used))| *used > 0).collect();
        names.sort_by(|a, b| b.1.1.cmp(&a.1.1));
        let busiest = names.into_iter().take(3).map(|(n, (count, _))| if count > 1 { format!("{n} ×{count}") } else { n }).collect();
        top.push(HeavyTerminal { session_id: id, name, cwd, cpu_percent, processes: pids.len() as u32, busiest, paused: false });
    }
    top.sort_by(|a, b| b.cpu_percent.cmp(&a.cpu_percent));
    top.truncate(TOP);
    let mut g = d.guard.inner();
    g.last = table.iter().map(|(&pid, r)| (pid, (r.cpu_ns, r.start))).collect();
    g.last_at = Some(now);
    if secs.is_some() {
        g.top = top;
    }
}

fn overload(d: &Daemon) {
    let (load1, _, _) = loadavg();
    let cpus = cpus();
    let pct = sys::load_percent(load1, cpus) as f64;
    let busy_at = busy_at(d) as f64;
    let overload_secs = d.setting("guard.overload_secs").as_i64().unwrap_or(120).max(0) as u64;
    let fire = {
        let mut g = d.guard.inner();
        if pct >= busy_at {
            let since = *g.busy_since.get_or_insert_with(Instant::now);
            let fire = !g.alerted && since.elapsed().as_secs() >= overload_secs;
            g.alerted |= fire;
            fire
        } else if pct < busy_at * CALM_SHARE {
            let was = g.alerted;
            g.busy_since = None;
            g.alerted = false;
            if was {
                drop(g);
                d.emit(kinds::SYSTEM_CALM, Actor::system(), None, None, json!({ "load1": round2(load1), "cpus": cpus }));
            }
            false
        } else {
            false
        }
    };
    if fire {
        let l = load(d);
        log(&format!("system overloaded: load {} on {} cores ({}%)", l.load1, l.cpus, l.load_percent));
        let mut data = json!(l);
        data["alert"] = json!(d.setting("guard.overload_alert").as_bool().unwrap_or(true));
        d.emit(kinds::SYSTEM_OVERLOADED, Actor::system(), None, None, data);
    }
}

/// Stop agents' background poll loops that have run past `guard.loop_max_hours`.
fn stop_old_loops(d: &Daemon, table: &HashMap<i32, procs::Row>) {
    let max_hours = d.setting("guard.loop_max_hours").as_i64().unwrap_or(3);
    if max_hours <= 0 {
        return;
    }
    let now = time::now_unix();
    for (id, _, _, root, agent) in terminals(d) {
        if !agent || d.guard.inner().paused.contains(&id) {
            continue;
        }
        for pid in procs::descendants(table, root) {
            let r = &table[&pid];
            let hours = (now - r.start) as f64 / 3600.;
            if pid == root || r.start <= 0 || hours < max_hours as f64 {
                continue;
            }
            let Some(command) = procs::command_line(pid) else { continue };
            if !sys::is_poll_loop(&command) {
                continue;
            }
            // Its children (the sleep, the pgrep) go too; the parent (the agent) stays.
            let tree = procs::descendants(table, pid);
            terminate(&tree);
            let short: String = command.chars().take(300).collect();
            log(&format!("stopped a {hours:.1}h-old poll loop under {id} (pid {pid}): {short}"));
            d.emit(kinds::PROCS_LOOP_STOPPED, Actor::system(), None, Some(id.clone()), json!({ "pid": pid, "command": short, "hours": round2(hours) }));
        }
    }
}

/// SIGTERM now, SIGKILL whatever is still running (same start time) after KILL_GRACE.
fn terminate(pids: &[i32]) {
    let starts: Vec<(i32, Option<i64>)> = pids.iter().map(|&p| (p, procs::table_start(p))).collect();
    for &(pid, _) in &starts {
        unsafe { libc::kill(pid, libc::SIGTERM) };
        // A stopped process only acts on SIGTERM once continued.
        unsafe { libc::kill(pid, libc::SIGCONT) };
    }
    let _ = std::thread::Builder::new().name("guard-kill".into()).spawn(move || {
        std::thread::sleep(KILL_GRACE);
        for (pid, start) in starts {
            if start.is_some() && procs::table_start(pid) == start {
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
    });
}

// ------------------------------------------------------------------ rpc

fn root_of(d: &Daemon, sid: &str) -> Result<i32, RpcError> {
    if !d.core().state.sessions.iter().any(|s| s.id == sid) {
        return Err(RpcError::not_found(format!("no terminal {sid}")));
    }
    d.terminal_pids().into_iter().find(|(id, _)| id == sid).map(|(_, pid)| pid).ok_or_else(|| RpcError::conflict(format!("terminal {sid} has no running process")))
}

pub fn system_load(d: &Daemon) -> Result<Value, RpcError> {
    Ok(json!(load(d)))
}

/// `session.pause` / `session.resume`: SIGSTOP / SIGCONT the terminal's whole process tree.
pub fn pause(d: &Daemon, actor: Actor, p: SessionPauseParams, on: bool) -> Result<Value, RpcError> {
    let root = root_of(d, &p.session_id)?;
    let table = procs::table();
    let pids = procs::descendants(&table, root);
    let sig = if on { libc::SIGSTOP } else { libc::SIGCONT };
    // Stop the parents first (so none starts a new child meanwhile); continue the leaves first.
    let order: Vec<i32> = if on { pids.clone() } else { pids.iter().rev().copied().collect() };
    for pid in &order {
        unsafe { libc::kill(*pid, sig) };
    }
    {
        let mut g = d.guard.inner();
        if on {
            g.paused.insert(p.session_id.clone());
        } else {
            g.paused.remove(&p.session_id);
        }
    }
    let kind = if on { kinds::SESSION_PAUSED } else { kinds::SESSION_RESUMED };
    d.emit(kind, actor, None, Some(p.session_id.clone()), json!({ "processes": pids.len() }));
    Ok(json!(SessionPauseResult { session_id: p.session_id, paused: on, processes: pids.len() as u32 }))
}

pub fn is_paused(d: &Daemon, sid: &str) -> bool {
    d.guard.inner().paused.contains(sid)
}

/// `session.stop_processes`: end what a terminal started; its own process stays.
pub fn stop_processes(d: &Daemon, actor: Actor, p: StopProcessesParams) -> Result<Value, RpcError> {
    let root = root_of(d, &p.session_id)?;
    let table = procs::table();
    let under: Vec<i32> = procs::descendants(&table, root).into_iter().filter(|&pid| pid != root).collect();
    let chosen: Vec<i32> = match &p.pids {
        None => under.clone(),
        Some(want) => {
            if let Some(bad) = want.iter().find(|pid| !under.contains(pid)) {
                return Err(RpcError::bad_params(format!("pid {bad} isn't a process under terminal {} (its own process can't be stopped this way)", p.session_id)));
            }
            // A chosen process takes its children with it.
            let mut all: Vec<i32> = vec![];
            for &pid in want {
                for c in procs::descendants(&table, pid) {
                    if !all.contains(&c) {
                        all.push(c);
                    }
                }
            }
            all
        }
    };
    let stopped: Vec<StoppedProcess> = chosen
        .iter()
        .map(|&pid| StoppedProcess { pid, name: table[&pid].name.clone(), command: procs::command_line(pid).unwrap_or_default().chars().take(300).collect() })
        .collect();
    terminate(&chosen);
    d.emit(kinds::PROCS_STOPPED, actor, None, Some(p.session_id.clone()), json!({ "stopped": stopped }));
    Ok(json!(StopProcessesResult { session_id: p.session_id, stopped }))
}
