//! `midna queue …`: messages midnad types into a terminal once its agent is ready.
use crate::args::Args;
use crate::print::plain;
use crate::{Fail, OutFn, Res, call};
use midna_proto::time;
use serde_json::{Value, json};
use std::io::Read;

/// Flags that take a value under `midna queue` only (`restart --idle` is a boolean).
pub const VALUE_FLAGS: &[&str] = &["idle", "at", "after", "position", "text"];

const WHEN_FLAGS: &[&str] = &["idle", "at", "after", "when-idle"];

fn s(v: &Value, k: &str) -> String {
    plain(v.get(k).unwrap_or(&Value::Null))
}

/// Idle minutes: `10m`, `1h`, `1h30m`, or a bare number of minutes (`90`).
pub fn parse_minutes(v: &str) -> Result<u32, String> {
    let bad = || format!("`{v}` is not a duration (10m, 1h, 1h30m or minutes like 90)");
    let v = v.trim();
    if v.is_empty() {
        return Err(bad());
    }
    let total = if let Ok(n) = v.parse::<u32>() {
        n
    } else {
        let (mut total, mut num) = (0u32, String::new());
        for c in v.chars() {
            match c {
                '0'..='9' => num.push(c),
                'h' | 'm' => {
                    let n: u32 = num.parse().map_err(|_| bad())?;
                    total = total.checked_add(if c == 'h' { n.checked_mul(60).ok_or_else(bad)? } else { n }).ok_or_else(bad)?;
                    num.clear();
                }
                _ => return Err(bad()),
            }
        }
        if !num.is_empty() {
            return Err(bad());
        }
        total
    };
    if total == 0 { Err(format!("`{v}`: the duration must be at least a minute")) } else { Ok(total) }
}

/// `HH:MM` local time as unix seconds: today if it is still ahead of `now`, else tomorrow.
/// `offset` is seconds east of UTC.
pub fn hhmm_to_unix(v: &str, now: i64, offset: i64) -> Option<i64> {
    let (h, m) = v.split_once(':')?;
    if h.is_empty() || h.len() > 2 || m.len() != 2 {
        return None;
    }
    let (h, m): (i64, i64) = (h.parse().ok()?, m.parse().ok()?);
    if !(0..24).contains(&h) || !(0..60).contains(&m) {
        return None;
    }
    let day = (now + offset).div_euclid(86_400) * 86_400 - offset;
    let t = day + h * 3600 + m * 60;
    Some(if t <= now { t + 86_400 } else { t })
}

/// `--at`: RFC 3339, or `HH:MM` meaning the next time the local clock reads that. Returns RFC 3339 (UTC).
pub fn parse_at(v: &str, now: i64, offset: i64) -> Result<String, String> {
    if let Some(t) = time::parse_rfc3339(v) {
        return Ok(time::format_unix(t));
    }
    hhmm_to_unix(v, now, offset).map(time::format_unix).ok_or_else(|| format!("`{v}` is not a time (HH:MM, or RFC 3339 like 2026-10-05T18:00:00-06:00)"))
}

/// The `when` from `--idle`/`--at`/`--after`/`--when-idle` (None when none was given).
pub fn when_from(a: &Args, now: i64, offset: i64) -> Result<Option<Value>, Fail> {
    let given: Vec<&str> = WHEN_FLAGS.iter().copied().filter(|f| a.has(f)).collect();
    if given.len() > 1 {
        return Err(Fail::Usage(format!("pick one of --{}", given.join(", --"))));
    }
    let need = |f: &str| a.get(f).ok_or_else(|| Fail::Usage(format!("--{f} needs a value")));
    Ok(match given.first().copied() {
        None => None,
        Some("when-idle") => Some(json!({ "kind": "idle" })),
        Some("idle") => Some(json!({ "kind": "idle_for", "minutes": parse_minutes(need("idle")?).map_err(|e| Fail::Usage(format!("--idle {e}")))? })),
        Some("at") => Some(json!({ "kind": "at", "at": parse_at(need("at")?, now, offset).map_err(|e| Fail::Usage(format!("--at {e}")))? })),
        Some(_) => Some(json!({ "kind": "after", "session": need("after")? })),
    })
}

/// A 1-based CLI position as the RPC's 0-based index.
pub fn zero_based(v: &str, what: &str) -> Result<usize, Fail> {
    match v.parse::<usize>() {
        Ok(n) if n >= 1 => Ok(n - 1),
        _ => Err(Fail::Usage(format!("{what}: `{v}` is not a position (1 = first)"))),
    }
}

