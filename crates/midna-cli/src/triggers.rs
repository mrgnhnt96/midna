//! `midna triggers …` and `midna webhooks …`.
use crate::args::Args;
use crate::print::plain;
use crate::{Fail, OutFn, Res, call};
use serde_json::{Value, json};
use std::io::{IsTerminal, Read};

fn s(v: &Value, k: &str) -> String {
    plain(v.get(k).unwrap_or(&Value::Null))
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

/// A value for one-line output: quoted when it has spaces (or is empty), cut at 60 chars.
fn q(v: &str) -> String {
    let short: String = if v.chars().count() > 60 { format!("{}…", v.chars().take(59).collect::<String>()) } else { v.to_string() };
    if short.is_empty() || short.contains(char::is_whitespace) || short.contains('"') { format!("{short:?}") } else { short }
}

/// One line saying what a trigger action does (`send: "/compact" → "{{last_prompt}}"`).
pub fn action_text(a: &Value) -> String {
    match a["kind"].as_str() {
        Some("start_agent") => format!("start {} in {}", s(a, "agent"), s(a, "project_id")),
        Some("run_command") => format!("run `{}` in {}", s(a, "command"), s(a, "project_id")),
        Some("attention") => format!("attention: {}", s(a, "message")),
        Some("send_to_session") => {
            let steps: Vec<String> = a["steps"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|st| {
                    let text = format!("\"{}\"", st["text"].as_str().unwrap_or(""));
                    if st["enter"].as_bool() == Some(false) { format!("{text} (no enter)") } else { text }
                })
                .collect();
            format!("send: {}", steps.join(" → "))
        }
        Some("set_status") => {
            let mut bits = vec![s(a, "color"), s(a, "base"), format!("clears on {}", a["clear_on"].as_str().unwrap_or("prompt"))];
            if let Some(i) = a["icon"].as_str() {
                bits.insert(1, format!("icon {i}"));
            }
            format!("status: {} ({})", s(a, "label"), bits.join(", "))
        }
        Some("clear_status") => "clear status".into(),
        Some("notify") => {
            let body = a["body"].as_str().filter(|b| !b.is_empty()).map(|b| format!(" — \"{b}\"")).unwrap_or_default();
            let silent = if a["sound"].as_bool() == Some(false) { " (silent)" } else { "" };
            format!("notify: \"{}\"{body}{silent}", s(a, "title"))
        }
        _ => a.to_string(),
    }
}

/// Filters as `key=value` words; local ones too (`session=… idle=55m match message="*Compact first*"`).
pub fn filter_text(f: &Value) -> String {
    let mut out: Vec<String> = ["repo", "branch", "action", "label", "session", "project", "agent"]
        .iter()
        .filter_map(|k| f.get(*k).and_then(Value::as_str).map(|v| format!("{k}={}", q(v))))
        .collect();
    if let Some(m) = f["idle_minutes"].as_u64() {
        out.push(format!("idle={m}m"));
    }
    if let Some(c) = f["cron"].as_str() {
        out.push(format!("cron={}", q(c)));
    }
    if let (Some(a), Some(b)) = (f["window"]["from"].as_str(), f["window"]["until"].as_str()) {
        out.push(format!("between={a}-{b}"));
    }
    for (k, word) in [("starts_at", "starts"), ("ends_at", "ends")] {
        if let Some(t) = f[k].as_str() {
            out.push(format!("{word}={}", q(&midna_proto::time::parse_rfc3339(t).map(midna_proto::time::format_local).unwrap_or_else(|| t.to_string()))));
        }
    }
    if let Some(n) = f["max_runs"].as_u64() {
        out.push(format!("max-runs={n}"));
    }
    if let Some(m) = f["match"].as_object().filter(|m| !m.is_empty()) {
        // Sorted: map order depends on serde_json's preserve_order, which the workspace may turn on.
        let mut pairs: Vec<String> = m.iter().map(|(k, v)| format!("{k}={}", q(v.as_str().unwrap_or("")))).collect();
        pairs.sort();
        out.push(format!("match {}", pairs.join(" ")));
    }
    out.join(" ")
}

fn print_triggers(v: &Value) {
    let list = v.as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        println!("no triggers");
        return;
    }
    let mut rows = vec![["ID", "STATE", "NAME", "SOURCE", "EVENT", "FILTER", "ACTION", "FIRED"].map(String::from).to_vec()];
    for t in list {
        let fired = match t["last_fired_at"].as_str() {
            Some(at) => format!("{} (last {at})", s(&t, "fired")),
            None => "never".into(),
        };
        let mut name = s(&t, "name");
        if t["builtin"].is_string() {
            name.push_str(" (builtin)");
        }
        rows.push(vec![s(&t, "id"), s(&t, "state"), name, s(&t, "source"), s(&t, "event"), filter_text(&t["filter"]), action_text(&t["action"]), fired]);
    }
    table(rows);
}

