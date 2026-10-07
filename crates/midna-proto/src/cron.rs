//! Five-field cron expressions in local time, for scheduled local triggers.
//!
//! `minute hour day-of-month month day-of-week`, each `*`, `n`, `a-b`, a step (`*/15`, `9-17/2`,
//! `5/10`) or a comma list of those. Months and weekdays take names (`jan`, `mon-fri`); weekday
//! 0 and 7 are Sunday. Like Vixie cron, when both day fields are restricted a day matching either
//! one counts. Shortcuts: `@hourly`, `@daily` (`@midnight`), `@weekly`, `@monthly`, `@yearly`
//! (`@annually`).
//!
//! `@every <duration>` (`@every 55m`, `@every 1h30m`, as in Go's robfig/cron) is a fixed interval
//! instead, for the ones cron can't say: `*/55` means minutes 0 and 55, not every 55 minutes. It
//! counts from an anchor (a schedule's `starts_at`), so its runs are `anchor + n × interval`
//! whenever the daemon was running, and don't drift after a missed one.
use crate::time;

const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
const DAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

/// How far ahead `next_after` looks (covers `0 0 29 2 *`).
const HORIZON_SECS: i64 = 5 * 366 * 86_400;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cron {
    minutes: u64,
    hours: u64,
    days: u64,
    months: u64,
    weekdays: u64,
    /// Day-of-month / day-of-week were given (not `*`), for the either-day rule.
    days_set: bool,
    weekdays_set: bool,
    /// `@every`: the interval in seconds (whole minutes), counted from `anchor`. The fields
    /// above are unused.
    every: Option<i64>,
    /// When an `@every` counts from (a minute boundary); `Schedule` sets it from `starts_at`.
    anchor: i64,
}

impl Cron {
    pub fn parse(expr: &str) -> Result<Cron, String> {
        let expr = expr.trim();
        if let Some(d) = expr.get(..6).filter(|p| p.eq_ignore_ascii_case("@every")).map(|_| &expr[6..]) {
            let secs = parse_every(d).map_err(|e| format!("`{expr}`: {e}"))?;
            return Ok(Cron { minutes: 0, hours: 0, days: 0, months: 0, weekdays: 0, days_set: false, weekdays_set: false, every: Some(secs), anchor: 0 });
        }
        let expanded = match expr.to_ascii_lowercase().as_str() {
            "@hourly" => "0 * * * *",
            "@daily" | "@midnight" => "0 0 * * *",
            "@weekly" => "0 0 * * 0",
            "@monthly" => "0 0 1 * *",
            "@yearly" | "@annually" => "0 0 1 1 *",
            s if s.starts_with('@') => return Err(format!("unknown shortcut `{expr}` (use @hourly, @daily, @weekly, @monthly, @yearly or @every 55m)")),
            _ => expr,
        };
        let f: Vec<&str> = expanded.split_whitespace().collect();
        if f.len() != 5 {
            return Err(format!("`{expr}` has {} field{}; cron needs 5: minute hour day-of-month month day-of-week (e.g. `0 9 * * mon-fri`)", f.len(), if f.len() == 1 { "" } else { "s" }));
        }
        let weekdays = field(f[4], "day-of-week", 0, 7, &DAYS, 0)?;
        // 7 is Sunday too.
        let weekdays = (weekdays | (weekdays >> 7)) & 0x7f;
        Ok(Cron {
            minutes: field(f[0], "minute", 0, 59, &[], 0)?,
            hours: field(f[1], "hour", 0, 23, &[], 0)?,
            days: field(f[2], "day-of-month", 1, 31, &[], 0)?,
            months: field(f[3], "month", 1, 12, &MONTHS, 1)?,
            weekdays,
            days_set: !f[2].starts_with('*'),
            weekdays_set: !f[4].starts_with('*'),
            every: None,
            anchor: 0,
        })
    }

    /// The `@every` interval in seconds, if it is one.
    pub fn every(&self) -> Option<i64> {
        self.every
    }

    /// Count an `@every` from `t` (rounded up to the minute) instead of the unix epoch.
    pub fn anchored(mut self, t: i64) -> Cron {
        self.anchor = ceil_minute(t);
        self
    }

