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
            kinds::AGENT_PROMPT_SUBMITTED if crate::prompts::is_human_prompt(e) => acc.bump(key, |x| x.messages += 1),
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
            kinds::AGENT_PROMPT_SUBMITTED if crate::prompts::is_human_prompt(e) => (InsightsMetric::Messages, 1.0),
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

// ------------------------------------------------------------------ detail

/// A session's time in one state: `[start, end)`.
struct Span {
    sid: String,
    state: StatusState,
    start: i64,
    end: i64,
    project: Option<Id>,
}

struct Turn {
    sid: String,
    start: i64,
    end: i64,
}

struct Wait {
    sid: Option<String>,
    raised: i64,
    resolved: i64,
    title: String,
}

/// Everything `detail` needs, gathered in one pass over the log.
#[derive(Default)]
struct Facts {
    spans: Vec<Span>,
    turns: Vec<Turn>,
    waits: Vec<Wait>,
    /// From a turn's end to the next prompt in that terminal (4 hours or less).
    idle: Vec<(String, i64, i64)>,
    /// (when, item title) for approvals.
    approved: Vec<(i64, String)>,
    denied: Vec<i64>,
    /// Turns the human interrupted (Esc while working, or a dismissed permission prompt).
    stopped: Vec<i64>,
    /// When the human acted: a prompt they typed, a needs-you item they answered.
    you: Vec<i64>,
    /// (when, model, USD).
    costs: Vec<(i64, String, f64)>,
    /// Terminals running an agent, with their project.
    agents: BTreeMap<String, Option<Id>>,
}

/// Longest gap from a turn's end to the next prompt that still counts as idle (else you were away).
const IDLE_MAX: i64 = 4 * 3600;
/// A prompt this soon after an agent's `session.input`, a queue send or a trigger firing in that
/// terminal was sent by them, not typed by you.
const MACHINE_PROMPT_SECS: i64 = 30;
/// `agent.turn_ended` reasons that mean you stopped the turn (see `rpc::session::settle_loop`).
const STOPPED_REASONS: &[&str] = &["stopped (title)", "prompt dismissed"];

