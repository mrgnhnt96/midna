//! Claude's plan usage limits, account-wide (`usage.get`, event `usage.limit_reached`).
//!
//! Claude Code's status line carries `rate_limits` (5-hour and weekly windows) on subscription
//! plans. Each terminal keeps its own last report in `AgentInfo.rate_limits`; the limits are the
//! account's, so every terminal's reports are merged here too (in `state.json`), and outlive the
//! terminal that reported them.
//!
//! A terminal's status line repeats the numbers from its own last API response, so an idle (or
//! just resumed) terminal keeps reporting old ones, and a window that has reset since may be left
//! out. Reports are merged per window, so the freshest numbers stick: a later reset is a new
//! window run and wins; in the same run, usage only goes up, so the higher number wins; an older
//! run, or a window left out, keeps what is stored. Different Claude accounts in different
//! terminals would mix: the higher numbers stick until their window resets.
//!
//! Anthropic can also reset a plan's limits early without moving `resets_at`, so usage does go
//! down within a run. A terminal whose own numbers just moved (since that agent process's last
//! report) is reading a fresh API response, so its numbers win even when lower. Once one has
//! lowered a window, a report that hasn't moved and sits above it is from before the reset.
//!
//! Each time the merged numbers move (a percentage changes or a new window run starts), a
//! `usage.sampled` event logs them, for Insights' usage chart. A repeat of the same numbers logs
//! nothing: the chart holds a reading until its window resets.
use crate::daemon::Daemon;
use midna_proto::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// What `state.json` keeps.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StoredUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude: Option<RateLimits>,
    /// The terminal that reported `claude`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Id>,
    /// window -> the `resets_at` a `usage.limit_reached` was emitted for (once per window run).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reached: BTreeMap<String, String>,
    /// window -> the `resets_at` of the run a fresh report lowered (an early reset).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub lowered: BTreeMap<String, String>,
}

/// Fold one Claude status line into the account's usage; emits `usage.limit_reached` when a
/// window reaches 100% (once per window and reset time).
pub fn observe(d: &Daemon, sid: &str, project: &str, payload: &Value) {
    let Some(limits) = crate::agent_work::rate_limits(payload, &time::now_rfc3339()) else { return };
    let (changed, sampled, reached) = {
        let mut core = d.core();
        let rt = core.agents.entry(sid.to_string()).or_default();
        let moved_here = moved_windows(rt.limits_seen.as_ref(), &limits);
        rt.limits_seen = Some(limits.clone());
        let u = core.state.usage.get_or_insert_with(Default::default);
        let (merged, fresh) = merge(u.claude.as_ref(), limits, &moved_here, &mut u.lowered);
        let session = if fresh { Some(sid.to_string()) } else { u.session.clone() };
        let changed = u.claude.as_ref().is_none_or(|c| !c.same_as(&merged)) || u.session != session;
        let sampled = u.claude.as_ref().is_none_or(|c| moved(c, &merged)).then(|| merged.clone());
        let reached = newly_reached(u, &merged, time::now_unix());
        u.claude = Some(merged);
        u.session = session;
        (changed, sampled, reached)
    };
    if changed {
        d.mark_dirty();
    }
    if let Some(l) = sampled {
        let mut data = json!({ "agent": "claude" });
        for (window, w) in l.windows() {
            data[window] = json!({ "used_percentage": w.used_percentage, "resets_at": w.resets_at });
        }
        d.emit(kinds::USAGE_SAMPLED, Actor::system(), Some(project.to_string()), Some(sid.to_string()), data);
    }
    for (window, w) in reached {
        let data = json!({ "agent": "claude", "window": window, "used_percentage": w.used_percentage, "resets_at": w.resets_at });
        d.emit(kinds::USAGE_LIMIT_REACHED, Actor::system(), Some(project.to_string()), Some(sid.to_string()), data);
    }
}

