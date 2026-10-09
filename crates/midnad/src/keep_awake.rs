//! Keep the Mac awake for agents (`keep_awake.status|set`, settings `keep_awake.*`).
//!
//! Inside the keep-awake hours midnad holds an IOKit `PreventUserIdleSystemSleep` assertion
//! named "midna: keeping awake for agents", in with_work mode only while there is work (an
//! agent working or with background work, subagents or a wakeup due, queued input, an agent
//! waiting to resume, a schedule trigger due before the hours end) and `keep_awake.linger_mins`
//! after it. It only stops idle system sleep: the display sleeps, the screen locks, closing the
//! lid sleeps. On battery below `keep_awake.min_battery` it is released (and comes back 5 points
//! above it, or on AC).
//!
//! The assertion belongs to this process, so macOS drops it when midnad exits; it is released
//! by hand before an upgrade's execv. Every `TICK` the decision is made again and the assertion
//! taken or released to match; `keep_awake.changed` fires when that changes. A daemon inside
//! another process (tests) never touches IOKit: it records what it would hold, and its battery
//! is whatever `set_battery` says.
use crate::daemon::Daemon;
use midna_proto::keep_awake::{self as ka, Battery, BatteryGate, Closed, Plan, Today};
use midna_proto::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_secs(5);
/// How long a battery reading is good for.
const BATTERY_EVERY: Duration = Duration::from_secs(30);
pub const ASSERTION_NAME: &str = "midna: keeping awake for agents";
/// With the hours open all day every day, work due within this long counts.
const FAR_SECS: i64 = 24 * 3600;

#[derive(Default)]
struct Inner {
    /// The assertion's IOKit id while held (0 for a daemon that doesn't own its process).
    held: Option<u32>,
    held_since: Option<i64>,
    gate: BatteryGate,
    battery: Option<Battery>,
    battery_at: Option<Instant>,
    /// When there was work last (for `keep_awake.linger_mins`).
    last_work: Option<Instant>,
    /// macOS refused the assertion: its error, until it is taken.
    error: Option<i32>,
    /// What the last decision was (and the scheduled wake), so `keep_awake.changed` fires on a
    /// change only.
    last: Option<(bool, KeepAwakeReason, Option<Timestamp>)>,
    /// Tests: the battery to report instead of IOKit's.
    fake_battery: Option<Option<Battery>>,
}

#[derive(Default)]
pub struct Runtime {
    inner: Mutex<Inner>,
}

