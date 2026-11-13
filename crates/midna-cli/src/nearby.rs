//! `midna peers …`: other Midnas on the same network (Nearby), and who can find this Mac.
use crate::args::Args;
use crate::print::plain;
use crate::{Fail, OutFn, Res, call};
use midna_proto::nearby::{self as nb, short_fingerprint};
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
    match me["local_network"].as_str() {
        Some("waiting") => println!("  network     waiting for Local Network access: choose Allow in the macOS prompt"),
        Some("denied") => {
            println!("  network     Local Network access is off for Midna: turn it on in System Settings › Privacy & Security › Local Network")
        }
        _ => {}
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
            let auto: Vec<&str> = p["auto_accept"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            let auto = if auto.is_empty() { String::new() } else { format!("  auto-accepts {}", auto.join(", ")) };
            println!(
                "  {}  id {}  {}  since {}{here}{auto}",
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
        "sessions" | "terminals" => {
            let who = a.pos.get(2..).map(|p| p.join(" ")).unwrap_or_default();
            if who.trim().is_empty() {
                return Err(Fail::Usage("peers sessions <name or id>: which of your paired devices".into()));
            }
            let v = call("peers.sessions", json!({ "peer": paired(&who)? }))?;
            out(&v, &print_sessions);
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
        "tier" => {
            let who = a.pos.get(2..).map(|p| p.join(" ")).unwrap_or_default();
            let Some(t) = a.get("as") else { return Err(Fail::Usage("peers tier <name or id> --as contact|my-device".into())) };
            let Some(t) = midna_proto::nearby::Tier::parse(t) else {
                return Err(Fail::Usage(format!("`{t}`: keep the device as contact or my-device")));
            };
            if who.trim().is_empty() {
                return Err(Fail::Usage("peers tier <name or id> --as contact|my-device: which paired device".into()));
            }
            let list = call("peers.list", json!({}))?;
            let fingerprint = find(list["paired"].as_array().map(Vec::as_slice).unwrap_or_default(), &who, "paired device")?;
            let v = call("peers.set_tier", json!({ "fingerprint": fingerprint, "as": t }))?;
            out(&v, &|v| println!("{} (id {}) is now kept as {}.", s(v, "name"), short_fingerprint(v["fingerprint"].as_str().unwrap_or("")), s(v, "tier")));
            Ok(())
        }
        "auto-accept" | "auto" => {
            const USAGE: &str = "peers auto-accept <name or id> <kinds|all> on|off (kinds: message, snippet, link, file, tab, object, agent-start, agent-ask)";
            let pos = a.pos.get(2..).unwrap_or_default();
            let [who @ .., kinds, on] = pos else { return Err(Fail::Usage(USAGE.into())) };
            let on = match on.as_str() {
                "on" | "yes" | "true" => true,
                "off" | "no" | "false" => false,
                _ => return Err(Fail::Usage(USAGE.into())),
            };
            let kinds: Vec<nb::ShareKind> = if kinds == "all" {
                nb::ShareKind::ALL.to_vec()
            } else {
                kinds
                    .split(',')
                    .map(|k| nb::ShareKind::parse(k).ok_or_else(|| Fail::Usage(format!("`{k}` isn't a kind of share; {USAGE}"))))
                    .collect::<Result<_, _>>()?
            };
            let who = who.join(" ");
            if who.trim().is_empty() {
                return Err(Fail::Usage(USAGE.into()));
            }
            let list = call("peers.list", json!({}))?;
            let fingerprint = find(list["paired"].as_array().map(Vec::as_slice).unwrap_or_default(), &who, "paired device")?;
            let mut v = Value::Null;
            for kind in kinds {
                v = call("peers.set_auto_accept", json!({ "fingerprint": fingerprint, "kind": kind, "on": on }))?;
            }
            out(&v, &|v| {
                let auto: Vec<&str> = v["auto_accept"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                let what = if auto.is_empty() { "nothing; every share asks first".to_string() } else { auto.join(", ") };
                println!("{} (id {}) auto-accepts {what}.", s(v, "name"), short_fingerprint(v["fingerprint"].as_str().unwrap_or("")));
            });
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
        other => Err(Fail::Usage(format!("unknown peers subcommand `{other}` (list, sessions, pair, confirm, tier, auto-accept, forget, visibility or name)"))),
    }
}

/// Flags `share` and `inbox` take a value from (`--link <url>`, `--file <path>`, `--object <type>`,
/// `--into <terminal>`, `--to <device>`).
const SHARE_VALUE_FLAGS: &[&str] = &["link", "file", "object", "into", "to"];
/// Flags that take a value elsewhere and none here.
const SHARE_BOOL_FLAGS: &[&str] = &["agent", "wait"];
/// `share send agent-start` and `agent-ask`: `--agent claude|codex` picks agent-start's agent.
const AGENT_START_BOOL_FLAGS: &[&str] = &["wait"];
/// How long `share send agent-start|agent-ask --wait` waits for the answer without `--timeout`.
const ANSWER_WAIT_DEFAULT: u64 = 30 * 60;

/// The arguments of `share` and `inbox`, whose flags differ from other commands'.
pub fn parse_args(raw: Vec<String>) -> Result<Args, crate::args::ArgError> {
    let a = Args::parse_flags(raw.clone(), SHARE_VALUE_FLAGS, SHARE_BOOL_FLAGS)?;
    if agent_kind(&a).is_some() {
        return Args::parse_flags(raw, SHARE_VALUE_FLAGS, AGENT_START_BOOL_FLAGS);
    }
    Ok(a)
}

/// `share send agent-start …` or `share send agent-ask …`: which.
fn agent_kind(a: &Args) -> Option<nb::ShareKind> {
    if a.pos.first().map(String::as_str) != Some("share") || a.pos.get(1).map(String::as_str) != Some("send") {
        return None;
    }
    a.pos.get(2).and_then(|k| nb::ShareKind::parse(k)).filter(|k| matches!(k, nb::ShareKind::AgentStart | nb::ShareKind::AgentAsk))
}

/// `peers sessions`: one line per agent terminal on the other Mac.
fn print_sessions(v: &Value) {
    let sessions = v["sessions"].as_array().cloned().unwrap_or_default();
    if sessions.is_empty() {
        println!("{} has no agent terminals open.", s(v, "peer_name"));
        return;
    }
    println!("Agent terminals on {}:", s(v, "peer_name"));
    for t in &sessions {
        let project = t["project"].as_str().map(|p| format!("  {p}")).unwrap_or_default();
        let busy = if t["busy"] == true { "busy" } else { "idle" };
        println!("  {}  “{}”  {}{project}  {busy}", s(t, "id"), s(t, "name"), s(t, "agent"));
    }
    println!("Ask one: midna share send agent-ask --to {} --session <name or id> --prompt '…' --wait", plain(&v["peer_name"]));
}

/// The paired device `who` names.
fn paired(who: &str) -> Result<String, Fail> {
    let list = call("peers.list", json!({}))?;
    find(list["paired"].as_array().map(Vec::as_slice).unwrap_or_default(), who, "paired device")
}

/// What an object share holds: its type and how many.
fn object_word(o: &Value) -> String {
    let n = |k: &str, one: &str, many: &str| {
        let n = o[k].as_array().map(Vec::len).or(o[k].as_object().map(|m| m.len())).unwrap_or(0);
        format!("{n} {}", if n == 1 { one } else { many })
    };
    match o["type"].as_str().unwrap_or("") {
        "summary" => format!("Summary “{}”", plain(&o["record"]["name"])),
        "rules" => n("rules", "rule", "rules"),
        "triggers" => n("triggers", "trigger", "triggers"),
        "theme" => format!("theme “{}”", plain(&o["theme"]["name"])),
        "settings" => n("values", "setting", "settings"),
        "queue" => n("prompts", "queued prompt", "queued prompts"),
        other => format!("object {other}"),
    }
}

/// An object share's contents, one line each.
fn object_lines(o: &Value) -> Vec<String> {
    let each = |k: &str| o[k].as_array().cloned().unwrap_or_default();
    match o["type"].as_str().unwrap_or("") {
        "summary" => {
            let r = &o["record"];
            let head = format!("“{}” in {}, {} prompts, closed {}", plain(&r["name"]), plain(&o["project"]), r["turns"], plain(&r["closed_at"]));
            std::iter::once(head).chain(each_of(&r["prompts"]).iter().map(|p| format!("  › {}", plain(&p["text"])))).collect()
        }
        "rules" => each("rules")
            .iter()
            .map(|r| {
                let at = r["project"].as_str().map(|p| format!(" in {p}")).unwrap_or_default();
                format!("{} {} `{}`{at}", plain(&r["effect"]), plain(&r["matcher"]["kind"]), plain(&r["matcher"]["pattern"]))
            })
            .collect(),
        "triggers" => each("triggers").iter().map(|t| format!("“{}” on {} ({})", plain(&t["name"]), plain(&t["event"]), plain(&t["action"]["kind"]))).collect(),
        "theme" => vec![format!("{} ({})", plain(&o["theme"]["id"]), if o["theme"]["dark"] == true { "dark" } else { "light" })],
        "settings" => o["values"].as_object().into_iter().flatten().map(|(k, v)| format!("{k} = {}", v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()))).collect(),
        "queue" => each("prompts").iter().map(|p| format!("› {}", plain(&p["text"]))).collect(),
        _ => vec![],
    }
}

fn each_of(v: &Value) -> Vec<Value> {
    v.as_array().cloned().unwrap_or_default()
}

fn kind_word(item: &Value) -> String {
    let p = &item["payload"];
    if p["kind"] == "agent_start" {
        return format!("{} start “{}”", s(p, "agent"), s(p, "title"));
    }
    if p["kind"] == "agent_ask" {
        return format!("ask of “{}”", s(p, "session"));
    }
    if p["kind"] == "object" {
        return object_word(&p["object"]);
    }
    if p["kind"] == "file" {
        let what = if p["folder"] == true { "folder" } else { "file" };
        return format!("{what} “{}”", nb::safe_name(p["name"].as_str().unwrap_or("")));
    }
    match (p["kind"].as_str(), p["title"].as_str()) {
        (Some(k), Some(t)) => format!("{k} “{t}”"),
        (Some(k), None) => k.to_string(),
        _ => "share".into(),
    }
}

/// The first line of a share's text or its URL, cut short; a file share's size.
fn preview(item: &Value) -> String {
    let p = &item["payload"];
    if p["kind"] == "object" {
        let first = object_lines(&p["object"]).into_iter().next().unwrap_or_default();
        let mut short: String = first.chars().take(60).collect();
        if short.len() < first.len() {
            short.push('…');
        }
        return short;
    }
    if p["kind"] == "file" {
        let size = nb::size_label(item["size"].as_u64().unwrap_or(0));
        return match p["files"].as_array().map(Vec::len) {
            Some(n) if p["folder"] == true => format!("{size}, {n} file{}", if n == 1 { "" } else { "s" }),
            _ => size,
        };
    }
    if p["kind"] == "agent_start" || p["kind"] == "agent_ask" {
        let first = p["prompt"].as_str().unwrap_or("").lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
        let mut short: String = first.chars().take(60).collect();
        if short.len() < first.len() {
            short.push('…');
        }
        return short;
    }
    if p["kind"] == "tab" || p["kind"] == "agent" {
        let place = p["project"]["remote"].as_str().map(|r| match p["project"]["path"].as_str() {
            Some(path) => format!("{r}/{path}"),
            None => r.to_string(),
        });
        return place.unwrap_or_else(|| s(p, "cwd"));
    }
    let text = p["url"].as_str().or(p["text"].as_str()).unwrap_or("");
    let first = text.lines().next().unwrap_or("").trim();
    let mut short: String = first.chars().take(60).collect();
    if short.len() < first.len() || text.trim_end().contains('\n') {
        short.push('…');
    }
    short
}

/// Where a file share's transfer is, in a few words; None before it starts.
fn moving(item: &Value) -> Option<String> {
    let t = &item["transfer"];
    let (done, total) = (t["done"].as_u64()?, t["total"].as_u64().unwrap_or(0));
    let pct = (done * 100).checked_div(total).unwrap_or(100);
    let files = match (t["files_done"].as_u64(), t["files"].as_u64()) {
        (Some(d), Some(n)) if n > 1 => format!(", {d}/{n} files"),
        _ => String::new(),
    };
    let why = t["error"].as_str().map(|e| format!(" ({e})")).unwrap_or_default();
    Some(match t["state"].as_str()? {
        "done" if item["direction"] == "incoming" => match t["saved_to"].as_str() {
            Some(to) => format!("saved to {to}"),
            None => "saved".into(),
        },
        "done" => "they have it".into(),
        "failed" => format!("failed{why}"),
        "moving" => format!("moving {pct}% ({} of {}{files})", nb::size_label(done), nb::size_label(total)),
        _ => format!("waiting at {pct}%{why}"),
    })
}

fn share_line(item: &Value) -> String {
    let dir = if item["direction"] == "incoming" { "from" } else { "to" };
    let state = match (item["state"].as_str().unwrap_or(""), item["result"].as_str(), moving(item)) {
        ("offered", _, _) if item["direction"] == "incoming" => "waiting for you".to_string(),
        ("offered", _, _) => "waiting for them".to_string(),
        (st, _, Some(m)) => format!("{st}: {m}"),
        (st, Some(r), None) => format!("{st}: {r}"),
        (st, None, None) => st.to_string(),
    };
    format!(
        "{}  {} {dir} {}  {}  {state}  {}",
        s(item, "id"),
        kind_word(item),
        s(item, "peer_name"),
        crate::guide::ago(item["sent_at"].as_str().unwrap_or("")),
        preview(item)
    )
}

fn print_sent(v: &Value) {
    if v["payload"]["kind"] == "agent_start" {
        let back = if v["payload"]["want_reply"] == true {
            format!("; its answer comes back when its first turn ends (midna share answer {})", s(v, "id"))
        } else {
            String::new()
        };
        println!(
            "Offered {} to {} ({}). Nothing runs until they accept it on their Mac; then a new {} terminal there gets the prompt{back}.",
            kind_word(v),
            s(v, "peer_name"),
            s(v, "id"),
            s(&v["payload"], "agent")
        );
        return;
    }
    if v["payload"]["kind"] == "agent_ask" {
        let back = if v["payload"]["want_reply"] == true {
            format!("; its answer comes back when that turn ends (midna share answer {})", s(v, "id"))
        } else {
            String::new()
        };
        println!(
            "Offered {} to {} ({}). Nothing is typed until they accept it on their Mac; then the prompt is queued in that \
             terminal and typed once its agent is idle{back}.",
            kind_word(v),
            s(v, "peer_name"),
            s(v, "id")
        );
        return;
    }
    if v["payload"]["kind"] == "agent" {
        println!(
            "Offered the conversation {} ({}) to {} ({}). Nothing moves until they accept it on their Mac; then their Mac \
             fetches the transcript as it was when you sent it and resumes it there (midna inbox show {}).",
            kind_word(v),
            preview(v),
            s(v, "peer_name"),
            s(v, "id"),
            s(v, "id")
        );
        return;
    }
    if v["payload"]["kind"] == "file" {
        println!(
            "Offered {} ({}) to {} ({}). Nothing moves until they accept it on their Mac; then their Mac fetches it \
             from this one, so leave it where it is until it's done (midna inbox show {}).",
            kind_word(v),
            preview(v),
            s(v, "peer_name"),
            s(v, "id"),
            s(v, "id")
        );
        return;
    }
    println!("Offered {} to {} ({}); they accept or decline it on their Mac.", kind_word(v), s(v, "peer_name"), s(v, "id"));
}

fn print_inbox(v: &Value) {
    let items = v["items"].as_array().cloned().unwrap_or_default();
    if items.is_empty() {
        println!("The inbox is empty.");
        return;
    }
    for it in &items {
        println!("{}", share_line(it));
    }
    match v["waiting"].as_u64().unwrap_or(0) {
        0 => {}
        n => println!("{n} waiting for you: midna inbox accept <id> (a snippet: --into <terminal>), or midna inbox decline <id>"),
    }
}

fn print_item(v: &Value) {
    println!("{}", share_line(v));
    let p = &v["payload"];
    if let Some(r) = p["reply_to"].as_str() {
        println!("in reply to {r}");
    }
    if p["kind"] == "agent_start" {
        let place = match (p["project"]["remote"].as_str(), p["project"]["path"].as_str()) {
            (Some(r), Some(path)) => format!("{r}/{path} (in {})", s(p, "cwd")),
            (Some(r), None) => format!("{r} (in {})", s(p, "cwd")),
            _ => s(p, "cwd"),
        };
        let back = if p["want_reply"] == true { ", answer wanted" } else { "" };
        println!("{} in {place}{back}", s(p, "agent"));
        println!("{}", p["prompt"].as_str().unwrap_or("").trim_end_matches('\n'));
        return;
    }
    if p["kind"] == "agent_ask" {
        let back = if p["want_reply"] == true { ", answer wanted" } else { "" };
        println!("for the agent terminal “{}”{back}", s(p, "session"));
        println!("{}", p["prompt"].as_str().unwrap_or("").trim_end_matches('\n'));
        return;
    }
    if p["kind"] == "agent" {
        let t = &p["transcript"];
        println!("{} conversation {} in {}, transcript {}", s(p, "agent"), s(p, "conversation"), s(p, "cwd"), nb::size_label(t["size"].as_u64().unwrap_or(0)));
        if let Some(m) = moving(v) {
            println!("{m}");
        }
        return;
    }
    if let Some(sb) = p["scrollback"].as_str() {
        println!("in {}{}", s(p, "cwd"), p["shell"].as_str().map(|sh| format!(" ({sh})")).unwrap_or_default());
        let lines: Vec<&str> = sb.lines().collect();
        for l in &lines[lines.len().saturating_sub(20)..] {
            println!("  {l}");
        }
        return;
    }
    match (p["url"].as_str(), p["text"].as_str()) {
        (Some(u), _) => println!("{u}"),
        (None, Some(t)) => println!("{}", t.trim_end_matches('\n')),
        _ => {}
    }
    if p["kind"] == "object" {
        for line in object_lines(&p["object"]) {
            println!("  {line}");
        }
    }
    let files = p["files"].as_array().cloned().unwrap_or_default();
    if p["folder"] == true {
        const SHOWN: usize = 50;
        for f in files.iter().take(SHOWN) {
            println!("  {}  {}", plain(&f["path"]), nb::size_label(f["size"].as_u64().unwrap_or(0)));
        }
        if files.len() > SHOWN {
            println!("  … and {} more", files.len() - SHOWN);
        }
    }
}

/// `midna share send <peer> [text…]`: offer a paired device a message, a snippet, a link, a
/// file or folder (`--file <path>`, or a single word that names one), a terminal tab (`--tab`), or
/// an agent terminal's conversation (`--agent`), or one of Midna's own things (`--object <type> [ids…]`).
/// `share send agent-start …` starts an agent there instead, and `share send agent-ask …` asks
/// one of its agent terminals ([`agent_start`]).
pub fn share(a: &Args, out: OutFn) -> Res {
    match a.pos.get(1).map(String::as_str) {
        Some("send") if agent_kind(a).is_some() => return agent_start(a, out),
        Some("send") => {}
        Some("answer") => return answer(a, out),
        Some(other) => return Err(Fail::Usage(format!("unknown share subcommand `{other}` (send or answer; incoming shares: midna inbox)"))),
        None => return Err(Fail::Usage("share send <peer> [text…] (or text on stdin, or --link <url>)".into())),
    }
    a.check(&["link", "file", "object", "title", "snippet", "message", "tab", "agent", "session", "project"])?;
    let who = a.need(2, "which paired device (midna peers lists them)")?;
    if let Some(object) = a.get("object") {
        if a.get("file").is_some() || a.get("link").is_some() || a.has("message") || a.has("snippet") || a.has("title") {
            return Err(Fail::Usage("--object is sent on its own: share send <name|id> --object <type> [ids…]".into()));
        }
        let ids: Vec<String> = a.pos.iter().skip(3).cloned().collect();
        let v = call("share.send", json!({ "peer": paired(&who)?, "kind": "object", "object": object, "ids": ids }))?;
        out(&v, &print_sent);
        return Ok(());
    }
    let words = a.text(3);
    if a.has("tab") && a.has("agent") {
        return Err(Fail::Usage("--tab or --agent, not both".into()));
    }
    let terminal = if a.has("agent") { Some("agent") } else if a.has("tab") { Some("tab") } else { None };
    if let Some(k) = terminal
        && (!words.is_empty() || a.has("link") || a.has("snippet") || a.has("message"))
    {
        return Err(Fail::Usage(format!("--{k} sends a terminal (this one, or --session <terminal>); nothing else goes with it")));
    }
    if a.get("project").is_some() && terminal != Some("tab") {
        return Err(Fail::Usage("--project goes with --tab: share send <device> --tab --project <its name there> [--session <terminal>]".into()));
    }
    if terminal.is_none() && a.get("session").is_some() {
        return Err(Fail::Usage("--session picks the terminal for --tab or --agent".into()));
    }
    let lone_path = (a.pos.len() == 4 && !a.has("message") && !a.has("snippet") && !a.has("title") && a.get("link").is_none())
        .then(|| a.pos[3].clone())
        .filter(|w| std::path::Path::new(w).exists());
    if let Some(path) = a.get("file").map(str::to_string).or(lone_path) {
        if a.pos.len() > 4 || (a.get("file").is_some() && a.pos.len() > 3) || a.get("link").is_some() || a.has("message") || a.has("snippet") || a.has("title") || terminal.is_some() {
            return Err(Fail::Usage("a file is sent on its own: share send <name|id> <path>".into()));
        }
        let abs = std::path::absolute(&path).map_err(|e| Fail::Usage(format!("{path}: {e}")))?;
        let v = call("share.send", json!({ "peer": paired(&who)?, "kind": "file", "path": abs.to_string_lossy() }))?;
        out(&v, &print_sent);
        return Ok(());
    }
    let mut params = json!({ "peer": paired(&who)? });
    if let Some(t) = a.get("title") {
        params["title"] = json!(t);
    }
    if let Some(k) = terminal {
        params["kind"] = json!(k);
        for f in ["session", "project"] {
            if let Some(v) = a.get(f) {
                params[f] = json!(v);
            }
        }
    } else if let Some(url) = a.get("link") {
        if !words.is_empty() || a.has("snippet") || a.has("message") {
            return Err(Fail::Usage("--link sends just the link (give it a --title instead of text)".into()));
        }
        params["kind"] = json!("link");
        params["url"] = json!(url);
    } else {
        let text = if words.is_empty() {
            use std::io::{IsTerminal, Read};
            if std::io::stdin().is_terminal() {
                return Err(Fail::Usage("nothing to send: give text, pipe it on stdin, or use --link <url>".into()));
            }
            let mut t = String::new();
            std::io::stdin().read_to_string(&mut t).map_err(|e| Fail::Usage(format!("reading stdin: {e}")))?;
            t
        } else {
            words.clone()
        };
        // Words are a message and stdin is a snippet, unless a flag says otherwise.
        let snippet = a.has("snippet") || (words.is_empty() && !a.has("message"));
        params["kind"] = json!(if snippet { "snippet" } else { "message" });
        params["text"] = json!(text);
    }
    let v = call("share.send", params)?;
    out(&v, &print_sent);
    Ok(())
}

const AGENT_START_USAGE: &str =
    "share send agent-start --to <device> --prompt <text> [--agent claude|codex] [--project <its name there> | --session <terminal>] [--title <t>] [--wait [--timeout 30m]]";
const AGENT_ASK_USAGE: &str =
    "share send agent-ask --to <device> --session <name or id there> --prompt <text> [--wait [--timeout 30m]] (midna peers sessions <device> lists them)";

/// `midna share send agent-start --to <device> --prompt <text>`: start an agent on a paired
/// device, in its checkout of this terminal's project (or `--session`'s, or in its project named
/// `--project`), with that prompt.
/// `midna share send agent-ask --to <device> --session <terminal> --prompt <text>`: ask one of the
/// agent terminals open there instead. The prompt may also come as words or on stdin. With
/// `--wait`, wait for its answer and print it.
fn agent_start(a: &Args, out: OutFn) -> Res {
    let ask = agent_kind(a) == Some(nb::ShareKind::AgentAsk);
    let usage = if ask { AGENT_ASK_USAGE } else { AGENT_START_USAGE };
    if ask {
        a.check(&["to", "prompt", "session", "wait", "timeout"])?;
    } else {
        a.check(&["to", "prompt", "agent", "session", "project", "title", "wait", "timeout"])?;
    }
    let who = a.get("to").ok_or_else(|| Fail::Usage(format!("which device? --to <name or id> (midna peers lists them); {usage}")))?;
    if ask && a.get("session").is_none_or(|t| t.trim().is_empty()) {
        return Err(Fail::Usage(format!("which terminal there? --session <name or id>; {usage}")));
    }
    let words = a.text(3);
    let prompt = match a.get("prompt") {
        Some(_) if !words.is_empty() => return Err(Fail::Usage(format!("give the prompt once: --prompt <text>, words or stdin; {usage}"))),
        Some(p) => p.to_string(),
        None if !words.is_empty() => words,
        None => {
            use std::io::{IsTerminal, Read};
            if std::io::stdin().is_terminal() {
                return Err(Fail::Usage(format!("what should it ask? --prompt <text> (or pipe it on stdin); {usage}")));
            }
            let mut t = String::new();
            std::io::stdin().read_to_string(&mut t).map_err(|e| Fail::Usage(format!("reading stdin: {e}")))?;
            t
        }
    };
    let timeout = match a.get("timeout") {
        Some(_) if !a.has("wait") => return Err(Fail::Usage("--timeout is for --wait".into())),
        Some(t) => crate::triggers::parse_duration(t, 1).map_err(|e| Fail::Usage(format!("--timeout {e}")))?,
        None => ANSWER_WAIT_DEFAULT,
    };
    let kind = if ask { "agent_ask" } else { "agent_start" };
    let mut params = json!({ "peer": paired(who)?, "kind": kind, "prompt": prompt, "want_reply": a.has("wait") });
    for k in ["agent", "session", "project", "title"] {
        if let Some(v) = a.get(k) {
            params[k] = json!(v);
        }
    }
    let v = call("share.send", params)?;
    if !a.has("wait") {
        out(&v, &print_sent);
        return Ok(());
    }
    let id = s(&v, "id");
    eprintln!("Offered {} to {} ({id}); waiting for its answer…", kind_word(&v), s(&v, "peer_name"));
    wait_for_answer(&id, timeout, out)
}

/// `midna share answer <id> [--wait] [--timeout 30m]`: the answer to an agent start or ask sent
/// with `--wait`, waiting for it with `--wait`.
fn answer(a: &Args, out: OutFn) -> Res {
    a.check(&["wait", "timeout"])?;
    let id = a.need(2, "which agent start or ask (sh_…; midna inbox --out lists them)")?;
    let timeout = match a.get("timeout") {
        Some(t) => crate::triggers::parse_duration(t, 1).map_err(|e| Fail::Usage(format!("--timeout {e}")))?,
        None if a.has("wait") => ANSWER_WAIT_DEFAULT,
        None => 0,
    };
    wait_for_answer(id, timeout, out)
}

/// Wait up to `timeout` seconds for agent start or ask `id`'s answer and print it.
fn wait_for_answer(id: &str, timeout: u64, out: OutFn) -> Res {
    // The daemon waits at most MAX_ANSWER_WAIT_SECS per call; ask again until the timeout.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout);
    let v = loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now()).as_secs().min(nb::MAX_ANSWER_WAIT_SECS);
        let v = call("share.answer", json!({ "id": id, "wait_secs": left }))?;
        if !v["answer"].is_null() || v["share"]["state"] == "declined" || std::time::Instant::now() >= deadline {
            break v;
        }
    };
    out(&v, &|v| {
        if let Some(text) = v["answer"]["payload"]["text"].as_str() {
            println!("{}", text.trim_end_matches('\n'));
        }
    });
    let share = &v["share"];
    if share["state"] == "declined" {
        let why = share["result"].as_str().map(|r| format!(": {r}")).unwrap_or_default();
        return Err(Fail::Other(format!("{} declined {id}{why}; nothing ran there", s(share, "peer_name"))));
    }
    if v["answer"].is_null() {
        let at = match share["state"].as_str() {
            Some("offered") => "still waits for them to accept it",
            _ if share["payload"]["kind"] == "agent_ask" => "was accepted; that agent hasn't finished the turn it asks for",
            _ => "was accepted; its agent hasn't finished its first turn",
        };
        return Err(Fail::Pending(format!("{id} {at}; ask again: midna share answer {id} --wait")));
    }
    Ok(())
}

fn item(id: &str) -> Result<Value, Fail> {
    let v = call("inbox.list", json!({ "limit": 100000 }))?;
    v["items"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|it| it["id"] == id)
        .cloned()
        .ok_or_else(|| Fail::Usage(format!("no share {id} in the inbox (midna inbox lists them)")))
}

/// `midna inbox …`: shares from and to paired devices.
pub fn inbox(a: &Args, out: OutFn) -> Res {
    let id = || a.need(2, "which share (sh_…; midna inbox lists them)");
    match a.pos.get(1).map(String::as_str).unwrap_or("list") {
        "list" | "ls" => {
            a.check(&["in", "out", "waiting", "limit"])?;
            let mut params = json!({});
            match (a.has("in"), a.has("out")) {
                (true, false) => params["direction"] = json!("incoming"),
                (false, true) => params["direction"] = json!("outgoing"),
                _ => {}
            }
            if a.has("waiting") {
                params["direction"] = json!("incoming");
                params["state"] = json!("offered");
            }
            if let Some(n) = a.get("limit") {
                let n: usize = n.parse().map_err(|_| Fail::Usage(format!("--limit {n}: a number")))?;
                params["limit"] = json!(n);
            }
            let v = call("inbox.list", params)?;
            out(&v, &print_inbox);
            Ok(())
        }
        "show" => {
            a.check(&[])?;
            let v = item(&id()?)?;
            out(&v, &print_item);
            Ok(())
        }
        "accept" => {
            a.check(&["into", "pin", "cwd"])?;
            let mut params = json!({ "id": id()? });
            if let Some(dir) = a.get("cwd") {
                let dir = std::path::absolute(dir).map_err(|e| Fail::Usage(format!("--cwd {dir}: {e}")))?;
                params["cwd"] = json!(dir.to_string_lossy());
            }
            if let Some(t) = a.get("into") {
                params["session"] = json!(t);
            }
            if a.has("pin") {
                params["action"] = json!("pin");
            }
            let v = call("share.accept", params)?;
            out(&v, &|v| println!("{}", share_line(v)));
            Ok(())
        }
        "decline" => {
            a.check(&[])?;
            let v = call("share.decline", json!({ "id": id()? }))?;
            out(&v, &|v| println!("{}", share_line(v)));
            Ok(())
        }
        "reply" => {
            a.check(&[])?;
            let id = id()?;
            let text = a.text(3);
            if text.trim().is_empty() {
                return Err(Fail::Usage("inbox reply <id> <text…>".into()));
            }
            let it = item(&id)?;
            if it["direction"] != "incoming" || it["payload"]["kind"] != "message" {
                return Err(Fail::Usage(format!("{id} isn't a message to this Mac")));
            }
            // Replying reads the message.
            if it["state"] == "offered" {
                call("share.accept", json!({ "id": id }))?;
            }
            let v = call("share.send", json!({ "peer": it["peer"], "kind": "message", "text": text, "reply_to": id }))?;
            out(&v, &print_sent);
            Ok(())
        }
        other => Err(Fail::Usage(format!("unknown inbox subcommand `{other}` (list, show, accept, decline or reply)"))),
    }
}
