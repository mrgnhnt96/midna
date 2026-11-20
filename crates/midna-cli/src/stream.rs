//! `midna stream …`: local event streams. `publish` hands midna an event from this Mac
//! (`streams.publish`); local triggers on `stream.<name>.<kind>` react to it. `events` lists a
//! stream's recent publishes and what midna did with each (`streams.events`), and `--replay`
//! runs one of them through the local triggers again (`trigger.replay`).
use crate::args::Args;
use crate::{Fail, OutFn, Res, call};
use serde_json::{Value, json};
use std::io::{IsTerminal, Read};

pub fn stream(a: &Args, out: OutFn) -> Res {
    match a.pos.get(1).map(String::as_str) {
        Some("publish" | "pub") => publish(a, out),
        Some("events" | "log") => events(a, out),
        Some(other) => Err(Fail::Usage(format!("unknown stream subcommand `{other}` (try `midna stream publish <name> <kind>` or `midna stream events <name>`)"))),
        None => Err(Fail::Usage("midna stream publish <name> <kind> [--id ID] [--data JSON|@file|-] | events <name> [--limit N] [--replay SEQ|ID]".into())),
    }
}

fn publish(a: &Args, out: OutFn) -> Res {
    a.check(&["id", "data", "json"])?;
    let name = a.need(2, "stream name (e.g. slack)")?;
    let kind = a.need(3, "event kind (e.g. message)")?;
    let data = payload(a.get("data"), std::io::stdin().is_terminal())?;
    let v = call("streams.publish", json!({ "stream": name, "kind": kind, "id": a.get("id"), "data": data }))?;
    out(&v, &|v| {
        match v["verdict"].as_str() {
            Some("duplicate") => println!("duplicate: {} already took id {} as event {}; not published again", name, a.get("id").unwrap_or(""), v["seq"]),
            _ => println!("published {} (event {})", v["event"].as_str().unwrap_or(""), v["seq"]),
        }
        if let Some(t) = v["caused_by"]["trigger"].as_str() {
            println!("trigger {t} started this publisher, so the event fires no triggers");
        }
    });
    Ok(())
}

fn events(a: &Args, out: OutFn) -> Res {
    a.check(&["limit", "json", "replay"])?;
    let name = a.need(2, "stream name (e.g. slack)")?;
    if let Some(event) = a.get("replay") {
        // Run a recorded event (its seq or id) through the local triggers again.
        let v = call("trigger.replay", json!({ "stream": name, "event": event }))?;
        out(&v, &crate::triggers::print_delivery);
        return Ok(());
    }
    let v = call("streams.events", json!({ "stream": name, "limit": a.num::<usize>("limit")? }))?;
    out(&v, &|v| print!("{}", events_text(v)));
    Ok(())
}

/// The stream's counts, then one line per publish, newest first.
fn events_text(v: &Value) -> String {
    let c = &v["counts"];
    let n = |k: &str| c[k].as_u64().unwrap_or(0);
    let name = v["stream"].as_str().unwrap_or("");
    let rows = v["events"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        return format!("stream {name}: no publishes yet\n");
    }
    let drops: Vec<String> = ["rate_limited", "too_large", "busy", "refused"].iter().filter(|k| n(k) > 0).map(|k| format!("{} {k}", n(k))).collect();
    let dropped = if drops.is_empty() { String::new() } else { format!(", dropped {}", drops.join(", ")) };
    let mut s = format!("stream {name}: {} accepted, {} duplicate{dropped} since midnad started\n", n("accepted"), n("duplicate"));
    for e in &rows {
        let id = e["id"].as_str().map(|i| format!("id {i}")).unwrap_or_default();
        let verdict = e["verdict"].as_str().unwrap_or("");
        let what = match (verdict, e["seq"].as_u64(), e["detail"].as_str()) {
            ("accepted", Some(seq), _) => format!("event {seq}"),
            (_, _, Some(d)) => d.to_string(),
            _ => String::new(),
        };
        let line = format!("  {:<8} {:<16} {:<20} {:<12} {what}", crate::guide::ago(e["at"].as_str().unwrap_or("")), e["kind"].as_str().unwrap_or(""), id, verdict);
        s.push_str(line.trim_end());
        s.push('\n');
    }
    s
}