    /// Does the local minute containing unix time `t` match?
    pub fn matches(&self, t: i64) -> bool {
        if let Some(n) = self.every {
            let m = t.div_euclid(60) * 60;
            return m >= self.anchor && (m - self.anchor) % n == 0;
        }
        let (_, mo, d, h, mi, wd) = time::local_parts(t);
        self.day_ok(mo, d, wd) && bit(self.hours, h) && bit(self.minutes, mi)
    }

    fn day_ok(&self, month: u32, day: u32, weekday: u32) -> bool {
        if !bit(self.months, month) {
            return false;
        }
        let (dm, dw) = (bit(self.days, day), bit(self.weekdays, weekday));
        if self.days_set && self.weekdays_set { dm || dw } else { dm && dw }
    }

    /// The first matching minute strictly after `t` (unix seconds, on a minute boundary), if
    /// any within five years.
    pub fn next_after(&self, t: i64) -> Option<i64> {
        if let Some(n) = self.every {
            let m = t.div_euclid(60) * 60;
            return Some(if m < self.anchor { self.anchor } else { self.anchor + ((m - self.anchor) / n + 1) * n });
        }
        let mut at = t.div_euclid(60) * 60 + 60;
        let end = t + HORIZON_SECS;
        while at <= end {
            let (_, mo, d, h, mi, wd) = time::local_parts(at);
            if !self.day_ok(mo, d, wd) || !bit(self.hours, h) {
                // Jump to the next local hour (DST moves whole hours, so this never skips one).
                at += i64::from(60 - mi) * 60;
                continue;
            }
            if bit(self.minutes, mi) {
                return Some(at);
            }
            at += 60;
        }
        None
    }

    /// The next `n` run times after `t`.
    pub fn upcoming(&self, t: i64, n: usize) -> Vec<i64> {
        let mut out = vec![];
        let mut at = t;
        while out.len() < n {
            let Some(next) = self.next_after(at) else { break };
            out.push(next);
            at = next;
        }
        out
    }
}

/// `Mon Oct 6 09:30` in local time.
pub fn local_label(t: i64) -> String {
    let (_, mo, d, h, mi, wd) = time::local_parts(t);
    let cap = |x: &str| format!("{}{}", x[..1].to_ascii_uppercase(), &x[1..]);
    format!("{} {} {d} {h:02}:{mi:02}", cap(DAYS[wd as usize]), cap(MONTHS[mo as usize - 1]))
}

fn ceil_minute(t: i64) -> i64 {
    t.div_euclid(60) * 60 + if t.rem_euclid(60) == 0 { 0 } else { 60 }
}

fn bit(set: u64, n: u32) -> bool {
    set & (1 << n) != 0
}

fn members(set: u64, lo: u32, hi: u32) -> Vec<u32> {
    (lo..=hi).filter(|n| bit(set, *n)).collect()
}

// ------------------------------------------------------------------ plain words

/// `9 AM`, `2:30 PM`, `12 PM` (noon), `12 AM` (midnight; also hour 24).
pub fn clock(h: u32, m: u32) -> String {
    let (h12, ap) = match h % 24 {
        0 => (12, "AM"),
        h @ 1..=11 => (h, "AM"),
        12 => (12, "PM"),
        h => (h - 12, "PM"),
    };
    if m == 0 { format!("{h12} {ap}") } else { format!("{h12}:{m:02} {ap}") }
}

/// `Oct 6` in local time.
pub fn local_day(t: i64) -> String {
    let (_, mo, d, ..) = time::local_parts(t);
    format!("{} {d}", cap(MONTHS[mo as usize - 1]))
}

fn cap(x: &str) -> String {
    format!("{}{}", x[..1].to_ascii_uppercase(), &x[1..])
}

/// The cron in plain words (`Every 5 min, 1 PM–5 PM, weekdays`, `Weekdays at 9 AM`,
/// `Oct 6 at 2:30 PM`), or the expression itself when it's too unusual to say simply.
pub fn describe(expr: &str) -> String {
    Cron::parse(expr).ok().and_then(|c| c.words()).unwrap_or_else(|| expr.trim().to_string())
}

