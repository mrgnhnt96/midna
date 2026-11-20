//! `midna stream …`: local event streams. `publish` hands midna an event from this Mac
//! (`streams.publish`); local triggers on `stream.<name>.<kind>` react to it.
use crate::args::Args;
use crate::{Fail, OutFn, Res, call};
use serde_json::{Value, json};
use std::io::{IsTerminal, Read};

pub fn stream(a: &Args, out: OutFn) -> Res {
    match a.pos.get(1).map(String::as_str) {
        Some("publish" | "pub") => publish(a, out),
        Some(other) => Err(Fail::Usage(format!("unknown stream subcommand `{other}` (try `midna stream publish <name> <kind>`)"))),
        None => Err(Fail::Usage("midna stream publish <name> <kind> [--id ID] [--data JSON|@file|-]".into())),
    }
}

fn publish(a: &Args, out: OutFn) -> Res {
    a.check(&["id", "data", "json"])?;
    let name = a.need(2, "stream name (e.g. slack)")?;
    let kind = a.need(3, "event kind (e.g. message)")?;
    let data = payload(a.get("data"), std::io::stdin().is_terminal())?;
    let v = call("streams.publish", json!({ "stream": name, "kind": kind, "id": a.get("id"), "data": data }))?;
    out(&v, &|v| println!("published {} (event {})", v["event"].as_str().unwrap_or(""), v["seq"]));
    Ok(())
}

/// `--data`: inline JSON, `@file`, or `-` / left out for stdin (`{}` when stdin is a terminal or
/// empty).
fn payload(flag: Option<&str>, tty: bool) -> Result<Value, Fail> {
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