/// `-` reads stdin (one trailing newline dropped).
fn text_arg(t: String) -> Result<String, Fail> {
    if t != "-" {
        return Ok(t);
    }
    let mut b = String::new();
    std::io::stdin().read_to_string(&mut b).map_err(|e| Fail::Other(e.to_string()))?;
    if b.ends_with('\n') {
        b.pop();
        if b.ends_with('\r') {
            b.pop();
        }
    }
    Ok(b)
}

/// Local `HH:MM`, with the date when it is not today.
fn local_time(at: &str) -> String {
    let Some(t) = time::parse_rfc3339(at) else { return at.to_string() };
    let off = time::local_offset_secs();
    let local = time::format_unix(t + off);
    let today = time::format_unix(time::now_unix() + off);
    if local.get(..10) == today.get(..10) { local[11..16].to_string() } else { format!("{} {}", &local[..10], &local[11..16]) }
}

/// The `when` in words: "when idle", "after 10 min idle", "at 18:00", "after ab12cd34".
pub fn when_text(w: &Value) -> String {
    match w["kind"].as_str() {
        Some("idle_for") => format!("after {} min idle", plain(&w["minutes"])),
        Some("at") => format!("at {}", local_time(w["at"].as_str().unwrap_or(""))),
        Some("after") => format!("after {}", s(w, "session")),
        _ => "when idle".into(),
    }
}

fn who(a: &Value) -> String {
    match (a["kind"].as_str(), a["session"].as_str(), a["name"].as_str()) {
        (Some("trigger"), _, Some(n)) => format!("trigger {n}"),
        (Some(k), Some(sid), _) => format!("{k}:{sid}"),
        (Some(k), ..) => k.to_string(),
        _ => String::new(),
    }
}

fn first_line(t: &str) -> String {
    let mut lines = t.lines();
    let first = lines.next().unwrap_or("");
    let more = lines.next().is_some();
    if first.chars().count() > 70 {
        format!("{}…", first.chars().take(69).collect::<String>())
    } else if more {
        format!("{first} …")
    } else {
        first.to_string()
    }
}

fn state_text(m: &Value) -> String {
    match m["state"].as_str() {
        Some("failed") => match m["error"].as_str() {
            Some(e) => format!("failed: {e}"),
            None => "failed".into(),
        },
        Some(st) => st.to_string(),
        None => "waiting".into(),
    }
}

/// One message on one line (no number).
fn item_line(m: &Value) -> String {
    let mut extra = vec![];
    if m["enter"] == false {
        extra.push("no enter".to_string());
    }
    if let Some(n) = m["images"].as_array().map(Vec::len).filter(|n| *n > 0) {
        extra.push(format!("{n} image{}", if n == 1 { "" } else { "s" }));
    }
    let extra = if extra.is_empty() { String::new() } else { format!(" ({})", extra.join(", ")) };
    format!("{}  {}  {}  by {}  {}{extra}", s(m, "id"), state_text(m), when_text(&m["when"]), who(&m["by"]), first_line(m["text"].as_str().unwrap_or("")))
}

/// `queue.list` (and `clear`/`move`/`pause`, which return the same shape).
pub fn print_list(v: &Value) {
    let items = v["items"].as_array().cloned().unwrap_or_default();
    let paused = if v["paused"] == true { " (paused)" } else { "" };
    if items.is_empty() {
        println!("queue is empty in {}{paused}", s(v, "session"));
        return;
    }
    println!("queue for {}{paused}", s(v, "session"));
    let mut rows = vec![];
    let mut shown_wait = false;
    for (i, m) in items.iter().enumerate() {
        rows.push(vec![
            format!("{:>2}", i + 1),
            s(m, "id"),
            state_text(m),
            when_text(&m["when"]),
            format!("by {}", who(&m["by"])),
            format!(
                "{}{}{}",
                first_line(m["text"].as_str().unwrap_or("")),
                if m["enter"] == false { "  (no enter)" } else { "" },
                m["images"].as_array().filter(|a| !a.is_empty()).map(|a| format!("  (+{} image)", a.len())).unwrap_or_default()
            ),
        ]);
        if !shown_wait && m["state"].as_str().unwrap_or("waiting") == "waiting" {
            shown_wait = true;
            let w: Vec<String> = m["waiting_for"].as_array().map(|w| w.iter().map(plain).collect()).unwrap_or_default();
            let line = if v["paused"] == true {
                "paused (midna queue resume)".to_string()
            } else if w.is_empty() {
                "about to go".to_string()
            } else {
                w.join("; ")
            };
            rows.push(vec![String::new(), format!("   waiting for: {line}")]);
        }
    }
    crate::print::table(rows);
}

