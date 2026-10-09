//! Keep-awake schedule: when midnad may hold a "don't idle-sleep" power assertion. Pure (local
//! times in, decisions out); midnad's `keep_awake.rs` owns the assertion, the battery and the
//! work it waits on.
//!
//! The schedule mirrors Taskboard's work hours (`{on, start, end, days}` and a today override),
//! with the same forgiving parsing: `8am`, `08:00`, `8:30pm`, `noon`; `weekdays`, `mon-fri`,
//! `sat,sun`. Times are stored as `HH:MM` (24-hour) and shown as `8 AM`.
//!
//! - A day's window runs from `start` to `end`; an `end` at or before `start` runs past
//!   midnight into the next day (`22:00`–`02:00`), and `start` = `end` is all day.
//! - `keep_awake.hours` gives single days their own window or turns them off
//!   (`fri = 09:00-15:00`, `sat = off`). A day listed there follows it whatever `days` says.
//! - The today override replaces the schedule for a while: `off` for the rest of today, `on`
//!   until midnight, or on until a time (`until 5pm`, `for 5 hours`), which may be past midnight
//!   (`until 1am` at 8 PM); when that time comes the schedule takes over again. It belongs to
//!   its date, so it ends by itself at midnight (or at its time past midnight).
use crate::time;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
/// On battery below `keep_awake.min_battery`, the assertion comes back only at this many
/// points above it (or on AC), so it doesn't flap around the line.
pub const BATTERY_HYSTERESIS: u8 = 5;
const ALL_DAY: &str = "all day";

// ------------------------------------------------------------------ parsing

/// A time of day as minutes past midnight: `8am`, `8 AM`, `08:00`, `8:30pm`, `20.30`, `0830`,
/// `noon`, `midnight`. `24:00` is midnight.
pub fn parse_time(v: &str) -> Result<u32, String> {
    let bad = || format!("`{}` isn't a time of day; use 8am, 5:30pm or 17:30", v.trim());
    let mut s = v.trim().to_lowercase().replace("a.m.", "am").replace("p.m.", "pm").replace(' ', "").replace('.', ":");
    match s.as_str() {
        "noon" | "midday" => return Ok(720),
        "midnight" => return Ok(0),
        _ => {}
    }
    let (am, pm) = (s.ends_with("am") || s.ends_with('a'), s.ends_with("pm") || s.ends_with('p'));
    if am || pm {
        s = s.trim_end_matches('m').trim_end_matches(['a', 'p']).to_string();
    }
    let (hs, ms) = match s.split_once(':') {
        Some((h, m)) => (h.to_string(), m.to_string()),
        None if s.len() > 2 => (s[..s.len() - 2].to_string(), s[s.len() - 2..].to_string()),
        None => (s.clone(), "0".into()),
    };
    if hs.is_empty() || !hs.chars().all(|c| c.is_ascii_digit()) || !ms.chars().all(|c| c.is_ascii_digit()) || ms.is_empty() || ms.len() > 2 {
        return Err(bad());
    }
    let (mut h, m): (u32, u32) = (hs.parse().map_err(|_| bad())?, ms.parse().map_err(|_| bad())?);
    if am || pm {
        if !(1..=12).contains(&h) {
            return Err(bad());
        }
        h = h % 12 + if pm { 12 } else { 0 };
    }
    if h == 24 && m == 0 {
        h = 0;
    }
    if h > 23 || m > 59 {
        return Err(bad());
    }
    Ok(h * 60 + m)
}

/// `HH:MM`, what settings store.
pub fn hhmm(min: u32) -> String {
    format!("{:02}:{:02}", min / 60 % 24, min % 60)
}

/// `9 AM`, `5:30 PM`, `12 AM` (midnight): what people see.
pub fn clock(min: u32) -> String {
    crate::cron::clock(min / 60 % 24, min % 60)
}

/// A stored `HH:MM` (or anything [`parse_time`] reads) as minutes; `fallback` if unreadable.
fn minutes(s: &str, fallback: u32) -> u32 {
    parse_time(s).unwrap_or(fallback)
}

fn day_index(word: &str) -> Option<usize> {
    let w: String = word.trim().chars().take(3).collect();
    DAYS.iter().position(|d| *d == w)
}