impl Cron {
    fn words(&self) -> Option<String> {
        if let Some(n) = self.every {
            return Some(every_words(n));
        }
        let mins = members(self.minutes, 0, 59);
        let hours = members(self.hours, 0, 23);
        let all_months = self.months.count_ones() == 12;
        // One pinned date: `30 14 6 10 *`.
        let days = members(self.days, 1, 31);
        let months = members(self.months, 1, 12);
        if self.days_set && !self.weekdays_set && days.len() == 1 && months.len() == 1 && mins.len() == 1 && hours.len() == 1 {
            return Some(format!("{} {} at {}", cap(MONTHS[months[0] as usize - 1]), days[0], clock(hours[0], mins[0])));
        }
        if !all_months {
            return None;
        }
        let on = self.day_words()?;
        let step = |v: &[u32], span: u32| -> Option<u32> {
            let d = v.get(1)? - v[0];
            (v[0] == 0 && v.windows(2).all(|w| w[1] - w[0] == d) && v[v.len() - 1] + d == span).then_some(d)
        };
        // A run of whole hours (`13-16` → `1 PM–5 PM`).
        let span = (hours.len() < 24 && hours.windows(2).all(|w| w[1] == w[0] + 1)).then(|| format!("{}–{}", clock(hours[0], 0), clock(hours[hours.len() - 1] + 1, 0)));
        let join = |head: String, rest: &[Option<String>]| -> String {
            let mut parts = vec![head];
            parts.extend(rest.iter().flatten().filter(|s| !s.is_empty()).cloned());
            parts.join(", ")
        };
        let days_tail = (!on.is_empty()).then(|| on.clone());
        if mins.len() == 60 {
            if hours.len() == 24 {
                return Some(join("Every minute".into(), &[days_tail]));
            }
            return Some(join("Every minute".into(), &[Some(span?), days_tail]));
        }
        if let Some(n) = step(&mins, 60) {
            let head = format!("Every {n} min");
            if hours.len() == 24 {
                return Some(join(head, &[days_tail]));
            }
            return Some(join(head, &[Some(span?), days_tail]));
        }
        if mins.len() != 1 {
            return None;
        }
        let m = mins[0];
        if hours.len() == 24 {
            let head = if m == 0 { "Hourly".to_string() } else { format!("Hourly at :{m:02}") };
            return Some(join(head, &[days_tail]));
        }
        if let Some(n) = step(&hours, 24).filter(|_| hours.len() > 1) {
            let head = if m == 0 { format!("Every {n} hours") } else { format!("Every {n} hours at :{m:02}") };
            return Some(join(head, &[days_tail]));
        }
        if hours.len() > 4 {
            return Some(join(format!("Hourly at :{m:02}"), &[Some(span?), days_tail]));
        }
        let at: Vec<String> = hours.iter().map(|h| clock(*h, m)).collect();
        let lead = if on.is_empty() { "Daily".to_string() } else { cap(&on) };
        Some(format!("{lead} at {}", at.join(", ")))
    }

    /// `` (every day), `weekdays`, `weekends`, `Mon, Wed`, `on the 1st`, `on days 1, 15`.
    fn day_words(&self) -> Option<String> {
        let wd = self.weekdays & 0x7f;
        match (self.days_set, self.weekdays_set) {
            (false, false) => Some(String::new()),
            (false, true) => Some(match wd {
                0x7f => String::new(),
                0b011_1110 => "weekdays".into(),
                0b100_0001 => "weekends".into(),
                _ => {
                    // Monday first.
                    let order = [1, 2, 3, 4, 5, 6, 0];
                    order.iter().filter(|d| bit(wd, **d)).map(|d| cap(DAYS[*d as usize])).collect::<Vec<_>>().join(", ")
                }
            }),
            (true, false) => {
                let days = members(self.days, 1, 31);
                Some(match days.as_slice() {
                    [d] => format!("on the {}", ordinal(*d)),
                    _ => format!("on days {}", days.iter().map(u32::to_string).collect::<Vec<_>>().join(", ")),
                })
            }
            (true, true) => None,
        }
    }
}

/// `Every 55 min`, `Every 2 hours`, `Every 1 h 30 min`, `Every day`.
fn every_words(secs: i64) -> String {
    let m = secs / 60;
    let (d, h, mi) = (m / 1440, m / 60 % 24, m % 60);
    let unit = |n: i64, one: &str, many: &str| if n == 1 { one.to_string() } else { format!("{n} {many}") };
    match (d, h, mi) {
        (0, 0, _) => format!("Every {}", unit(mi, "minute", "min")),
        (0, _, 0) => format!("Every {}", unit(h, "hour", "hours")),
        (_, 0, 0) => format!("Every {}", unit(d, "day", "days")),
        (0, _, _) => format!("Every {h} h {mi} min"),
        _ => format!("Every {m} min"),
    }
}

