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

fn action_text(a: &Value) -> String {
    match a["kind"].as_str() {
        Some("start_agent") => format!("start {} in {}", s(a, "agent"), s(a, "project_id")),
        Some("run_command") => format!("run `{}` in {}", s(a, "command"), s(a, "project_id")),
        Some("attention") => format!("attention: {}", s(a, "message")),
        _ => a.to_string(),
    }
}

fn filter_text(f: &Value) -> String {
    ["repo", "branch", "action", "label"].iter().filter_map(|k| f.get(*k).and_then(Value::as_str).map(|v| format!("{k}={v}"))).collect::<Vec<_>>().join(" ")
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
        rows.push(vec![s(&t, "id"), s(&t, "state"), s(&t, "name"), s(&t, "source"), s(&t, "event"), filter_text(&t["filter"]), action_text(&t["action"]), fired]);
    }
    table(rows);
}

fn print_trigger(t: &Value) {
    println!("{}  {}  ({})", s(t, "id"), s(t, "name"), s(t, "state"));
    println!("  on      {} {} {}", s(t, "source"), s(t, "event"), filter_text(&t["filter"]));
    println!("  does    {}", action_text(&t["action"]));
    if let Some(p) = t["action"]["prompt_template"].as_str() {
        println!("  prompt  {p}");
    }
    let secret = if t["secret_set"].as_bool() == Some(true) { format!("set {} ({})", s(t, "secret_set_at"), s(t, "secret_store")) } else { "not set".into() };
    println!("  secret  {secret}");
    match t["state"].as_str() {
        Some("needs_secret") => println!("  next    a human runs `midna triggers set-secret {}`", s(t, "id")),
        Some("draft") => println!("  next    a human runs `midna triggers enable {}`", s(t, "id")),
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

/// Build a TriggerAction from flags, if any action flag was given.
fn action_from(a: &Args, project: Option<String>) -> Result<Option<Value>, Fail> {
    let need_project = || project.clone().ok_or_else(|| Fail::Usage("--project is required for this action".into()));
    if let Some(agent) = a.get("agent") {
        let prompt = a.get("prompt").ok_or_else(|| Fail::Usage("--agent needs --prompt TEMPLATE".into()))?;
        return Ok(Some(json!({ "kind": "start_agent", "project_id": need_project()?, "agent": agent, "prompt_template": prompt })));
    }
    if let Some(cmd) = a.get("run") {
        return Ok(Some(json!({ "kind": "run_command", "project_id": need_project()?, "command": cmd })));
    }
    if let Some(msg) = a.get("attention") {
        return Ok(Some(json!({ "kind": "attention", "message": msg })));
    }
    Ok(None)
}

fn filter_from(a: &Args, base: Value) -> (Value, bool) {
    let mut f = if base.is_object() { base } else { json!({}) };
    let mut touched = false;
    for (flag, key) in [("repo", "repo"), ("branch", "branch"), ("action", "action"), ("label", "label")] {
        if let Some(v) = a.get(flag) {
            touched = true;
            f[key] = if v.is_empty() { Value::Null } else { json!(v) };
        }
    }
    (f, touched)
}

const ADD_FLAGS: &[&str] = &["name", "source", "event", "repo", "branch", "action", "label", "agent", "prompt", "run", "attention", "project", "hook-id", "session-name"];

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
            let event = a.get("event").ok_or_else(|| Fail::Usage("missing --event (e.g. pull_request.opened)".into()))?;
            let action = action_from(a, a.get("project").map(str::to_string))?.ok_or_else(|| Fail::Usage("pick an action: --agent claude|codex --prompt T, --run CMD, or --attention MSG".into()))?;
            let (filter, _) = filter_from(a, json!({}));
            let p = json!({
                "name": name, "source": a.get("source").unwrap_or("github"), "event": event, "filter": filter, "action": action,
                "github_hook_id": a.num::<u64>("hook-id")?, "session_name_template": a.get("session-name"),
            });
            let v = call("trigger.add", p)?;
            out(&v, &|v| {
                println!("added {} ({})", s(v, "id"), s(v, "state"));
                println!("next: a human runs `midna triggers set-secret {}` then `midna triggers enable {}`", s(v, "id"), s(v, "id"));
            });
        }
        "update" | "edit" => {
            a.check(ADD_FLAGS)?;
            let id = a.need(2, "trigger id")?;
            let cur = find(id)?;
            let project = a.get("project").map(str::to_string).or_else(|| cur["action"]["project_id"].as_str().map(str::to_string));
            let (filter, touched) = filter_from(a, cur["filter"].clone());
            let mut p = json!({ "id": id, "name": a.get("name"), "event": a.get("event"), "source": a.get("source"),
                "action": action_from(a, project)?, "github_hook_id": a.num::<u64>("hook-id")?, "session_name_template": a.get("session-name") });
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
            a.check(&["payload", "event"])?;
            let id = a.need(2, "trigger id")?;
            let raw = match a.get("payload") {
                None | Some("-") => {
                    let mut b = String::new();
                    std::io::stdin().read_to_string(&mut b).map_err(|e| Fail::Other(e.to_string()))?;
                    b
                }
                Some(path) => std::fs::read_to_string(path).map_err(|e| Fail::Usage(format!("--payload {path}: {e}")))?,
            };
            let payload: Value = serde_json::from_str(&raw).map_err(|e| Fail::Usage(format!("payload must be JSON: {e}")))?;
            let v = call("trigger.test", json!({ "trigger_id": id, "payload": payload, "event": a.get("event") }))?;
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