/// Days as `mon` … `sun`, in week order: `weekdays`, `weekends`, `daily`, `mon-fri`, `fri-mon`
/// (wraps), `mon,wed,fri`, `tuesday thursday`, or a JSON array of those.
pub fn parse_days(v: &Value) -> Result<Vec<String>, String> {
    let words: Vec<String> = match v {
        Value::String(s) => s.to_lowercase().split([',', ' ', '\n', '/', '+']).map(str::to_string).collect(),
        Value::Array(a) => a.iter().map(|x| x.as_str().map(str::to_lowercase).ok_or("days are names like mon or weekdays")).collect::<Result<_, _>>()?,
        _ => return Err("days are names like mon, weekdays or mon-fri".into()),
    };
    let mut on = [false; 7];
    for w in words.iter().map(|w| w.trim()).filter(|w| !w.is_empty() && !matches!(*w, "and" | "day" | "days")) {
        match w {
            "all" | "every" | "everyday" | "daily" | "week" => on = [true; 7],
            "weekdays" | "weekday" | "workdays" => on[..5].iter_mut().for_each(|d| *d = true),
            "weekends" | "weekend" => on[5..].iter_mut().for_each(|d| *d = true),
            _ => match w.split_once(['-', '–']) {
                Some((a, b)) => {
                    let (Some(i), Some(j)) = (day_index(a), day_index(b)) else {
                        return Err(format!("`{w}` isn't a range of days; use mon-fri"));
                    };
                    let mut k = i;
                    loop {
                        on[k] = true;
                        if k == j {
                            break;
                        }
                        k = (k + 1) % 7;
                    }
                }
                None => on[day_index(w).ok_or_else(|| format!("`{w}` isn't a day; use mon, tue … sun, weekdays or weekends"))?] = true,
            },
        }
    }
    let days: Vec<String> = DAYS.iter().zip(on).filter(|(_, o)| *o).map(|(d, _)| d.to_string()).collect();
    if days.is_empty() {
        return Err("pick at least one day (or turn keep_awake.enabled off)".into());
    }
    Ok(days)
}

/// One day's own hours (`keep_awake.hours`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DayRule {
    Off,
    /// Minutes past midnight; `end` <= `start` runs past midnight, equal = all day.
    Hours { start: u32, end: u32 },
}

impl DayRule {
    /// `off`, `all day`, `9am-3pm`, `9am to 3pm`, `21:00–02:00`.
    pub fn parse(s: &str) -> Result<DayRule, String> {
        let t = s.trim().to_lowercase();
        match t.as_str() {
            "off" | "none" | "no" | "closed" | "false" => return Ok(DayRule::Off),
            "all day" | "allday" | "all" | "24h" | "always" | "on" => return Ok(DayRule::Hours { start: 0, end: 0 }),
            _ => {}
        }
        let (a, b) = t
            .split_once(" to ")
            .or_else(|| t.split_once(['–', '—']))
            .or_else(|| t.split_once('-'))
            .ok_or_else(|| format!("`{}` isn't a day's hours; use 9am-3pm, off or all day", s.trim()))?;
        Ok(DayRule::Hours { start: parse_time(a)?, end: parse_time(b)? })
    }

    /// `off`, `all day` or `09:00-15:00`: what `keep_awake.hours` stores.
    pub fn stored(self) -> String {
        match self {
            DayRule::Off => "off".into(),
            DayRule::Hours { start, end } if start == end => ALL_DAY.into(),
            DayRule::Hours { start, end } => format!("{}-{}", hhmm(start), hhmm(end)),
        }
    }

    /// `off`, `all day` or `9 AM–3 PM`.
    pub fn words(self) -> String {
        match self {
            DayRule::Off => "off".into(),
            DayRule::Hours { start, end } if start == end => ALL_DAY.into(),
            DayRule::Hours { start, end } => format!("{}–{}", clock(start), clock(end)),
        }
    }
}

/// Normalize `keep_awake.hours`: a list of `<days> = <hours>` rules (`fri = 9am-3pm`,
/// `sat-sun = off`), comma or newline separated text, or an object `{"fri": "9am-3pm"}`.
/// Stored as one `day = hours` rule per day, in week order; a later rule for a day wins.
pub fn coerce_hours(v: &Value) -> Result<Value, String> {
    let pairs: Vec<(String, String)> = match v {
        Value::Object(o) => o.iter().map(|(k, v)| (k.clone(), v.as_str().map(str::to_string).unwrap_or_else(|| if v.is_null() { "default".into() } else { v.to_string() }))).collect(),
        Value::Array(_) | Value::String(_) => {
            let items: Vec<String> = match v {
                Value::Array(a) => a.iter().map(|x| x.as_str().map(str::to_string).ok_or("keep_awake.hours expects rules like `fri = 9am-3pm`")).collect::<Result<_, _>>()?,
                Value::String(s) => s.split([',', '\n', ';']).map(str::to_string).collect(),
                _ => unreachable!(),
            };
            let mut out = vec![];
            for i in items.iter().map(|i| i.trim()).filter(|i| !i.is_empty()) {
                let (d, h) = i.split_once('=').or_else(|| i.split_once(':')).ok_or_else(|| format!("keep_awake.hours: `{i}` must look like `fri = 9am-3pm` or `sat = off`"))?;
                out.push((d.to_string(), h.to_string()));
            }
            out
        }
        _ => return Err("keep_awake.hours expects rules like `fri = 9am-3pm` or `sat = off`".into()),
    };
    let mut per: [Option<DayRule>; 7] = [None; 7];
    for (days, hours) in pairs {
        let days = parse_days(&json!(days.trim())).map_err(|e| format!("keep_awake.hours: {e}"))?;
        let rule = match hours.trim().to_lowercase().as_str() {
            "default" | "normal" | "clear" | "" => None,
            _ => Some(DayRule::parse(&hours).map_err(|e| format!("keep_awake.hours: {e}"))?),
        };
        for d in days {
            per[day_index(&d).unwrap_or(0)] = rule;
        }
    }
    Ok(json!(DAYS.iter().zip(per).filter_map(|(d, r)| Some(format!("{d} = {}", r?.stored()))).collect::<Vec<_>>()))
}