fn print_trigger(t: &Value) {
    let local = t["source"] == "local";
    println!("{}  {}  ({})", s(t, "id"), s(t, "name"), s(t, "state"));
    println!("  on       {} {} {}", s(t, "source"), s(t, "event"), filter_text(&t["filter"]));
    println!("  does     {}", action_text(&t["action"]));
    if let Some(p) = t["action"]["prompt_template"].as_str() {
        println!("  prompt   {p}");
    }
    if local {
        for (i, st) in t["action"]["steps"].as_array().into_iter().flatten().enumerate() {
            let enter = if st["enter"].as_bool() == Some(false) { "  (no enter)" } else { "" };
            println!("  step {}   {}{enter}", i + 1, st["text"].as_str().unwrap_or(""));
        }
        let filter: midna_proto::TriggerFilter = serde_json::from_value(t["filter"].clone()).unwrap_or_default();
        if let (Some(c), Ok(sc)) = (filter.cron.as_deref(), midna_proto::cron::Schedule::of(&filter, t["fired"].as_u64().unwrap_or(0))) {
            let runs: Vec<String> = sc.upcoming(midna_proto::time::now_unix(), 3).into_iter().map(midna_proto::cron::local_label).collect();
            println!("  when     {}", sc.describe(c));
            println!("  runs     {} (local time)", if runs.is_empty() { "never again".into() } else { format!("{}, …", runs.join(", ")) });
        } else {
            println!("  cooldown {}s per terminal", t["cooldown_secs"].as_u64().unwrap_or(60));
        }
    } else {
        let secret = if t["secret_set"].as_bool() == Some(true) { format!("set {} ({})", s(t, "secret_set_at"), s(t, "secret_store")) } else { "not set".into() };
        println!("  secret   {secret}");
    }
    if let Some(b) = t["builtin"].as_str() {
        println!("  builtin  {b} (ships with midna; edit, pause or remove it like any other)");
    }
    match t["state"].as_str() {
        Some("needs_secret") => println!("  next     a human runs `midna triggers set-secret {}`", s(t, "id")),
        Some("draft" | "paused") if local => println!("  next     `midna triggers enable {}` (agents may, when the human asked for it)", s(t, "id")),
        Some("draft") => println!("  next     a human runs `midna triggers enable {}`", s(t, "id")),
        _ => {}
    }
}

fn print_deliveries(v: &Value) {
    let list = v.as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        println!("no deliveries");
        return;
    }
    let mut rows = vec![["ID", "RECEIVED", "VERDICT", "EVENT", "REPO", "SUBJECT", "OUTCOME"].map(String::from).to_vec()];
    for d in list {
        let ev = match d["action"].as_str() {
            Some(a) if d["source"] == "github" => format!("{}.{a}", s(&d, "event")),
            _ => s(&d, "event"),
        };
        let mut verdict = s(&d, "verdict");
        if d["recovered"].as_bool() == Some(true) && verdict != "recovered" {
            verdict.push_str(" (recovered)");
        }
        rows.push(vec![s(&d, "id"), s(&d, "received_at"), verdict, ev, s(&d, "repo"), s(&d, "subject"), s(&d, "summary")]);
    }
    table(rows);
}

fn print_delivery(d: &Value) {
    println!("{} {}  {}", s(d, "id"), s(d, "verdict"), s(d, "summary"));
    for e in d["eval"].as_array().into_iter().flatten() {
        println!("  {}", plain(e));
    }
}

fn print_status(v: &Value) {
    println!("path      {} ({})", s(v, "path_name"), s(v, "path"));
    println!("health    {}: {}", s(v, "health"), s(v, "detail"));
    if let Some(u) = v["public_url"].as_str() {
        println!("github    {u}");
    }
    if let Some(u) = v["bitbucket_url"].as_str() {
        println!("bitbucket {u}");
    }
    let r = &v["receiver"];
    let local = if r["listening"].as_bool() == Some(true) { format!("listening on 127.0.0.1:{}", s(r, "bound_port")) } else { "not listening".into() };
    println!("receiver  {local}{}", r["error"].as_str().map(|e| format!(" ({e})")).unwrap_or_default());
    if let Some(d) = v.get("last_delivery").filter(|d| !d.is_null()) {
        println!("last      {} {} {}", s(d, "received_at"), s(d, "verdict"), s(d, "summary"));
    }
    let rc = &v["reconcile"];
    if let Some(at) = rc["last_run_at"].as_str() {
        println!("recovery  {at} ({}): {} recovered from {} hook(s){}", s(rc, "reason"), s(rc, "recovered"), s(rc, "hooks_checked"), rc["error"].as_str().map(|e| format!("; {e}")).unwrap_or_default());
    }
    println!("secrets   {}", s(v, "secret_store"));
    if let Some(f) = v["fix"].as_str() {
        println!("fix       {f}");
    }
}

/// Event namespaces that can only be local (GitHub/Bitbucket never send these).
const LOCAL_EVENT_PREFIXES: &[&str] = &["hook.", "agent.", "session.", "needs_you."];

/// Flags that only make sense on a local trigger; any of them makes `add` default to `--source local`.
const LOCAL_FLAGS: &[&str] = &[
    "session", "in-project", "for-agent", "idle-for", "cron", "between", "starts", "ends", "max-runs", "match", "send", "send-no-enter", "set-status",
    "clear-status", "cooldown", "enable",
];

fn is_local_event(e: &str) -> bool {
    e == "idle" || e == "schedule" || LOCAL_EVENT_PREFIXES.iter().any(|p| e.starts_with(p))
}

