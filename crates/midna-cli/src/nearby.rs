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
        other => Err(Fail::Usage(format!("unknown peers subcommand `{other}` (list, pair, confirm, tier, forget, visibility or name)"))),
    }
}

/// Flags `share` and `inbox` take a value from (`--link <url>`, `--file <path>`, `--into <terminal>`).
pub const SHARE_VALUE_FLAGS: &[&str] = &["link", "file", "into"];

/// The paired device `who` names.
fn paired(who: &str) -> Result<String, Fail> {
    let list = call("peers.list", json!({}))?;
    find(list["paired"].as_array().map(Vec::as_slice).unwrap_or_default(), who, "paired device")
}

fn kind_word(item: &Value) -> String {
    let p = &item["payload"];
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
    if p["kind"] == "file" {
        let size = nb::size_label(item["size"].as_u64().unwrap_or(0));
        return match p["files"].as_array().map(Vec::len) {
            Some(n) if p["folder"] == true => format!("{size}, {n} file{}", if n == 1 { "" } else { "s" }),
            _ => size,
        };
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
    match (p["url"].as_str(), p["text"].as_str()) {
        (Some(u), _) => println!("{u}"),
        (None, Some(t)) => println!("{}", t.trim_end_matches('\n')),
        _ => {}
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

/// `midna share send <peer> [text…]`: offer a paired device a message, a snippet, a link, or a
/// file or folder (`--file <path>`, or a single word that names one).
pub fn share(a: &Args, out: OutFn) -> Res {
    a.check(&["link", "file", "title", "snippet", "message"])?;
    match a.pos.get(1).map(String::as_str) {
        Some("send") => {}
        Some(other) => return Err(Fail::Usage(format!("unknown share subcommand `{other}` (send; incoming shares: midna inbox)"))),
        None => return Err(Fail::Usage("share send <peer> [text…] (or text on stdin, or --link <url>)".into())),
    }
    let who = a.need(2, "which paired device (midna peers lists them)")?;
    let words = a.text(3);
    let lone_path = (a.pos.len() == 4 && !a.has("message") && !a.has("snippet") && !a.has("title") && a.get("link").is_none())
        .then(|| a.pos[3].clone())
        .filter(|w| std::path::Path::new(w).exists());
    if let Some(path) = a.get("file").map(str::to_string).or(lone_path) {
        if a.pos.len() > 4 || (a.get("file").is_some() && a.pos.len() > 3) || a.get("link").is_some() || a.has("message") || a.has("snippet") || a.has("title") {
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
    if let Some(url) = a.get("link") {
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
            a.check(&["into", "pin"])?;
            let mut params = json!({ "id": id()? });
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
