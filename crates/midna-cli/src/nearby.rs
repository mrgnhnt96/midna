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
    if let Some(port) = me["port"].as_u64() {
        println!("  port        {port} (Noise, bound to the device keys)");
    }
    let pairings = v["pairings"].as_array().cloned().unwrap_or_default();
    if !pairings.is_empty() {
        println!("Pairings ({}):", pairings.len());
        for p in &pairings {
            println!("  {}", pairing_line(p));
        }
    }
    let paired = v["paired"].as_array().cloned().unwrap_or_default();
    if !paired.is_empty() {
        println!("Paired ({}):", paired.len());
        for p in &paired {
            let here = if p["nearby"] == true { "  nearby now" } else { "" };
            println!(
                "  {}  id {}  {}  since {}{here}",
                s(p, "name"),
                short_fingerprint(p["fingerprint"].as_str().unwrap_or("")),
                tier(p["tier"].as_str()),
                crate::guide::ago(p["paired_at"].as_str().unwrap_or(""))
            );
        }
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
        let trust = match (p["tier"].as_str(), p["claims_name_of"].as_str()) {
            (Some(t), _) => format!("  {}", tier(Some(t))),
            (None, Some(fp)) => format!(
                "  NOT PAIRED: uses the name of paired device {}, but its key is different",
                short_fingerprint(fp)
            ),
            (None, None) => "  not paired".into(),
        };
        println!("  {}  id {}{at}  seen since {since}{trust}{speaks}", s(&p, "name"), short_fingerprint(p["fingerprint"].as_str().unwrap_or("")));
    }
}

fn tier(t: Option<&str>) -> &'static str {
    match t {
        Some("my_device") => "my device",
        Some(_) => "contact",
        None => "not paired",
    }
}

fn pairing_line(p: &Value) -> String {
    let code = midna_proto::nearby::spaced_code(p["code"].as_str().unwrap_or(""));
    let dir = if p["role"] == "incoming" { "from" } else { "with" };
    let state = match p["state"].as_str().unwrap_or("") {
        "comparing" => {
            let mine = if p["confirmed"] == true { "you confirmed" } else { "waiting for you" };
            let theirs = if p["peer_confirmed"] == true { "they confirmed" } else { "waiting for them" };
            format!("code {code}, {mine}, {theirs}, {} left", left(p["expires_at"].as_str().unwrap_or("")))
        }
        st => match p["error"].as_str() {
            Some(e) => format!("{st}: {e}"),
            None => st.to_string(),
        },
    };
    let clash = match p["claims_name_of"].as_str() {
        Some(fp) => format!("  CAREFUL: not the paired device {} of that name", short_fingerprint(fp)),
        None => String::new(),
    };
    format!("{}  {dir} {} (id {})  {state}{clash}", s(p, "id"), s(p, "name"), short_fingerprint(p["fingerprint"].as_str().unwrap_or("")))
}

/// The fingerprint of the device `who` (a name, a full id or the start of one) in `list`.
fn find(list: &[Value], who: &str, what: &str) -> Result<String, Fail> {
    let w = who.trim().to_lowercase();
    let fp = |p: &Value| p["fingerprint"].as_str().unwrap_or("").to_string();
    if let Some(p) = list.iter().find(|p| fp(p) == w) {
        return Ok(fp(p));
    }
    let hits: Vec<&Value> = list
        .iter()
        .filter(|p| p["name"].as_str().is_some_and(|n| n.trim().to_lowercase() == w) || (w.len() >= 4 && fp(p).starts_with(&w)))
        .collect();
    match hits.as_slice() {
        [one] => Ok(fp(one)),
        [] => Err(Fail::Usage(format!("no {what} called or with id `{who}` (midna peers lists them)"))),
        many => Err(Fail::Usage(format!(
            "`{who}` matches {} {what}s; use the id: {}",
            many.len(),
            many.iter().map(|p| short_fingerprint(&fp(p))).collect::<Vec<_>>().join(", ")
        ))),
    }
}

fn print_pairing(v: &Value) {
    println!("{}", pairing_line(v));
    if v["state"] == "comparing" {
        println!(
            "Check that {} shows the same code, then confirm on both Macs: midna peers confirm {} [--as my-device]\n\
             (or approve the needs-you item). Different code? midna peers confirm {} --no",
            s(v, "name"),
            s(v, "id"),
            s(v, "id")
        );
    }
}

fn print_forgot(v: &Value) {
    let f = &v["forgotten"];
    println!("Forgot {} (id {}); it's a stranger to this Mac again.", s(f, "name"), short_fingerprint(f["fingerprint"].as_str().unwrap_or("")));
}

pub fn peers(a: &Args, out: OutFn) -> Res {
    a.check(&["as", "no"])?;
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
        "pair" => {
            let who = a.pos.get(2..).map(|p| p.join(" ")).unwrap_or_default();
            if who.trim().is_empty() {
                return Err(Fail::Usage("peers pair <name or id>: which nearby Midna (midna peers lists them)".into()));
            }
            let list = call("peers.list", json!({}))?;
            let fingerprint = find(list["peers"].as_array().map(Vec::as_slice).unwrap_or_default(), &who, "nearby Midna")?;
            let v = call("peers.pair", json!({ "fingerprint": fingerprint }))?;
            out(&v, &print_pairing);
            Ok(())
        }
        "confirm" => {
            let id = match a.pos.get(2) {
                Some(id) => id.clone(),
                None => {
                    let list = call("peers.list", json!({}))?;
                    let open: Vec<String> = list["pairings"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|p| p["state"] == "comparing")
                        .filter_map(|p| p["id"].as_str().map(String::from))
                        .collect();
                    match open.as_slice() {
                        [one] => one.clone(),
                        [] => return Err(Fail::Usage("no pairing is waiting to be confirmed".into())),
                        _ => return Err(Fail::Usage(format!("several pairings are waiting; say which: {}", open.join(", ")))),
                    }
                }
            };
            let mut params = json!({ "pairing": id, "accept": !a.has("no") });
            if let Some(t) = a.get("as") {
                let Some(t) = midna_proto::nearby::Tier::parse(t) else {
                    return Err(Fail::Usage(format!("`{t}`: keep the device as contact or my-device")));
                };
                params["as"] = json!(t);
            }
            let v = call("peers.confirm", params)?;
            out(&v, &|v| println!("{}", pairing_line(v)));
            Ok(())
        }
        "forget" | "unpair" => {
            let who = a.pos.get(2..).map(|p| p.join(" ")).unwrap_or_default();
            if who.trim().is_empty() {
                return Err(Fail::Usage("peers forget <name or id>: which paired device".into()));
            }
            let list = call("peers.list", json!({}))?;
            let fingerprint = find(list["paired"].as_array().map(Vec::as_slice).unwrap_or_default(), &who, "paired device")?;
            let v = call("peers.forget", json!({ "fingerprint": fingerprint }))?;
            out(&v, &print_forgot);
            Ok(())
        }
        other => Err(Fail::Usage(format!("unknown peers subcommand `{other}` (list, pair, confirm, forget, visibility or name)"))),
    }
}
