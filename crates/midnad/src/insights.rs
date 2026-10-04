//! Insights computed purely from the event log (no other data source).
use midna_proto::*;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

/// Who/what an event or interval is attributed to when grouping.
#[derive(Clone, Default)]
struct SessionInfo {
    project: Option<Id>,
    agent: Option<String>,
    state: Option<StatusState>,
    since: i64,
}

pub struct Labels {
    pub projects: HashMap<Id, String>,
    pub sessions: HashMap<Id, String>,
}

fn day_key(t: i64) -> String {
    let off = time::local_offset_secs();
    time::format_unix(t + off)[..10].to_string()
}

fn key_for(by: Option<InsightsBy>, sid: Option<&str>, info: Option<&SessionInfo>, project: Option<&str>, t: i64) -> Option<String> {
    match by? {
        InsightsBy::Project => project.or(info.and_then(|i| i.project.as_deref())).map(str::to_string),
        InsightsBy::Agent => info.and_then(|i| i.agent.clone()).or(Some("shell".into())),
        InsightsBy::Terminal => sid.map(str::to_string),
        InsightsBy::Day => Some(day_key(t)),
    }
}

struct Acc {
    from: i64,
    to: i64,
    by: Option<InsightsBy>,
    totals: InsightsTotals,
    rows: BTreeMap<String, InsightsTotals>,
}

impl Acc {
    fn bump(&mut self, key: Option<String>, f: impl Fn(&mut InsightsTotals)) {
        f(&mut self.totals);
        if let Some(k) = key {
            f(self.rows.entry(k).or_default());
        }
    }

    /// Add [start, end) of `state` time, clipped to the range (split per day when grouping by day).
    fn interval(&mut self, sid: &str, info: &SessionInfo, end: i64) {
        let Some(state) = info.state else { return };
        let (s, e) = (info.since.max(self.from), end.min(self.to));
        if e <= s || !matches!(state, StatusState::Working | StatusState::NeedsYou) {
            return;
        }
        let mut cur = s;
        while cur < e {
            let next = if self.by == Some(InsightsBy::Day) { (time::local_day_start(cur) + 86_400).min(e) } else { e };
            let secs = next - cur;
            let key = key_for(self.by, Some(sid), Some(info), None, cur);
            match state {
                StatusState::Working => self.bump(key, |t| t.working_secs += secs),
                _ => self.bump(key, |t| t.waiting_secs += secs),
            }
            cur = next;
        }
    }
}

fn compute(events: &[Event], from: i64, to: i64, by: Option<InsightsBy>) -> Acc {
    let mut acc = Acc { from, to, by, totals: Default::default(), rows: BTreeMap::new() };
    let mut sessions: HashMap<Id, SessionInfo> = HashMap::new();
    for e in events {
        let Some(t) = time::parse_rfc3339(&e.at) else { continue };
        if t >= to {
            break;
        }
        let sid = e.session_id.as_deref();
        // Session lifecycle drives working/waiting intervals (events before `from` set the state).
        match e.kind.as_str() {
            kinds::SESSION_OPENED => {
                if let Some(sid) = sid {
                    let agent = e.data.get("agent").and_then(Value::as_str).map(str::to_string);
                    let state = e.data.pointer("/status/state").cloned().and_then(|v| serde_json::from_value(v).ok());
                    sessions.insert(sid.into(), SessionInfo { project: e.project_id.clone(), agent, state, since: t });
                }
            }
            kinds::SESSION_STATUS => {
                if let Some(sid) = sid {
                    let mut info = sessions.remove(sid).unwrap_or_else(|| SessionInfo { project: e.project_id.clone(), ..Default::default() });
                    acc.interval(sid, &info, t);
                    info.state = e.data.get("state").cloned().and_then(|v| serde_json::from_value(v).ok());
                    info.since = t;
                    sessions.insert(sid.into(), info);
                }
            }
            kinds::SESSION_CLOSED => {
                if let Some(sid) = sid
                    && let Some(info) = sessions.remove(sid) {
                        acc.interval(sid, &info, t);
                    }
            }
            _ => {}
        }
        if t < from {
            continue;
        }
        let info = sid.and_then(|s| sessions.get(s));
        let key = key_for(by, sid, info, e.project_id.as_deref(), t);
        match e.kind.as_str() {
            kinds::AGENT_TURN_STARTED => acc.bump(key, |x| x.turns += 1),
            kinds::AGENT_PROMPT_SUBMITTED => acc.bump(key, |x| x.messages += 1),
            kinds::AGENT_COST => {
                let d = e.data.get("delta_usd").and_then(Value::as_f64).unwrap_or(0.0);
                acc.bump(key, |x| x.spend_usd += d);
            }
            kinds::NEEDS_YOU_RESOLVED if e.data.pointer("/resolution/kind").and_then(Value::as_str) == Some("approve") => {
                acc.bump(key, |x| x.approvals += 1)
            }
            kinds::TRIGGER_FIRED => acc.bump(key, |x| x.triggers_fired += 1),
            _ => {}
        }
    }
    // Sessions still in a state at the end of the range.
    let open: Vec<(Id, SessionInfo)> = sessions.into_iter().collect();
    for (sid, info) in open {
        acc.interval(&sid, &info, to);
    }
    acc
}

