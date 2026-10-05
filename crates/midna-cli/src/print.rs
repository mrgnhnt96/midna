//! Human-readable output (default). `--json` prints the raw results instead.
use serde_json::Value;

fn s(v: &Value, k: &str) -> String {
    plain(v.get(k).unwrap_or(&Value::Null))
}

/// A JSON value as plain text (strings unquoted, null empty).
pub fn plain(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Array(a) if a.iter().all(Value::is_string) => a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(","),
        v => v.to_string(),
    }
}

fn table(rows: Vec<Vec<String>>) {
    let n = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..n).map(|i| rows.iter().map(|r| r.get(i).map_or(0, |c| c.chars().count())).max().unwrap_or(0)).collect();
    for r in rows {
        let mut line = String::new();
        for (i, c) in r.iter().enumerate() {
            if i + 1 == r.len() {
                line.push_str(c);
            } else {
                line.push_str(&format!("{c:<w$}  ", w = widths[i]));
            }
        }
        println!("{}", line.trim_end());
    }
}

pub fn kv(v: &Value) {
    if let Some(o) = v.as_object() {
        let w = o.keys().map(String::len).max().unwrap_or(0);
        for (k, val) in o {
            println!("{k:<w$}  {}", plain(val));
        }
    }
}

pub fn sessions(v: &Value) {
    let list = v.as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        println!("no terminals");
        return;
    }
    let mut rows = vec![vec!["ID".into(), "STATUS".into(), "KIND".into(), "NAME".into(), "PROJECT".into(), "TITLE".into()]];
    for x in list {
        let kind = match x.get("agent") {
            Some(a) if !a.is_null() => plain(a),
            _ => s(&x, "kind"),
        };
        rows.push(vec![s(&x, "id"), plain(&x["status"]["state"]), kind, s(&x, "name"), s(&x, "project_id"), s(&x, "title")]);
    }
    table(rows);
}

pub fn needs_you(v: &Value) {
    let list = v.as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        println!("nothing needs you");
        return;
    }
    let mut rows = vec![vec!["ID".into(), "KIND".into(), "SESSION".into(), "TITLE".into()]];
    for x in list {
        rows.push(vec![s(&x, "id"), s(&x, "kind"), s(&x, "session_id"), s(&x, "title")]);
    }
    table(rows);
}

fn scope(v: &Value) -> String {
    match (v["kind"].as_str(), v.get("id")) {
        (Some(k), Some(id)) => format!("{k}:{}", plain(id)),
        (Some(k), None) => k.into(),
        _ => String::new(),
    }
}

pub fn rules(v: &Value) {
    let list = v.as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        println!("no rules");
        return;
    }
    let mut rows = vec![vec!["ID".into(), "EFFECT".into(), "KIND".into(), "PATTERN".into(), "SCOPE".into(), "FIRED".into(), "NOTE".into()]];
    for x in list {
        let mut note = vec![];
        if let Some(e) = x["expires_at"].as_str() {
            note.push(format!("expires {e}"));
        }
        if x.get("removal_request").is_some_and(|r| !r.is_null()) {
            note.push("removal requested".into());
        }
        rows.push(vec![
            s(&x, "id"),
            s(&x, "effect"),
            plain(&x["matcher"]["kind"]),
            plain(&x["matcher"]["pattern"]),
            scope(&x["scope"]),
            s(&x, "fired"),
            note.join(", "),
        ]);
    }
    table(rows);
}

pub fn check(v: &Value) {
    let rule = v.get("rule").filter(|r| !r.is_null()).map(|r| format!(" (rule {})", plain(&r["id"]))).unwrap_or_else(|| format!(" ({})", s(v, "source")));
    println!("{}{rule}", s(v, "decision"));
    for t in v["trace"].as_array().into_iter().flatten() {
        println!("  {} {:<10} {}", if t["matched"].as_bool() == Some(true) { "✓" } else { "·" }, s(t, "rule_id"), s(t, "reason"));
    }
}

pub fn settings(v: &Value) {
    let mut rows = vec![vec!["KEY".into(), "VALUE".into(), "DEFAULT".into(), "".into()]];
    for x in v.as_array().into_iter().flatten() {
        let flag = if x["human_only"].as_bool() == Some(true) { "human only" } else { "" };
        rows.push(vec![s(x, "key"), s(x, "value"), s(x, "default"), flag.into()]);
    }
    table(rows);
}

fn dur(secs: i64) -> String {
    let a = secs.abs();
    let t = if a >= 3600 { format!("{}h{:02}m", a / 3600, a % 3600 / 60) } else { format!("{}m", a / 60) };
    if secs < 0 { format!("-{t}") } else { t }
}

fn totals_row(label: String, t: &Value) -> Vec<String> {
    let i = |k: &str| t[k].as_i64().unwrap_or(0);
    vec![
        label,
        i("turns").to_string(),
        i("messages").to_string(),
        format!("${:.2}", t["spend_usd"].as_f64().unwrap_or(0.0)),
        dur(i("working_secs")),
        dur(i("waiting_secs")),
        i("approvals").to_string(),
        i("triggers_fired").to_string(),
    ]
}

pub fn insights(v: &Value) {
    println!("{} ({} → {})", s(v, "range"), s(v, "from"), s(v, "to"));
    let mut rows = vec![["", "TURNS", "MSGS", "SPEND", "WORKING", "WAITING", "APPROVALS", "TRIGGERS"].map(String::from).to_vec()];
    rows.push(totals_row("total".into(), &v["totals"]));
    let d = &v["vs_previous"];
    rows.push(totals_row("vs prev".into(), d));
    for r in v["rows"].as_array().into_iter().flatten() {
        rows.push(totals_row(format!("  {}", s(r, "label")), &r["totals"]));
    }
    table(rows);
}

pub fn event(e: &Value) {
    let who = match (e["actor"]["kind"].as_str(), e["actor"]["session"].as_str()) {
        (Some(k), Some(sid)) => format!("{k}:{sid}"),
        (Some(k), None) => k.into(),
        _ => String::new(),
    };
    let target = e["session_id"].as_str().or(e["project_id"].as_str()).unwrap_or("");
    let mut data = e["data"].to_string();
    if data.chars().count() > 160 {
        data = format!("{}…", data.chars().take(160).collect::<String>());
    }
    println!("{:>6} {} {:<24} {:<14} {:<9} {}", s(e, "seq"), s(e, "at"), s(e, "kind"), who, target, data);
}

/// `session.prompts`: one line per prompt, ▸ on the one the agent's view is in.
pub fn prompts(v: &Value) {
    let list = v["prompts"].as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        println!("no prompts");
        return;
    }
    let here = v["here"].as_u64();
    for p in &list {
        let n = p["n"].as_u64().unwrap_or(0);
        let mark = if Some(n) == here { "▸" } else { " " };
        let time = s(p, "at").get(11..16).unwrap_or("").to_string();
        let text: String = s(p, "text").lines().next().unwrap_or("").chars().take(100).collect();
        let gone = if p["on_screen"].as_bool() == Some(false) { "  (before /clear)" } else { "" };
        println!("{mark} {n:>3}  {time}  {text}{gone}");
    }
    println!("{}", if v["scrolled"].as_bool() == Some(true) { "(scrolled back)" } else { "(live)" });
}