/// `55m`, `1h30m`, `90s`, `2d`; a bare number counts in `bare_unit` seconds. Returns seconds.
pub fn parse_duration(v: &str, bare_unit: u64) -> Result<u64, String> {
    let v = v.trim().to_ascii_lowercase();
    if v.is_empty() {
        return Err("empty duration".into());
    }
    if let Ok(n) = v.parse::<u64>() {
        return Ok(n * bare_unit);
    }
    let (mut total, mut num) = (0u64, String::new());
    for c in v.chars() {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        let unit = match c {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            'd' => 86400,
            _ => return Err(format!("`{v}`: unknown unit `{c}` (use s, m, h or d, e.g. 55m or 1h30m)")),
        };
        let n: u64 = num.parse().map_err(|_| format!("`{v}`: expected a number before `{c}`"))?;
        total += n * unit;
        num.clear();
    }
    if !num.is_empty() {
        return Err(format!("`{v}`: a number without a unit at the end (e.g. 55m)"));
    }
    Ok(total)
}

/// `--match key=glob` → (dotted path, glob). An empty glob removes that key on update.
pub fn parse_match(v: &str) -> Result<(String, String), String> {
    let (k, g) = v.split_once('=').ok_or_else(|| format!("--match `{v}`: expected path=glob, e.g. message=*Compact first*"))?;
    let k = k.trim();
    if k.is_empty() {
        return Err(format!("--match `{v}`: empty path"));
    }
    Ok((k.to_string(), g.to_string()))
}

fn json_arg(a: &Args, flag: &str) -> Result<Option<Value>, Fail> {
    let Some(raw) = a.get(flag) else { return Ok(None) };
    let v: Value = serde_json::from_str(raw).map_err(|e| Fail::Usage(format!("--{flag} must be JSON: {e}")))?;
    if !v.is_object() {
        return Err(Fail::Usage(format!("--{flag} must be a JSON object")));
    }
    Ok(Some(v))
}

fn one_of(flag: &str, v: &str, ok: &[&str]) -> Result<(), Fail> {
    if ok.contains(&v) { Ok(()) } else { Err(Fail::Usage(format!("--{flag} `{v}`: use one of {}", ok.join(", ")))) }
}

/// Build a TriggerAction from flags, if any action flag was given. `cur` is the current
/// action (update), so `--color`/`--base`/`--clear-on`/`--icon` alone can edit a set_status.
fn action_from(a: &Args, project: Option<String>, cur: Option<&Value>) -> Result<Option<Value>, Fail> {
    let picked: Vec<&str> = ["agent", "run", "attention", "send", "send-no-enter", "set-status", "clear-status", "notify", "action-json"]
        .into_iter()
        .filter(|f| a.has(f))
        .filter(|f| *f != "send-no-enter" || !a.has("send"))
        .collect();
    if picked.len() > 1 {
        let names: Vec<String> = picked.iter().map(|f| format!("--{f}")).collect();
        return Err(Fail::Usage(format!("pick one action, not {}", names.join(" and "))));
    }
    let status_mods = ["color", "base", "clear-on", "icon"];
    if !a.has("set-status") && status_mods.iter().any(|f| a.has(f)) {
        let Some(cur) = cur.filter(|c| c["kind"] == "set_status" && picked.is_empty()) else {
            return Err(Fail::Usage("--color, --base, --clear-on and --icon go with --set-status LABEL".into()));
        };
        let mut act = cur.clone();
        if let Some(c) = a.get("color") {
            if !midna_proto::valid_status_color(c) {
                return Err(Fail::Usage(format!("--color `{c}`: use one of {} or #rrggbb", midna_proto::STATUS_COLORS.join(", "))));
            }
            act["color"] = json!(c);
        }
        if let Some(b) = a.get("base") {
            one_of("base", b, &["idle", "working", "needs_you", "done", "failed"])?;
            act["base"] = json!(b);
        }
        if let Some(c) = a.get("clear-on") {
            one_of("clear-on", c, &["prompt", "turn", "status", "never"])?;
            act["clear_on"] = json!(c);
        }
        if let Some(i) = a.get("icon") {
            act["icon"] = if i.is_empty() { Value::Null } else { json!(i) };
        }
        return Ok(Some(act));
    }
    let need_project = || project.clone().ok_or_else(|| Fail::Usage("--project is required for this action".into()));
    if let Some(v) = json_arg(a, "action-json")? {
        if !v["kind"].is_string() {
            return Err(Fail::Usage("--action-json needs a \"kind\" (start_agent, run_command, attention, notify, send_to_session, set_status, clear_status)".into()));
        }
        return Ok(Some(v));
    }
    if let Some(agent) = a.get("agent") {
        let prompt = a.get("prompt").ok_or_else(|| Fail::Usage("--agent needs --prompt TEMPLATE (to only fire for one agent's terminals, use --for-agent)".into()))?;
        return Ok(Some(json!({ "kind": "start_agent", "project_id": need_project()?, "agent": agent, "prompt_template": prompt })));
    }
    if let Some(cmd) = a.get("run") {
        return Ok(Some(json!({ "kind": "run_command", "project_id": need_project()?, "command": cmd })));
    }
    if let Some(msg) = a.get("attention") {
        return Ok(Some(json!({ "kind": "attention", "message": msg })));
    }
    let sends = a.ordered(&["send", "send-no-enter"]);
    if !sends.is_empty() {
        let steps: Vec<Value> = sends.iter().map(|(f, t)| json!({ "text": t, "enter": *f == "send" })).collect();
        return Ok(Some(json!({ "kind": "send_to_session", "steps": steps })));
    }
    if let Some(label) = a.get("set-status") {
        let color = a.get("color").ok_or_else(|| Fail::Usage(format!("--set-status needs --color ({} or #rrggbb)", midna_proto::STATUS_COLORS.join(", "))))?;
        if !midna_proto::valid_status_color(color) {
            return Err(Fail::Usage(format!("--color `{color}`: use one of {} or #rrggbb", midna_proto::STATUS_COLORS.join(", "))));
        }
        let base = a.get("base").ok_or_else(|| Fail::Usage("--set-status needs --base idle|working|needs_you|done|failed (the built-in state underneath)".into()))?;
        one_of("base", base, &["idle", "working", "needs_you", "done", "failed"])?;
        let clear_on = a.get("clear-on").unwrap_or("prompt");
        one_of("clear-on", clear_on, &["prompt", "turn", "status", "never"])?;
        let mut act = json!({ "kind": "set_status", "label": label, "color": color, "base": base, "clear_on": clear_on });
        if let Some(i) = a.get("icon").filter(|i| !i.is_empty()) {
            act["icon"] = json!(i);
        }
        return Ok(Some(act));
    }
    if a.has("clear-status") {
        return Ok(Some(json!({ "kind": "clear_status" })));
    }
    if let Some(title) = a.get("notify") {
        let mut action = json!({ "kind": "notify", "title": title, "body": a.get("notify-body").unwrap_or(""), "sound": !a.has("silent") });
        if let Some(k) = a.get("notify-kind") {
            action["category"] = json!(k);
        }
        return Ok(Some(action));
    }
    if a.has("notify-body") || a.has("notify-kind") || a.has("silent") {
        return Err(Fail::Usage("--notify-body, --notify-kind and --silent go with --notify TITLE".into()));
    }
    Ok(None)
}