// ------------------------------------------------------------------ the plan

/// The schedule from settings (already normalized by the catalog).
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub start: u32,
    pub end: u32,
    /// mon … sun.
    pub days: [bool; 7],
    pub per_day: [Option<DayRule>; 7],
}

impl Default for Plan {
    fn default() -> Plan {
        Plan { start: 9 * 60, end: 18 * 60, days: [true, true, true, true, true, false, false], per_day: [None; 7] }
    }
}

impl Plan {
    /// From the stored values of `keep_awake.start`, `.end`, `.days` and `.hours`.
    pub fn from_settings(start: &str, end: &str, days: &[String], hours: &[String]) -> Plan {
        let d = Plan::default();
        let mut p = Plan { start: minutes(start, d.start), end: minutes(end, d.end), days: [false; 7], per_day: [None; 7] };
        for day in days {
            if let Some(i) = day_index(day) {
                p.days[i] = true;
            }
        }
        for rule in hours {
            if let Some((day, h)) = rule.split_once('=')
                && let (Some(i), Ok(r)) = (day_index(day), DayRule::parse(h))
            {
                p.per_day[i] = Some(r);
            }
        }
        p
    }

    /// Day `wd`'s window (0 = mon) as (start, length in minutes), None when the day is off.
    fn span(&self, wd: usize) -> Option<(u32, u32)> {
        let (s, e) = match self.per_day[wd] {
            Some(DayRule::Off) => return None,
            Some(DayRule::Hours { start, end }) => (start, end),
            None if self.days[wd] => (self.start, self.end),
            None => return None,
        };
        let len = (e + 1440 - s) % 1440;
        Some((s, if len == 0 { 1440 } else { len }))
    }

    /// `9 AM–6 PM weekdays; Fri 9 AM–3 PM; Sat off`.
    pub fn describe(&self) -> String {
        let hours = DayRule::Hours { start: self.start, end: self.end }.words();
        let mut out = format!("{hours} {}", days_words(&self.days));
        for (i, r) in self.per_day.iter().enumerate() {
            if let Some(r) = r {
                out.push_str(&format!("; {} {}", cap(DAYS[i]), r.words()));
            }
        }
        out
    }
}

/// `every day`, `weekdays`, `weekends`, `Mon, Wed, Fri` (mon … sun flags); reads back with [`parse_days`].
pub fn days_words(on: &[bool; 7]) -> String {
    let on: Vec<usize> = (0..7).filter(|i| on[*i]).collect();
    match on.as_slice() {
        [0, 1, 2, 3, 4, 5, 6] => "every day".into(),
        [0, 1, 2, 3, 4] => "weekdays".into(),
        [5, 6] => "weekends".into(),
        [] => "no days".into(),
        l => l.iter().map(|i| cap(DAYS[*i])).collect::<Vec<_>>().join(", "),
    }
}

/// A stored `keep_awake.*` value as people read (and can type back): `9 AM` for start/end,
/// `weekdays` for days, `fri = 9 AM–3 PM, sat = off` for hours. None for other keys.
pub fn display(key: &str, v: &Value) -> Option<String> {
    match key {
        "keep_awake.start" | "keep_awake.end" => Some(clock(parse_time(v.as_str()?).ok()?)),
        "keep_awake.days" => {
            let mut on = [false; 7];
            for d in v.as_array()?.iter().filter_map(Value::as_str) {
                on[day_index(d)?] = true;
            }
            Some(days_words(&on))
        }
        "keep_awake.hours" => {
            let rules = v.as_array()?.iter().filter_map(Value::as_str);
            Some(rules.filter_map(|r| r.split_once(" = ").and_then(|(d, h)| Some(format!("{d} = {}", DayRule::parse(h).ok()?.words())))).collect::<Vec<_>>().join(", "))
        }
        _ => None,
    }
}

fn cap(d: &str) -> String {
    format!("{}{}", d[..1].to_uppercase(), &d[1..])
}