fn sub(a: &InsightsTotals, b: &InsightsTotals) -> InsightsTotals {
    InsightsTotals {
        turns: a.turns - b.turns,
        messages: a.messages - b.messages,
        spend_usd: ((a.spend_usd - b.spend_usd) * 1e6).round() / 1e6,
        working_secs: a.working_secs - b.working_secs,
        waiting_secs: a.waiting_secs - b.waiting_secs,
        approvals: a.approvals - b.approvals,
        triggers_fired: a.triggers_fired - b.triggers_fired,
    }
}

pub fn summary(events: &[Event], range: InsightsRange, by: Option<InsightsBy>, now: i64, labels: &Labels) -> InsightsSummary {
    let today = time::local_day_start(now);
    // Ranges are [from, to); "now" ranges include the current second.
    let (from, to) = match range {
        InsightsRange::Today => (today, now + 1),
        InsightsRange::Yesterday => (today - 86_400, today),
        InsightsRange::Week => (today - 6 * 86_400, now + 1),
        InsightsRange::Month => (today - 29 * 86_400, now + 1),
    };
    let cur = compute(events, from, to, by);
    let prev = compute(events, from - (to - from), from, None);
    let rows = cur
        .rows
        .into_iter()
        .map(|(key, totals)| {
            let label = match by {
                Some(InsightsBy::Project) => labels.projects.get(&key).cloned(),
                Some(InsightsBy::Terminal) => labels.sessions.get(&key).cloned().or_else(|| session_name_from_events(events, &key)),
                Some(InsightsBy::Agent) => Some(agent_label(&key)),
                _ => None,
            }
            .unwrap_or_else(|| key.clone());
            InsightsRow { key, label, totals }
        })
        .collect();
    let mut totals = cur.totals;
    totals.spend_usd = (totals.spend_usd * 1e6).round() / 1e6;
    InsightsSummary { range, from: time::format_unix(from), to: time::format_unix(to), vs_previous: sub(&totals, &prev.totals), totals, rows }
}

// ------------------------------------------------------------------ series

/// One contribution to a metric: a point event (`start == end`) or a time interval.
struct Hit<'a> {
    metric: InsightsMetric,
    start: i64,
    end: i64,
    amount: f64,
    sid: Option<&'a str>,
    project: Option<&'a str>,
    agent: Option<&'a str>,
}