/// Apply filter flags on top of `base` (the current filter on update). `--filter-json`
/// replaces the whole filter first. Returns whether anything changed.
fn filter_from(a: &Args, base: Value) -> Result<(Value, bool), Fail> {
    let mut touched = false;
    let mut f = match json_arg(a, "filter-json")? {
        Some(v) => {
            touched = true;
            v
        }
        None if base.is_object() => base,
        None => json!({}),
    };
    for (flag, key) in [("repo", "repo"), ("branch", "branch"), ("action", "action"), ("label", "label"), ("session", "session"), ("in-project", "project")] {
        if let Some(v) = a.get(flag) {
            touched = true;
            f[key] = if v.is_empty() { Value::Null } else { json!(v) };
        }
    }
    if let Some(v) = a.get("for-agent") {
        if !v.is_empty() {
            one_of("for-agent", v, &["claude", "codex"])?;
        }
        touched = true;
        f["agent"] = if v.is_empty() { Value::Null } else { json!(v) };
    }
    if let Some(v) = a.get("idle-for") {
        touched = true;
        f["idle_minutes"] = if v.is_empty() {
            Value::Null
        } else {
            let secs = parse_duration(v, 60).map_err(|e| Fail::Usage(format!("--idle-for {e}")))?;
            if secs < 60 || secs % 60 != 0 {
                return Err(Fail::Usage(format!("--idle-for `{v}`: whole minutes, at least 1m")));
            }
            json!(secs / 60)
        };
    }
    if let Some(v) = a.get("cron") {
        touched = true;
        f["cron"] = if v.trim().is_empty() {
            Value::Null
        } else {
            midna_proto::cron::Cron::parse(v).map_err(|e| Fail::Usage(format!("--cron {e}")))?;
            json!(v.trim())
        };
    }
    if let Some(v) = a.get("between") {
        touched = true;
        f["window"] = if v.trim().is_empty() {
            Value::Null
        } else {
            let (from, until) = v.split_once('-').ok_or_else(|| Fail::Usage(format!("--between `{v}`: use HH:MM-HH:MM, 24-hour (e.g. 13:00-17:00)")))?;
            for x in [from, until] {
                midna_proto::cron::parse_hm(x).map_err(|e| Fail::Usage(format!("--between {e}")))?;
            }
            json!({ "from": from.trim(), "until": until.trim() })
        };
    }
    for (flag, key) in [("starts", "starts_at"), ("ends", "ends_at")] {
        if let Some(v) = a.get(flag) {
            touched = true;
            f[key] = if v.trim().is_empty() {
                Value::Null
            } else {
                let t = midna_proto::time::parse_local(v)
                    .ok_or_else(|| Fail::Usage(format!("--{flag} `{v}`: use a local date and time, `2026-10-06 13:00` or `2026-10-06`")))?;
                json!(midna_proto::time::format_unix(t))
            };
        }
    }
    if let Some(v) = a.get("max-runs") {
        touched = true;
        f["max_runs"] = if v.trim().is_empty() {
            Value::Null
        } else {
            match v.trim().parse::<u64>() {
                Ok(n) if n > 0 => json!(n),
                _ => return Err(Fail::Usage(format!("--max-runs `{v}`: a whole number, at least 1"))),
            }
        };
    }
    if !a.all("match").is_empty() {
        touched = true;
        let mut m = f.get("match").and_then(Value::as_object).cloned().unwrap_or_default();
        for raw in a.all("match") {
            let (k, g) = parse_match(raw).map_err(Fail::Usage)?;
            if g.is_empty() {
                m.remove(&k);
            } else {
                m.insert(k, json!(g));
            }
        }
        f["match"] = Value::Object(m);
    }
    if f.get("match").is_some_and(Value::is_null) {
        f["match"] = json!({});
    }
    Ok((f, touched))
}

