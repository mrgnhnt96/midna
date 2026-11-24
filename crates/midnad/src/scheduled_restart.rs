//! Scheduled restart: once a night, inside `daemon.restart_window` (local time, `off` = never),
//! midnad restarts itself through the same handoff an update uses (`upgrade::request` with our own
//! binary), so shells, agents and screens keep running. A safeguard against midnad slowing down
//! over weeks of uptime.
//!
//! It goes only once midnad has run `daemon.restart_min_uptime_hours` (wall clock, sleep
//! included), and never while something is waiting on an answer or an agent is mid-work: an
//! approval, prompt or request is open, an agent is working, in a turn, waiting on you, has
//! background work or subagents, a queued agent restart or an upgrade is running, or the Mac is
//! busy (`system.busy_load`, which would refuse the handoff anyway). Then it tries again every
//! minute until the window closes, and the next night after that. Notes, failures and failing
//! streams don't hold it: nobody waits on them, and they can stay open for a day.
use crate::daemon::Daemon;
use midna_proto::keep_awake::DayRule;
use midna_proto::*;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

const TICK: Duration = Duration::from_secs(60);
/// After a restart that didn't happen (selftest failed, handoff refused), wait this long before
/// trying again, so a broken binary isn't selftested every minute all night.
const RETRY_AFTER: Duration = Duration::from_secs(30 * 60);

/// `daemon.restart_window` as stored (`HH:MM-HH:MM`, `all day` or `off`): (start, end) in
/// minutes past midnight, `None` = off. Unreadable counts as off.
pub fn window(stored: &str) -> Option<(u32, u32)> {
    match DayRule::parse(stored) {
        Ok(DayRule::Hours { start, end }) => Some((start, end)),
        _ => None,
    }
}

/// Whether local minute `m` is inside `start`..`end`; an end at or before the start runs past
/// midnight, and start = end is all day.
pub fn in_window((start, end): (u32, u32), m: u32) -> bool {
    match start.cmp(&end) {
        std::cmp::Ordering::Equal => true,
        std::cmp::Ordering::Less => (start..end).contains(&m),
        std::cmp::Ordering::Greater => m >= start || m < end,
    }
}

/// What holds a restart back right now, counted from the daemon's state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Holds {
    /// Open needs-you items someone waits on (approvals, prompts, blocked, requests).
    pub waiting_items: usize,
    pub agents_working: usize,
    pub agents_waiting_on_you: usize,
    /// Agents idle between turns but with background tasks or subagents still running.
    pub agents_with_background: usize,
    pub agents_restarting: usize,
    pub upgrading: bool,
    /// The Mac's load is at or above `system.busy_load` (load per core, percent).
    pub busy_load: Option<u32>,
}