pub fn queue(a: &Args, out: OutFn) -> Res {
    let session = a.get("session");
    let verb = a.pos.get(1).map(String::as_str).unwrap_or("list");
    let item = |method: &str, done: &str| -> Res {
        a.check(&["session"])?;
        let id = a.need(2, "queued message id (q_…)")?;
        let v = call(method, json!({ "session": session, "id": id }))?;
        out(&v, &|_| println!("{done} {id}"));
        Ok(())
    };
    match verb {
        "list" | "ls" => {
            a.check(&["session"])?;
            out(&call("queue.list", json!({ "session": session }))?, &print_list);
        }
        "add" => {
            a.check(&["session", "idle", "at", "after", "when-idle", "no-enter", "image", "first", "position"])?;
            let text = text_arg(a.text(2))?;
            let cwd = std::env::current_dir().unwrap_or_default();
            let images: Vec<String> = a.all("image").iter().map(|p| cwd.join(p).display().to_string()).collect();
            if text.is_empty() && images.is_empty() {
                return Err(Fail::Usage("nothing to queue: give the text (or - to read stdin), or --image".into()));
            }
            let position = match (a.has("first"), a.get("position")) {
                (true, Some(_)) => return Err(Fail::Usage("pick one of --first, --position".into())),
                (true, None) => Some(0),
                (false, Some(p)) => Some(zero_based(p, "--position")?),
                (false, None) => None,
            };
            let mut p = json!({ "session": session, "text": text, "enter": !a.has("no-enter"), "images": images });
            if let Some(w) = when_from(a, time::now_unix(), time::local_offset_secs())? {
                p["when"] = w;
            }
            if let Some(pos) = position {
                p["position"] = json!(pos);
            }
            let v = call("queue.add", p)?;
            out(&v, &|v| println!("queued {}", item_line(v)));
        }
        "edit" | "update" => {
            a.check(&["session", "text", "idle", "at", "after", "when-idle", "enter", "no-enter", "retry"])?;
            let id = a.need(2, "queued message id (q_…)")?;
            let mut p = json!({ "session": session, "id": id, "retry": a.has("retry") });
            if let Some(t) = a.get("text") {
                p["text"] = json!(text_arg(t.to_string())?);
            }
            match (a.has("enter"), a.has("no-enter")) {
                (true, true) => return Err(Fail::Usage("pick one of --enter, --no-enter".into())),
                (true, false) => p["enter"] = json!(true),
                (false, true) => p["enter"] = json!(false),
                _ => {}
            }
            if let Some(w) = when_from(a, time::now_unix(), time::local_offset_secs())? {
                p["when"] = w;
            }
            if p.as_object().is_some_and(|o| o.len() == 3) && !a.has("retry") {
                return Err(Fail::Usage("nothing to change: give --text, --idle/--at/--after/--when-idle, --enter/--no-enter or --retry".into()));
            }
            let v = call("queue.update", p)?;
            out(&v, &|v| println!("updated {}", item_line(v)));
        }
        "rm" | "remove" => item("queue.remove", "removed")?,
        "send-now" | "send_now" => item("queue.send_now", "sent")?,
        "clear" => {
            a.check(&["session"])?;
            let v = call("queue.clear", json!({ "session": session }))?;
            out(&v, &|v| println!("cleared the queue in {}", s(v, "session")));
        }
        "mv" | "move" => {
            a.check(&["session"])?;
            let id = a.need(2, "queued message id (q_…)")?;
            let to = zero_based(a.need(3, "new position (1 = first)")?, "position")?;
            out(&call("queue.move", json!({ "session": session, "id": id, "to": to }))?, &print_list);
        }
        verb @ ("pause" | "resume") => {
            a.check(&["session"])?;
            let v = call("queue.pause", json!({ "session": session, "paused": verb == "pause" }))?;
            out(&v, &|v| println!("queue {} in {} ({} waiting)", if verb == "pause" { "paused" } else { "resumed" }, s(v, "session"), v["items"].as_array().map_or(0, Vec::len)));
        }
        other => return Err(Fail::Usage(format!("unknown `queue {other}`; see midna queue --help"))),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Args {
        Args::parse_with(v.iter().map(|s| s.to_string()), VALUE_FLAGS).unwrap()
    }

    #[test]
    fn minutes() {
        assert_eq!(parse_minutes("10m"), Ok(10));
        assert_eq!(parse_minutes("1h"), Ok(60));
        assert_eq!(parse_minutes("1h30m"), Ok(90));
        assert_eq!(parse_minutes("90"), Ok(90));
        for bad in ["", "0", "0m", "10s", "m", "1h30", "abc", "-5"] {
            assert!(parse_minutes(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn hhmm_is_today_or_tomorrow_local() {
        // 2026-10-05T12:00:00Z; local offset -06:00 → local 06:00.
        let now = time::parse_rfc3339("2026-10-05T12:00:00Z").unwrap();
        let off = -6 * 3600;
        assert_eq!(parse_at("18:00", now, off).unwrap(), "2026-10-06T00:00:00Z");
        // 05:30 local has passed: tomorrow.
        assert_eq!(parse_at("05:30", now, off).unwrap(), "2026-10-06T11:30:00Z");
        // Exactly now counts as passed.
        assert_eq!(parse_at("06:00", now, off).unwrap(), "2026-10-06T12:00:00Z");
        assert_eq!(parse_at("9:05", now, 0).unwrap(), "2026-10-06T09:05:00Z");
        assert_eq!(parse_at("13:00", now, 0).unwrap(), "2026-10-05T13:00:00Z");
        // East of UTC, across midnight: 23:00Z is 01:00 on the 6th at +02:00.
        let late = time::parse_rfc3339("2026-10-05T23:00:00Z").unwrap();
        assert_eq!(parse_at("02:00", late, 2 * 3600).unwrap(), "2026-10-06T00:00:00Z");
        assert_eq!(parse_at("00:30", late, 2 * 3600).unwrap(), "2026-10-06T22:30:00Z");
        // RFC 3339 passes through (normalized to UTC).
        assert_eq!(parse_at("2026-10-05T18:00:00-06:00", now, off).unwrap(), "2026-10-06T00:00:00Z");
        for bad in ["24:00", "12:60", "1200", "12:5", "noon", ""] {
            assert!(parse_at(bad, now, off).is_err(), "{bad}");
        }
    }

    #[test]
    fn when_json() {
        let now = time::parse_rfc3339("2026-10-05T12:00:00Z").unwrap();
        let w = |v: &[&str]| when_from(&args(v), now, 0);
        assert_eq!(w(&["queue", "add", "hi"]).ok().unwrap(), None);
        assert_eq!(w(&["queue", "add", "hi", "--when-idle"]).ok().unwrap(), Some(json!({ "kind": "idle" })));
        assert_eq!(w(&["queue", "add", "hi", "--idle", "10m"]).ok().unwrap(), Some(json!({ "kind": "idle_for", "minutes": 10 })));
        assert_eq!(w(&["queue", "add", "hi", "--idle=2h"]).ok().unwrap(), Some(json!({ "kind": "idle_for", "minutes": 120 })));
        assert_eq!(w(&["queue", "add", "hi", "--at", "18:00"]).ok().unwrap(), Some(json!({ "kind": "at", "at": "2026-10-05T18:00:00Z" })));
        assert_eq!(w(&["queue", "add", "hi", "--after", "ab12cd34"]).ok().unwrap(), Some(json!({ "kind": "after", "session": "ab12cd34" })));
        assert!(matches!(w(&["queue", "add", "hi", "--idle", "10m", "--after", "x"]), Err(Fail::Usage(_))));
        assert!(matches!(w(&["queue", "add", "hi", "--idle", "soon"]), Err(Fail::Usage(_))));
        assert!(matches!(w(&["queue", "add", "hi", "--at", "later"]), Err(Fail::Usage(_))));
        // The value flags take their value, so the text stays positional.
        let a = args(&["queue", "add", "--idle", "10m", "run", "the", "tests"]);
        assert_eq!(a.text(2), "run the tests");
    }

    #[test]
    fn positions_are_one_based() {
        assert_eq!(zero_based("1", "to").ok(), Some(0));
        assert_eq!(zero_based("3", "to").ok(), Some(2));
        assert!(zero_based("0", "to").is_err());
        assert!(zero_based("x", "to").is_err());
    }

    #[test]
    fn when_words() {
        assert_eq!(when_text(&json!({ "kind": "idle" })), "when idle");
        assert_eq!(when_text(&json!({ "kind": "idle_for", "minutes": 10 })), "after 10 min idle");
        assert_eq!(when_text(&json!({ "kind": "after", "session": "ab12cd34" })), "after ab12cd34");
        assert!(when_text(&json!({ "kind": "at", "at": "2020-01-02T03:04:05Z" })).starts_with("at 2020-01-0"));
        assert_eq!(first_line("one\ntwo"), "one …");
    }
}