fn cooldown(a: &Args) -> Result<Option<u64>, Fail> {
    a.get("cooldown").map(|v| parse_duration(v, 1).map_err(|e| Fail::Usage(format!("--cooldown {e}")))).transpose()
}

const ADD_FLAGS: &[&str] = &[
    "name", "source", "event", "repo", "branch", "action", "label", "agent", "prompt", "run", "attention", "project", "hook-id", "session-name",
    "session", "in-project", "for-agent", "idle-for", "cron", "between", "starts", "ends", "max-runs", "match", "send", "send-no-enter",
    "set-status", "color", "base", "clear-on", "icon", "clear-status", "cooldown", "enable", "action-json", "filter-json", "notify", "notify-body", "notify-kind", "silent",
];

/// Read a secret: hidden from a TTY, else all of stdin (one trailing newline dropped).
fn read_secret(id: &str) -> Result<String, Fail> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        eprint!("Webhook secret for {id} (input hidden): ");
        let fd = 0;
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        let have = unsafe { libc::tcgetattr(fd, &mut t) } == 0;
        if have {
            let mut quiet = t;
            quiet.c_lflag &= !libc::ECHO;
            quiet.c_lflag |= libc::ECHONL;
            unsafe { libc::tcsetattr(fd, libc::TCSANOW, &quiet) };
        }
        let mut line = String::new();
        let r = stdin.read_line(&mut line);
        if have {
            unsafe { libc::tcsetattr(fd, libc::TCSANOW, &t) };
        }
        r.map_err(|e| Fail::Other(e.to_string()))?;
        Ok(line.trim_end_matches(['\n', '\r']).to_string())
    } else {
        let mut buf = String::new();
        stdin.lock().read_to_string(&mut buf).map_err(|e| Fail::Other(e.to_string()))?;
        let s = buf.strip_suffix('\n').unwrap_or(&buf);
        Ok(s.strip_suffix('\r').unwrap_or(s).to_string())
    }
}

fn find(id: &str) -> Result<Value, Fail> {
    let list = call("trigger.list", json!({}))?;
    list.as_array().into_iter().flatten().find(|t| t["id"] == id).cloned().ok_or_else(|| Fail::Other(format!("no trigger {id}")))
}