impl Holds {
    /// Why it can't go, in words (empty = go).
    pub fn reasons(&self) -> Vec<String> {
        let n = |n: usize, one: &str, many: &str| (n > 0).then(|| format!("{n} {}", if n == 1 { one } else { many }));
        [
            n(self.waiting_items, "item needs you", "items need you"),
            n(self.agents_working, "agent working", "agents working"),
            n(self.agents_waiting_on_you, "agent waiting on you", "agents waiting on you"),
            n(self.agents_with_background, "agent with background work", "agents with background work"),
            n(self.agents_restarting, "agent restarting", "agents restarting"),
            self.upgrading.then(|| "an update or restart is already running".to_string()),
            self.busy_load.map(|p| format!("the Mac is busy (load {p}% per core)")),
        ]
        .into_iter()
        .flatten()
        .collect()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    Off,
    OutsideWindow,
    /// midnad hasn't run `daemon.restart_min_uptime_hours` yet.
    TooSoon,
    Held(Vec<String>),
    Go,
}

/// The decision (pure, for tests): `window` from [`window`], `minute` the local minute of day,
/// `uptime_secs` since midnad started (this image), `min_hours` the uptime it needs.
pub fn decide(window: Option<(u32, u32)>, minute: u32, uptime_secs: i64, min_hours: i64, holds: &Holds) -> Decision {
    let Some(w) = window else { return Decision::Off };
    if !in_window(w, minute) {
        return Decision::OutsideWindow;
    }
    if uptime_secs < min_hours.max(1) * 3600 {
        return Decision::TooSoon;
    }
    let r = holds.reasons();
    if r.is_empty() { Decision::Go } else { Decision::Held(r) }
}

/// Count what holds a restart back.
pub fn holds(d: &Daemon) -> Holds {
    let mut h = Holds { upgrading: d.upgrading.load(Ordering::SeqCst), ..Default::default() };
    {
        let core = d.core();
        h.waiting_items = core
            .state
            .needs_you
            .iter()
            .filter(|n| !matches!(n.kind, NeedsYouKind::Note | NeedsYouKind::Failed | NeedsYouKind::StreamFailing))
            .count();
        for s in core.state.sessions.iter().filter(|s| s.pid.is_some()) {
            let agent = core.agents.get(&s.id);
            if agent.is_some_and(|a| a.restarting) {
                h.agents_restarting += 1;
            } else if s.status.state == StatusState::Working || agent.is_some_and(|a| a.in_turn) {
                h.agents_working += 1;
            } else if s.status.state == StatusState::NeedsYou {
                h.agents_waiting_on_you += 1;
            } else if s.agent_info.as_ref().is_some_and(|i| !i.background.is_empty() || !i.subagents.is_empty()) {
                h.agents_with_background += 1;
            }
        }
    }
    if h.waiting_items + h.agents_working + h.agents_waiting_on_you + h.agents_with_background + h.agents_restarting == 0 && !h.upgrading {
        // Only when nothing else holds it: reading the load is the costly part.
        h.busy_load = crate::guard::busy(d).map(|l| l.load_percent);
    }
    h
}

fn log(msg: &str) {
    eprintln!("midnad[{}]: scheduled restart: {msg}", std::process::id());
}

pub fn start(d: &Arc<Daemon>) {
    if !d.cfg.owns_process {
        return; // in-process test daemons can't re-exec
    }
    let w = Arc::downgrade(d);
    let _ = std::thread::Builder::new().name("scheduled-restart".into()).spawn(move || {
        let mut last_try: Option<std::time::Instant> = None;
        let mut last_held: Vec<String> = vec![];
        loop {
            std::thread::sleep(TICK);
            let Some(d) = w.upgrade() else { return };
            if d.shutting_down.load(Ordering::Relaxed) {
                return;
            }
            if last_try.is_some_and(|t| t.elapsed() < RETRY_AFTER) {
                continue;
            }
            match tick(&d) {
                Decision::Held(r) => {
                    if r != last_held {
                        log(&format!("waiting: {}", r.join(", ")));
                        last_held = r;
                    }
                }
                Decision::Go => {
                    last_try = Some(std::time::Instant::now());
                    last_held.clear();
                }
                _ => last_held.clear(),
            }
        }
    });
}

/// Decide, and on Go start the handoff (it re-execs this process shortly after).
fn tick(d: &Arc<Daemon>) -> Decision {
    let now = time::now_unix();
    let (stored, min_hours) = {
        let core = d.core();
        (core.state.setting_str("daemon.restart_window"), core.state.setting_i64("daemon.restart_min_uptime_hours"))
    };
    let (.., hour, minute, _) = time::local_parts(now);
    let uptime = now - time::parse_rfc3339(&d.started_at).unwrap_or(now);
    let (w, m) = (window(&stored), hour * 60 + minute);
    // Cheap checks first: count the holds only inside the window, once midnad is old enough.
    let decision = match decide(w, m, uptime, min_hours, &Holds::default()) {
        Decision::Go => decide(w, m, uptime, min_hours, &holds(d)),
        other => other,
    };
    if decision == Decision::Go {
        let hours = uptime / 3600;
        log(&format!("restarting after {hours}h (daemon.restart_window {stored})"));
        let bin = crate::upgrade::restart_binary(&d.cfg);
        if let Err(e) = crate::upgrade::request(d, &bin, "scheduled_restart", false) {
            log(&format!("didn't restart: {}", e.message));
        }
    }
    decision
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> u32 {
        midna_proto::keep_awake::parse_time(s).unwrap()
    }

    const DAY: i64 = 24 * 3600;

    #[test]
    fn reads_the_stored_window() {
        assert_eq!(window("03:00-05:00"), Some((180, 300)));
        assert_eq!(window("23:00-02:00"), Some((1380, 120)));
        assert_eq!(window("all day"), Some((0, 0)));
        assert_eq!(window("off"), None);
        assert_eq!(window("nonsense"), None);
    }

    #[test]
    fn window_runs_past_midnight_and_start_equal_end_is_all_day() {
        let w = (at("3am"), at("5am"));
        assert!(!in_window(w, at("2:59am")));
        assert!(in_window(w, at("3am")));
        assert!(in_window(w, at("4:59am")));
        assert!(!in_window(w, at("5am")));
        let late = (at("11pm"), at("2am"));
        assert!(in_window(late, at("11:30pm")));
        assert!(in_window(late, at("midnight")));
        assert!(in_window(late, at("1:59am")));
        assert!(!in_window(late, at("2am")));
        assert!(!in_window(late, at("noon")));
        assert!(in_window((at("9am"), at("9am")), at("4pm")));
    }

    #[test]
    fn goes_only_inside_the_window_once_old_enough() {
        let w = window("03:00-05:00");
        let quiet = Holds::default();
        assert_eq!(decide(None, at("4am"), 30 * DAY, 12, &quiet), Decision::Off);
        assert_eq!(decide(w, at("2am"), 30 * DAY, 12, &quiet), Decision::OutsideWindow);
        assert_eq!(decide(w, at("4pm"), 30 * DAY, 12, &quiet), Decision::OutsideWindow);
        assert_eq!(decide(w, at("4am"), 11 * 3600, 12, &quiet), Decision::TooSoon);
        assert_eq!(decide(w, at("4am"), 12 * 3600, 12, &quiet), Decision::Go);
        assert_eq!(decide(w, at("3am"), 30 * DAY, 168, &quiet), Decision::Go);
        assert_eq!(decide(w, at("3am"), 6 * DAY, 168, &quiet), Decision::TooSoon);
        // a fresh image (just restarted) never goes again in the same window
        assert_eq!(decide(w, at("3:05am"), 300, 1, &quiet), Decision::TooSoon);
        assert_eq!(decide(w, at("3:05am"), 300, 0, &quiet), Decision::TooSoon);
    }

    #[test]
    fn anything_waiting_or_working_holds_it() {
        let w = window("03:00-05:00");
        let held = |h: Holds| match decide(w, at("4am"), 30 * DAY, 12, &h) {
            Decision::Held(r) => r,
            other => panic!("{h:?} wasn't held: {other:?}"),
        };
        assert_eq!(held(Holds { waiting_items: 1, ..Default::default() }), ["1 item needs you"]);
        assert_eq!(held(Holds { agents_working: 2, ..Default::default() }), ["2 agents working"]);
        assert_eq!(held(Holds { agents_waiting_on_you: 1, ..Default::default() }), ["1 agent waiting on you"]);
        assert_eq!(held(Holds { agents_with_background: 1, ..Default::default() }), ["1 agent with background work"]);
        assert_eq!(held(Holds { agents_restarting: 1, ..Default::default() }), ["1 agent restarting"]);
        assert_eq!(held(Holds { upgrading: true, ..Default::default() }), ["an update or restart is already running"]);
        assert_eq!(held(Holds { busy_load: Some(310), ..Default::default() }), ["the Mac is busy (load 310% per core)"]);
        assert_eq!(held(Holds { waiting_items: 3, agents_working: 1, ..Default::default() }), ["3 items need you", "1 agent working"]);
        // outside the window or too soon, holds don't matter
        let busy = Holds { agents_working: 1, ..Default::default() };
        assert_eq!(decide(w, at("noon"), 30 * DAY, 12, &busy), Decision::OutsideWindow);
        assert_eq!(decide(w, at("4am"), 3600, 12, &busy), Decision::TooSoon);
    }
}