fn gather(events: &[Event], end: i64) -> Facts {
    let mut f = Facts::default();
    let mut sessions: HashMap<&str, SessionInfo> = HashMap::new();
    let mut open_turn: HashMap<&str, i64> = HashMap::new();
    let mut idle_from: HashMap<&str, i64> = HashMap::new();
    // needs-you id -> (raised at, title, kind)
    let mut raised: HashMap<String, (i64, String, String)> = HashMap::new();
    // A permission prompt closed as `done` (answered in the terminal): approved if the agent goes
    // straight back to work.
    let mut answered_in_terminal: HashMap<&str, (i64, String)> = HashMap::new();
    let mut machine_input: HashMap<&str, i64> = HashMap::new();
    let close = |f: &mut Facts, sid: &str, info: &SessionInfo, t: i64| {
        if let Some(state @ (StatusState::Working | StatusState::NeedsYou)) = info.state
            && t > info.since
        {
            f.spans.push(Span { sid: sid.into(), state, start: info.since, end: t, project: info.project.clone() });
        }
    };
    for e in events {
        let Some(t) = time::parse_rfc3339(&e.at) else { continue };
        if t >= end {
            break;
        }
        let sid = e.session_id.as_deref();
        if let Some(sid) = sid
            && (e.kind.starts_with("agent.") || e.data.get("agent").is_some_and(|a| !a.is_null()))
            && e.kind != kinds::SESSION_CLOSED
        {
            let project = sessions.get(sid).and_then(|i| i.project.clone()).or(e.project_id.clone());
            f.agents.entry(sid.to_string()).or_insert(project);
        }
        match (e.kind.as_str(), sid) {
            (kinds::SESSION_OPENED, Some(sid)) => {
                let agent = e.data.get("agent").and_then(Value::as_str).map(str::to_string);
                let state = e.data.pointer("/status/state").cloned().and_then(|v| serde_json::from_value(v).ok());
                sessions.insert(sid, SessionInfo { project: e.project_id.clone(), agent, state, since: t });
            }
            (kinds::SESSION_STATUS, Some(sid)) => {
                let mut info = sessions.remove(sid).unwrap_or_else(|| SessionInfo { project: e.project_id.clone(), ..Default::default() });
                close(&mut f, sid, &info, t);
                info.state = e.data.get("state").cloned().and_then(|v| serde_json::from_value(v).ok());
                info.since = t;
                if let Some((at, title)) = answered_in_terminal.remove(sid)
                    && t - at <= 5
                    && info.state == Some(StatusState::Working)
                {
                    f.approved.push((t, title));
                    f.you.push(t);
                }
                sessions.insert(sid, info);
            }
            (kinds::SESSION_CLOSED, Some(sid)) => {
                if let Some(info) = sessions.remove(sid) {
                    close(&mut f, sid, &info, t);
                }
                open_turn.remove(sid);
                idle_from.remove(sid);
            }
            (kinds::AGENT_TURN_STARTED, Some(sid)) => {
                open_turn.entry(sid).or_insert(t);
            }
            (kinds::AGENT_TURN_ENDED, Some(sid)) => {
                if let Some(start) = open_turn.remove(sid) {
                    f.turns.push(Turn { sid: sid.into(), start, end: t });
                }
                if e.data.get("reason").and_then(Value::as_str).is_some_and(|r| STOPPED_REASONS.contains(&r)) {
                    f.stopped.push(t);
                }
                idle_from.insert(sid, t);
            }
            (kinds::AGENT_PROMPT_SUBMITTED, Some(sid)) => {
                // Codex's legacy notify reports the prompt only once its turn is over.
                let after_the_fact = e.data.get("via").and_then(Value::as_str) == Some("notify");
                if let Some(from) = idle_from.remove(sid)
                    && !after_the_fact
                    && t - from <= IDLE_MAX
                {
                    f.idle.push((sid.into(), from, t));
                }
                let machine = e.actor.kind == ActorKind::Trigger || machine_input.remove(sid).is_some_and(|at| t - at <= MACHINE_PROMPT_SECS);
                if !machine {
                    f.you.push(t);
                }
            }
            (kinds::SESSION_INPUT_BY_AGENT, Some(sid)) if e.data.get("enter").and_then(Value::as_bool) == Some(true) => {
                machine_input.insert(sid, t);
            }
            (kinds::SESSION_QUEUE, Some(sid)) if e.data.get("action").and_then(Value::as_str) == Some("sent") => {
                machine_input.insert(sid, t);
            }
            (kinds::AGENT_COST, _) => {
                let model = e.data.get("model").and_then(Value::as_str).filter(|m| !m.is_empty()).unwrap_or("Unknown");
                f.costs.push((t, model.to_string(), e.data.get("delta_usd").and_then(Value::as_f64).unwrap_or(0.0)));
            }
            _ => {}
        }
        match e.kind.as_str() {
            kinds::TRIGGER_FIRED => {
                // the terminal it typed into or started
                if let Some(s) = e.data.get("session_id").and_then(Value::as_str).or(sid) {
                    machine_input.insert(s, t);
                }
            }
            kinds::NEEDS_YOU_RAISED | kinds::NEEDS_YOU_UPDATED => {
                let s = |k: &str| e.data.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                let id = s("id");
                if !id.is_empty() {
                    let at = raised.get(&id).map_or(t, |r| r.0);
                    raised.insert(id, (at, s("title"), s("kind")));
                }
            }
            kinds::NEEDS_YOU_RESOLVED => {
                let id = e.data.get("id").and_then(Value::as_str).unwrap_or("");
                let Some((at, title, kind)) = raised.remove(id) else { continue };
                let res = e.data.pointer("/resolution/kind").and_then(Value::as_str).unwrap_or("");
                let auto = e.data.pointer("/resolution/auto").and_then(Value::as_bool) == Some(true);
                f.waits.push(Wait { sid: sid.map(str::to_string), raised: at, resolved: t, title: title.clone() });
                if e.actor.kind == ActorKind::Human {
                    f.you.push(t);
                }
                match res {
                    "approve" => f.approved.push((t, title)),
                    "deny" => f.denied.push(t),
                    "done" if auto && kind == "permission_prompt" => {
                        if let Some(sid) = sid {
                            answered_in_terminal.insert(sid, (t, title));
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    for (sid, info) in &sessions {
        close(&mut f, sid, info, end);
    }
    f
}

/// Group approvals by what was approved: `permission Bash(cargo test -p x)` -> `Bash(cargo test)`,
/// `permission Edit(/a/b.rs)` -> `Edit`, a question -> `Question`; other titles as they are.
fn approval_label(title: &str) -> String {
    let t = title.trim();
    let t = t.strip_prefix("permission ").unwrap_or(t);
    if t.starts_with("question") {
        return "Question".into();
    }
    let Some((tool, arg)) = t.split_once('(').filter(|(tool, _)| !tool.is_empty() && !tool.contains(' ')) else { return t.to_string() };
    if tool != "Bash" {
        return tool.to_string();
    }
    let arg = arg.strip_suffix(')').unwrap_or(arg);
    let mut words = arg.split_whitespace();
    let Some(first) = words.next() else { return tool.to_string() };
    match words.next().filter(|w| w.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') && !w.starts_with('-')) {
        Some(second) => format!("Bash({first} {second})"),
        None => format!("Bash({first})"),
    }
}

/// Split concurrent spans into `(start, end, count)` pieces where `count` sessions overlap (> 0).
fn overlaps(spans: impl Iterator<Item = (i64, i64)>) -> Vec<(i64, i64, u32)> {
    let mut points: Vec<(i64, i32)> = spans.filter(|(s, e)| e > s).flat_map(|(s, e)| [(s, 1), (e, -1)]).collect();
    // ends before starts at the same second: back-to-back isn't overlap
    points.sort();
    let mut out = vec![];
    let mut count = 0i32;
    for (i, &(t, delta)) in points.iter().enumerate() {
        count += delta;
        if let Some(&(next, _)) = points.get(i + 1)
            && count > 0
            && next > t
        {
            out.push((t, next, count as u32));
        }
    }
    out
}

fn clip(s: i64, e: i64, from: i64, to: i64) -> Option<(i64, i64)> {
    let (s, e) = (s.max(from), e.min(to));
    (e > s).then_some((s, e))
}

/// Add `[s, e)` into fixed-size slots starting at `origin`, `per` once per second covered.
fn spread(slots: &mut [f64], origin: i64, size: i64, s: i64, e: i64, per: f64) {
    let mut cur = s;
    while cur < e {
        let i = (cur - origin) / size;
        let next = (origin + (i + 1) * size).min(e);
        if let Some(v) = usize::try_from(i).ok().and_then(|i| slots.get_mut(i)) {
            *v += (next - cur) as f64 * per;
        }
        cur = next;
    }
}

fn counts_desc(map: HashMap<String, f64>, label: impl Fn(&str) -> String) -> Vec<InsightsCount> {
    let mut v: Vec<InsightsCount> = map.into_iter().map(|(key, value)| InsightsCount { label: label(&key), key, value: round6(value) }).collect();
    v.sort_by(|a, b| b.value.partial_cmp(&a.value).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.key.cmp(&b.key)));
    v
}

pub fn detail(events: &[Event], range: InsightsRange, now: i64, labels: &Labels) -> InsightsDetail {
    let (from, to, axis_end) = range_bounds(range, now);
    let f = gather(events, now + 1);
    let working = || f.spans.iter().filter(|s| s.state == StatusState::Working);
    let in_range = |t: i64| t >= from && t < to;

    // concurrency
    let step = match range {
        InsightsRange::Today | InsightsRange::Yesterday => 600,
        _ => 3600,
    };
    let n = ((axis_end - from + step - 1) / step).max(1) as usize;
    let mut samples = vec![0.0; n];
    let pieces = overlaps(working().filter_map(|s| clip(s.start, s.end, from, to)));
    let (mut busy, mut weighted, mut multi_secs) = (0i64, 0f64, 0i64);
    let (mut peak, mut peak_at) = (0u32, None);
    for &(s, e, c) in &pieces {
        spread(&mut samples, from, step, s, e, f64::from(c) / step as f64);
        busy += e - s;
        weighted += f64::from(c) * (e - s) as f64;
        if c >= 2 {
            multi_secs += e - s;
        }
        if c > peak {
            (peak, peak_at) = (c, Some(time::format_unix(s)));
        }
    }
    let concurrency = InsightsConcurrency {
        step_secs: step,
        samples: samples.into_iter().map(|v| (v * 1000.0).round() / 1000.0).collect(),
        peak,
        peak_at,
        avg_while_working: if busy > 0 { (weighted / busy as f64 * 100.0).round() / 100.0 } else { 0.0 },
        multi_secs,
    };

    // turn lengths
    let mut lens: Vec<(i64, &Turn)> = f.turns.iter().filter(|t| in_range(t.end)).map(|t| (t.end - t.start, t)).collect();
    let mut bins = vec![0i64; 6];
    for (len, _) in &lens {
        let i = [60, 300, 900, 1800, 3600].iter().position(|lim| len < lim).unwrap_or(5);
        bins[i] += 1;
    }
    lens.sort_by_key(|(len, _)| *len);
    let median_secs = match lens.len() {
        0 => 0,
        k if k % 2 == 1 => lens[k / 2].0,
        k => (lens[k / 2 - 1].0 + lens[k / 2].0) / 2,
    };
    let longest = lens.last();
    let turns = InsightsTurnLengths {
        bins,
        median_secs,
        longest_secs: longest.map_or(0, |l| l.0),
        longest_session: longest.map(|l| l.1.sid.clone()),
    };

    // waits
    let waits = f
        .waits
        .iter()
        .filter(|w| in_range(w.resolved))
        .map(|w| InsightsWait { secs: w.resolved - w.raised, resolved_at: time::format_unix(w.resolved), session_id: w.sid.clone(), title: w.title.clone() })
        .collect();

    // time per agent terminal
    let mut per: BTreeMap<&str, (i64, i64, i64)> = BTreeMap::new();
    for s in f.spans.iter().filter(|s| f.agents.contains_key(&s.sid)) {
        if let Some((a, b)) = clip(s.start, s.end, from, to) {
            let row = per.entry(&s.sid).or_default();
            match s.state {
                StatusState::Working => row.0 += b - a,
                _ => row.1 += b - a,
            }
        }
    }
    for (sid, s, e) in &f.idle {
        if let Some((a, b)) = clip(*s, *e, from, to) {
            per.entry(sid).or_default().2 += b - a;
        }
    }
    let mut agent_time: Vec<InsightsAgentTime> = per
        .into_iter()
        .filter(|(_, (w, b, i))| w + b + i > 0)
        .map(|(sid, (working_secs, blocked_secs, idle_secs))| InsightsAgentTime {
            key: sid.to_string(),
            label: labels.sessions.get(sid).cloned().or_else(|| session_name_from_events(events, sid)).unwrap_or_else(|| sid.to_string()),
            project_id: f.agents.get(sid).cloned().flatten(),
            working_secs,
            blocked_secs,
            idle_secs,
        })
        .collect();
    agent_time.sort_by(|a, b| (b.blocked_secs + b.idle_secs).cmp(&(a.blocked_secs + a.idle_secs)).then_with(|| b.working_secs.cmp(&a.working_secs)).then_with(|| a.key.cmp(&b.key)));

    // approvals by what was approved
    let mut approved_map: HashMap<String, f64> = HashMap::new();
    for (_, title) in f.approved.iter().filter(|(t, _)| in_range(*t)) {
        *approved_map.entry(approval_label(title)).or_default() += 1.0;
    }
    let mut approved = counts_desc(approved_map, str::to_string);
    approved.truncate(10);

    // corrections per local day
    let first_day = time::local_day_start(from);
    let days = ((to - first_day + 86_399) / 86_400).max(1);
    let corrections = (0..days)
        .map(|i| {
            let d = first_day + i * 86_400;
            let on = |t: &&i64| **t >= d.max(from) && **t < (d + 86_400).min(to);
            InsightsCorrections { day: time::format_unix(d), denied: f.denied.iter().filter(on).count() as i64, stopped: f.stopped.iter().filter(on).count() as i64 }
        })
        .collect();

    // the last 7 days × 24 hours
    let today = time::local_day_start(now);
    let h0 = today - 6 * 86_400;
    let mut hours = vec![0.0; 7 * 24];
    for s in working() {
        if let Some((a, b)) = clip(s.start, s.end, h0, now + 1) {
            spread(&mut hours, h0, 3600, a, b, 1.0);
        }
    }
    let mut you = [false; 7 * 24];
    for t in f.you.iter().filter(|t| **t >= h0 && **t <= now) {
        you[((t - h0) / 3600) as usize] = true;
    }
    let heatmap = (0..7)
        .map(|d| InsightsHeatDay {
            day: time::format_unix(h0 + d as i64 * 86_400),
            working_secs: hours[d * 24..(d + 1) * 24].iter().map(|v| *v as i64).collect(),
            you: you[d * 24..(d + 1) * 24].to_vec(),
        })
        .collect();

    // working time per project
    let mut project_map: HashMap<String, f64> = HashMap::new();
    for s in working() {
        if let Some((a, b)) = clip(s.start, s.end, from, to) {
            *project_map.entry(s.project.clone().unwrap_or_else(|| "none".into())).or_default() += (b - a) as f64;
        }
    }
    let projects = counts_desc(project_map, |key| {
        labels.projects.get(key).cloned().unwrap_or_else(|| match key {
            "none" => "No project".into(),
            ROOT_PROJECT_ID => "root".into(),
            k => k.to_string(),
        })
    });

    // spend per model
    let mut model_map: HashMap<String, f64> = HashMap::new();
    for (_, model, usd) in f.costs.iter().filter(|(t, ..)| in_range(*t)) {
        *model_map.entry(model.clone()).or_default() += usd;
    }
    let models = counts_desc(model_map, str::to_string).into_iter().filter(|c| c.value > 0.0).collect();

    // records over the whole log
    let mut per_day: BTreeMap<i64, i64> = BTreeMap::new();
    for s in working() {
        let mut cur = s.start;
        while cur < s.end {
            let day = time::local_day_start(cur);
            let next = (day + 86_400).min(s.end);
            *per_day.entry(day).or_default() += next - cur;
            cur = next;
        }
    }
    let busiest = per_day.iter().fold(None, |best: Option<(i64, i64)>, (d, secs)| match best {
        Some((_, b)) if b >= *secs => best,
        _ => Some((*d, *secs)),
    });
    let longest_turn = f.turns.iter().fold(None, |best: Option<&Turn>, t| match best {
        Some(b) if b.end - b.start >= t.end - t.start => best,
        _ => Some(t),
    });
    let all_peak = overlaps(working().map(|s| (s.start, s.end))).into_iter().fold(None, |best: Option<(i64, u32)>, (s, _, c)| match best {
        Some((_, b)) if b >= c => best,
        _ => Some((s, c)),
    });
    let bests = InsightsBests {
        busiest_day: busiest.map(|(d, _)| time::format_unix(d)),
        busiest_day_secs: busiest.map_or(0, |(_, s)| s),
        longest_turn_secs: longest_turn.map_or(0, |t| t.end - t.start),
        longest_turn_at: longest_turn.map(|t| time::format_unix(t.end)),
        peak_agents: all_peak.map_or(0, |(_, c)| c),
        peak_at: all_peak.map(|(s, _)| time::format_unix(s)),
    };

    InsightsDetail {
        from: time::format_unix(from),
        to: time::format_unix(axis_end.max(to)),
        concurrency,
        turns,
        waits,
        agent_time,
        approved,
        corrections,
        heatmap,
        projects,
        models,
        bests,
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

    // ------------------------------------------------------------------ detail

    fn no_labels() -> Labels {
        Labels { projects: HashMap::new(), sessions: HashMap::new() }
    }

    fn by(mut e: Event, kind: ActorKind) -> Event {
        e.actor = Actor { kind, session: None, name: None };
        e
    }

    fn opened(seq: u64, at: i64, sid: &str, agent: Option<&str>) -> Event {
        ev(seq, at, "session.opened", sid, json!({"agent": agent, "name": format!("term {sid}"), "status": {"state": "idle"}}))
    }

    /// Today at 05:02 local, and 01:00 local.
    fn clock() -> (i64, i64) {
        let day = time::local_day_start(time::now_unix());
        (day + 5 * 3600 + 120, day + 3600)
    }

    #[test]
    fn detail_concurrency_from_overlapping_sessions() {
        let (now, t0) = clock();
        let events = vec![
            opened(1, t0 - 60, "s1", Some("claude")),
            opened(2, t0 - 60, "s2", Some("codex")),
            ev(3, t0, "session.status", "s1", json!({"state": "working"})),
            ev(4, t0 + 600, "session.status", "s2", json!({"state": "working"})),
            ev(5, t0 + 1200, "session.status", "s1", json!({"state": "idle"})),
            ev(6, t0 + 1800, "session.status", "s2", json!({"state": "done"})),
            // back to back is not overlap
            ev(7, t0 + 1800, "session.status", "s1", json!({"state": "working"})),
            ev(8, t0 + 2400, "session.status", "s1", json!({"state": "idle"})),
        ];
        let d = detail(&events, InsightsRange::Today, now, &no_labels());
        let c = &d.concurrency;
        assert_eq!(c.step_secs, 600);
        assert_eq!(c.samples.len(), 144, "today's axis runs to midnight");
        assert_eq!(&c.samples[5..11], &[0.0, 1.0, 2.0, 1.0, 1.0, 0.0]);
        assert_eq!((c.peak, c.peak_at.clone()), (2, Some(time::format_unix(t0 + 600))));
        assert_eq!(c.multi_secs, 600);
        assert_eq!(c.avg_while_working, 1.25); // (600 + 1200 + 600 + 600) / 2400
        assert_eq!((d.bests.peak_agents, d.bests.peak_at.clone()), (2, Some(time::format_unix(t0 + 600))));
        assert_eq!(d.bests.busiest_day_secs, 3000);
        assert_eq!(d.projects.len(), 1);
        assert_eq!((d.projects[0].key.as_str(), d.projects[0].value), ("p_1", 3000.0));

        let w = detail(&events, InsightsRange::Week, now, &no_labels());
        assert_eq!((w.concurrency.step_secs, w.concurrency.samples.len()), (3600, 7 * 24));
        assert_eq!(w.corrections.len(), 7);
        let y = detail(&events, InsightsRange::Yesterday, now, &no_labels());
        assert_eq!((y.concurrency.samples.len(), y.concurrency.peak), (144, 0));
        assert_eq!(y.bests.peak_agents, 2, "records span the whole log");
    }

    #[test]
    fn detail_turn_lengths() {
        let (now, t0) = clock();
        let mut events = vec![opened(1, t0 - 7200, "s1", Some("claude"))];
        let mut seq = 2;
        // one turn that ended yesterday (left out), then 30s, 200s, 1000s, 4000s (started yesterday)
        let day = time::local_day_start(now);
        for (start, len) in [(day - 7200, 100), (day - 3000, 4000), (t0 + 1100, 30), (t0 + 1200, 200), (t0 + 2000, 1000)] {
            events.push(ev(seq, start, "agent.turn_started", "s1", json!({})));
            events.push(ev(seq + 1, start + len, "agent.turn_ended", "s1", json!({"reason": "Stop"})));
            seq += 2;
        }
        events.sort_by_key(|e| time::parse_rfc3339(&e.at));
        let d = detail(&events, InsightsRange::Today, now, &no_labels());
        assert_eq!(d.turns.bins, vec![1, 1, 0, 1, 0, 1]);
        assert_eq!(d.turns.median_secs, 600); // (200 + 1000) / 2
        assert_eq!((d.turns.longest_secs, d.turns.longest_session.as_deref()), (4000, Some("s1")));
        assert_eq!((d.bests.longest_turn_secs, d.bests.longest_turn_at.clone()), (4000, Some(time::format_unix(day + 1000))));
    }

    #[test]
    fn detail_waits_approvals_and_corrections() {
        let (now, t0) = clock();
        let raise = |seq, at, id: &str, title: &str| ev(seq, at, "needs_you.raised", "s1", json!({"id": id, "kind": "permission_prompt", "title": title}));
        let resolve = |seq, at, id: &str, res: Value| by(ev(seq, at, "needs_you.resolved", "s1", json!({"id": id, "resolution": res})), ActorKind::Human);
        let approve = || json!({"kind": "approve", "scope": {"kind": "once"}});
        let events = vec![
            opened(1, t0 - 60, "s1", Some("claude")),
            raise(2, t0, "n_1", "permission Bash(cargo test -p midnad)"),
            resolve(3, t0 + 90, "n_1", approve()),
            raise(4, t0 + 100, "n_2", "permission Bash(cargo test --all)"),
            ev(5, t0 + 101, "needs_you.updated", "s1", json!({"id": "n_2", "kind": "permission_prompt", "title": "permission Bash(cargo test --workspace)"})),
            resolve(6, t0 + 160, "n_2", approve()),
            raise(7, t0 + 200, "n_3", "permission Edit(/a/b.rs)"),
            resolve(8, t0 + 210, "n_3", json!({"kind": "deny"})),
            // answered in the terminal: closed as done and the agent goes back to work
            raise(9, t0 + 300, "n_4", "permission Edit(/a/c.rs)"),
            ev(10, t0 + 320, "needs_you.resolved", "s1", json!({"id": "n_4", "kind": "permission_prompt", "resolution": {"kind": "done", "auto": true}})),
            ev(11, t0 + 320, "session.status", "s1", json!({"state": "working"})),
            ev(12, t0 + 400, "agent.turn_started", "s1", json!({})),
            ev(13, t0 + 500, "agent.turn_ended", "s1", json!({"reason": "stopped (title)"})),
            ev(14, t0 + 500, "agent.turn_ended", "s1", json!({"reason": "Stop"})),
            // resolved without a raise in the log: no wait
            resolve(15, t0 + 600, "n_old", approve()),
        ];
        let d = detail(&events, InsightsRange::Today, now, &no_labels());
        let waits: Vec<(i64, &str)> = d.waits.iter().map(|w| (w.secs, w.title.as_str())).collect();
        assert_eq!(waits, vec![(90, "permission Bash(cargo test -p midnad)"), (60, "permission Bash(cargo test --workspace)"), (10, "permission Edit(/a/b.rs)"), (20, "permission Edit(/a/c.rs)")]);
        assert_eq!(d.waits[0].session_id.as_deref(), Some("s1"));
        let approved: Vec<(&str, f64)> = d.approved.iter().map(|c| (c.label.as_str(), c.value)).collect();
        assert_eq!(approved, vec![("Bash(cargo test)", 2.0), ("Edit", 1.0)]);
        assert_eq!(d.corrections.len(), 1);
        assert_eq!((d.corrections[0].denied, d.corrections[0].stopped), (1, 1));
        assert_eq!(d.corrections[0].day, time::format_unix(time::local_day_start(now)));
    }

    #[test]
    fn approval_labels() {
        assert_eq!(approval_label("permission Bash(git push origin main)"), "Bash(git push)");
        assert_eq!(approval_label("permission Bash(ls -la)"), "Bash(ls)");
        assert_eq!(approval_label("permission WebFetch(https://docs.rs)"), "WebFetch");
        assert_eq!(approval_label("question: Which one?"), "Question");
        assert_eq!(approval_label("Allow deploy to prod?"), "Allow deploy to prod?");
    }

    #[test]
    fn detail_agent_time_counts_idle_gaps_up_to_four_hours() {
        let (now, _) = clock();
        let day = time::local_day_start(now);
        let events = vec![
            opened(1, day - 7 * 3600, "s1", Some("claude")),
            opened(2, day - 7 * 3600, "sh", None),
            // a 5h gap (you were away) that ends today: dropped
            ev(3, day - 3 * 3600, "agent.turn_ended", "s1", json!({"reason": "Stop"})),
            ev(4, day + 2 * 3600, "agent.prompt_submitted", "s1", json!({})),
            ev(5, day + 2 * 3600, "session.status", "s1", json!({"state": "working"})),
            ev(6, day + 2 * 3600 + 100, "session.status", "s1", json!({"state": "needs_you"})),
            ev(7, day + 2 * 3600 + 160, "session.status", "s1", json!({"state": "idle"})),
            ev(8, day + 2 * 3600 + 160, "agent.turn_ended", "s1", json!({"reason": "Stop"})),
            // 300s until your next prompt: idle
            ev(9, day + 2 * 3600 + 460, "agent.prompt_submitted", "s1", json!({})),
            // a turn that ends with nothing after it: not idle
            ev(10, day + 2 * 3600 + 500, "agent.turn_ended", "s1", json!({"reason": "Stop"})),
            // a plain shell is left out even when it shows time
            ev(11, day + 3600, "session.status", "sh", json!({"state": "working"})),
            ev(12, day + 3700, "session.status", "sh", json!({"state": "idle"})),
        ];
        let labels = Labels { projects: HashMap::new(), sessions: [("s1".to_string(), "api".to_string())].into() };
        let d = detail(&events, InsightsRange::Today, now, &labels);
        assert_eq!(d.agent_time.len(), 1, "{:?}", d.agent_time);
        let a = &d.agent_time[0];
        assert_eq!((a.key.as_str(), a.label.as_str(), a.project_id.as_deref()), ("s1", "api", Some("p_1")));
        assert_eq!((a.working_secs, a.blocked_secs, a.idle_secs), (100, 60, 300));
        let unlabeled = detail(&events, InsightsRange::Today, now, &no_labels());
        assert_eq!(unlabeled.agent_time[0].label, "term s1", "closed terminals are named from the log");
    }

    #[test]
    fn detail_heatmap_and_you() {
        let (now, _) = clock();
        let day = time::local_day_start(now);
        let events = vec![
            opened(1, day - 2 * 86_400, "s1", Some("claude")),
            ev(2, day - 2 * 86_400 + 9 * 3600, "session.status", "s1", json!({"state": "working"})),
            ev(3, day - 2 * 86_400 + 10 * 3600 + 1800, "session.status", "s1", json!({"state": "idle"})),
            ev(4, day + 3600 + 5, "agent.prompt_submitted", "s1", json!({})),
            // typed by an agent through session.input, by the queue, by a trigger: not you
            ev(5, day + 2 * 3600, "session.input_by_agent", "s1", json!({"text": "go", "enter": true})),
            ev(6, day + 2 * 3600 + 3, "agent.prompt_submitted", "s1", json!({})),
            ev(7, day + 3 * 3600, "session.queue", "s1", json!({"action": "sent", "id": "q_1"})),
            ev(8, day + 3 * 3600 + 2, "agent.prompt_submitted", "s1", json!({})),
            by(ev(9, day + 4 * 3600, "trigger.fired", "s1", json!({"trigger_id": "t_1"})), ActorKind::Trigger),
            ev(10, day + 4 * 3600 + 10, "agent.prompt_submitted", "s1", json!({})),
            // a needs-you item you answered
            ev(11, day + 5 * 3600 - 100, "needs_you.raised", "s1", json!({"id": "n_1", "title": "permission Bash(ls)"})),
            by(ev(12, day + 5 * 3600 + 1, "needs_you.resolved", "s1", json!({"id": "n_1", "resolution": {"kind": "approve"}})), ActorKind::Human),
        ];
        let d = detail(&events, InsightsRange::Today, now, &no_labels());
        assert_eq!(d.heatmap.len(), 7);
        assert!(d.heatmap.iter().all(|h| h.working_secs.len() == 24 && h.you.len() == 24));
        assert_eq!(d.heatmap[6].day, time::format_unix(day));
        assert_eq!(d.heatmap[0].day, time::format_unix(day - 6 * 86_400));
        assert_eq!((d.heatmap[4].working_secs[9], d.heatmap[4].working_secs[10], d.heatmap[4].working_secs[11]), (3600, 1800, 0));
        let you: Vec<usize> = (0..24).filter(|h| d.heatmap[6].you[*h]).collect();
        assert_eq!(you, vec![1, 5]);
        assert_eq!(d.bests.busiest_day, Some(time::format_unix(day - 2 * 86_400)));
    }

    #[test]
    fn detail_spend_per_model() {
        let (now, t0) = clock();
        let events = vec![
            ev(1, t0 - 86_400, "agent.cost", "s1", json!({"delta_usd": 9.0, "model": "Opus"})), // yesterday
            ev(2, t0, "agent.cost", "s1", json!({"delta_usd": 0.5, "model": "Opus"})),
            ev(3, t0 + 10, "agent.cost", "s2", json!({"delta_usd": 0.1, "model": null})),
            ev(4, t0 + 20, "agent.cost", "s1", json!({"delta_usd": 0.25, "model": "Opus"})),
        ];
        let d = detail(&events, InsightsRange::Today, now, &no_labels());
        let models: Vec<(&str, f64)> = d.models.iter().map(|c| (c.label.as_str(), c.value)).collect();
        assert_eq!(models, vec![("Opus", 0.75), ("Unknown", 0.1)]);
    }
}