/// Walk the log like `compute`, reporting every contribution inside [from, to) to `f`.
/// Intervals are clipped to the range; the caller splits them across buckets.
fn walk<'a>(events: &'a [Event], from: i64, to: i64, mut f: impl FnMut(Hit<'_>)) {
    let mut sessions: HashMap<&'a str, SessionInfo> = HashMap::new();
    let interval = |sid: &str, info: &SessionInfo, end: i64, f: &mut dyn FnMut(Hit<'_>)| {
        let metric = match info.state {
            Some(StatusState::Working) => InsightsMetric::Working,
            Some(StatusState::NeedsYou) => InsightsMetric::Waiting,
            _ => return,
        };
        let (s, e) = (info.since.max(from), end.min(to));
        if e > s {
            f(Hit { metric, start: s, end: e, amount: (e - s) as f64, sid: Some(sid), project: info.project.as_deref(), agent: info.agent.as_deref() });
        }
    };
    for e in events {
        let Some(t) = time::parse_rfc3339(&e.at) else { continue };
        if t >= to {
            break;
        }
        let sid = e.session_id.as_deref();
        match (e.kind.as_str(), sid) {
            (kinds::SESSION_OPENED, Some(sid)) => {
                let agent = e.data.get("agent").and_then(Value::as_str).map(str::to_string);
                let state = e.data.pointer("/status/state").cloned().and_then(|v| serde_json::from_value(v).ok());
                sessions.insert(sid, SessionInfo { project: e.project_id.clone(), agent, state, since: t });
            }
            (kinds::SESSION_STATUS, Some(sid)) => {
                let mut info = sessions.remove(sid).unwrap_or_else(|| SessionInfo { project: e.project_id.clone(), ..Default::default() });
                interval(sid, &info, t, &mut f);
                info.state = e.data.get("state").cloned().and_then(|v| serde_json::from_value(v).ok());
                info.since = t;
                sessions.insert(sid, info);
            }
            (kinds::SESSION_CLOSED, Some(sid)) => {
                if let Some(info) = sessions.remove(sid) {
                    interval(sid, &info, t, &mut f);
                }
            }
            _ => {}
        }
        if t < from {
            continue;
        }
        let (metric, amount) = match e.kind.as_str() {
            kinds::AGENT_TURN_STARTED => (InsightsMetric::Turns, 1.0),
            kinds::AGENT_PROMPT_SUBMITTED => (InsightsMetric::Messages, 1.0),
            kinds::AGENT_COST => (InsightsMetric::Spend, e.data.get("delta_usd").and_then(Value::as_f64).unwrap_or(0.0)),
            kinds::NEEDS_YOU_RESOLVED if e.data.pointer("/resolution/kind").and_then(Value::as_str) == Some("approve") => (InsightsMetric::Approvals, 1.0),
            kinds::TRIGGER_FIRED => (InsightsMetric::Triggers, 1.0),
            _ => continue,
        };
        let info = sid.and_then(|s| sessions.get(s));
        let project = e.project_id.as_deref().or(info.and_then(|i| i.project.as_deref()));
        let agent = info.and_then(|i| i.agent.as_deref()).or_else(|| e.data.get("agent").and_then(Value::as_str));
        f(Hit { metric, start: t, end: t, amount, sid, project, agent });
    }
    for (sid, info) in sessions.iter() {
        interval(sid, info, to, &mut f);
    }
}

/// `[from, to)` for computing (to = now + 1 for ranges that include today) and the end of the
/// chart axis (the end of today, so today's chart always spans the whole day).
fn range_bounds(range: InsightsRange, now: i64) -> (i64, i64, i64) {
    let today = time::local_day_start(now);
    match range {
        InsightsRange::Today => (today, now + 1, today + 86_400),
        InsightsRange::Yesterday => (today - 86_400, today, today),
        InsightsRange::Week => (today - 6 * 86_400, now + 1, today + 86_400),
        InsightsRange::Month => (today - 29 * 86_400, now + 1, today + 86_400),
    }
}

