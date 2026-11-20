//! `midna stream …`: local event streams. `publish` hands midna an event from this Mac
//! (`streams.publish`); local triggers on `stream.<name>.<kind>` react to it. `events` lists a
//! stream's recent publishes and what midna did with each (`streams.events`), and `--replay`
//! runs one of them through the local triggers again (`trigger.replay`). `add` has midnad run a
//! producer whose stdout lines are events (`streams.add`); `list`, `pause`, `resume`, `restart`
//! and `remove` manage those.
use crate::args::Args;
use crate::{Fail, OutFn, Res, call};
use serde_json::{Value, json};
use std::io::{IsTerminal, Read};

pub fn stream(a: &Args, out: OutFn) -> Res {
    match a.pos.get(1).map(String::as_str) {
        Some("publish" | "pub") => publish(a, out),
        Some("events" | "log") => events(a, out),
        Some("add") => add(a, out),
        Some("list" | "ls") => list(a, out),
        Some("remove" | "rm") => named(a, out, "streams.remove", |n, _| println!("removed stream {n}; its producer was stopped")),
        Some("pause") => named(a, out, "streams.set_enabled", |_, v| println!("paused: {}", status_line(v))),
        Some("resume") => named(a, out, "streams.set_enabled", |_, v| println!("resumed: {}", status_line(v))),
        Some("restart") => named(a, out, "streams.restart", |_, v| println!("restarted: {}", status_line(v))),
        Some(other) => Err(Fail::Usage(format!("unknown stream subcommand `{other}` (try `midna stream publish <name> <kind>`, `midna stream list` or `midna help stream`)"))),
        None => Err(Fail::Usage(USAGE.into())),
    }
}

const USAGE: &str = "midna stream publish <name> <kind> [--id ID] [--data JSON|@file|-] | events <name> [--limit N] [--replay SEQ|ID]\n\
                     \x20      | add <name> [--project P] [--secret NAME]... [--cwd DIR] -- <command...>\n\
                     \x20      | list | pause <name> | resume <name> | restart <name> | remove <name>";

fn add(a: &Args, out: OutFn) -> Res {
    a.check(&["project", "secret", "cwd", "json"])?;
    let name = a.need(2, "stream name (e.g. slack)")?;
    if a.rest.is_empty() {
        return Err(Fail::Usage("midna stream add <name> [--project P] [--secret NAME]... [--cwd DIR] -- <command...>: the command goes after `--`".into()));
    }
    // Like `midna open`: --cwd, else the project's folder, else this one.
    let cwd = match a.get("cwd") {
        Some(c) => Some(std::path::absolute(c).map_err(|e| Fail::Usage(format!("--cwd {c}: {e}")))?.to_string_lossy().into_owned()),
        None if a.get("project").is_none() => std::env::current_dir().ok().map(|p| p.to_string_lossy().into_owned()),
        None => None,
    };
    let v = call("streams.add", json!({ "name": name, "command": a.rest, "cwd": cwd, "project_id": a.get("project"), "secrets": a.all("secret") }))?;
    out(&v, &|v| {
        println!("added stream {name}: {}", status_line(v));
        println!("its events are stream.{name}.<kind>; its log is {}", v["log"].as_str().unwrap_or(""));
    });
    Ok(())
}

fn list(a: &Args, out: OutFn) -> Res {
    a.check(&["json"])?;
    let v = call("streams.list", json!({}))?;
    out(&v, &|v| print!("{}", list_text(v)));
    Ok(())
}

/// `remove|pause|resume|restart <name>`.
fn named(a: &Args, out: OutFn, method: &str, text: fn(&str, &Value)) -> Res {
    a.check(&["json"])?;
    let name = a.need(2, "stream name")?;
    let p = match a.pos[1].as_str() {
        "pause" => json!({ "name": name, "enabled": false }),
        "resume" => json!({ "name": name, "enabled": true }),
        _ => json!({ "name": name }),
    };
    let v = call(method, p)?;
    out(&v, &|v| text(name, v));
    Ok(())
}

/// Every stream midnad runs, one line each, then its command.
fn list_text(v: &Value) -> String {
    let rows = v.as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        return "no streams (add one with `midna stream add <name> -- <command...>`)\n".into();
    }
    let mut s = String::new();
    for r in &rows {
        s.push_str(&format!("{:<16} {}\n", r["name"].as_str().unwrap_or(""), status_line(r)));
        let words: Vec<&str> = r["command"].as_array().map(|c| c.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
        s.push_str(&format!("{:<16} $ {}\n", "", words.join(" ")));
    }
    s
}

/// `running (pid 42), 3 restarts, last exit code 1 2m ago, last event 5s ago`.
fn status_line(v: &Value) -> String {
    let mut parts = vec![match (v["state"].as_str().unwrap_or(""), v["pid"].as_u64()) {
        ("running", Some(pid)) => format!("running (pid {pid})"),
        ("backoff", _) => format!("waiting to start again ({})", v["retry_at"].as_str().map(|t| format!("at {t}")).unwrap_or_default()),
        (s, _) => s.to_string(),
    }];
    if let Some(e) = v["error"].as_str() {
        parts.push(format!("couldn't start: {e}"));
    }
    match v["restarts"].as_u64() {
        Some(1) => parts.push("1 restart".into()),
        Some(n) if n > 1 => parts.push(format!("{n} restarts")),
        _ => {}
    }
    let e = &v["last_exit"];
    if e.is_object() {
        let how = match (e["code"].as_i64(), e["signal"].as_i64()) {
            (Some(c), _) => format!("code {c}"),
            (_, Some(s)) => format!("signal {s}"),
            _ => "unknown".into(),
        };
        parts.push(format!("last exit {how} {}", crate::guide::ago(e["at"].as_str().unwrap_or(""))));
    }
    if let Some(at) = v["last_event"]["at"].as_str() {
        parts.push(format!("last event {} {}", v["last_event"]["kind"].as_str().unwrap_or(""), crate::guide::ago(at)));
    }
    if !v["secrets"].as_array().is_none_or(Vec::is_empty) {
        let names: Vec<&str> = v["secrets"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        parts.push(format!("secrets {}", names.join(", ")));
    }
    parts.join(", ")
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
    fn stream_list_shows_state_restarts_exit_and_last_event() {
        let v = json!([
            {
                "name": "slack", "state": "running", "pid": 42, "restarts": 2, "command": ["slack-tail", "--json"],
                "secrets": ["SLACK_TOKEN"],
                "last_exit": { "at": "2020-01-01T00:00:00Z", "code": 1, "ran_secs": 3 },
                "last_event": { "at": "2020-01-01T00:00:00Z", "kind": "message", "verdict": "accepted", "seq": 9 }
            },
            { "name": "ci", "state": "paused", "restarts": 0, "command": ["watch-ci"] }
        ]);
        let s = list_text(&v);
        let lines: Vec<&str> = s.lines().collect();
        assert!(lines[0].starts_with("slack") && lines[0].contains("running (pid 42), 2 restarts, last exit code 1 "), "{s}");
        assert!(lines[0].contains("last event message ") && lines[0].ends_with("secrets SLACK_TOKEN"), "{s}");
        assert!(lines[1].trim_start().starts_with("$ slack-tail --json"), "{s}");
        assert!(lines[2].starts_with("ci") && lines[2].ends_with(" paused"), "{s}");
        assert!(list_text(&json!([])).starts_with("no streams"));
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