/// The one-off override for one local date: off for the rest of it, or on until `until`
/// (`HH:MM`; none = midnight) and the schedule after. `until_date` set means `until` is on that
/// later date (`for 5 hours` at 8 PM): on through midnight until then. Kept in midnad's state
/// and dropped once [`Today::ended`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Today {
    /// `YYYY-MM-DD`, local.
    pub date: String,
    pub on: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    /// `YYYY-MM-DD`, local: the next day, when `until` runs past midnight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until_date: Option<String>,
}

/// The longest an on override may run.
const MAX_SPAN: u32 = 24 * 60;

impl Today {
    /// `off for the rest of today`, `on until 5 PM today`, `on until 1 AM tomorrow`, `on for the
    /// rest of today`. `date` is the local date now.
    pub fn words(&self, date: &str) -> String {
        let day = if self.until_date.as_deref().is_some_and(|d| d != date) { "tomorrow" } else { "today" };
        match (self.on, self.until.as_deref()) {
            (false, _) => "off for the rest of today".into(),
            (true, Some(u)) => format!("on until {} {day}", clock(minutes(u, 0))),
            (true, None) => "on for the rest of today".into(),
        }
    }

    /// Whether it still applies on local `date`.
    pub fn covers(&self, date: &str) -> bool {
        self.date == date || self.until_date.as_deref() == Some(date)
    }

    /// Whether it's over at unix time `now`: its dates are past, or it's on and its `until` came.
    pub fn ended(&self, now: i64) -> bool {
        let date = local_date(now);
        let (.., h, mi, _) = time::local_parts(now);
        let last_day = self.until_date.as_deref().is_none_or(|d| d == date);
        !self.covers(&date) || (self.on && last_day && self.until.as_deref().is_some_and(|u| h * 60 + mi >= minutes(u, 0)))
    }
}

/// A length of time in minutes: `5h`, `5 hours`, `90 min`, `1h30m`, `1.5 hours`, `an hour`.
pub fn parse_span(v: &str) -> Result<u32, String> {
    let bad = || format!("`{}` isn't a length of time; use 5h, 90 min or 1h30m", v.trim());
    let s = v.trim().to_lowercase().replace("an hour", "1h").replace("a hour", "1h").replace(' ', "");
    let (mut total, mut parts) = (0f64, 0);
    let mut rest = s.as_str();
    while !rest.is_empty() {
        let n: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
        rest = &rest[n.len()..];
        let unit: String = rest.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
        rest = &rest[unit.len()..];
        let n: f64 = n.parse().map_err(|_| bad())?;
        let per = match unit.as_str() {
            "h" | "hr" | "hrs" | "hour" | "hours" => 60.,
            "m" | "min" | "mins" | "minute" | "minutes" => 1.,
            _ => return Err(bad()),
        };
        total += n * per;
        parts += 1;
    }
    if parts == 0 || total < 1. {
        return Err(bad());
    }
    Ok(total.round() as u32)
}

/// Read a today override: `off`, `on` (until midnight), `until 5pm`, `on until 5pm`, `5pm`,
/// `for 5 hours`, `5h`, `true`/`false`, `{"on": true, "until": "5pm"}`, `{"on": true, "for":
/// "5h"}`; `clear`, `normal` or null = none. `now` is the current local time (unix): the date it
/// applies to. An `until` at or before now is tomorrow's (`until 1am` at 8 PM), and a span ends
/// that long from now; either may run past midnight, up to 24 hours.
pub fn parse_today(v: &Value, now: i64) -> Result<Option<Today>, String> {
    let date = local_date(now);
    let (.., h, mi, _) = time::local_parts(now);
    let now_min = h * 60 + mi;
    // Minutes from now until the end.
    let make = |on: bool, ahead: Option<u32>| -> Result<Option<Today>, String> {
        let Some(ahead) = ahead.filter(|_| on) else {
            return Ok(Some(Today { date: date.clone(), on, until: None, until_date: None }));
        };
        if ahead > MAX_SPAN {
            return Err("keep-awake can be turned on for 24 hours at most".into());
        }
        let end = now + i64::from(ahead) * 60;
        let (.., eh, emi, _) = time::local_parts(end);
        let end_date = local_date(end);
        let (until, until_date) = match (end_date == date, eh * 60 + emi) {
            (true, m) => (Some(hhmm(m)), None),
            // Midnight tonight: the rest of today.
            (false, 0) if local_date(end - 60) == date => (None, None),
            (false, m) => (Some(hhmm(m)), Some(end_date)),
        };
        Ok(Some(Today { date: date.clone(), on, until, until_date }))
    };
    let until = |u: &str| -> Result<Option<u32>, String> {
        Ok(match parse_time(u)? {
            m if m > now_min => Some(m - now_min),
            m => Some(m + 1440 - now_min),
        })
    };
    match v {
        Value::Null => Ok(None),
        Value::Bool(b) => make(*b, None),
        Value::Object(o) => {
            let off = o.get("off").and_then(Value::as_bool) == Some(true);
            let on = match o.get("on") {
                Some(Value::Bool(b)) => *b,
                None | Some(Value::Null) => !off,
                Some(x) => return Err(format!("today.on expects true or false, not {x}")),
            };
            let ahead = match (o.get("until").and_then(Value::as_str).map(str::trim).filter(|u| !u.is_empty()), o.get("for").and_then(Value::as_str)) {
                (Some(_), Some(_)) => return Err("today takes until or for, not both".into()),
                (Some(u), None) => until(u)?,
                (None, Some(f)) => Some(parse_span(f)?),
                (None, None) => None,
            };
            make(on && !off, ahead)
        }
        Value::String(s) => {
            let t = s.trim().to_lowercase();
            match t.as_str() {
                "" | "clear" | "normal" | "none" | "default" | "schedule" | "reset" => Ok(None),
                "off" | "false" | "no" | "sleep" => make(false, None),
                "on" | "true" | "yes" | "all day" | "rest of day" | "until midnight" | "on until midnight" => make(true, None),
                _ => {
                    let rest = t.trim_start_matches("on").trim();
                    let span = rest.trim_start_matches("for").trim().trim_start_matches("the").trim().trim_start_matches("next").trim();
                    match parse_span(span) {
                        Ok(m) => make(true, Some(m)),
                        Err(_) if rest.starts_with("for") => Err(parse_span(span).unwrap_err()),
                        Err(_) => make(true, until(rest.trim_start_matches("until").trim())?),
                    }
                }
            }
        }
        x => Err(format!("today expects off, on, until <time>, for <hours> or clear, not {x}")),
    }
}