pub fn triggers(a: &Args, out: OutFn) -> Res {
    match a.pos.get(1).map(String::as_str).unwrap_or("list") {
        "list" | "ls" => {
            a.check(&[])?;
            let v = call("trigger.list", json!({}))?;
            out(&v, &print_triggers);
        }
        "show" | "get" => {
            let t = find(a.need(2, "trigger id")?)?;
            out(&t, &print_trigger);
        }
        "add" => {
            a.check(ADD_FLAGS)?;
            let name = a.get("name").map(str::to_string).unwrap_or_else(|| a.text(2));
            if name.is_empty() {
                return Err(Fail::Usage("missing --name".into()));
            }
            let event = match (a.get("event"), a.has("idle-for"), a.has("cron")) {
                (Some(e), _, _) => e,
                (None, true, _) => "idle",
                (None, _, true) => "schedule",
                (None, false, false) => {
                    return Err(Fail::Usage("missing --event (e.g. pull_request.opened, hook.Stop, agent.prompt_blocked, idle, schedule)".into()));
                }
            };
            let local_flag = LOCAL_FLAGS.iter().find(|f| a.has(f));
            let source = match a.get("source") {
                Some(src) => {
                    if let (true, Some(f)) = (src != "local", local_flag) {
                        return Err(Fail::Usage(format!("--{f} is for local triggers; use --source local")));
                    }
                    src
                }
                None if local_flag.is_some() || is_local_event(event) => "local",
                None => "github",
            };
            let action = action_from(a, a.get("project").map(str::to_string), None)?.ok_or_else(|| {
                Fail::Usage(if source == "local" {
                    "pick an action: --send TEXT (repeatable), --set-status LABEL --color C --base B, --clear-status, --notify TITLE, \
                     --attention MSG, --run CMD --project P, --agent claude|codex --prompt T --project P, or --action-json JSON"
                        .into()
                } else {
                    "pick an action: --agent claude|codex --prompt T, --run CMD, --attention MSG, or --notify TITLE".into()
                })
            })?;
            let (filter, _) = filter_from(a, json!({}))?;
            let p = json!({
                "name": name, "source": source, "event": event, "filter": filter, "action": action,
                "github_hook_id": a.num::<u64>("hook-id")?, "session_name_template": a.get("session-name"),
                "cooldown_secs": cooldown(a)?, "enabled": a.has("enable"),
            });
            let v = call("trigger.add", p)?;
            out(&v, &|v| {
                println!("added {} ({})", s(v, "id"), s(v, "state"));
                match (v["source"].as_str(), v["state"].as_str()) {
                    (_, Some("active")) => {}
                    (Some("local"), _) => println!("next: `midna triggers enable {}` when the human wants it on (or add with --enable)", s(v, "id")),
                    _ => println!("next: a human runs `midna triggers set-secret {}` then `midna triggers enable {}`", s(v, "id"), s(v, "id")),
                }
            });
        }
        "update" | "edit" => {
            a.check(&ADD_FLAGS.iter().copied().filter(|f| *f != "enable").collect::<Vec<_>>())?;
            let id = a.need(2, "trigger id")?;
            let cur = find(id)?;
            let project = a.get("project").map(str::to_string).or_else(|| cur["action"]["project_id"].as_str().map(str::to_string));
            let (filter, touched) = filter_from(a, cur["filter"].clone())?;
            let event = a.get("event").or(if a.has("idle-for") && cur["event"] != "idle" {
                Some("idle")
            } else if a.get("cron").is_some_and(|c| !c.trim().is_empty()) && cur["event"] != "schedule" {
                Some("schedule")
            } else {
                None
            });
            let mut p = json!({ "id": id, "name": a.get("name"), "event": event, "source": a.get("source"),
                "action": action_from(a, project, Some(&cur["action"]))?, "github_hook_id": a.num::<u64>("hook-id")?,
                "session_name_template": a.get("session-name"), "cooldown_secs": cooldown(a)? });
            if touched {
                p["filter"] = filter;
            }
            let v = call("trigger.update", p)?;
            out(&v, &print_trigger);
        }
        verb @ ("enable" | "disable" | "pause") => {
            let id = a.need(2, "trigger id")?;
            let v = call("trigger.set_enabled", json!({ "id": id, "enabled": verb == "enable" }))?;
            out(&v, &|v| println!("{} is {}", s(v, "id"), s(v, "state")));
        }
        "set-secret" => {
            a.check(&[])?;
            let id = a.need(2, "trigger id")?.to_string();
            let secret = read_secret(&id)?;
            if secret.is_empty() {
                return Err(Fail::Usage("empty secret".into()));
            }
            let v = call("trigger.set_secret", json!({ "id": id, "secret": secret }))?;
            out(&v, &|_| println!("secret saved for {id}"));
        }
        "remove" | "rm" => {
            let id = a.need(2, "trigger id")?;
            let v = call("trigger.remove", json!({ "id": id }))?;
            out(&v, &|_| println!("removed {id}"));
        }
        "deliveries" => {
            a.check(&["trigger", "limit"])?;
            let v = call("trigger.deliveries", json!({ "trigger_id": a.get("trigger").or(a.pos.get(2).map(String::as_str)), "limit": a.num::<u32>("limit")? }))?;
            out(&v, &print_deliveries);
        }
        "replay" => {
            let id = a.need(2, "delivery id")?;
            let v = call("trigger.replay", json!({ "delivery_id": id }))?;
            out(&v, &print_delivery);
        }
        "test" => {
            a.check(&["payload", "event", "session"])?;
            let id = a.need(2, "trigger id")?;
            let raw = match a.get("payload") {
                Some(inline) if inline.trim_start().starts_with(['{', '[']) => inline.to_string(),
                None if std::io::stdin().is_terminal() => "{}".into(),
                None | Some("-") => {
                    let mut b = String::new();
                    std::io::stdin().read_to_string(&mut b).map_err(|e| Fail::Other(e.to_string()))?;
                    b
                }
                Some(path) => std::fs::read_to_string(path).map_err(|e| Fail::Usage(format!("--payload {path}: {e}")))?,
            };
            let raw = if raw.trim().is_empty() { "{}".to_string() } else { raw };
            let payload: Value = serde_json::from_str(&raw).map_err(|e| Fail::Usage(format!("payload must be JSON: {e}")))?;
            let v = call("trigger.test", json!({ "trigger_id": id, "payload": payload, "event": a.get("event"), "session": a.get("session") }))?;
            out(&v, &print_delivery);
        }
        other => return Err(Fail::Usage(format!("unknown triggers subcommand `{other}`"))),
    }
    Ok(())
}