impl Runtime {
    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Tests: report this battery (None = a Mac without one).
#[doc(hidden)]
pub fn set_battery(d: &Daemon, b: Option<Battery>) {
    let mut i = d.keep_awake.inner();
    i.fake_battery = Some(b);
    i.battery_at = None;
}

pub fn start(d: &Arc<Daemon>) {
    let w = Arc::downgrade(d);
    let _ = std::thread::Builder::new().name("keep-awake".into()).spawn(move || {
        loop {
            let Some(d) = w.upgrade() else { return };
            if d.shutting_down.load(Ordering::Relaxed) {
                release(&d);
                return;
            }
            tick(&d);
            drop(d);
            std::thread::sleep(TICK);
        }
    });
}

/// Let go of the assertion now (before an upgrade's execv). The next tick takes it again if
/// it should be held.
pub fn release(d: &Daemon) {
    let mut i = d.keep_awake.inner();
    if let Some(id) = i.held.take() {
        if d.cfg.owns_process {
            iokit::release(id);
        }
        i.held_since = None;
    }
}

// ------------------------------------------------------------------ deciding

/// The keep_awake.* settings, as stored.
fn settings(st: &crate::state::State) -> KeepAwakeSettings {
    let list = |k: &str| -> Vec<String> { serde_json::from_value(st.setting(k)).unwrap_or_default() };
    let hours = list("keep_awake.hours").iter().filter_map(|r| r.split_once(" = ").map(|(d, h)| (d.to_string(), h.to_string()))).collect();
    KeepAwakeSettings {
        enabled: st.setting_bool("keep_awake.enabled"),
        mode: st.setting_str("keep_awake.mode"),
        start: st.setting_str("keep_awake.start"),
        end: st.setting_str("keep_awake.end"),
        days: list("keep_awake.days"),
        hours,
        min_battery: st.setting_i64("keep_awake.min_battery"),
        linger_mins: st.setting_i64("keep_awake.linger_mins"),
        wake: st.setting_bool("keep_awake.wake"),
    }
}

fn plan(s: &KeepAwakeSettings) -> Plan {
    let hours: Vec<String> = s.hours.iter().map(|(d, h)| format!("{d} = {h}")).collect();
    Plan::from_settings(&s.start, &s.end, &s.days, &hours)
}

/// What there is to do that needs the Mac awake, in words (`2 agents working`), counting what
/// is due before `horizon` (unix).
fn work(d: &Daemon, now: i64, horizon: i64) -> Vec<String> {
    let due_cron = |expr: &str| cron::Cron::parse(expr).ok().and_then(|c| c.next_after(now)).is_some_and(|t| t < horizon);
    let (mut working, mut wakeups, mut queued) = (0, 0, 0);
    let schedules = {
        let core = d.core();
        for s in core.state.sessions.iter().filter(|s| s.pid.is_some()) {
            let info = s.agent_info.as_ref();
            let busy = s.status.state == StatusState::Working
                || core.agents.get(&s.id).is_some_and(|a| a.in_turn)
                || info.is_some_and(|i| !i.background.is_empty() || !i.subagents.is_empty());
            if s.agent.is_some() && busy {
                working += 1;
            } else if info.is_some_and(|i| i.crons.iter().any(|c| due_cron(&c.schedule))) {
                wakeups += 1;
            }
            let head_due = s.queue.first().is_some_and(|m| {
                m.state != QueueState::Failed
                    && match &m.when {
                        SendWhen::At { at } => time::parse_rfc3339(at).is_none_or(|t| t < horizon),
                        _ => true,
                    }
            });
            if !s.queue_paused && head_due {
                queued += 1;
            }
        }
        core.state
            .triggers
            .iter()
            .filter(|t| crate::local::is_live(t) && t.event.trim().eq_ignore_ascii_case("schedule"))
            .filter(|t| cron::Schedule::of(&t.filter, t.fired).ok().and_then(|c| c.upcoming(now, 1).first().copied()).is_some_and(|at| at < horizon))
            .count()
    };
    let resuming = crate::resume::watching(d);
    let n = |n: usize, one: &str, many: &str| (n > 0).then(|| format!("{n} {}", if n == 1 { one } else { many }));
    [
        n(working, "agent working", "agents working"),
        n(wakeups, "agent with a wakeup due", "agents with wakeups due"),
        n(queued, "terminal with queued input", "terminals with queued input"),
        n(resuming, "agent waiting to resume", "agents waiting to resume"),
        n(schedules, "schedule trigger due", "schedule triggers due"),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// The decision, given the inputs (pure, for tests).
#[derive(Clone, Copy, Debug)]
struct Inputs {
    enabled: bool,
    always: bool,
    window: Result<(), Closed>,
    battery_ok: bool,
    working: bool,
    lingering: bool,
}

fn decide(i: Inputs) -> (bool, KeepAwakeReason) {
    use KeepAwakeReason::*;
    let r = match i.window {
        _ if !i.enabled => Disabled,
        Err(Closed::OutsideHours) => OutsideHours,
        Err(Closed::DayOff) => DayOff,
        Err(Closed::TodayOff) => TodayOff,
        Ok(()) if !i.battery_ok => BatteryLow,
        Ok(()) if i.always => Always,
        Ok(()) if i.working || i.lingering => Work,
        Ok(()) => NoWork,
    };
    (matches!(r, Work | Always), r)
}

/// `9 AM`, `tomorrow 9 AM`, `Mon 9 AM`.
pub(crate) fn when(t: i64, now: i64) -> String {
    let (.., h, mi, wd) = time::local_parts(t);
    let at = cron::clock(h, mi);
    let day = (time::local_day_start(t) - time::local_day_start(now)) / 86_400;
    match day {
        0 => at,
        1 => format!("tomorrow {at}"),
        _ => format!("{} {at}", ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][wd as usize]),
    }
}

/// Decide, take or release the assertion to match, and say where things stand. Fires
/// `keep_awake.changed` when held or the reason changed (or `force`).
fn tick_with(d: &Daemon, force: bool) -> KeepAwakeStatus {
    let now = time::now_unix();
    let date = ka::local_date(now);
    let (s, today) = {
        let mut core = d.core();
        let stale = core.state.keep_awake_today.as_ref().is_some_and(|t| t.date != date);
        if stale {
            core.state.keep_awake_today = None;
        }
        let r = (settings(&core.state), core.state.keep_awake_today.clone());
        drop(core);
        if stale {
            d.mark_dirty();
        }
        r
    };
    let p = plan(&s);
    let window = ka::open_at(&p, today.as_ref(), now);
    let change = ka::next_change(&p, today.as_ref(), now);
    // keep_awake.wake: the next work due inside the hours. Close to it (the Mac woke for it)
    // keep-awake holds even before the hours open, so it can run.
    let due = if s.enabled && s.wake { crate::wake::next_due(d, &p, today.as_ref(), now) } else { None };
    let woke = due.as_ref().map(|(t, _)| *t).filter(|t| t - now <= crate::wake::LEAD + 60);
    let (hold, horizon) = match (window, woke) {
        (Err(_), Some(t)) => (Ok(()), t + 60),
        _ => (window, change.unwrap_or(now + FAR_SECS)),
    };
    let work = if s.enabled && hold.is_ok() { work(d, now, horizon) } else { vec![] };

    let mut i = d.keep_awake.inner();
    if i.battery_at.is_none_or(|at| at.elapsed() >= BATTERY_EVERY) {
        i.battery = match i.fake_battery {
            Some(b) => b,
            None if d.cfg.owns_process => iokit::battery(),
            None => None,
        };
        i.battery_at = Some(Instant::now());
    }
    let battery = i.battery;
    let battery_ok = i.gate.update(battery, s.min_battery.clamp(0, 100) as u8);
    if !work.is_empty() {
        i.last_work = Some(Instant::now());
    }
    let linger = Duration::from_secs(s.linger_mins.max(0) as u64 * 60);
    let lingering = i.last_work.is_some_and(|at| at.elapsed() < linger);
    let inputs = Inputs { enabled: s.enabled, always: s.mode == "always", window: hold, battery_ok, working: !work.is_empty(), lingering };
    let (want, mut reason) = decide(inputs);
    // A handoff is about to exec: don't take it again in between (the new image will).
    let handing_off = d.upgrading.load(Ordering::Relaxed);
    if want && i.held.is_none() && !handing_off {
        let taken = if d.cfg.owns_process { iokit::create(ASSERTION_NAME) } else { Ok(0) };
        match taken {
            Ok(id) => {
                i.held = Some(id);
                i.held_since = Some(now);
                i.error = None;
            }
            Err(e) => i.error = Some(e),
        }
    } else if !want && let Some(id) = i.held.take() {
        if d.cfg.owns_process {
            iokit::release(id);
        }
        i.held_since = None;
        i.error = None;
    }
    let held = i.held.is_some();
    if want && !held {
        reason = KeepAwakeReason::Failed;
    }
    let lingered = i.last_work.map(|at| at.elapsed());
    let error = i.error;
    let held_since = i.held_since;
    let low = i.gate.low;
    drop(i);
    let wake = crate::wake::sync(d, s.enabled && s.wake && battery_ok, due.as_ref(), now);
    let mut i = d.keep_awake.inner();
    let changed = i.last != Some((held, reason, wake.next.clone()));
    i.last = Some((held, reason, wake.next.clone()));
    drop(i);

    let schedule = p.describe();
    let next = change.map(|t| when(t, now));
    let until = next.as_ref().filter(|_| window.is_ok()).map(|n| format!(" until {n}")).unwrap_or_default();
    let again = next.as_ref().filter(|_| window.is_err()).map(|n| format!("; on again {n}")).unwrap_or_default();
    use KeepAwakeReason::*;
    let line = match reason {
        Work if work.is_empty() => {
            let left = linger.saturating_sub(lingered.unwrap_or_default()).as_secs().div_ceil(60);
            format!("Keeping awake {left} more min after the last work{until}")
        }
        Work => format!("Keeping awake{until}: {}", work.join(", ")),
        Always => format!("Keeping awake{until} (always during hours)"),
        Disabled => "Keep-awake is off (keep_awake.enabled)".into(),
        OutsideHours => format!("Not keeping awake: outside hours ({schedule}){again}"),
        DayOff => format!("Not keeping awake: today is off ({schedule}){again}"),
        TodayOff => format!("Not keeping awake: {}{again}", today.as_ref().map(Today::words).unwrap_or_else(|| "off today".into())),
        BatteryLow => {
            let pct = battery.map(|b| b.percent).unwrap_or_default();
            let back = (s.min_battery as u8).saturating_add(ka::BATTERY_HYSTERESIS);
            format!("Not keeping awake: battery at {pct}% (below {}%); again at {back}% or on power", s.min_battery)
        }
        NoWork => format!("Ready to keep awake{until}: no work right now"),
        Failed => format!("Couldn't keep awake: macOS refused the power assertion (IOReturn {:#x}); retrying", error.unwrap_or_default()),
    };
    let status = KeepAwakeStatus {
        held,
        reason,
        line,
        window_open: window.is_ok(),
        next_on: change.filter(|_| window.is_err()).map(time::format_unix),
        next_off: change.filter(|_| window.is_ok()).map(time::format_unix),
        work,
        held_since: held_since.map(time::format_unix),
        battery: battery.map(|b| KeepAwakeBattery { percent: b.percent, on_ac: b.on_ac, low }),
        schedule,
        settings: s,
        today: today.map(|t| KeepAwakeToday { line: t.words(), date: t.date, on: t.on, until: t.until }),
        wake,
    };
    if changed || force {
        d.emit(kinds::KEEP_AWAKE_CHANGED, Actor::system(), None, None, serde_json::to_value(&status).unwrap_or_default());
    }
    status
}

pub fn tick(d: &Daemon) -> KeepAwakeStatus {
    tick_with(d, false)
}

// ------------------------------------------------------------------ RPC

/// `keep_awake.status`.
pub fn status(d: &Daemon) -> crate::rpc::R {
    crate::rpc::ok(tick(d))
}

/// `keep_awake.set`: check every field, then save them (as the `keep_awake.*` settings and
/// today's override) and decide again at once.
pub fn set(d: &Daemon, ctx: &crate::rpc::Ctx, p: KeepAwakeSetParams) -> crate::rpc::R {
    let bad = |e: String| RpcError::bad_params(e);
    let mut values: Vec<(&str, Value)> = vec![];
    let mut put = |key: &'static str, v: Option<Value>| -> Result<(), RpcError> {
        if let Some(v) = v {
            let spec = midna_proto::settings::setting(key).ok_or_else(|| RpcError::internal(format!("no setting {key}")))?;
            values.push((key, spec.coerce(&v).map_err(bad)?));
        }
        Ok(())
    };
    put("keep_awake.enabled", p.enabled.map(Value::Bool))?;
    put("keep_awake.mode", p.mode.map(Value::String))?;
    put("keep_awake.start", p.start.map(Value::String))?;
    put("keep_awake.end", p.end.map(Value::String))?;
    put("keep_awake.days", p.days)?;
    let hours = match p.hours {
        // An object changes only the days it names.
        Some(Value::Object(o)) => {
            let current = settings(&d.core().state).hours;
            let mut merged: BTreeMap<String, Value> = current.into_iter().map(|(d, h)| (d, json!(h))).collect();
            for (day, h) in o {
                merged.insert(day, if h.is_null() { json!("default") } else { h });
            }
            Some(ka::coerce_hours(&Value::Object(merged.into_iter().collect())).map_err(bad)?)
        }
        other => other,
    };
    put("keep_awake.hours", hours)?;
    put("keep_awake.min_battery", p.min_battery)?;
    put("keep_awake.linger_mins", p.linger_mins)?;
    put("keep_awake.wake", p.wake.map(Value::Bool))?;
    let today = match p.today {
        Some(v) => Some(ka::parse_today(&v, time::now_unix()).map_err(|e| bad(format!("today: {e}")))?),
        None => None,
    };
    let enabled_after = values.iter().find(|(k, _)| *k == "keep_awake.enabled").map(|(_, v)| v == &json!(true)).unwrap_or_else(|| d.core().state.setting_bool("keep_awake.enabled"));
    if matches!(today, Some(Some(_))) && !enabled_after {
        return Err(bad("keep-awake is off: pass enabled: true with today, or turn keep_awake.enabled on first".into()));
    }
    for (key, value) in values {
        crate::rpc::settings::set(d, ctx, SettingSetParams { key: key.into(), value })?;
    }
    if let Some(t) = today {
        d.core().state.keep_awake_today = t;
        d.mark_dirty();
    }
    crate::rpc::ok(tick_with(d, true))
}

// ------------------------------------------------------------------ IOKit

mod iokit {
    use core_foundation::array::{CFArray, CFArrayRef};
    use core_foundation::base::{CFType, CFTypeRef, TCFType};
    use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
    use core_foundation::number::CFNumber;
    use core_foundation::string::{CFString, CFStringRef};
    use midna_proto::keep_awake::Battery;

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(kind: CFStringRef, level: u32, name: CFStringRef, id: *mut u32) -> i32;
        fn IOPMAssertionRelease(id: u32) -> i32;
        fn IOPSCopyPowerSourcesInfo() -> CFTypeRef;
        fn IOPSCopyPowerSourcesList(blob: CFTypeRef) -> CFArrayRef;
        fn IOPSGetPowerSourceDescription(blob: CFTypeRef, source: CFTypeRef) -> CFDictionaryRef;
    }