/// `YYYY-MM-DD` of unix time `t`, local.
pub fn local_date(t: i64) -> String {
    let (y, m, d, ..) = time::local_parts(t);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Why the window is closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Closed {
    OutsideHours,
    DayOff,
    /// The today override turned it off.
    TodayOff,
}

/// Whether the window is open at local `date` (`YYYY-MM-DD`), weekday `wd` (0 = mon), minute `m`.
pub fn open_local(plan: &Plan, today: Option<&Today>, date: &str, wd: usize, m: u32) -> Result<(), Closed> {
    if let Some(t) = today.filter(|t| t.covers(date)) {
        if !t.on {
            return Err(Closed::TodayOff);
        }
        // On until a time (today's, or past midnight on until_date); the schedule after it.
        let runs_past_today = t.date == date && t.until_date.is_some();
        if runs_past_today || t.until.as_deref().is_none_or(|u| m < minutes(u, 0)) {
            return Ok(());
        }
    }
    let own = plan.span(wd);
    if let Some((s, len)) = own
        && m >= s
        && m < s + len
    {
        return Ok(());
    }
    // Yesterday's window running past midnight.
    if let Some((s, len)) = plan.span((wd + 6) % 7)
        && m + 1440 < s + len
    {
        return Ok(());
    }
    Err(if own.is_none() { Closed::DayOff } else { Closed::OutsideHours })
}

/// Whether the window is open at unix time `t`.
pub fn open_at(plan: &Plan, today: Option<&Today>, t: i64) -> Result<(), Closed> {
    let (y, mo, d, h, mi, wd) = time::local_parts(t);
    open_local(plan, today, &format!("{y:04}-{mo:02}-{d:02}"), (wd as usize + 6) % 7, h * 60 + mi)
}

/// The next time after `now` (unix) the window opens or closes, within 8 days. None when it
/// never changes (all day every day).
pub fn next_change(plan: &Plan, today: Option<&Today>, now: i64) -> Option<i64> {
    let was = open_at(plan, today, now).is_ok();
    let mut marks: Vec<u32> = vec![0, plan.start, plan.end];
    for wd in 0..7 {
        if let Some((s, len)) = plan.span(wd) {
            marks.extend([s, (s + len) % 1440]);
        }
    }
    if let Some(u) = today.and_then(|t| t.until.as_deref()) {
        marks.push(minutes(u, 0));
    }
    marks.sort_unstable();
    marks.dedup();
    let (y, mo, d, ..) = time::local_parts(now);
    let noon = time::local_unix(y, mo, d, 12, 0);
    for k in 0..=8 {
        let (y, mo, d, ..) = time::local_parts(noon + k * 86_400);
        for m in &marks {
            let t = time::local_unix(y, mo, d, m / 60, m % 60);
            if t > now && open_at(plan, today, t).is_ok() != was {
                return Some(t);
            }
        }
    }
    None
}

// ------------------------------------------------------------------ battery

/// The battery as macOS reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Battery {
    pub percent: u8,
    /// Plugged in (charging or full).
    pub on_ac: bool,
}

/// `keep_awake.min_battery` with hysteresis: low below `min` on battery, and stays low until
/// `min + BATTERY_HYSTERESIS` or AC. No battery (a desktop Mac) or `min` 0 is never low.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BatteryGate {
    pub low: bool,
}