/// Two reports this close apart in `resets_at` are the same window run.
const SAME_RUN_SECS: i64 = 600;

/// `new` folded into `old` window by window (see the module docs), and whether `new` was current
/// (no window of it older than what is stored): only a current report moves `observed_at` and
/// `session`, so an idle terminal repeating old numbers doesn't keep them looking fresh. `moved`
/// names the windows whose numbers the reporting process just changed; `lowered` remembers runs
/// an early reset brought down.
fn merge(old: Option<&RateLimits>, new: RateLimits, moved: &[&str], lowered: &mut BTreeMap<String, String>) -> (RateLimits, bool) {
    let Some(old) = old else { return (new, true) };
    let mut current = true;
    let mut pick = |name: &str, o: &Option<RateLimitWindow>, n: Option<RateLimitWindow>| match (o, n) {
        (o, None) => o.clone(),
        (None, n) => n,
        (Some(o), Some(n)) => match (reset(o), reset(&n)) {
            (Some(ro), Some(rn)) if rn - ro > SAME_RUN_SECS => Some(n),
            (Some(ro), Some(rn)) if ro - rn > SAME_RUN_SECS => {
                current = false;
                Some(o.clone())
            }
            // A fresh API response: right even below what is stored (an early reset).
            _ if moved.contains(&name) => {
                if n.used_percentage < o.used_percentage {
                    lowered.insert(name.to_string(), n.resets_at.clone().unwrap_or_default());
                }
                Some(n)
            }
            _ if o.used_percentage > n.used_percentage => {
                current = false;
                Some(o.clone())
            }
            // Unmoved and above a run an early reset lowered: numbers from before the reset.
            _ if o.used_percentage < n.used_percentage && lowered_run(lowered, name, &n) => {
                current = false;
                Some(o.clone())
            }
            _ => Some(n),
        },
    };
    let seven_day = pick("seven_day", &old.seven_day, new.seven_day);
    // Claude is retiring the 5-hour window: a current report without one clears it, and a stored
    // one that has expired with nothing newer is dropped. A stale report keeps what is stored.
    let now = time::now_unix();
    let five_hour = match new.five_hour {
        n @ Some(_) => pick("five_hour", &old.five_hour, n),
        None if current => None,
        None => old.five_hour.clone().filter(|w| !reset(w).is_some_and(|r| r <= now)),
    };
    let observed_at = if current { new.observed_at } else { old.observed_at.clone() };
    (RateLimits { five_hour, seven_day, observed_at }, current)
}

fn reset(w: &RateLimitWindow) -> Option<i64> {
    w.resets_at.as_deref().and_then(time::parse_rfc3339)
}

/// Whether an early reset lowered `w`'s window run.
fn lowered_run(lowered: &BTreeMap<String, String>, name: &str, w: &RateLimitWindow) -> bool {
    let run = lowered.get(name).and_then(|r| time::parse_rfc3339(r));
    run.zip(reset(w)).is_some_and(|(a, b)| (a - b).abs() <= SAME_RUN_SECS)
}

/// The windows whose numbers changed since this agent process's last report, in the same run:
/// numbers that come from a fresh API response, not a repeat of old ones.
fn moved_windows(prev: Option<&RateLimits>, new: &RateLimits) -> Vec<&'static str> {
    let Some(prev) = prev else { return vec![] };
    let at = |l: &RateLimits, name: &str| l.windows().find(|(n, _)| *n == name).map(|(_, w)| w.clone());
    new.windows()
        .filter(|(name, n)| {
            at(prev, name).is_some_and(|p| {
                p.used_percentage != n.used_percentage && reset(&p).zip(reset(n)).is_some_and(|(a, b)| (a - b).abs() <= SAME_RUN_SECS)
            })
        })
        .map(|(name, _)| name)
        .collect()
}