    /// kIOPMAssertionLevelOn.
    const LEVEL_ON: u32 = 255;

    /// Take a PreventUserIdleSystemSleep assertion; its id, or the IOReturn error.
    pub fn create(name: &str) -> Result<u32, i32> {
        let kind = CFString::from_static_string("PreventUserIdleSystemSleep");
        let name = CFString::new(name);
        let mut id = 0u32;
        let r = unsafe { IOPMAssertionCreateWithName(kind.as_concrete_TypeRef(), LEVEL_ON, name.as_concrete_TypeRef(), &mut id) };
        if r == 0 { Ok(id) } else { Err(r) }
    }

    pub fn release(id: u32) {
        let r = unsafe { IOPMAssertionRelease(id) };
        if r != 0 {
            eprintln!("midnad: releasing the keep-awake assertion failed (IOReturn {r:#x})");
        }
    }

    /// The internal battery: charge and whether it's on AC. None without one.
    pub fn battery() -> Option<Battery> {
        let blob = unsafe { IOPSCopyPowerSourcesInfo() };
        if blob.is_null() {
            return None;
        }
        let blob = unsafe { CFType::wrap_under_create_rule(blob) };
        let list = unsafe { IOPSCopyPowerSourcesList(blob.as_CFTypeRef()) };
        if list.is_null() {
            return None;
        }
        let list: CFArray<CFType> = unsafe { CFArray::wrap_under_create_rule(list) };
        for source in list.iter() {
            let desc = unsafe { IOPSGetPowerSourceDescription(blob.as_CFTypeRef(), source.as_CFTypeRef()) };
            if desc.is_null() {
                continue;
            }
            let desc: CFDictionary<CFString, CFType> = unsafe { CFDictionary::wrap_under_get_rule(desc) };
            let get = |k: &'static str| desc.find(CFString::from_static_string(k)).map(|v| (*v).clone());
            let num = |k| get(k).and_then(|v| v.downcast::<CFNumber>()).and_then(|n| n.to_i64());
            let text = |k| get(k).and_then(|v| v.downcast::<CFString>()).map(|s| s.to_string());
            if text("Type").as_deref() != Some("InternalBattery") {
                continue;
            }
            let max = num("Max Capacity").filter(|m| *m > 0).unwrap_or(100);
            let percent = (num("Current Capacity")? * 100 / max).clamp(0, 100) as u8;
            return Some(Battery { percent, on_ac: text("Power Source State").as_deref() == Some("AC Power") });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use KeepAwakeReason::*;

    fn inputs() -> Inputs {
        Inputs { enabled: true, always: false, window: Ok(()), battery_ok: true, working: true, lingering: false }
    }

    #[test]
    fn decides_in_order_off_hours_battery_work() {
        assert_eq!(decide(inputs()), (true, Work));
        assert_eq!(decide(Inputs { enabled: false, ..inputs() }), (false, Disabled));
        assert_eq!(decide(Inputs { window: Err(Closed::DayOff), battery_ok: false, ..inputs() }), (false, DayOff), "the hours say why first");
        assert_eq!(decide(Inputs { window: Err(Closed::TodayOff), ..inputs() }), (false, TodayOff));
        assert_eq!(decide(Inputs { battery_ok: false, ..inputs() }), (false, BatteryLow));
        assert_eq!(decide(Inputs { working: false, ..inputs() }), (false, NoWork));
        assert_eq!(decide(Inputs { working: false, lingering: true, ..inputs() }), (true, Work));
        assert_eq!(decide(Inputs { working: false, always: true, ..inputs() }), (true, Always));
    }

    #[test]
    fn reads_this_macs_battery_without_crashing() {
        // Desktop Macs have none; laptops report 0-100.
        if let Some(b) = iokit::battery() {
            assert!(b.percent <= 100);
        }
    }
}