/// `--data`: inline JSON, `@file`, or `-` / left out for stdin (`{}` when stdin is a terminal or
/// empty).
pub(crate) fn payload(flag: Option<&str>, tty: bool) -> Result<Value, Fail> {
    let (from, text) = match flag {
        Some(f) if f.starts_with('@') => {
            let path = &f[1..];
            (path.to_string(), std::fs::read_to_string(path).map_err(|e| Fail::Usage(format!("--data {f}: {e}")))?)
        }
        Some(f) if f != "-" => ("--data".to_string(), f.to_string()),
        _ if tty => return Ok(json!({})),
        _ => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s).map_err(|e| Fail::Usage(format!("reading stdin: {e}")))?;
            ("stdin".to_string(), s)
        }
    };
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    match serde_json::from_str(&text) {
        Ok(v @ Value::Object(_)) => Ok(v),
        Ok(_) => Err(Fail::Usage(format!("{from} must be a JSON object, e.g. {{\"text\":\"…\"}}"))),
        Err(e) => Err(Fail::Usage(format!("{from} isn't JSON: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(r: Result<Value, Fail>) -> Value {
        r.unwrap_or_else(|_| panic!("payload refused"))
    }

    #[test]
    fn events_list_counts_then_each_publish() {
        let v = json!({
            "stream": "slack",
            "counts": { "accepted": 2, "duplicate": 1, "rate_limited": 3, "too_large": 0, "busy": 0, "refused": 0 },
            "events": [
                { "at": "2020-01-01T00:00:00Z", "kind": "message", "verdict": "rate_limited", "detail": "over 60 a minute (bursts of 20)" },
                { "at": "2020-01-01T00:00:00Z", "kind": "message", "id": "1700.1", "verdict": "duplicate", "seq": 7, "detail": "same id as event 7" },
                { "at": "2020-01-01T00:00:00Z", "kind": "message", "id": "1700.1", "verdict": "accepted", "seq": 7 }
            ]
        });
        let s = events_text(&v);
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(lines[0], "stream slack: 2 accepted, 1 duplicate, dropped 3 rate_limited since midnad started");
        assert!(lines[1].contains("message") && lines[1].contains("rate_limited") && lines[1].ends_with("over 60 a minute (bursts of 20)"), "{s}");
        assert!(lines[2].contains("id 1700.1") && lines[2].ends_with("same id as event 7"), "{s}");
        assert!(lines[3].contains("accepted") && lines[3].ends_with("event 7"), "{s}");
        assert_eq!(events_text(&json!({ "stream": "ci", "events": [], "counts": {} })), "stream ci: no publishes yet\n");
    }

    #[test]
    fn payload_comes_inline_from_a_file_or_empty() {
        assert_eq!(ok(payload(Some(r#"{"text":"hi"}"#), true)), json!({ "text": "hi" }));
        assert_eq!(ok(payload(None, true)), json!({}));
        assert_eq!(ok(payload(Some("  "), true)), json!({}));
        let f = std::env::temp_dir().join(format!("midna-stream-{}.json", std::process::id()));
        std::fs::write(&f, r#"{"n": 2}"#).unwrap();
        assert_eq!(ok(payload(Some(&format!("@{}", f.display())), true)), json!({ "n": 2 }));
        std::fs::remove_file(&f).ok();
        assert!(matches!(payload(Some("[1]"), true), Err(Fail::Usage(m)) if m.contains("object")));
        assert!(matches!(payload(Some("{nope"), true), Err(Fail::Usage(m)) if m.contains("isn't JSON")));
        assert!(matches!(payload(Some("@/nonexistent/x.json"), true), Err(Fail::Usage(_))));
    }
}