fn agent_label(key: &str) -> String {
    match key {
        "claude" => "Claude".into(),
        "codex" => "Codex".into(),
        "shell" => "Shell".into(),
        k => k.to_string(),
    }
}

/// Name a terminal from the log when it is no longer in state (closed terminals).
fn session_name_from_events(events: &[Event], sid: &str) -> Option<String> {
    events.iter().rev().filter(|e| e.session_id.as_deref() == Some(sid)).find_map(|e| match e.kind.as_str() {
        kinds::SESSION_OPENED | "session.renamed" => e.data.get("name").and_then(Value::as_str).map(str::to_string),
        _ => None,
    })
}

fn round6(v: f64) -> f64 {
    (v * 1e6).round() / 1e6
}

pub fn series(events: &[Event], p: &InsightsSeriesParams, now: i64, labels: &Labels) -> InsightsSeries {
    let (from, to, axis_end) = range_bounds(p.range, now);
    let bucket = p.bucket.unwrap_or(match p.range {
        InsightsRange::Today | InsightsRange::Yesterday => InsightsBucketSize::Hour,
        _ => InsightsBucketSize::Day,
    });
    let size = match bucket {
        InsightsBucketSize::Hour => 3_600,
        InsightsBucketSize::Day => 86_400,
    };
    let n = ((axis_end - from + size - 1) / size).max(1) as usize;
    let mut buckets: Vec<InsightsSeriesBucket> = (0..n)
        .map(|i| {
            let s = from + i as i64 * size;
            InsightsSeriesBucket { start: time::format_unix(s), end: time::format_unix(s + size), ..Default::default() }
        })
        .collect();
    let mut groups: BTreeMap<String, f64> = BTreeMap::new();
    let mut total = 0.0;
    let key_of = |h: &Hit, t: i64| -> Option<String> {
        Some(match p.by? {
            InsightsBy::Project => h.project.unwrap_or("none").to_string(),
            InsightsBy::Agent => h.agent.unwrap_or("shell").to_string(),
            InsightsBy::Terminal => h.sid.unwrap_or("none").to_string(),
            InsightsBy::Day => day_key(t),
        })
    };
    walk(events, from, to, |h| {
        if h.metric != p.metric {
            return;
        }
        let mut add = |t: i64, amount: f64| {
            let i = ((t - from) / size) as usize;
            let Some(b) = buckets.get_mut(i) else { return };
            b.total += amount;
            total += amount;
            if let Some(k) = key_of(&h, t) {
                *b.values.entry(k.clone()).or_default() += amount;
                *groups.entry(k).or_default() += amount;
            }
        };
        if h.end > h.start {
            // split the interval across bucket boundaries
            let mut cur = h.start;
            while cur < h.end {
                let next = (from + ((cur - from) / size + 1) * size).min(h.end);
                add(cur, (next - cur) as f64);
                cur = next;
            }
        } else {
            add(h.start, h.amount);
        }
    });
    let mut previous_total = 0.0;
    walk(events, from - (to - from), from, |h| {
        if h.metric == p.metric {
            previous_total += h.amount;
        }
    });
    for b in &mut buckets {
        b.total = round6(b.total);
        for v in b.values.values_mut() {
            *v = round6(*v);
        }
    }
    let mut groups: Vec<InsightsSeriesGroup> = groups
        .into_iter()
        .map(|(key, total)| {
            let label = match p.by {
                Some(InsightsBy::Project) => labels.projects.get(&key).cloned().or_else(|| match key.as_str() {
                    "none" => Some("No project".to_string()),
                    ROOT_PROJECT_ID => Some("root".to_string()),
                    _ => None,
                }),
                Some(InsightsBy::Terminal) => labels.sessions.get(&key).cloned().or_else(|| session_name_from_events(events, &key)),
                Some(InsightsBy::Agent) => Some(agent_label(&key)),
                _ => None,
            }
            .unwrap_or_else(|| key.clone());
            InsightsSeriesGroup { key, label, total: round6(total) }
        })
        .collect();
    groups.sort_by(|a, b| b.total.partial_cmp(&a.total).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.key.cmp(&b.key)));
    let unit = match p.metric {
        InsightsMetric::Spend => "usd",
        InsightsMetric::Working | InsightsMetric::Waiting => "secs",
        _ => "count",
    };
    InsightsSeries {
        range: p.range,
        metric: p.metric,
        bucket,
        unit: unit.into(),
        from: time::format_unix(from),
        to: time::format_unix(axis_end.max(to)),
        buckets,
        groups,
        total: round6(total),
        previous_total: round6(previous_total),
    }
}

