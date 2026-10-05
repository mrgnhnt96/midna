//! Minimal RFC 3339 UTC timestamps (second precision) without a date crate.

pub fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

pub fn now_rfc3339() -> String {
    format_unix(now_unix())
}

// Howard Hinnant's civil-from-days / days-from-civil.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn format_unix(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Parse `YYYY-MM-DDTHH:MM:SS[.fff](Z|±HH:MM)` into unix seconds.
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20 {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (n(0..4)?, n(5..7)?, n(8..10)?, n(11..13)?, n(14..16)?, n(17..19)?);
    let mut i = 19;
    if b.get(i) == Some(&b'.') {
        i += 1;
        while b.get(i).is_some_and(|c| c.is_ascii_digit()) {
            i += 1;
        }
    }
    let off = match b.get(i)? {
        b'Z' | b'z' => 0,
        sign @ (b'+' | b'-') => {
            let oh = n(i + 1..i + 3)?;
            let om = n(i + 4..i + 6)?;
            let o = oh * 3600 + om * 60;
            if *sign == b'+' { o } else { -o }
        }
        _ => return None,
    };
    Some(days_from_civil(y, mo as u32, d as u32) * 86_400 + h * 3600 + mi * 60 + se - off)
}

/// Seconds east of UTC for the local timezone right now (via libc localtime_r).
pub fn local_offset_secs() -> i64 {
    local_offset_at(now_unix())
}

/// Seconds east of UTC for the local timezone at unix time `t` (follows DST changes).
pub fn local_offset_at(t: i64) -> i64 {
    unsafe extern "C" {
        fn localtime_r(t: *const i64, out: *mut Tm) -> *mut Tm;
    }
    #[repr(C)]
    struct Tm {
        sec: i32, min: i32, hour: i32, mday: i32, mon: i32, year: i32, wday: i32, yday: i32, isdst: i32,
        gmtoff: std::ffi::c_long,
        zone: *const std::ffi::c_char,
    }
    let mut tm: Tm = unsafe { std::mem::zeroed() };
    let r = unsafe { localtime_r(&t, &mut tm) };
    if r.is_null() { 0 } else { tm.gmtoff as i64 }
}

/// Local wall clock at unix time `t`: (year, month 1-12, day 1-31, hour, minute, weekday 0=Sunday).
pub fn local_parts(t: i64) -> (i64, u32, u32, u32, u32, u32) {
    let l = t + local_offset_at(t);
    let days = l.div_euclid(86_400);
    let rem = l.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    (y, m, d, (rem / 3600) as u32, (rem % 3600 / 60) as u32, (days + 4).rem_euclid(7) as u32)
}

/// Unix seconds of local midnight for the day containing `secs`.
pub fn local_day_start(secs: i64) -> i64 {
    let off = local_offset_secs();
    (secs + off).div_euclid(86_400) * 86_400 - off
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip() {
        for t in [0, 1_759_500_000, 951_782_400 /* 2000-02-29 */] {
            assert_eq!(parse_rfc3339(&format_unix(t)), Some(t));
        }
        assert_eq!(format_unix(0), "1970-01-01T00:00:00Z");
        assert_eq!(parse_rfc3339("1970-01-01T01:00:00+01:00"), Some(0));
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00.123Z"), Some(0));
    }
}