pub fn webhooks(a: &Args, out: OutFn) -> Res {
    match a.pos.get(1).map(String::as_str).unwrap_or("status") {
        "status" => {
            let v = call("webhooks.status", json!({}))?;
            out(&v, &print_status);
        }
        "configure" => {
            a.check(&["port", "relay-url"])?;
            let path = a.need(2, "path (tailscale_funnel|self_relay|midna_relay|off)")?;
            let v = call("webhooks.configure", json!({ "path": path, "port": a.num::<u16>("port")?, "relay_url": a.get("relay-url") }))?;
            out(&v, &|v| {
                println!("{}", s(v, "message"));
                if let Some(u) = v["enable_url"].as_str() {
                    println!("enable Funnel: {u}");
                }
                print_status(&v["status"]);
            });
            if v["ok"].as_bool() != Some(true) {
                std::process::exit(1);
            }
        }
        "reconcile" => {
            let v = call("webhooks.reconcile", json!({}))?;
            out(&v, &|v| println!("recovered {} from {} hook(s){}", s(v, "recovered"), s(v, "hooks_checked"), v["error"].as_str().map(|e| format!(" ({e})")).unwrap_or_default()));
        }
        other => return Err(Fail::Usage(format!("unknown webhooks subcommand `{other}`"))),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok<T>(r: Result<T, Fail>) -> T {
        match r {
            Ok(v) => v,
            Err(Fail::Usage(m)) => panic!("usage error: {m}"),
            Err(_) => panic!("failed"),
        }
    }

    fn args(v: &[&str]) -> Args {
        Args::parse(v.iter().map(|s| s.to_string())).unwrap()
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("55m", 60), Ok(3300));
        assert_eq!(parse_duration("1h30m", 1), Ok(5400));
        assert_eq!(parse_duration("90s", 1), Ok(90));
        assert_eq!(parse_duration("2d", 1), Ok(172_800));
        assert_eq!(parse_duration("1H", 1), Ok(3600));
        assert_eq!(parse_duration("45", 60), Ok(2700));
        assert_eq!(parse_duration("45", 1), Ok(45));
        assert!(parse_duration("", 1).is_err());
        assert!(parse_duration("5x", 1).is_err());
        assert!(parse_duration("1h30", 1).is_err());
        assert!(parse_duration("m", 1).is_err());
    }

    #[test]
    fn matches() {
        assert_eq!(parse_match("message=*Compact first*"), Ok(("message".into(), "*Compact first*".into())));
        assert_eq!(parse_match("tool_input.command=git push*=x"), Ok(("tool_input.command".into(), "git push*=x".into())));
        assert_eq!(parse_match("message="), Ok(("message".into(), String::new())));
        assert!(parse_match("message").is_err());
        assert!(parse_match("=x").is_err());
    }

    #[test]
    fn local_events() {
        for e in ["hook.Stop", "agent.prompt_blocked", "session.status", "needs_you.raised", "idle", "schedule", "hook.*"] {
            assert!(is_local_event(e), "{e}");
        }
        for e in ["pull_request.opened", "push", "pullrequest:created", "project.created"] {
            assert!(!is_local_event(e), "{e}");
        }
    }

    #[test]
    fn send_steps_keep_order() {
        let a = args(&["triggers", "add", "--send", "/compact", "--send-no-enter", "draft", "--send", "{{last_prompt}}"]);
        let act = ok(action_from(&a, None, None)).unwrap();
        assert_eq!(
            act,
            json!({ "kind": "send_to_session", "steps": [
                { "text": "/compact", "enter": true }, { "text": "draft", "enter": false }, { "text": "{{last_prompt}}", "enter": true },
            ] })
        );
        assert_eq!(action_text(&act), r#"send: "/compact" → "draft" (no enter) → "{{last_prompt}}""#);
    }

    #[test]
    fn set_status_flags() {
        let a = args(&["t", "--set-status", "Prompt blocked", "--color", "amber", "--base", "needs_you"]);
        let act = ok(action_from(&a, None, None)).unwrap();
        assert_eq!(act, json!({ "kind": "set_status", "label": "Prompt blocked", "color": "amber", "base": "needs_you", "clear_on": "prompt" }));
        assert_eq!(action_text(&act), "status: Prompt blocked (amber, needs_you, clears on prompt)");
        assert!(action_from(&args(&["t", "--set-status", "X", "--color", "mauve", "--base", "idle"]), None, None).is_err());
        assert!(action_from(&args(&["t", "--set-status", "X", "--color", "#ff8800"]), None, None).is_err(), "base required");
        assert!(action_from(&args(&["t", "--set-status", "X", "--color", "#ff8800", "--base", "exited"]), None, None).is_err());
        // update: modifiers alone edit the current set_status
        let edited = ok(action_from(&args(&["t", "--color", "#ff8800", "--clear-on", "turn"]), None, Some(&act))).unwrap();
        assert_eq!(edited["color"], "#ff8800");
        assert_eq!(edited["clear_on"], "turn");
        assert_eq!(edited["label"], "Prompt blocked");
        assert!(action_from(&args(&["t", "--color", "red"]), None, None).is_err());
        assert!(action_from(&args(&["t", "--color", "red", "--send", "x"]), None, Some(&act)).is_err());
    }

    #[test]
    fn one_action_only() {
        assert!(action_from(&args(&["t", "--send", "x", "--clear-status"]), None, None).is_err());
        assert!(action_from(&args(&["t", "--attention", "x", "--set-status", "y"]), None, None).is_err());
        assert_eq!(ok(action_from(&args(&["t", "--clear-status"]), None, None)), Some(json!({ "kind": "clear_status" })));
        let n = ok(action_from(&args(&["t", "--notify", "{{session.name}} is blocked", "--notify-body", "{{message}}", "--silent"]), None, None));
        assert_eq!(n, Some(json!({ "kind": "notify", "title": "{{session.name}} is blocked", "body": "{{message}}", "sound": false })));
        assert_eq!(action_text(&n.unwrap()), r#"notify: "{{session.name}} is blocked" — "{{message}}" (silent)"#);
        assert!(action_from(&args(&["t", "--notify", "x", "--send", "y"]), None, None).is_err());
        assert!(action_from(&args(&["t", "--notify-body", "x"]), None, None).is_err());
        let raw = ok(action_from(&args(&["t", "--action-json", r#"{"kind":"clear_status"}"#]), None, None));
        assert_eq!(raw, Some(json!({ "kind": "clear_status" })));
        assert!(action_from(&args(&["t", "--action-json", r#"{"steps":[]}"#]), None, None).is_err());
        assert!(action_from(&args(&["t", "--agent", "claude"]), Some("p".into()), None).is_err(), "start_agent needs --prompt");
    }

    #[test]
    fn local_filters() {
        let a = args(&["t", "--session", "s_1", "--in-project", "p_1", "--for-agent", "claude", "--idle-for", "1h", "--match", "message=*Compact*", "--match", "hook=UserPromptSubmit"]);
        let (f, touched) = ok(filter_from(&a, json!({})));
        assert!(touched);
        assert_eq!(f, json!({ "session": "s_1", "project": "p_1", "agent": "claude", "idle_minutes": 60, "match": { "message": "*Compact*", "hook": "UserPromptSubmit" } }));
        assert_eq!(filter_text(&f), "session=s_1 project=p_1 agent=claude idle=60m match hook=UserPromptSubmit message=*Compact*");
        // update: merge match, empty glob removes, empty value clears
        let a = args(&["t", "--match", "hook=", "--match", "prompt=*deploy*", "--session", ""]);
        let (f2, _) = ok(filter_from(&a, f));
        assert_eq!(f2["match"], json!({ "message": "*Compact*", "prompt": "*deploy*" }));
        assert!(f2["session"].is_null());
        assert!(filter_from(&args(&["t", "--idle-for", "90s"]), json!({})).is_err());
        assert!(filter_from(&args(&["t", "--for-agent", "gpt"]), json!({})).is_err());
        let (f3, _) = ok(filter_from(&args(&["t", "--filter-json", r#"{"match":{"message":"*x y*"}}"#, "--idle-for", "55"]), json!({ "repo": "a/b" })));
        assert_eq!(f3, json!({ "match": { "message": "*x y*" }, "idle_minutes": 55 }));
        assert_eq!(filter_text(&f3), r#"idle=55m match message="*x y*""#);
        let (f5, _) = ok(filter_from(&args(&["t", "--cron", "0 9 * * mon-fri", "--in-project", "p_1"]), json!({})));
        assert_eq!(f5, json!({ "cron": "0 9 * * mon-fri", "project": "p_1" }));
        assert_eq!(filter_text(&f5), r#"project=p_1 cron="0 9 * * mon-fri""#);
        assert!(matches!(filter_from(&args(&["t", "--cron", "0 9 * *"]), json!({})), Err(Fail::Usage(e)) if e.contains("cron needs 5")));
        let (fe, _) = ok(filter_from(&args(&["t", "--cron", "@every 55m"]), json!({})));
        assert_eq!(fe, json!({ "cron": "@every 55m" }));
        assert!(matches!(filter_from(&args(&["t", "--cron", "@every 30s"]), json!({})), Err(Fail::Usage(e)) if e.contains("whole minutes")));
        let (fw, _) = ok(filter_from(&args(&["t", "--cron", "*/5 * * * mon-fri", "--between", "13:00-17:00", "--ends", "2026-10-10", "--max-runs", "3"]), json!({})));
        assert_eq!((fw["window"].clone(), fw["max_runs"].clone()), (json!({ "from": "13:00", "until": "17:00" }), json!(3)));
        assert_eq!(midna_proto::time::parse_rfc3339(fw["ends_at"].as_str().unwrap()), Some(midna_proto::time::local_unix(2026, 10, 10, 0, 0)));
        assert!(filter_text(&fw).contains("between=13:00-17:00") && filter_text(&fw).contains("ends=\"2026-10-10 00:00\"") && filter_text(&fw).contains("max-runs=3"), "{}", filter_text(&fw));
        let (cleared, _) = ok(filter_from(&args(&["t", "--between", "", "--ends", "", "--max-runs", ""]), fw));
        assert!(cleared["window"].is_null() && cleared["ends_at"].is_null() && cleared["max_runs"].is_null());
        for bad in [["--between", "1pm-5pm"], ["--between", "13:00"], ["--starts", "tomorrow"], ["--max-runs", "0"]] {
            assert!(matches!(filter_from(&args(&["t", bad[0], bad[1]]), json!({})), Err(Fail::Usage(_))), "{bad:?}");
        }
        let (f6, _) = ok(filter_from(&args(&["t", "--cron", ""]), f5));
        assert!(f6["cron"].is_null());
        let (f4, touched) = ok(filter_from(&args(&["t"]), json!({ "repo": "a/b" })));
        assert!(!touched);
        assert_eq!(f4, json!({ "repo": "a/b" }));
    }
}