/// Forget what an agent process reported: a new one (startup or resume) may repeat old numbers.
pub fn forget_process(d: &Daemon, sid: &str) {
    if let Some(rt) = d.core().agents.get_mut(sid) {
        rt.limits_seen = None;
    }
}

/// Whether `new` says something `old` didn't: a window's percentage changed, one came or went, or
/// a new window run started (`resets_at` alone drifts by seconds between reports).
fn moved(old: &RateLimits, new: &RateLimits) -> bool {
    let differ = |o: &Option<RateLimitWindow>, n: &Option<RateLimitWindow>| match (o, n) {
        (Some(o), Some(n)) => o.used_percentage != n.used_percentage || reset(o).zip(reset(n)).is_some_and(|(a, b)| (a - b).abs() > SAME_RUN_SECS),
        (o, n) => o.is_some() != n.is_some(),
    };
    differ(&old.five_hour, &new.five_hour) || differ(&old.seven_day, &new.seven_day)
}

/// Windows of `limits` at their limit that `u` has not announced yet (and remembers them).
fn newly_reached(u: &mut StoredUsage, limits: &RateLimits, now: i64) -> Vec<(&'static str, RateLimitWindow)> {
    let mut out = vec![];
    for (name, w) in limits.windows() {
        if !w.limited(now) {
            continue;
        }
        let key = w.resets_at.clone().unwrap_or_default();
        if u.reached.get(name) != Some(&key) {
            u.reached.insert(name.to_string(), key);
            out.push((name, w.clone()));
        }
    }
    out
}

/// `usage.get`.
pub fn get(d: &Daemon) -> UsageGetResult {
    let u = d.core().state.usage.clone().unwrap_or_default();
    UsageGetResult { claude: u.claude.map(|l| summary(l, u.session, time::now_unix())) }
}