/// An `@every` duration in seconds: Go-style `55m`, `1h30m`, `90s`, `2h`, `1d`; whole minutes,
/// at least one.
fn parse_every(d: &str) -> Result<i64, String> {
    let d = d.trim();
    let usage = "use a duration like 55m, 1h30m or 2h";
    if d.is_empty() {
        return Err(format!("@every needs a duration; {usage}"));
    }
    let (mut secs, mut num) = (0i64, String::new());
    for c in d.chars() {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        let per = match c.to_ascii_lowercase() {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            'd' => 86_400,
            _ => return Err(format!("`{d}` isn't a duration; {usage}")),
        };
        let n: i64 = num.parse().map_err(|_| format!("`{d}` isn't a duration; {usage}"))?;
        secs = n.checked_mul(per).and_then(|x| x.checked_add(secs)).ok_or_else(|| format!("`{d}` is too long"))?;
        num.clear();
    }
    if !num.is_empty() {
        return Err(format!("`{d}` needs a unit after {num} (s, m, h or d)"));
    }
    if secs < 60 || secs % 60 != 0 {
        return Err(format!("`{d}`: schedules run on whole minutes, at least 1m"));
    }
    if secs > HORIZON_SECS {
        return Err(format!("`{d}` is too long"));
    }
    Ok(secs)
}

fn ordinal(n: u32) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

// ------------------------------------------------------------------ schedules

/// `HH:MM` (24-hour) as minutes past midnight.
pub fn parse_hm(s: &str) -> Result<u32, String> {
    let bad = || format!("`{s}` isn't a time of day; use HH:MM, 24-hour (e.g. 13:00)");
    let (h, m) = s.trim().split_once(':').ok_or_else(bad)?;
    let (h, m): (u32, u32) = (h.parse().map_err(|_| bad())?, m.parse().map_err(|_| bad())?);
    if h > 23 || m > 59 || s.trim().len() > 5 {
        return Err(bad());
    }
    Ok(h * 60 + m)
}

/// A local `schedule` trigger's full rule: its cron, plus the optional time-of-day window,
/// start, end and run limit from its filter.
#[derive(Clone, Debug)]
pub struct Schedule {
    pub cron: Cron,
    /// Minutes past local midnight: from (inclusive), until (exclusive).
    window: Option<(u32, u32)>,
    starts: Option<i64>,
    ends: Option<i64>,
    max_runs: Option<u64>,
    /// Firings left before `max_runs` is reached.
    runs_left: Option<u64>,
}

impl Schedule {
    /// From a trigger's filter and how often it has fired. Errors name the bad field.
    pub fn of(f: &crate::TriggerFilter, fired: u64) -> Result<Schedule, String> {
        let cron = Cron::parse(f.cron.as_deref().ok_or("a schedule needs filter.cron")?).map_err(|e| format!("filter.cron: {e}"))?;
        let window = match &f.window {
            None => None,
            Some(w) => {
                let (a, b) = (parse_hm(&w.from).map_err(|e| format!("filter.window.from: {e}"))?, parse_hm(&w.until).map_err(|e| format!("filter.window.until: {e}"))?);
                if a == b {
                    return Err("filter.window: from and until are the same time".into());
                }
                Some((a, b))
            }
        };
        let at = |v: &Option<String>, k: &str| -> Result<Option<i64>, String> {
            v.as_deref().map(|s| time::parse_rfc3339(s).ok_or_else(|| format!("filter.{k}: `{s}` isn't an RFC 3339 time (e.g. 2026-10-06T13:00:00Z)"))).transpose()
        };
        let (starts, ends) = (at(&f.starts_at, "starts_at")?, at(&f.ends_at, "ends_at")?);
        if let (Some(s), Some(e)) = (starts, ends)
            && e <= s
        {
            return Err("filter.ends_at must be after filter.starts_at".into());
        }
        if f.max_runs == Some(0) {
            return Err("filter.max_runs must be at least 1".into());
        }
        // An `@every` counts from its start (`anchor_every` fills one in when a trigger is saved).
        let cron = match starts {
            Some(s) if cron.every.is_some() => cron.anchored(s),
            _ => cron,
        };
        Ok(Schedule { cron, window, starts, ends, max_runs: f.max_runs, runs_left: f.max_runs.map(|m| m.saturating_sub(fired)) })
    }