/// Kinds shown in the activity feed.
const ACTIVITY: &[&str] = &[
    "session.opened", "session.closed", "session.exited", "session.status", "agent.turn_", "agent.prompt_submitted",
    "needs_you.", "rule.", "trigger.fired", "settings.changed",
];

pub fn activity(events: &[Event], since: i64, filter: &EventFilter, limit: usize) -> Vec<Event> {
    events
        .iter()
        .rev()
        .take_while(|e| time::parse_rfc3339(&e.at).is_none_or(|t| t >= since))
        .filter(|e| ACTIVITY.iter().any(|k| e.kind.starts_with(k)) && filter.matches(e))
        .take(limit)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(seq: u64, at: i64, kind: &str, sid: &str, data: Value) -> Event {
        Event { seq, at: time::format_unix(at), kind: kind.into(), actor: Actor::system(), project_id: Some("p_1".into()), session_id: Some(sid.into()), data }
    }

    #[test]
    fn totals_from_events() {
        let now = time::now_unix();
        let t0 = time::local_day_start(now) + 10; // early today
        let now = now.max(t0 + 400);
        let events = vec![
            ev(1, t0, "session.opened", "s1", json!({"agent": "claude", "status": {"state": "idle"}})),
            ev(2, t0 + 10, "agent.prompt_submitted", "s1", json!({})),
            ev(3, t0 + 10, "agent.turn_started", "s1", json!({})),
            ev(4, t0 + 10, "session.status", "s1", json!({"state": "working"})),
            ev(5, t0 + 70, "session.status", "s1", json!({"state": "needs_you"})),
            ev(6, t0 + 100, "needs_you.resolved", "s1", json!({"resolution": {"kind": "approve"}})),
            ev(7, t0 + 100, "session.status", "s1", json!({"state": "working"})),
            ev(8, t0 + 200, "agent.cost", "s1", json!({"delta_usd": 0.25})),
            ev(9, t0 + 200, "session.status", "s1", json!({"state": "done"})),
        ];
        let labels = Labels { projects: HashMap::new(), sessions: HashMap::new() };
        let s = summary(&events, InsightsRange::Today, Some(InsightsBy::Agent), now, &labels);
        assert_eq!((s.totals.turns, s.totals.messages, s.totals.approvals), (1, 1, 1));
        assert_eq!((s.totals.working_secs, s.totals.waiting_secs), (160, 30));
        assert_eq!(s.totals.spend_usd, 0.25);
        assert_eq!(s.rows[0].key, "claude");
    }

    fn evp(seq: u64, at: i64, kind: &str, project: &str, sid: &str, data: Value) -> Event {
        Event { seq, at: time::format_unix(at), kind: kind.into(), actor: Actor::system(), project_id: Some(project.into()), session_id: Some(sid.into()), data }
    }

    fn params(range: InsightsRange, metric: InsightsMetric, by: Option<InsightsBy>) -> InsightsSeriesParams {
        InsightsSeriesParams { range, metric, bucket: None, by }
    }

    #[test]
    fn series_buckets_by_project_and_splits_intervals() {
        let day = time::local_day_start(time::now_unix());
        let now = day + 5 * 3600 + 120; // 05:02 local
        let t = day + 3600; // 01:00
        let events = vec![
            evp(1, day - 3 * 3600, "session.opened", "p_a", "s1", json!({"agent": "claude", "name": "api", "status": {"state": "idle"}})),
            evp(2, day - 3600, "agent.turn_started", "p_a", "s1", json!({})), // yesterday -> previous_total
            evp(3, t + 10, "agent.turn_started", "p_a", "s1", json!({})),
            evp(4, t + 20, "agent.turn_started", "p_a", "s1", json!({})),
            evp(5, t + 30, "session.opened", "p_b", "s2", json!({"agent": "codex", "name": "web", "status": {"state": "idle"}})),
            evp(6, t + 3600, "agent.turn_started", "p_b", "s2", json!({})),
            evp(7, t + 1800, "session.status", "p_a", "s1", json!({"state": "working"})), // 01:30
            evp(8, t + 3600 + 600, "session.status", "p_a", "s1", json!({"state": "idle"})), // 02:10
            evp(9, t + 3600 + 700, "agent.cost", "p_b", "s2", json!({"delta_usd": 0.5})),
        ];
        let labels = Labels { projects: [("p_a".to_string(), "alpha".to_string())].into(), sessions: HashMap::new() };
        let s = series(&events, &params(InsightsRange::Today, InsightsMetric::Turns, Some(InsightsBy::Project)), now, &labels);
        assert_eq!(s.bucket, InsightsBucketSize::Hour);
        assert_eq!(s.buckets.len(), 24, "today spans the whole day");
        assert_eq!(s.buckets[1].total, 2.0);
        assert_eq!(s.buckets[1].values.get("p_a"), Some(&2.0));
        assert_eq!(s.buckets[2].values.get("p_b"), Some(&1.0));
        assert_eq!((s.total, s.previous_total), (3.0, 1.0));
        assert_eq!(s.groups[0].key, "p_a");
        assert_eq!(s.groups[0].label, "alpha");
        assert_eq!(s.groups[1].label, "p_b", "unknown projects fall back to the id");

        let w = series(&events, &params(InsightsRange::Today, InsightsMetric::Working, Some(InsightsBy::Agent)), now, &labels);
        assert_eq!(w.unit, "secs");
        assert_eq!(w.buckets[1].total, 1800.0);
        assert_eq!(w.buckets[2].total, 600.0);
        assert_eq!(w.groups[0].label, "Claude");

        let sp = series(&events, &params(InsightsRange::Today, InsightsMetric::Spend, None), now, &labels);
        assert_eq!((sp.unit.as_str(), sp.total), ("usd", 0.5));
        assert!(sp.groups.is_empty() && sp.buckets[2].values.is_empty());
    }

    #[test]
    fn series_month_uses_day_buckets() {
        let now = time::now_unix();
        let day = time::local_day_start(now);
        let events = vec![
            evp(1, day - 40 * 86_400, "agent.prompt_submitted", "p_a", "s1", json!({})),
            evp(2, day - 10 * 86_400 + 60, "agent.prompt_submitted", "p_a", "s1", json!({})),
            evp(3, now, "agent.prompt_submitted", "p_a", "s1", json!({})),
        ];
        let labels = Labels { projects: HashMap::new(), sessions: HashMap::new() };
        let s = series(&events, &params(InsightsRange::Month, InsightsMetric::Messages, None), now, &labels);
        assert_eq!(s.bucket, InsightsBucketSize::Day);
        assert_eq!(s.buckets.len(), 30);
        assert_eq!(s.buckets[19].total, 1.0);
        assert_eq!(s.buckets[29].total, 1.0);
        assert_eq!((s.total, s.previous_total), (2.0, 1.0));
        let m = summary(&events, InsightsRange::Month, None, now, &labels);
        assert_eq!(m.totals.messages, 2);
    }
}
