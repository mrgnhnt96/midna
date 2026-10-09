//! Keeps the Mac usable when agents get carried away (see midna_proto::system).
//!
//! Every TICK: read the load average, sample CPU time for every process under a terminal, and
//! - after `guard.overload_secs` busy, emit `system.overloaded` with the heaviest terminals
//!   (the app offers Pause / Stop processes), and `system.calm` once the load drops back;
//! - stop background poll loops under agent terminals older than `guard.loop_max_hours`
//!   (`until ! pgrep -f X; do sleep 5; done` never ends when two agents wait on each other);
//! - while busy, put what the heaviest agent terminals started into macOS's background mode
//!   (`guard.background_agents`), and take it back out once the load drops. The agent process
//!   itself (known from the hooks it runs) and the shell above it stay as they are, so new
//!   commands start at normal priority and are caught on the next tick.
//!
//! `hold` is the other half: `policy.request` asks it before an agent's tool call, and it denies
//! new subagents past `guard.max_subagents`, and new subagents and heavy commands while busy
//! (`guard.busy_gate`).
//!
//! `session.pause` stops (SIGSTOP) a terminal's whole process tree, agent included, and
//! `session.resume` continues it; `session.stop_processes` ends what a terminal started
//! (SIGTERM, SIGKILL after 3s) while its shell or agent keeps running. A pause is kept on the
//! terminal (`Session.paused`, so it outlives a daemon upgrade) and dropped once the terminal
//! runs a new process (a restart).
//!
//! Dev: `MIDNA_DEBUG_LOAD=<load1>` reports that load average instead of the real one.
use crate::daemon::Daemon;
use crate::procs;
use midna_proto::system::{self as sys, HeavyTerminal, Paused, SystemLoad};
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
/// An agent terminal using at least this much CPU (percent of one core) while the Mac is busy
/// goes into background mode.
const BACKGROUND_FROM: u32 = 100;
/// Shells and launchers between an agent and the hook command it runs.
const LAUNCHERS: [&str; 7] = ["sh", "bash", "zsh", "dash", "fish", "env", "midna"];

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
    /// Agent terminals whose processes are in background mode.
    backgrounded: HashSet<Id>,
    /// Each agent terminal's agent process (pid, start), from the hooks it runs.
    agents: HashMap<Id, (i32, i64)>,
    /// Background mode was cleared from every agent terminal once since midnad started (a
    /// restart forgets which terminals it put there).
    swept: bool,
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
    let (busy_since, sampled, backgrounded) = {
        let g = d.guard.inner();
        (g.busy_since, g.top.clone(), g.backgrounded.clone())
    };
    let busy_for = busy_since.map(|t| t.elapsed().as_secs()).unwrap_or(0);
    // Paused terminals use no CPU now; they're listed with what they used when paused.
    let paused: Vec<HeavyTerminal> = d
        .core()
        .state
        .sessions
        .iter()
        .filter_map(|s| {
            let p = s.paused.as_ref()?;
            Some(HeavyTerminal { session_id: s.id.clone(), name: s.name.clone(), cwd: s.cwd.clone(), cpu_percent: p.cpu_percent, processes: p.processes, busiest: p.busiest.clone(), paused: true, backgrounded: false })
        })
        .collect();
    let mut top: Vec<HeavyTerminal> = sampled.into_iter().filter(|t| !paused.iter().any(|p| p.session_id == t.session_id)).collect();
    top.truncate(TOP);
    for t in &mut top {
        t.backgrounded = backgrounded.contains(&t.session_id);
    }
    top.extend(paused);
    top.sort_by(|a, b| b.cpu_percent.cmp(&a.cpu_percent));
    SystemLoad {
        load1: round2(load1),
        load5: round2(load5),
        load15: round2(load15),
        cpus,
        load_percent: pct,
        busy_at,
        busy: pct >= busy_at,
        busy_for_secs: busy_for,
        overloaded: busy_since.is_some() && busy_for >= overload_secs,
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
    background(d, &table);
    stop_old_loops(d, &table);
    drop_stale_pauses(d);
}

/// A paused terminal running another process now (restarted, or its process ended) isn't
/// paused anymore.
fn drop_stale_pauses(d: &Daemon) {
    let pids: HashMap<Id, i32> = d.terminal_pids().into_iter().collect();
    let stale: Vec<Id> = {
        let mut core = d.core();
        let mut stale = vec![];
        for s in core.state.sessions.iter_mut() {
            if s.paused.as_ref().is_some_and(|p| pids.get(&s.id) != Some(&p.pid)) {
                s.paused = None;
                stale.push(s.id.clone());
            }
        }
        stale
    };
    if stale.is_empty() {
        return;
    }
    d.mark_dirty();
    for id in stale {
        d.emit(kinds::SESSION_RESUMED, Actor::system(), None, Some(id), json!({ "processes": 0, "reason": "restarted" }));
    }
}