impl BatteryGate {
    /// Fold in a reading; true when the battery allows keeping awake.
    pub fn update(&mut self, battery: Option<Battery>, min: u8) -> bool {
        self.low = match battery {
            Some(b) if min > 0 && !b.on_ac => b.percent < if self.low { min.saturating_add(BATTERY_HYSTERESIS) } else { min },
            _ => false,
        };
        !self.low
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(start: &str, end: &str, days: &str, hours: &[&str]) -> Plan {
        let days = parse_days(&json!(days)).unwrap();
        let hours: Vec<String> = serde_json::from_value(coerce_hours(&json!(hours)).unwrap()).unwrap();
        Plan::from_settings(&hhmm(parse_time(start).unwrap()), &hhmm(parse_time(end).unwrap()), &days, &hours)
    }

    /// 2026-10-05 is a Monday.
    fn at(day: u32, h: u32, m: u32) -> i64 {
        time::local_unix(2026, 10, day, h, m)
    }

    #[test]
    fn times_read_like_people_write_them() {
        assert_eq!(parse_time("8am"), Ok(480));
        assert_eq!(parse_time("8 AM"), Ok(480));
        assert_eq!(parse_time("08:00"), Ok(480));
        assert_eq!(parse_time("8:30pm"), Ok(20 * 60 + 30));
        assert_eq!(parse_time("8.30 p.m."), Ok(20 * 60 + 30));
        assert_eq!(parse_time("0830"), Ok(510));
        assert_eq!(parse_time("12am"), Ok(0));
        assert_eq!(parse_time("12pm"), Ok(720));
        assert_eq!(parse_time("noon"), Ok(720));
        assert_eq!(parse_time("24:00"), Ok(0));
        for bad in ["25:00", "13pm", "8:75", "soon", "", "0am"] {
            assert!(parse_time(bad).is_err(), "{bad}");
        }
        assert_eq!(hhmm(480), "08:00");
        assert_eq!(clock(480), "8 AM");
        assert_eq!(clock(15 * 60 + 30), "3:30 PM");
        assert_eq!(clock(0), "12 AM");
    }

    #[test]
    fn days_take_names_ranges_and_lists() {
        assert_eq!(parse_days(&json!("weekdays")).unwrap(), ["mon", "tue", "wed", "thu", "fri"]);
        assert_eq!(parse_days(&json!("mon-fri")).unwrap().len(), 5);
        assert_eq!(parse_days(&json!("fri-mon")).unwrap(), ["mon", "fri", "sat", "sun"]);
        assert_eq!(parse_days(&json!("weekends")).unwrap(), ["sat", "sun"]);
        assert_eq!(parse_days(&json!("Tuesday, thursday")).unwrap(), ["tue", "thu"]);
        assert_eq!(parse_days(&json!(["sun", "mon"])).unwrap(), ["mon", "sun"]);
        assert_eq!(parse_days(&json!("weekdays sat")).unwrap().len(), 6);
        assert_eq!(parse_days(&json!("every day")).unwrap().len(), 7);
        // What the app shows reads back.
        for days in ["weekdays", "every day", "Mon, Wed, Fri", "weekends"] {
            let stored = json!(parse_days(&json!(days)).unwrap());
            assert_eq!(display("keep_awake.days", &stored).as_deref(), Some(days));
        }
        let hours = coerce_hours(&json!("fri = 9am-3pm, sat = off, sun = all day")).unwrap();
        let shown = display("keep_awake.hours", &hours).unwrap();
        assert_eq!(shown, "fri = 9 AM–3 PM, sat = off, sun = all day");
        assert_eq!(coerce_hours(&json!(shown)).unwrap(), hours);
        assert_eq!(display("keep_awake.start", &json!("20:30")).as_deref(), Some("8:30 PM"));
        assert!(parse_days(&json!("funday")).is_err());
        assert!(parse_days(&json!("")).is_err());
    }

    #[test]
    fn day_hours_normalize_and_later_rules_win() {
        assert_eq!(coerce_hours(&json!("fri = 9am-3pm, sat=off")).unwrap(), json!(["fri = 09:00-15:00", "sat = off"]));
        assert_eq!(coerce_hours(&json!(["mon-tue = 8am to 6pm", "tue = all day"])).unwrap(), json!(["mon = 08:00-18:00", "tue = all day"]));
        assert_eq!(coerce_hours(&json!({"sun": "off", "fri": "21:00–02:00"})).unwrap(), json!(["fri = 21:00-02:00", "sun = off"]));
        assert_eq!(coerce_hours(&json!(["fri = off", "fri = default"])).unwrap(), json!([]));
        assert_eq!(coerce_hours(&json!("")).unwrap(), json!([]));
        assert!(coerce_hours(&json!("fri")).is_err());
        assert!(coerce_hours(&json!("fri = later")).is_err());
        assert!(coerce_hours(&json!("someday = off")).is_err());
    }

    #[test]
    fn window_edges() {
        let p = plan("9am", "6pm", "weekdays", &[]);
        assert_eq!(open_at(&p, None, at(5, 8, 59)), Err(Closed::OutsideHours));
        assert_eq!(open_at(&p, None, at(5, 9, 0)), Ok(()), "start is inside");
        assert_eq!(open_at(&p, None, at(5, 17, 59)), Ok(()));
        assert_eq!(open_at(&p, None, at(5, 18, 0)), Err(Closed::OutsideHours), "end is outside");
        assert_eq!(open_at(&p, None, at(10, 12, 0)), Err(Closed::DayOff), "Saturday");
        assert_eq!(p.describe(), "9 AM–6 PM weekdays");
    }

    #[test]
    fn overnight_windows_run_into_the_next_day() {
        let p = plan("10pm", "2am", "mon", &[]);
        assert_eq!(open_at(&p, None, at(5, 21, 59)), Err(Closed::OutsideHours));
        assert_eq!(open_at(&p, None, at(5, 23, 0)), Ok(()));
        assert_eq!(open_at(&p, None, at(6, 1, 59)), Ok(()), "Tuesday morning is Monday's night");
        assert_eq!(open_at(&p, None, at(6, 2, 0)), Err(Closed::DayOff));
        assert_eq!(open_at(&p, None, at(5, 1, 0)), Err(Closed::OutsideHours), "Sunday night wasn't on");
        // start = end is all day.
        let all = plan("0:00", "0:00", "sat", &[]);
        assert_eq!(open_at(&all, None, at(10, 0, 0)), Ok(()));
        assert_eq!(open_at(&all, None, at(10, 23, 59)), Ok(()));
        assert_eq!(open_at(&all, None, at(11, 0, 0)), Err(Closed::DayOff));
    }

    #[test]
    fn a_day_can_have_its_own_hours_or_be_off() {
        let p = plan("9am", "6pm", "weekdays", &["fri = 9am-3pm", "wed = off", "sat = 10am-noon"]);
        assert_eq!(open_at(&p, None, at(9, 14, 59)), Ok(()));
        assert_eq!(open_at(&p, None, at(9, 15, 0)), Err(Closed::OutsideHours));
        assert_eq!(open_at(&p, None, at(7, 12, 0)), Err(Closed::DayOff));
        assert_eq!(open_at(&p, None, at(10, 11, 0)), Ok(()), "a day listed in hours is on");
        assert_eq!(p.describe(), "9 AM–6 PM weekdays; Wed off; Fri 9 AM–3 PM; Sat 10 AM–12 PM");
    }

    #[test]
    fn today_overrides_replace_the_rest_of_today_and_expire_at_midnight() {
        let p = plan("9am", "6pm", "weekdays", &[]);
        let now = at(5, 10, 0);
        let off = parse_today(&json!("off"), now).unwrap();
        assert_eq!(open_at(&p, off.as_ref(), at(5, 12, 0)), Err(Closed::TodayOff));
        assert_eq!(open_at(&p, off.as_ref(), at(6, 10, 0)), Ok(()), "the next day is back to normal");
        let late = parse_today(&json!("until 8pm"), now).unwrap();
        assert_eq!(late.as_ref().unwrap().until.as_deref(), Some("20:00"));
        assert_eq!(open_at(&p, late.as_ref(), at(5, 19, 0)), Ok(()));
        assert_eq!(open_at(&p, late.as_ref(), at(5, 20, 0)), Err(Closed::OutsideHours), "then the schedule");
        let early = parse_today(&json!({"on": true, "until": "3pm"}), now).unwrap();
        assert_eq!(open_at(&p, early.as_ref(), at(5, 16, 0)), Ok(()), "after on until, the schedule");
        assert!(!early.as_ref().unwrap().ended(at(5, 14, 59)) && early.as_ref().unwrap().ended(at(5, 15, 0)));
        assert_eq!(open_at(&p, late.as_ref(), at(5, 21, 0)), Err(Closed::OutsideHours), "the schedule is closed at 9 PM");
        assert!(!off.as_ref().unwrap().ended(at(5, 23, 59)) && off.as_ref().unwrap().ended(at(6, 0, 0)), "off lasts the day");
        let sat = parse_today(&json!("on"), at(10, 8, 0)).unwrap();
        assert_eq!(open_at(&p, sat.as_ref(), at(10, 23, 59)), Ok(()), "a day off can be turned on");
        assert_eq!(open_at(&p, sat.as_ref(), at(11, 0, 0)), Err(Closed::DayOff), "until midnight");
        assert_eq!(parse_today(&json!("clear"), now), Ok(None));
        assert_eq!(parse_today(&Value::Null, now), Ok(None));
        assert!(parse_today(&json!("whenever"), now).is_err());
        assert_eq!(parse_today(&json!(false), now).unwrap().unwrap().on, false);
    }

    #[test]
    fn an_override_can_run_past_midnight() {
        let p = plan("9am", "6pm", "weekdays", &[]);
        // Monday 8 PM, on for the next 5 hours: until 1 AM Tuesday, then Tuesday's schedule.
        let now = at(5, 20, 0);
        let five = parse_today(&json!("for the next 5 hours"), now).unwrap().unwrap();
        assert_eq!((five.until.as_deref(), five.until_date.as_deref()), (Some("01:00"), Some("2026-10-06")));
        assert_eq!(five.words("2026-10-05"), "on until 1 AM tomorrow");
        assert_eq!(five.words("2026-10-06"), "on until 1 AM today");
        assert!(five.covers("2026-10-06") && !five.covers("2026-10-07"));
        assert!(!five.ended(at(5, 23, 0)) && !five.ended(at(6, 0, 59)) && five.ended(at(6, 1, 0)));
        let t = Some(&five);
        assert_eq!(open_at(&p, t, at(5, 23, 59)), Ok(()));
        assert_eq!(open_at(&p, t, at(6, 0, 30)), Ok(()), "past midnight");
        assert_eq!(open_at(&p, t, at(6, 1, 0)), Err(Closed::OutsideHours), "then the schedule");
        assert_eq!(open_at(&p, t, at(6, 10, 0)), Ok(()));
        assert_eq!(next_change(&p, t, now), Some(at(6, 1, 0)));
        // The same said other ways.
        for v in [json!("5h"), json!("for 5 hours"), json!("until 1am"), json!("on until 1 AM"), json!({"for": "4h 60m"}), json!({"on": true, "until": "01:00"})] {
            assert_eq!(parse_today(&v, now).unwrap().as_ref(), Some(&five), "{v}");
        }
        // A time already past today is tomorrow's.
        let nine = parse_today(&json!("until 9am"), at(5, 10, 0)).unwrap().unwrap();
        assert_eq!(nine.until_date.as_deref(), Some("2026-10-06"));
        // Ending at midnight is the rest of today; a span inside today is an until.
        let four = parse_today(&json!("for 4 hours"), now).unwrap().unwrap();
        assert_eq!((four.until, four.until_date), (None, None));
        let short = parse_today(&json!("for 90 min"), now).unwrap().unwrap();
        assert_eq!((short.until.as_deref(), short.until_date), (Some("21:30"), None));
        assert!(parse_today(&json!("for 25 hours"), now).unwrap_err().contains("24 hours"));
        assert!(parse_today(&json!("for a while"), now).is_err());
        assert!(parse_today(&json!({"until": "1am", "for": "5h"}), now).is_err());
        assert_eq!(parse_span("1h30m"), Ok(90));
        assert_eq!(parse_span("1.5 hours"), Ok(90));
        assert_eq!(parse_span("an hour"), Ok(60));
        assert!(parse_span("5").is_err());
    }

    #[test]
    fn next_change_finds_the_next_edge() {
        let p = plan("9am", "6pm", "weekdays", &[]);
        assert_eq!(next_change(&p, None, at(5, 10, 0)), Some(at(5, 18, 0)));
        assert_eq!(next_change(&p, None, at(5, 7, 0)), Some(at(5, 9, 0)));
        assert_eq!(next_change(&p, None, at(9, 18, 30)), Some(at(12, 9, 0)), "Friday evening -> Monday");
        let night = plan("10pm", "2am", "mon", &[]);
        assert_eq!(next_change(&night, None, at(5, 23, 0)), Some(at(6, 2, 0)));
        let today = parse_today(&json!("until 8pm"), at(5, 10, 0)).unwrap();
        assert_eq!(next_change(&p, today.as_ref(), at(5, 19, 0)), Some(at(5, 20, 0)));
        assert_eq!(next_change(&p, today.as_ref(), at(5, 20, 0)), Some(at(6, 9, 0)));
        let always = plan("0:00", "0:00", "daily", &[]);
        assert_eq!(next_change(&always, None, at(5, 10, 0)), None);
    }

    #[test]
    fn battery_has_hysteresis() {
        let mut g = BatteryGate::default();
        let on_battery = |percent| Some(Battery { percent, on_ac: false });
        assert!(g.update(on_battery(25), 20));
        assert!(g.update(on_battery(20), 20), "at the line is fine");
        assert!(!g.update(on_battery(19), 20), "below it releases");
        assert!(!g.update(on_battery(22), 20), "not back until 5 points above");
        assert!(!g.update(on_battery(24), 20));
        assert!(g.update(on_battery(25), 20));
        assert!(!g.update(on_battery(10), 20));
        assert!(g.update(Some(Battery { percent: 10, on_ac: true }), 20), "plugged in");
        assert!(!g.update(on_battery(10), 20), "unplugged again");
        assert!(g.update(on_battery(10), 0), "0 = no limit");
        assert!(g.update(None, 20), "no battery");
    }
}