    fn in_window(&self, t: i64) -> bool {
        let Some((a, b)) = self.window else { return true };
        let (_, _, _, h, mi, _) = time::local_parts(t);
        let m = h * 60 + mi;
        if a < b { a <= m && m < b } else { m >= a || m < b }
    }

    /// Is minute `t` inside the start, end, window and run limit (whatever the cron says)?
    pub fn allows(&self, t: i64) -> bool {
        self.runs_left != Some(0) && self.starts.is_none_or(|s| t >= s) && self.ends.is_none_or(|e| t < e) && self.in_window(t)
    }

    /// Does the local minute containing `t` fire?
    pub fn matches(&self, t: i64) -> bool {
        self.cron.matches(t) && self.allows(t)
    }

    /// The next `n` firings after `t` (fewer when it ends sooner).
    pub fn upcoming(&self, t: i64, n: usize) -> Vec<i64> {
        let n = self.runs_left.map_or(n, |r| n.min(r as usize));
        let mut out = vec![];
        let mut at = match self.starts {
            Some(s) if s > t => s - 60,
            _ => t,
        };
        // Bounded: a one-minute window on an every-minute cron skips ~1440 minutes per run.
        let mut tries = 0;
        while out.len() < n && tries < 20_000 {
            tries += 1;
            let Some(next) = self.cron.next_after(at) else { break };
            if self.ends.is_some_and(|e| next >= e) {
                break;
            }
            if self.allows(next) {
                out.push(next);
            }
            at = next;
        }
        out
    }

    /// Done for good: past its end, out of runs, or never matching again.
    pub fn ended(&self, now: i64) -> bool {
        self.runs_left == Some(0) || self.ends.is_some_and(|e| e <= now) || self.upcoming(now, 1).is_empty()
    }

    /// Plain words for the whole rule: `Every 5 min, weekdays, 1 PM–5 PM · from Oct 7 · until
    /// Oct 10`, `Once, Oct 6 at 2:30 PM`.
    pub fn describe(&self, expr: &str) -> String {
        let once = self.max_runs == Some(1);
        let mut s = describe(expr);
        if let Some((a, b)) = self.window {
            s.push_str(&format!(", {}–{}", clock(a / 60, a % 60), clock(b / 60, b % 60)));
        }
        // An `@every`'s first run (its anchor) is just when it starts counting from.
        let now = time::now_unix();
        let anchor_only = |st: i64| self.cron.every.is_some_and(|n| st - now <= n);
        if let Some(st) = self.starts.filter(|st| *st > now && !once && !anchor_only(*st)) {
            s.push_str(&format!(" · from {}", local_day(st)));
        }
        if let Some(e) = self.ends {
            s.push_str(&format!(" · until {}", local_day(e)));
        }
        match self.max_runs {
            Some(1) => return format!("Once, {s}"),
            Some(n) => s.push_str(&format!(" · {n} runs")),
            None => {}
        }
        s
    }
}

/// Give an `@every` schedule without a start one: one interval after `created` (rounded up to
/// the minute), as robfig/cron runs one. Its runs then stay put across restarts and edits.
pub fn anchor_every(f: &mut crate::TriggerFilter, created: i64) {
    if f.starts_at.is_some() {
        return;
    }
    if let Some(n) = f.cron.as_deref().and_then(|c| Cron::parse(c).ok()).and_then(|c| c.every) {
        f.starts_at = Some(time::format_unix(ceil_minute(created) + n));
    }
}