// ------------------------------------------------------------------ background mode

/// Note the agent behind a hook: `pid` runs the hook, the agent is its nearest ancestor that
/// isn't a shell or launcher.
pub fn note_hook_caller(d: &Daemon, sid: &str, pid: i32) {
    let known = d.guard.inner().agents.get(sid).copied();
    if known.is_some_and(|(p, start)| procs::table_start(p) == Some(start)) {
        return;
    }
    let mut p = pid;
    for _ in 0..8 {
        let Some(up) = procs::parent(p).filter(|&up| up > 1) else { return };
        p = up;
        let Some(name) = procs::name(p) else { return };
        if !LAUNCHERS.contains(&name.as_str()) {
            if let Some(start) = procs::table_start(p) {
                d.guard.inner().agents.insert(sid.to_string(), (p, start));
            }
            return;
        }
    }
}

/// The agent process and everything above it up to the terminal's own process: never put into
/// background mode, so what it starts next begins at normal priority.
fn agent_chain(table: &HashMap<i32, procs::Row>, root: i32, agent: (i32, i64)) -> Option<Vec<i32>> {
    let (mut p, start) = agent;
    if table.get(&p).map(|r| r.start) != Some(start) {
        return None;
    }
    let mut chain = vec![p];
    while p != root {
        p = table.get(&p).map(|r| r.ppid).filter(|&up| up > 1)?;
        chain.push(p);
    }
    Some(chain)
}

/// Every process under `root` but `keep` into (or out of) background mode; how many changed.
fn set_tree_background(table: &HashMap<i32, procs::Row>, root: i32, keep: &[i32], on: bool) -> usize {
    procs::descendants(table, root).into_iter().filter(|pid| !keep.contains(pid)).filter(|&pid| procs::set_background(pid, on)).count()
}

fn background(d: &Daemon, table: &HashMap<i32, procs::Row>) {
    let enabled = d.setting("guard.background_agents").as_bool().unwrap_or(true);
    let (load1, _, _) = loadavg();
    let pct = sys::load_percent(load1, cpus()) as f64;
    let busy_at = busy_at(d) as f64;
    let (busy, calm) = (pct >= busy_at, pct < busy_at * CALM_SHARE);
    let terms = terminals(d);
    let (heavy, agents, was, swept) = {
        let g = d.guard.inner();
        let heavy: HashSet<Id> = g.top.iter().filter(|t| t.cpu_percent >= BACKGROUND_FROM).map(|t| t.session_id.clone()).collect();
        (heavy, g.agents.clone(), g.backgrounded.clone(), g.swept)
    };
    let mut now = was.clone();
    if !enabled || calm {
        now.clear();
    } else if busy {
        now.extend(heavy);
    }
    for (id, _, _, root, agent) in &terms {
        let chain = agents.get(id).and_then(|&a| agent_chain(table, *root, a));
        if now.contains(id) {
            // Keep the agent itself as it is; without it known, leave the terminal alone.
            let Some(chain) = chain.filter(|_| *agent && !is_paused(d, id)) else {
                now.remove(id);
                continue;
            };
            let n = set_tree_background(table, *root, &chain, true);
            if !was.contains(id) {
                log(&format!("busy: {id}'s processes go into background mode ({n})"));
                d.emit(kinds::GUARD_BACKGROUNDED, Actor::system(), None, Some(id.clone()), json!({ "processes": n }));
            }
        } else if was.contains(id) || (!swept && *agent) {
            let n = set_tree_background(table, *root, &[], false);
            if was.contains(id) {
                log(&format!("calm: {id}'s processes leave background mode ({n})"));
                d.emit(kinds::GUARD_FOREGROUNDED, Actor::system(), None, Some(id.clone()), json!({ "processes": n }));
            }
        }
    }
    let live: HashSet<&Id> = terms.iter().map(|t| &t.0).collect();
    now.retain(|id| live.contains(id));
    let mut g = d.guard.inner();
    g.backgrounded = now;
    g.agents.retain(|id, _| live.contains(id));
    g.swept |= !busy;
}

// ------------------------------------------------------------------ hold

