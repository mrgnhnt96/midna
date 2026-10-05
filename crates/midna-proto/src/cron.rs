//! Five-field cron expressions in local time, for scheduled local triggers.
//!
//! `minute hour day-of-month month day-of-week`, each `*`, `n`, `a-b`, a step (`*/15`, `9-17/2`,
//! `5/10`) or a comma list of those. Months and weekdays take names (`jan`, `mon-fri`); weekday
//! 0 and 7 are Sunday. Like Vixie cron, when both day fields are restricted a day matching either
//! one counts. Shortcuts: `@hourly`, `@daily` (`@midnight`), `@weekly`, `@monthly`, `@yearly`
//! (`@annually`).
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
}

impl Cron {
    pub fn parse(expr: &str) -> Result<Cron, String> {
        let expr = expr.trim();
        let expanded = match expr.to_ascii_lowercase().as_str() {
            "@hourly" => "0 * * * *",
            "@daily" | "@midnight" => "0 0 * * *",
            "@weekly" => "0 0 * * 0",
            "@monthly" => "0 0 1 * *",
            "@yearly" | "@annually" => "0 0 1 1 *",
            s if s.starts_with('@') => return Err(format!("unknown shortcut `{expr}` (use @hourly, @daily, @weekly, @monthly or @yearly)")),
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
        })
    }

    /// Does the local minute containing unix time `t` match?
    pub fn matches(&self, t: i64) -> bool {
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

fn bit(set: u64, n: u32) -> bool {
    set & (1 << n) != 0
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
    fn either_day_rule() {
        // The 1st of the month OR any Monday.
        let c = Cron::parse("0 0 1 * mon").unwrap();
        assert!(c.day_ok(10, 1, 4) && c.day_ok(10, 5, 1) && !c.day_ok(10, 6, 2));
        // Only one restricted: both must hold (the `*` one always does).
        let c = Cron::parse("0 0 * * mon").unwrap();
        assert!(!c.day_ok(10, 1, 4) && c.day_ok(10, 5, 1));
    }
}