/// One field as a bit set. `names[i]` stands for `i + name_base`.
fn field(s: &str, what: &str, lo: u32, hi: u32, names: &[&str], name_base: u32) -> Result<u64, String> {
    let num = |x: &str| -> Result<u32, String> {
        let x = x.to_ascii_lowercase();
        if let Some(i) = names.iter().position(|n| *n == x) {
            return Ok(i as u32 + name_base);
        }
        let n: u32 = x.parse().map_err(|_| format!("{what} `{s}`: `{x}` isn't a number{}", if names.is_empty() { String::new() } else { format!(" or a name ({}…)", names[..3].join(", ")) }))?;
        if n < lo || n > hi {
            return Err(format!("{what} `{s}`: {n} is outside {lo}-{hi}"));
        }
        Ok(n)
    };
    let mut set = 0u64;
    for part in s.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, st)) => {
                let st: u32 = st.parse().map_err(|_| format!("{what} `{s}`: step `{st}` isn't a number"))?;
                if st == 0 {
                    return Err(format!("{what} `{s}`: step must be at least 1"));
                }
                (r, st)
            }
            None => (part, 1),
        };
        let (a, b) = match range {
            "*" => (lo, hi),
            r => match r.split_once('-') {
                Some((a, b)) => (num(a)?, num(b)?),
                // `5/10` means 5, 15, 25, …
                None if step > 1 => (num(r)?, hi),
                None => {
                    let n = num(r)?;
                    (n, n)
                }
            },
        };
        if a > b {
            return Err(format!("{what} `{s}`: range {a}-{b} runs backwards"));
        }
        let mut n = a;
        while n <= b {
            set |= 1 << n;
            n += step;
        }
    }
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fields() {
        let c = Cron::parse("*/15 9-17 * * mon-fri").unwrap();
        assert_eq!(c.minutes, 1 << 0 | 1 << 15 | 1 << 30 | 1 << 45);
        assert_eq!(c.hours.count_ones(), 9);
        assert_eq!(c.weekdays, 0b0111110);
        assert!(!c.days_set && c.weekdays_set);
        assert_eq!(Cron::parse("0 0 * * 7").unwrap().weekdays, 1);
        assert_eq!(Cron::parse("5/20 * * * *").unwrap().minutes, 1 << 5 | 1 << 25 | 1 << 45);
        assert_eq!(Cron::parse("@daily").unwrap(), Cron::parse("0 0 * * *").unwrap());
        assert_eq!(Cron::parse("0 12 1,15 jan,JUL *").unwrap().months, 1 << 1 | 1 << 7);
    }

    #[test]
    fn rejects_bad_expressions() {
        for (e, why) in [
            ("* * * *", "4 fields"),
            ("60 * * * *", "outside 0-59"),
            ("* * 0 * *", "outside 1-31"),
            ("*/0 * * * *", "at least 1"),
            ("* 5-2 * * *", "backwards"),
            ("* * * * funday", "isn't a number or a name"),
            ("@often", "unknown shortcut"),
            ("@every", "needs a duration"),
            ("@every 55", "needs a unit"),
            ("@every 30s", "whole minutes"),
            ("@every 90s", "whole minutes"),
            ("@every 5 min", "isn't a duration"),
            ("@every 99999999999999h", "too long"),
        ] {
            let err = Cron::parse(e).unwrap_err();
            assert!(err.contains(why), "{e}: {err}");
        }
    }

    #[test]
    fn finds_next_runs() {
        // Compare in local time, whatever this machine's zone is.
        let base = time::parse_rfc3339("2026-10-05T12:00:00Z").unwrap();
        let c = Cron::parse("30 9 * * mon-fri").unwrap();
        let runs = c.upcoming(base, 6);
        assert_eq!(runs.len(), 6);
        for r in &runs {
            let (_, _, _, h, mi, wd) = time::local_parts(*r);
            assert_eq!((h, mi), (9, 30));
            assert!((1..=5).contains(&wd));
            assert!(c.matches(*r) && !c.matches(r + 60));
        }
        assert!(runs.windows(2).all(|w| w[1] - w[0] >= 23 * 3600));
        let every = Cron::parse("*/5 * * * *").unwrap();
        let n = every.next_after(base).unwrap();
        assert!(n > base && n - base <= 300 && time::local_parts(n).4 % 5 == 0);
        // Feb 29 is found years out; Feb 30 never.
        assert!(Cron::parse("0 0 29 2 *").unwrap().next_after(base).is_some());
        assert_eq!(Cron::parse("0 0 30 2 *").unwrap().next_after(base), None);
    }

    #[test]
    fn describes_in_plain_words() {
        for (e, want) in [
            ("*/5 13-16 * * 1-5", "Every 5 min, 1 PM–5 PM, weekdays"),
            ("*/30 * * * *", "Every 30 min"),
            ("0 9 * * mon-fri", "Weekdays at 9 AM"),
            ("57 8 * * *", "Daily at 8:57 AM"),
            ("30 14 6 10 *", "Oct 6 at 2:30 PM"),
            ("0 * * * *", "Hourly"),
            ("7 * * * *", "Hourly at :07"),
            ("0 */2 * * *", "Every 2 hours"),
            ("0 10 * * mon,wed", "Mon, Wed at 10 AM"),
            ("0 0 1 * *", "On the 1st at 12 AM"),
            ("0 9,17 * * sat,sun", "Weekends at 9 AM, 5 PM"),
            ("* * * * *", "Every minute"),
            ("0 0 1 jan *", "Jan 1 at 12 AM"),
            ("5,10 * * * *", "5,10 * * * *"),
            // Uneven steps leave a short gap at the top of the hour (day), so they aren't "every".
            ("*/55 * * * *", "*/55 * * * *"),
            ("*/7 * * * *", "*/7 * * * *"),
            ("0 */5 * * *", "0 */5 * * *"),
            ("@every 55m", "Every 55 min"),
            ("@every 1m", "Every minute"),
            ("@every 2h", "Every 2 hours"),
            ("@every 1h30m", "Every 1 h 30 min"),
            ("@every 1d", "Every day"),
            ("@every 25h", "Every 1500 min"),
            ("not a cron", "not a cron"),
        ] {
            assert_eq!(describe(e), want, "{e}");
        }
        assert_eq!((clock(0, 0), clock(12, 0), clock(23, 5), clock(24, 0)), ("12 AM".into(), "12 PM".into(), "11:05 PM".into(), "12 AM".into()));
    }

    fn filter(cron: &str) -> crate::TriggerFilter {
        crate::TriggerFilter { cron: Some(cron.into()), ..Default::default() }
    }

    #[test]
    fn schedules_honor_window_start_end_and_runs() {
        let at = |h, m| time::local_unix(2026, 10, 6, h, m);
        let mut f = filter("*/5 * * * *");
        f.window = Some(crate::TimeWindow { from: "13:00".into(), until: "17:00".into() });
        let s = Schedule::of(&f, 0).unwrap();
        assert!(s.matches(at(13, 0)) && s.matches(at(16, 55)) && !s.matches(at(17, 0)) && !s.matches(at(12, 55)));
        assert_eq!(s.upcoming(at(16, 50), 2), vec![at(16, 55), time::local_unix(2026, 10, 7, 13, 0)]);
        assert_eq!(s.describe("*/5 * * * *"), "Every 5 min, 1 PM–5 PM");
        // Past midnight.
        f.window = Some(crate::TimeWindow { from: "22:00".into(), until: "02:00".into() });
        let s = Schedule::of(&f, 0).unwrap();
        assert!(s.matches(at(23, 0)) && s.matches(at(1, 55)) && !s.matches(at(2, 0)) && !s.matches(at(12, 0)));
        // Starts and ends.
        f.window = None;
        f.starts_at = Some(time::format_unix(at(10, 2)));
        f.ends_at = Some(time::format_unix(at(10, 20)));
        let s = Schedule::of(&f, 0).unwrap();
        assert_eq!(s.upcoming(at(9, 0), 10), vec![at(10, 5), at(10, 10), at(10, 15)]);
        assert!(!s.matches(at(10, 0)) && !s.matches(at(10, 20)));
        assert!(s.ended(at(10, 20)) && !s.ended(at(10, 0)));
        // Run limits count what already fired.
        f.starts_at = None;
        f.ends_at = None;
        f.max_runs = Some(3);
        assert_eq!(Schedule::of(&f, 1).unwrap().upcoming(at(9, 0), 10).len(), 2);
        assert!(Schedule::of(&f, 3).unwrap().ended(at(9, 0)) && !Schedule::of(&f, 3).unwrap().matches(at(9, 5)));
        let mut once = filter("30 14 6 10 *");
        once.max_runs = Some(1);
        assert_eq!(Schedule::of(&once, 0).unwrap().describe("30 14 6 10 *"), "Once, Oct 6 at 2:30 PM");
    }

    #[test]
    fn rejects_bad_schedules() {
        let mut f = filter("*/5 * * * *");
        f.window = Some(crate::TimeWindow { from: "1pm".into(), until: "17:00".into() });
        assert!(Schedule::of(&f, 0).unwrap_err().contains("window.from"));
        f.window = Some(crate::TimeWindow { from: "13:00".into(), until: "13:00".into() });
        assert!(Schedule::of(&f, 0).unwrap_err().contains("same time"));
        f.window = None;
        f.starts_at = Some("2026-10-07T00:00:00Z".into());
        f.ends_at = Some("2026-10-06T00:00:00Z".into());
        assert!(Schedule::of(&f, 0).unwrap_err().contains("after"));
        f.ends_at = Some("tomorrow".into());
        assert!(Schedule::of(&f, 0).unwrap_err().contains("ends_at"));
        f.ends_at = None;
        f.max_runs = Some(0);
        assert!(Schedule::of(&f, 0).unwrap_err().contains("at least 1"));
        assert_eq!(parse_hm("09:30"), Ok(570));
        assert!(parse_hm("24:00").is_err() && parse_hm("9").is_err());
    }

    #[test]
    fn reads_local_times() {
        let t = time::local_unix(2026, 10, 6, 13, 30);
        assert_eq!(time::parse_local("2026-10-06 13:30"), Some(t));
        assert_eq!(time::parse_local("2026-10-06T13:30"), Some(t));
        assert_eq!(time::parse_local("2026-10-06"), Some(time::local_unix(2026, 10, 6, 0, 0)));
        assert_eq!(time::format_local(t), "2026-10-06 13:30");
        assert_eq!(time::parse_local(&time::format_unix(t)), Some(t));
        assert_eq!(time::parse_local("Oct 6"), None);
        assert_eq!(time::parse_local("2026-13-01"), None);
    }

    #[test]
    fn every_counts_from_its_start() {
        let at = |h, m| time::local_unix(2026, 10, 6, h, m);
        let c = Cron::parse("@every 55m").unwrap();
        assert_eq!(c.every(), Some(3300));
        assert_eq!(Cron::parse("@EVERY 1h30m").unwrap().every(), Some(5400));
        assert_eq!(Cron::parse("@every 90s90s").unwrap().every(), Some(180));
        let c = c.anchored(at(9, 0));
        assert_eq!(c.upcoming(at(8, 0), 4), vec![at(9, 0), at(9, 55), at(10, 50), at(11, 45)]);
        assert!(c.matches(at(10, 50)) && c.matches(at(10, 50) + 59) && !c.matches(at(10, 51)) && !c.matches(at(8, 5)));
        // A start with seconds counts from the next minute.
        assert_eq!(Cron::parse("@every 5m").unwrap().anchored(at(9, 0) + 1).next_after(at(8, 0)), Some(at(9, 1)));
        // Through a schedule: starts_at anchors it, and window, end and runs still apply.
        let mut f = filter("@every 55m");
        f.starts_at = Some(time::format_unix(at(9, 0)));
        f.window = Some(crate::TimeWindow { from: "09:00".into(), until: "12:00".into() });
        f.max_runs = Some(3);
        let s = Schedule::of(&f, 0).unwrap();
        assert_eq!(s.upcoming(at(8, 0), 10), vec![at(9, 0), at(9, 55), at(10, 50)]);
        assert!(s.matches(at(11, 45)) && !s.matches(at(12, 40)));
        assert_eq!(s.describe("@every 55m"), "Every 55 min, 9 AM–12 PM · 3 runs");
        // A saved trigger without a start runs one interval after it was made, then keeps its pace.
        let mut f = filter("@every 55m");
        anchor_every(&mut f, at(9, 0) + 20);
        assert_eq!(f.starts_at.as_deref().and_then(time::parse_rfc3339), Some(at(9, 56)));
        let before = f.starts_at.clone();
        anchor_every(&mut f, at(15, 0));
        assert_eq!(f.starts_at, before);
        let mut plain = filter("*/5 * * * *");
        anchor_every(&mut plain, at(9, 0));
        assert_eq!(plain.starts_at, None);
    }

    #[test]
    fn either_day_rule() {
        // The 1st of the month OR any Monday.
        let c = Cron::parse("0 0 1 * mon").unwrap();
        assert!(c.day_ok(10, 1, 4) && c.day_ok(10, 5, 1) && !c.day_ok(10, 6, 2));
        // Only one restricted: both must hold (the `*` one always does).
        let c = Cron::parse("0 0 * * mon").unwrap();
        assert!(!c.day_ok(10, 1, 4) && c.day_ok(10, 5, 1));
    }
}