/// Why an agent's tool call should wait, if it should: a subagent past `guard.max_subagents`,
/// or (`guard.busy_gate`) a subagent or heavy command while the Mac is busy.
pub fn hold(d: &Daemon, a: &PolicyAction) -> Option<String> {
    if a.kind != ActionKind::Tool {
        return None;
    }
    let sid = a.session.as_deref()?;
    let (tool, arg) = match a.value.split_once('(') {
        Some((t, rest)) => (t, rest.strip_suffix(')').unwrap_or(rest)),
        None => (a.value.as_str(), ""),
    };
    let subagent = matches!(tool, "Agent" | "Task");
    let reason = subagent.then(|| too_many_subagents(d, sid)).flatten().or_else(|| {
        let heavy = subagent || tool == "Bash" && sys::is_heavy_command(arg);
        if !heavy || !d.setting("guard.busy_gate").as_bool().unwrap_or(true) {
            return None;
        }
        let l = busy(d)?;
        let what = if subagent { "more subagents" } else { "builds, tests or installs" };
        Some(format!(
            "the Mac is busy (load {} on {} cores, {}%; busy from {}%). Don't start {what} now: wait a few minutes and try again (`midna system` shows the load). Work already running carries on.",
            l.load1, l.cpus, l.load_percent, l.busy_at
        ))
    });
    if reason.is_some() && subagent {
        // A denied Agent call never starts its subagent: it mustn't count as one about to.
        let mut core = d.core();
        if let Some(info) = core.state.session_mut(sid).and_then(|s| s.agent_info.as_mut()) {
            info.pending_agents.pop();
        }
    }
    reason
}

fn too_many_subagents(d: &Daemon, sid: &str) -> Option<String> {
    let max = d.setting("guard.max_subagents").as_i64().unwrap_or(4).max(0) as usize;
    let running = running_subagents(d, sid);
    (max > 0 && running >= max).then(|| format!("this terminal already runs {running} subagents (guard.max_subagents = {max}). Wait for one to finish, then start the next."))
}

/// Subagents running in `sid` or about to start: the Agent call being decided is already
/// among the pending ones (its PreToolUse came first), so it doesn't count.
fn running_subagents(d: &Daemon, sid: &str) -> usize {
    let core = d.core();
    let Some(info) = core.state.session(sid).and_then(|s| s.agent_info.as_ref()) else { return 0 };
    let mut ids: HashSet<&str> = info.subagents.iter().map(|a| a.id.as_str()).collect();
    ids.extend(info.background.iter().filter(|t| t.kind.contains("agent")).map(|t| t.id.as_str()));
    ids.len() + info.pending_agents.len().saturating_sub(1)
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
        top.push(HeavyTerminal { session_id: id, name, cwd, cpu_percent, processes: pids.len() as u32, busiest, paused: false, backgrounded: false });
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
        if !agent || is_paused(d, &id) {
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
    let paused = on.then(|| {
        // What it was using at the last sample, for the app to show while it's paused.
        let was = d.guard.inner().top.iter().find(|t| t.session_id == p.session_id).cloned().unwrap_or_default();
        Paused { since: time::now_rfc3339(), processes: pids.len() as u32, cpu_percent: was.cpu_percent, busiest: was.busiest, pid: root }
    });
    if let Some(s) = d.core().state.session_mut(&p.session_id) {
        // Pausing an already paused terminal keeps what it was using before.
        if !(on && s.paused.is_some()) {
            s.paused = paused;
        }
    }
    d.mark_dirty();
    let kind = if on { kinds::SESSION_PAUSED } else { kinds::SESSION_RESUMED };
    d.emit(kind, actor, None, Some(p.session_id.clone()), json!({ "processes": pids.len() }));
    Ok(json!(SessionPauseResult { session_id: p.session_id, paused: on, processes: pids.len() as u32 }))
}

pub fn is_paused(d: &Daemon, sid: &str) -> bool {
    d.core().state.session(sid).is_some_and(|s| s.paused.is_some())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    /// Scheduling priority `ps` reports (background mode drops it to 4).
    fn pri(pid: i32) -> i32 {
        let o = Command::new("ps").args(["-o", "pri=", "-p", &pid.to_string()]).output().unwrap();
        String::from_utf8_lossy(&o.stdout).trim().parse().unwrap()
    }

    #[test]
    fn background_mode_skips_the_agent_and_comes_back_out() {
        let mut sh = Command::new("/bin/sh").args(["-c", "sleep 30 & wait"]).stdout(Stdio::null()).spawn().unwrap();
        let root = sh.id() as i32;
        let child = loop {
            let t = procs::table();
            if let Some(p) = procs::descendants(&t, root).into_iter().find(|&p| p != root) {
                break p;
            }
            std::thread::yield_now();
        };
        let table = procs::table();
        let chain = agent_chain(&table, root, (root, table[&root].start)).unwrap();
        assert_eq!(chain, vec![root]);
        assert_eq!(set_tree_background(&table, root, &chain, true), 1);
        assert!(pri(child) <= 4 && pri(root) > 4, "child {} root {}", pri(child), pri(root));
        set_tree_background(&table, root, &[], false);
        assert!(pri(child) > 4, "{}", pri(child));
        // An agent outside the terminal's tree, or a reused pid, isn't one of its processes.
        assert!(agent_chain(&table, root, (std::process::id() as i32, 0)).is_none());
        assert!(agent_chain(&table, root, (root, table[&root].start + 1)).is_none());
        terminate(&[child]);
        let _ = sh.wait();
    }
}
