//! `midna peers …`: other Midnas on the same network (Nearby), and who can find this Mac.
use crate::args::Args;
use crate::print::plain;
use crate::{Fail, OutFn, Res, call};
use midna_proto::nearby::short_fingerprint;
use midna_proto::time;
use serde_json::{Value, json};

fn s(v: &Value, k: &str) -> String {
    plain(v.get(k).unwrap_or(&Value::Null))
}

/// How long until an RFC 3339 time, in a few words.
fn left(ts: &str) -> String {
    match time::parse_rfc3339(ts) {
        Some(t) => {
            let d = (t - time::now_unix()).max(0);
            if d < 90 { format!("{d}s") } else { format!("{} min", (d + 59) / 60) }
        }
        None => ts.to_string(),
    }
}

pub fn print_list(v: &Value) {
    let me = &v["me"];
    println!("This Mac: {}  (id {})", s(me, "name"), short_fingerprint(me["fingerprint"].as_str().unwrap_or("")));
    let who = match me["visibility"].as_str() {
        Some("off") => "no one (Nearby is off)".to_string(),
        Some("everyone") => match me["everyone_until"].as_str() {
            Some(t) => format!("everyone, for {} more (then paired devices only)", left(t)),
            None => "everyone".into(),
        },
        _ => "paired devices only".into(),
    };
    println!("  visible to  {who}");
    match me["service"].as_str() {
        None => println!("  bonjour     off for this daemon"),
        Some(svc) => {
            let looking = if me["browsing"] == true { "looking for others" } else { "not looking" };
            let showing = if me["advertising"] == true { "showing this Mac" } else { "not showing this Mac" };
            println!("  bonjour     {svc}, {looking}, {showing}");
        }
    }
    if let Some(e) = me["error"].as_str() {
        println!("  error       {e}");
    }
    let peers = v["peers"].as_array().cloned().unwrap_or_default();
    if peers.is_empty() {
        println!("No other Midnas nearby.");
        return;
    }
    println!("Nearby ({}):", peers.len());
    let protocol = me["protocol"].as_u64();
    for p in peers {
        let at = match (p["host"].as_str(), p["port"].as_u64()) {
            (Some(h), Some(port)) => format!("  {}:{port}", h.trim_end_matches('.')),
            _ => String::new(),
        };
        let speaks = if p["protocol"].as_u64() != protocol { format!("  (speaks Nearby {}, this Mac {}: one side needs an update)", p["protocol"], me["protocol"]) } else { String::new() };
        let since = crate::guide::ago(p["seen_since"].as_str().unwrap_or(""));
        println!("  {}  id {}{at}  seen since {since}{speaks}", s(&p, "name"), short_fingerprint(p["fingerprint"].as_str().unwrap_or("")));
    }
}

pub fn peers(a: &Args, out: OutFn) -> Res {
    a.check(&[])?;
    let set = |key: &str, value: Value| -> Res {
        call("settings.set", json!({ "key": key, "value": value }))?;
        let v = call("peers.list", json!({}))?;
        out(&v, &print_list);
        Ok(())
    };
    match a.pos.get(1).map(String::as_str).unwrap_or("list") {
        "list" | "ls" => {
            let v = call("peers.list", json!({}))?;
            out(&v, &print_list);
            Ok(())
        }
        "visibility" | "visible" => {
            let v = a.need(2, "who can find this Mac (off, paired or everyone)")?;
            if midna_proto::nearby::Visibility::parse(&v).is_none() {
                return Err(Fail::Usage(format!("`{v}`: visibility is off, paired or everyone")));
            }
            set("nearby.visibility", json!(v))
        }
        "name" => {
            let name = a.pos.get(2..).map(|p| p.join(" ")).unwrap_or_default();
            set("nearby.name", json!(name.trim()))
        }
        other => Err(Fail::Usage(format!("unknown peers subcommand `{other}` (list, visibility or name)"))),
    }
}
