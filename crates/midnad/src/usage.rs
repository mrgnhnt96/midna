//! Claude's plan usage limits, account-wide (`usage.get`, event `usage.limit_reached`).
//!
//! Claude Code's status line carries `rate_limits` (5-hour and weekly windows) on subscription
//! plans. Each terminal keeps its own last report in `AgentInfo.rate_limits`; the limits are the
//! account's, so the latest report from any terminal is kept here too (in `state.json`), and
//! outlives the terminal that reported it. Different Claude accounts in different terminals
//! would overwrite each other: the latest report wins.
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
}

/// Fold one Claude status line into the account's usage; emits `usage.limit_reached` when a
/// window reaches 100% (once per window and reset time).
pub fn observe(d: &Daemon, sid: &str, project: &str, payload: &Value) {
    let Some(limits) = crate::agent_work::rate_limits(payload, &time::now_rfc3339()) else { return };
    let (changed, reached) = {
        let mut core = d.core();
        let u = core.state.usage.get_or_insert_with(Default::default);
        let changed = u.claude.as_ref().is_none_or(|c| !c.same_as(&limits)) || u.session.as_deref() != Some(sid);
        let reached = newly_reached(u, &limits, time::now_unix());
        u.claude = Some(limits);
        u.session = Some(sid.to_string());
        (changed, reached)
    };
    if changed {
        d.mark_dirty();
    }
    for (window, w) in reached {
        let data = json!({ "agent": "claude", "window": window, "used_percentage": w.used_percentage, "resets_at": w.resets_at });
        d.emit(kinds::USAGE_LIMIT_REACHED, Actor::system(), Some(project.to_string()), Some(sid.to_string()), data);
    }
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