/// The API view of a report: expired windows marked, and whether (and until when) it is limited.
pub fn summary(l: RateLimits, session: Option<Id>, now: i64) -> AgentUsage {
    let mark = |w: Option<RateLimitWindow>| {
        w.map(|mut w| {
            w.expired = w.resets_at.as_deref().and_then(time::parse_rfc3339).is_some_and(|r| r <= now);
            w
        })
    };
    let limited_until = l.windows().filter(|(_, w)| w.limited(now)).filter_map(|(_, w)| w.resets_at.clone()).max();
    let limited = l.windows().any(|(_, w)| w.limited(now));
    AgentUsage { five_hour: mark(l.five_hour), seven_day: mark(l.seven_day), observed_at: l.observed_at, session, limited, limited_until }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(used: f64, resets: i64) -> Option<RateLimitWindow> {
        Some(RateLimitWindow { used_percentage: used, resets_at: Some(time::format_unix(resets)), expired: false })
    }

    #[test]
    fn a_full_window_is_announced_once_per_reset() {
        let mut u = StoredUsage::default();
        let now = 1_000_000;
        let l = RateLimits { five_hour: win(100.0, now + 600), seven_day: win(40.0, now + 9000), observed_at: "t".into() };
        let r = newly_reached(&mut u, &l, now);
        assert_eq!(r.iter().map(|(n, _)| *n).collect::<Vec<_>>(), ["five_hour"]);
        assert!(newly_reached(&mut u, &l, now).is_empty(), "same window run: once");
        let next = RateLimits { five_hour: win(100.0, now + 18_600), ..l.clone() };
        assert_eq!(newly_reached(&mut u, &next, now).len(), 1, "the next window run announces again");
        assert!(newly_reached(&mut u, &l, now + 700).is_empty(), "a full window that has reset is not limited");
    }

    #[test]
    fn stale_reports_never_replace_fresher_numbers() {
        let now = time::now_unix();
        let l = |five: Option<RateLimitWindow>, week: Option<RateLimitWindow>, at: &str| RateLimits { five_hour: five, seven_day: week, observed_at: at.into() };
        let fresh = l(win(97.0, now + 600), win(29.0, now + 9000), "t2");
        let pct = |w: &Option<RateLimitWindow>| w.as_ref().map(|w| w.used_percentage);

        // An idle terminal repeating its last numbers: same run, lower usage.
        let (m, current) = merge(Some(&fresh), l(win(92.0, now + 600), win(28.0, now + 9000), "t3"), &[], &mut BTreeMap::new());
        assert_eq!((pct(&m.five_hour), pct(&m.seven_day), m.observed_at.as_str(), current), (Some(97.0), Some(29.0), "t2", false));

        // A resumed terminal from days ago: its 5-hour window is gone, its weekly is far behind.
        let (m, current) = merge(Some(&fresh), l(None, win(8.0, now + 9000), "t3"), &[], &mut BTreeMap::new());
        assert_eq!((pct(&m.five_hour), pct(&m.seven_day), current), (Some(97.0), Some(29.0), false));

        // A report from an older window run.
        let (m, _) = merge(Some(&fresh), l(win(99.0, now - 18_000), None, "t3"), &[], &mut BTreeMap::new());
        assert_eq!(pct(&m.five_hour), Some(97.0));

        // Fresher numbers win, and a new window run wins however low it starts.
        let (m, current) = merge(Some(&fresh), l(win(98.0, now + 630), win(29.0, now + 9000), "t3"), &[], &mut BTreeMap::new());
        assert_eq!((pct(&m.five_hour), m.observed_at.as_str(), current), (Some(98.0), "t3", true));
        let (m, current) = merge(Some(&fresh), l(win(2.0, now + 18_600), win(29.0, now + 9000), "t3"), &[], &mut BTreeMap::new());
        assert_eq!((pct(&m.five_hour), current), (Some(2.0), true));
    }

    #[test]
    fn an_early_reset_that_keeps_resets_at_is_taken_from_a_fresh_report() {
        let now = time::now_unix();
        let week = |used: f64| RateLimits { five_hour: None, seven_day: win(used, now + 9000), observed_at: "t".into() };
        let pct = |l: &RateLimits| l.seven_day.as_ref().map(|w| w.used_percentage);
        let mut lowered = BTreeMap::new();
        let stored = week(97.0);

        // The reset's first reports repeat the same numbers in each process: not proof yet.
        assert!(moved_windows(None, &week(1.0)).is_empty(), "a new process's first report");
        assert!(moved_windows(Some(&week(1.0)), &week(1.0)).is_empty(), "a repeat");
        let (m, current) = merge(Some(&stored), week(1.0), &[], &mut lowered);
        assert_eq!((pct(&m), current), (Some(97.0), false));

        // A process whose own numbers dropped (or moved at all) read a fresh API response.
        let moved = moved_windows(Some(&week(97.0)), &week(1.0));
        assert_eq!(moved, ["seven_day"]);
        let (m, current) = merge(Some(&stored), week(1.0), &moved, &mut lowered);
        assert_eq!((pct(&m), current), (Some(1.0), true));
        let moved = moved_windows(Some(&week(1.0)), &week(2.0));
        let (m2, _) = merge(Some(&stored), week(2.0), &moved, &mut BTreeMap::new());
        assert_eq!(pct(&m2), Some(2.0), "a rise still below the stored numbers is fresh too");

        // An idle terminal still repeating its pre-reset numbers can't bring them back.
        let (m, current) = merge(Some(&m), week(84.0), &[], &mut lowered);
        assert_eq!((pct(&m), current), (Some(1.0), false));
        // Real usage after the reset climbs through fresh reports.
        let (m, _) = merge(Some(&m), week(5.0), &["seven_day"], &mut lowered);
        assert_eq!(pct(&m), Some(5.0));

        // A new window run is not a move within the run, and forgets nothing it shouldn't.
        let next = RateLimits { seven_day: win(0.0, now + 9000 + 604_800), ..week(0.0) };
        assert!(moved_windows(Some(&week(5.0)), &next).is_empty());
        let (m, _) = merge(Some(&m), next, &[], &mut lowered);
        assert_eq!(pct(&m), Some(0.0));
        let later = RateLimits { seven_day: win(40.0, now + 9000 + 604_800), ..week(0.0) };
        assert_eq!(pct(&merge(Some(&m), later, &[], &mut lowered).0), Some(40.0), "the lowered mark is per run");
    }

    #[test]
    fn a_current_report_without_five_hour_clears_it() {
        let now = time::now_unix();
        let l = |five: Option<RateLimitWindow>, week: Option<RateLimitWindow>, at: &str| RateLimits { five_hour: five, seven_day: week, observed_at: at.into() };
        let old = l(win(97.0, now + 600), win(29.0, now + 9000), "t2");

        let (m, current) = merge(Some(&old), l(None, win(30.0, now + 9000), "t3"), &[], &mut BTreeMap::new());
        assert!(m.five_hour.is_none() && current, "current report drops the stale 5h");
        assert_eq!(m.seven_day.map(|w| w.used_percentage), Some(30.0));

        // seven_day missing too: still falls back to the old weekly value.
        let (m, _) = merge(Some(&old), l(None, None, "t3"), &[], &mut BTreeMap::new());
        assert!(m.five_hour.is_none());
        assert_eq!(m.seven_day.map(|w| w.used_percentage), Some(29.0));
    }

    #[test]
    fn a_stale_report_drops_only_an_expired_five_hour() {
        let now = time::now_unix();
        let l = |five: Option<RateLimitWindow>, week: Option<RateLimitWindow>, at: &str| RateLimits { five_hour: five, seven_day: week, observed_at: at.into() };
        // Stale (weekly far behind): the live 5h is kept, the expired one is dropped.
        let live = l(win(97.0, now + 600), win(29.0, now + 9000), "t2");
        let (m, current) = merge(Some(&live), l(None, win(8.0, now + 9000), "t3"), &[], &mut BTreeMap::new());
        assert!(!current && m.five_hour.is_some());
        let expired = l(win(97.0, now - 60), win(29.0, now + 9000), "t2");
        let (m, current) = merge(Some(&expired), l(None, win(8.0, now + 9000), "t3"), &[], &mut BTreeMap::new());
        assert!(!current && m.five_hour.is_none());
    }

    #[test]
    fn only_a_new_percentage_or_window_run_is_a_sample() {
        let now = 1_000_000;
        let l = |five: Option<RateLimitWindow>, week: Option<RateLimitWindow>| RateLimits { five_hour: five, seven_day: week, observed_at: "t".into() };
        let old = l(win(40.0, now + 600), win(8.0, now + 9000));
        assert!(!moved(&old, &l(win(40.0, now + 610), win(8.0, now + 9000))), "resets_at drifting is the same reading");
        assert!(moved(&old, &l(win(41.0, now + 600), win(8.0, now + 9000))));
        assert!(moved(&old, &l(win(40.0, now + 18_600), win(8.0, now + 9000))), "a new window run");
        assert!(moved(&old, &l(None, win(8.0, now + 9000))));
    }

    #[test]
    fn summary_says_limited_until_and_marks_expired_windows() {
        let now = 1_000_000;
        let l = RateLimits { five_hour: win(100.0, now + 600), seven_day: win(100.0, now + 9000), observed_at: "t".into() };
        let s = summary(l, Some("s_1".into()), now);
        assert!(s.limited);
        assert_eq!(s.limited_until, Some(time::format_unix(now + 9000)));
        let l = RateLimits { five_hour: win(100.0, now - 5), seven_day: win(30.0, now + 9000), observed_at: "t".into() };
        let s = summary(l, None, now);
        assert!(!s.limited && s.limited_until.is_none());
        assert!(s.five_hour.unwrap().expired);
        assert!(!s.seven_day.unwrap().expired);
    }
}
