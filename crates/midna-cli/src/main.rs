//! `midna`: the CLI for agents and humans. Every verb maps to a catalog method; `midna call`
//! reaches any method directly. Exit codes: 0 ok, 1 refused/failed, 2 bad args, 3 daemon unreachable.
mod args;
mod guide;
mod help;
mod hook;
mod mcp;
mod print;
mod queue;
mod secret;
mod shim;
mod triggers;

use args::{ArgError, Args};
use midna_proto::client::Notification;
use midna_proto::error::{BAD_PARAMS, PENDING, UNKNOWN_METHOD};
use midna_proto::{Client, ClientError, RpcError};
use serde_json::{Value, json};

pub enum Fail {
    Usage(String),
    Unreachable(String),
    Rpc(RpcError),
    Other(String),
    /// Still waiting on the human (`needs wait` timed out); exit 4.
    Pending(String),
}

/// `--no-wait`: calls that need an approval return its needs-you id instead of blocking.
static NO_WAIT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

impl From<ArgError> for Fail {
    fn from(e: ArgError) -> Self {
        Fail::Usage(e.0)
    }
}

impl From<ClientError> for Fail {
    fn from(e: ClientError) -> Self {
        match e {
            ClientError::Io(e) => Fail::Unreachable(e.to_string()),
            ClientError::Rpc(e) => Fail::Rpc(e),
            ClientError::Decode(e) => Fail::Other(e),
        }
    }
}

impl Fail {
    fn code(&self) -> i32 {
        match self {
            Fail::Usage(_) => 2,
            Fail::Unreachable(_) => 3,
            Fail::Rpc(e) if e.code == BAD_PARAMS || e.code == UNKNOWN_METHOD => 2,
            Fail::Rpc(e) if e.code == PENDING => 4,
            Fail::Pending(_) => 4,
            Fail::Rpc(_) | Fail::Other(_) => 1,
        }
    }
}

pub type Res = Result<(), Fail>;

/// Prints JSON with `--json`, else calls the human printer.
type OutFn<'a> = &'a dyn Fn(&Value, &dyn Fn(&Value));

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    // `claude` in a midna shell: every argument is Claude's, so none are parsed here.
    if raw.first().map(String::as_str) == Some("shim") {
        shim::run(&raw[1..]);
    }
    // `queue` gives a few flags values (`--idle 10m`) that are booleans elsewhere (`restart --idle`).
    let args = match Args::parse(raw.clone()) {
        Ok(a) if a.pos.first().map(String::as_str) == Some("queue") => Args::parse_with(raw, queue::VALUE_FLAGS),
        r => r,
    };
    let args = match args {
        Ok(a) => a,
        Err(e) => finish(Err(e.into()), false),
    };
    let json = args.has("json");
    if let Some(s) = args.get("socket") {
        // SAFETY: single-threaded at this point.
        unsafe { std::env::set_var("MIDNA_SOCKET", s) };
    }
    NO_WAIT.store(args.has("no-wait"), std::sync::atomic::Ordering::Relaxed);
    finish(run(&args), json);
}

fn finish(r: Res, json: bool) -> ! {
    match r {
        Ok(()) => std::process::exit(0),
        Err(f) => {
            let code = f.code();
            let msg = match &f {
                Fail::Usage(m) => format!("{m}\nrun `midna help` for usage"),
                Fail::Unreachable(m) => format!("midnad is not reachable at {}: {m}", midna_proto::paths::socket_path().display()),
                Fail::Rpc(e) => e.message.clone(),
                Fail::Other(m) | Fail::Pending(m) => m.clone(),
            };
            // `--no-wait`: the needs-you id alone on stdout, for scripts (`id=$(midna close X --force --no-wait)`).
            let pending = match &f {
                Fail::Rpc(e) if e.code == PENDING => e.data.as_ref().and_then(|d| d["needs_you_id"].as_str()),
                _ => None,
            };
            if let (Some(id), false) = (pending, json) {
                println!("{id}");
            }
            if json {
                let err = match &f {
                    Fail::Rpc(e) => json!({ "error": e }),
                    _ => json!({ "error": { "message": msg } }),
                };
                println!("{err}");
            } else {
                eprintln!("midna: {msg}");
            }
            std::process::exit(code)
        }
    }
}

pub fn connect() -> Result<Client, Fail> {
    Client::connect_default().map_err(|e| Fail::Unreachable(e.to_string()))
}

pub(crate) fn call(method: &str, params: Value) -> Result<Value, Fail> {
    let mut c = connect()?;
    if NO_WAIT.load(std::sync::atomic::Ordering::Relaxed) {
        c = c.no_wait();
    }
    match c.call_value(method, params.clone()) {
        Ok(v) => Ok(v),
        Err(ClientError::Rpc(e)) => Err(Fail::Rpc(guide::with_next_step(method, &params, e, guide::Surface::Cli))),
        Err(e) => Err(e.into()),
    }
}

fn daemon(a: &Args, out: OutFn) -> Res {
    let sub = a.need(1, "daemon subcommand (upgrade|restart|stop|info)")?;
    let scheduled = |v: &Value| println!("{} terminal(s) carried over to {}; watch `midna events --kind daemon.`", v["sessions"], v["binary"].as_str().unwrap_or(""));
    match sub {
        "info" => out(&call("daemon.info", json!({}))?, &print::kv),
        "upgrade" => {
            let path = a.need(2, "path to the new midnad binary")?;
            let abs = std::fs::canonicalize(path).map_err(|e| Fail::Usage(format!("{path}: {e}")))?;
            out(&call("daemon.upgrade", json!({ "binary_path": abs }))?, &scheduled);
        }
        "restart" => out(&call("daemon.restart", json!({}))?, &scheduled),
        "stop" => out(&call("daemon.stop", json!({}))?, &|_| println!("midnad is stopping")),
        "reset" => {
            a.check(&["drop-rules"])?;
            let v = call("daemon.reset", json!({ "keep_rules": !a.has("drop-rules") }))?;
            out(&v, &print::kv);
        }
        other => return Err(Fail::Usage(format!("unknown daemon subcommand `{other}`"))),
    }
    Ok(())
}

fn run(a: &Args) -> Res {
    // `--help` on any verb prints usage; it must never run the verb (`open --help` used to
    // open a terminal).
    let first = a.pos.first().map(String::as_str).unwrap_or("help");
    if a.has("help") || first == "help" || first == "--help" {
        let topic = if first == "help" || first == "--help" { a.pos.get(1).map(String::as_str) } else { Some(first) };
        return match topic.map(|t| help::verb(t).ok_or(t)) {
            None => {
                println!("{}", help::usage());
                Ok(())
            }
            Some(Ok(v)) => {
                println!("{}", help::verb_help(v));
                Ok(())
            }
            Some(Err(t)) => Err(Fail::Usage(format!("unknown verb `{t}`"))),
        };
    }
    // Aliases resolve to the canonical verb name.
    let verb = help::verb(first).map(|v| v.name).unwrap_or(first);
    let json = a.has("json");
    let out = |v: &Value, human: &dyn Fn(&Value)| {
        if json {
            println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
        } else {
            human(v);
        }
    };
    match verb {
        "capabilities" => {
            if json {
                println!("{}", serde_json::to_string_pretty(&guide::capabilities_json()).unwrap_or_default());
            } else {
                println!("{}", guide::capabilities());
            }
            Ok(())
        }
        "skill" => {
            if a.has("path") {
                let home = std::env::var("MIDNA_SKILL").ok().filter(|s| !s.is_empty());
                match home {
                    Some(p) => println!("{p}"),
                    None => return Err(Fail::Other("MIDNA_SKILL is not set (not in a midna terminal); `midna skill` prints the guide".into())),
                }
            } else {
                print!("{}", guide::SKILL_MD);
            }
            Ok(())
        }
        "explain" => {
            let mut words: Vec<String> = a.pos[1..].to_vec();
            words.extend(a.rest.iter().cloned());
            let mut client: Option<Client> = None;
            let mut caller = |m: &str, p: Value| -> Result<Value, RpcError> {
                if client.is_none() {
                    client = Some(Client::connect_default().map_err(|e| RpcError::internal(format!("midnad is not reachable: {e}")))?);
                }
                client.as_mut().unwrap().call_value(m, p).map_err(|e| match e {
                    ClientError::Rpc(e) => e,
                    other => RpcError::internal(other.to_string()),
                })
            };
            let text = guide::explain(&mut caller, &words, guide::Surface::Cli).map_err(|e| match e.code {
                BAD_PARAMS => Fail::Usage(e.message),
                _ if e.message.starts_with("midnad is not reachable") => Fail::Unreachable(e.message),
                _ => Fail::Rpc(e),
            })?;
            out(&json!({ "text": text }), &|_| println!("{text}"));
            Ok(())
        }
        "projects" => projects(a, &out),
        "restart" => {
            a.check(&["idle", "fresh", "force", "cancel", "decline", "reason"])?;
            let id = a.need(1, "terminal id")?;
            if a.has("cancel") {
                let v = call("session.restart_cancel", json!({ "id": id }))?;
                out(&v, &|_| println!("cancelled the queued restart of {id}"));
                return Ok(());
            }
            if a.has("decline") {
                let v = call("session.update_decline", json!({ "id": id }))?;
                out(&v, &|_| println!("not now: {id}'s update prompt stays hidden until a newer update"));
                return Ok(());
            }
            let mut p = json!({ "id": id, "when": if a.has("idle") { "idle" } else { "now" }, "force": a.has("force") });
            if a.has("fresh") {
                p["resume"] = json!(false);
            }
            if let Some(r) = a.get("reason") {
                p["reason"] = json!(r);
            }
            let v = call("session.restart", p)?;
            out(&v, &|v| match v.pointer("/agent_info/restart") {
                Some(q) => {
                    let waiting: Vec<String> = q["waiting_for"].as_array().map(|w| w.iter().map(print::plain).collect()).unwrap_or_default();
                    println!("restart of {id} queued; it runs when the agent is idle");
                    for w in waiting {
                        println!("  waiting for: {w}");
                    }
                }
                None => println!("restarted {} (pid {})", id, print::plain(&v["pid"])),
            });
            Ok(())
        }
        "subagent" | "subagents" => {
            let id = a.need(1, "terminal id")?;
            let agent = a.need(2, "agent id")?;
            let v = call("session.subagent_log", json!({ "id": id, "agent": agent }))?;
            out(&v, &|_| {
                let ag = &v["agent"];
                let state = if v["running"].as_bool() == Some(true) { "running" } else { "finished" };
                println!("{} ({}, {state}): {}", agent, print::plain(&ag["agent_type"]), ag["description"].as_str().unwrap_or(""));
                for e in v["entries"].as_array().into_iter().flatten() {
                    let text = e["text"].as_str().unwrap_or("");
                    match e["kind"].as_str().unwrap_or("") {
                        "prompt" => println!("\n> {}\n", text.replace('\n', "\n  ")),
                        "tool" => println!("⏺ {text}"),
                        "result" | "error" => println!("  ⎿ {text}"),
                        _ => println!("⏺ {}", text.replace('\n', "\n  ")),
                    }
                }
            });
            Ok(())
        }
        "procs" | "processes" => {
            let id = a.need(1, "terminal id")?;
            let procs = call("session.processes", json!({ "id": id }))?;
            let s = call("session.get", json!({ "id": id }))?;
            out(&json!({ "processes": procs, "agent_info": s["agent_info"] }), &|_| {
                for p in procs.as_array().into_iter().flatten() {
                    let depth = p["depth"].as_u64().unwrap_or(0) as usize;
                    let cmd = p["command"].as_str().filter(|c| !c.is_empty()).or(p["name"].as_str()).unwrap_or("");
                    let task = p["task_id"].as_str().map(|t| format!("  [task {t}]")).unwrap_or_default();
                    println!("{:>7} {:>7}  {}{}{}", print::plain(&p["pid"]), print::plain(&p["pgid"]), "  ".repeat(depth), cmd, task);
                }
                let info = &s["agent_info"];
                let list = |k: &str| info[k].as_array().cloned().unwrap_or_default();
                for t in list("background") {
                    let what = t["command"].as_str().or(t["name"].as_str()).or(t["agent_type"].as_str()).or(t["description"].as_str()).unwrap_or("");
                    println!("background {} {} ({}): {}", print::plain(&t["kind"]), print::plain(&t["id"]), print::plain(&t["status"]), what);
                }
                for x in list("subagents") {
                    let bg = if x["background"].as_bool() == Some(true) { ", background" } else { "" };
                    println!("subagent {} ({}{bg}): {}", print::plain(&x["id"]), print::plain(&x["agent_type"]), x["description"].as_str().unwrap_or(""));
                }
                for x in list("finished_subagents") {
                    println!("subagent {} ({}) finished: {}", print::plain(&x["id"]), print::plain(&x["agent_type"]), x["description"].as_str().unwrap_or(""));
                }
                for c in list("crons") {
                    let cron: midna_proto::AgentCron = serde_json::from_value(c.clone()).unwrap_or_default();
                    let when = if cron.recurring { "repeats" } else { "once" };
                    let ends = cron.expires_at().map(|t| format!(", expires {}", midna_proto::cron::local_label(t))).unwrap_or_default();
                    let words = midna_proto::cron::describe(&cron.schedule);
                    println!("scheduled {} {} ({when}: {words}{ends}): {}", print::plain(&c["id"]), print::plain(&c["schedule"]), print::plain(&c["prompt"]));
                }
                if let Some(q) = info.get("restart").filter(|q| q.is_object()) {
                    println!("restart queued: {}", print::plain(&q["reason"]));
                }
            });
            Ok(())
        }
        "links" => links(a, &out),
        "queue" => queue::queue(a, &out),
        "notify" | "notifications" => notify(a, &out),
        "key" => {
            let id = a.need(1, "terminal id")?;
            if a.pos.len() < 3 {
                return Err(Fail::Usage("missing key (e.g. ctrl-c, escape, enter, up)".into()));
            }
            for k in &a.pos[2..] {
                call("session.key", json!({ "id": id, "key": k }))?;
            }
            out(&json!({ "ok": true }), &|_| {});
            Ok(())
        }
        "info" => {
            let v = call("daemon.info", json!({}))?;
            out(&v, &print::kv);
            Ok(())
        }
        "daemon" => daemon(a, &out),
        "schema" => schema(a),
        "list" | "ls" => {
            a.check(&["project"])?;
            let v = call("session.list", json!({ "project_id": a.get("project") }))?;
            out(&v, &print::sessions);
            Ok(())
        }
        "open" => open(a, &out),
        "close" => {
            a.check(&["force"])?;
            let id = a.need(1, "terminal id")?;
            let v = call("session.close", json!({ "id": id, "force": a.has("force") }))?;
            out(&v, &|_| println!("closed {id}"));
            Ok(())
        }
        "rename" => {
            let id = a.need(1, "terminal id")?;
            let name = a.pos[2..].join(" ");
            if name.is_empty() {
                return Err(Fail::Usage("missing new name".into()));
            }
            let v = call("session.rename", json!({ "id": id, "name": name }))?;
            out(&v, &|_| println!("renamed {id} to {name}"));
            Ok(())
        }
        "background" | "bg" => {
            a.check(&["off"])?;
            let id = a.need(1, "terminal id")?;
            let on = !a.has("off");
            let v = call("session.set_background", json!({ "id": id, "background": on }))?;
            out(&v, &|_| println!("{} {id}", if on { "backgrounded" } else { "foregrounded" }));
            Ok(())
        }
        "read" => {
            a.check(&["lines", "screen"])?;
            // `read --screen <id>`: the id was taken as --screen's value.
            let id = match (a.pos.get(1).map(String::as_str), a.get("screen")) {
                (Some(id), _) | (None, Some(id)) => id,
                (None, None) => return Err(Fail::Usage("missing terminal id".into())),
            };
            let v = call("session.read", json!({ "id": id, "lines": a.num::<u32>("lines")?, "screen": a.has("screen") }))?;
            out(&v, &|v| println!("{}", v["text"].as_str().unwrap_or("")));
            Ok(())
        }
        "prompts" => {
            a.check(&["jump"])?;
            let id = a.need(1, "terminal id")?;
            if let Some(to) = a.get("jump") {
                let params = match to.parse::<u32>() {
                    Ok(n) => json!({ "id": id, "n": n }),
                    Err(_) => json!({ "id": id, "to": to }),
                };
                let v = call("session.jump_prompt", params)?;
                out(&v, &|v| match (v["ok"].as_bool(), v["n"].as_u64()) {
                    (Some(true), Some(n)) => println!("at prompt {n}"),
                    (Some(true), None) => println!("back to live"),
                    _ => eprintln!("{}", v["reason"].as_str().unwrap_or("could not jump")),
                });
                return Ok(());
            }
            let v = call("session.prompts", json!({ "id": id }))?;
            out(&v, &print::prompts);
            Ok(())
        }
        "send" => {
            a.check(&["no-enter", "image"])?;
            let id = a.need(1, "terminal id")?;
            let text = a.text(2);
            // The daemon runs elsewhere: hand it absolute paths.
            let cwd = std::env::current_dir().unwrap_or_default();
            let images: Vec<String> = a.all("image").iter().map(|p| cwd.join(p).display().to_string()).collect();
            if text.is_empty() && images.is_empty() && a.has("no-enter") {
                return Err(Fail::Usage("nothing to send: give text, --image, or drop --no-enter".into()));
            }
            let v = call("session.input", json!({ "id": id, "text": text, "enter": !a.has("no-enter"), "images": images }))?;
            out(&v, &|_| {});
            Ok(())
        }
        "focus" => {
            let id = a.need(1, "terminal id")?;
            let v = call("session.focus", json!({ "id": id }))?;
            out(&v, &|v| if v["delivered"].as_u64() == Some(0) { eprintln!("(the midna GUI is not running)") });
            Ok(())
        }
        "attention" => {
            a.check(&["note", "detail"])?;
            let msg = a.text(1);
            if msg.is_empty() {
                return Err(Fail::Usage("missing message".into()));
            }
            let kind = if a.has("note") { "note" } else { "blocked" };
            let v = call("needs_you.raise", json!({ "kind": kind, "message": msg, "detail": a.get("detail") }))?;
            out(&v, &|v| println!("raised {} ({kind})", v["id"].as_str().unwrap_or("")));
            Ok(())
        }
        "needs" | "needs-you" => needs(a, &out),
        "get" => {
            a.check(&[])?;
            let id = a.need(1, "terminal id")?;
            let v = call("session.get", json!({ "id": id }))?;
            out(&v, &print::session);
            Ok(())
        }
        "approve" => approve(a, &out),
        "check" => {
            a.check(&[])?;
            let kind = a.need(1, "kind (command|tool|path|cli|window)")?;
            let value = a.text(2);
            let v = call("policy.check", json!({ "action": { "kind": kind, "value": value } }))?;
            out(&v, &print::check);
            Ok(())
        }
        "rules" | "rule" => rules(a, &out),
        "triggers" | "trigger" => triggers::triggers(a, &out),
        "webhooks" | "webhook" => triggers::webhooks(a, &out),
        "secret" | "secrets" => secret::secret(a, &out),
        "settings" | "setting" => settings(a, &out),
        "commands" => commands(a, &out),
        "updates" | "update" => updates(a, &out),
        "themes" | "theme" => themes(a, &out),
        "permissions" => permissions(a, &out),
        "hooks" => hooks(a, &out),
        "events" => events(a),
        "insights" => insights(a, &out),
        "usage" => {
            a.check(&[])?;
            out(&call("usage.get", json!({}))?, &print::usage);
            Ok(())
        }
        "window" if a.pos.get(1).map(String::as_str) == Some("list") => {
            let v = call("window.list", json!({}))?;
            out(&v, &print::kv);
            Ok(())
        }
        "window" => {
            let action = a.need(1, "window action")?;
            let value = a.pos.get(3).map(|v| serde_json::from_str::<Value>(v).unwrap_or_else(|_| json!(v)));
            let v = call("window.command", json!({ "action": action, "target": a.pos.get(2), "value": value }))?;
            out(&v, &|v| println!("delivered to {} GUI window(s)", v["delivered"]));
            Ok(())
        }
        "hook" => hook::run(a),
        "mcp" => mcp::run(),
        "call" => {
            let method = a.need(1, "method")?;
            if method == "secret.exec_env" {
                return Err(Fail::Usage("secret.exec_env is internal to `midna secret exec NAME -- <command>`; values are never printed".into()));
            }
            let params: Value = match a.pos.get(2) {
                Some(p) => serde_json::from_str(p).map_err(|e| Fail::Usage(format!("params must be JSON: {e}")))?,
                None => json!({}),
            };
            let v = call(method, params)?;
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            Ok(())
        }
        other => Err(Fail::Usage(format!("unknown verb `{other}`"))),
    }
}

fn open(a: &Args, out: OutFn) -> Res {
    a.check(&["agent", "prompt", "resume", "monitor", "name", "project", "cwd", "background", "close-on-exit"])?;
    let cwd = match a.get("cwd") {
        Some(c) => Some(c.to_string()),
        None if a.get("project").is_none() => std::env::current_dir().ok().map(|p| p.to_string_lossy().into_owned()),
        None => None,
    };
    let mut p = json!({ "project_id": a.get("project"), "name": a.get("name"), "cwd": cwd, "background": a.has("background"), "close_on_exit": a.has("close-on-exit") });
    if let Some(agent) = a.get("agent") {
        p["kind"] = json!("agent");
        p["agent"] = json!(agent);
        let prompt = a.get("prompt").map(str::to_string).or_else(|| (a.pos.len() > 1).then(|| a.pos[1..].join(" ")));
        p["prompt"] = json!(prompt);
        // Everything after `--` is the agent's own arguments.
        if !a.rest.is_empty() {
            p["agent_args"] = json!(a.rest);
        }
        if let Some(r) = a.get("resume") {
            p["resume"] = json!(r);
        }
    } else if a.has("resume") {
        return Err(Fail::Usage("--resume needs --agent claude|codex".into()));
    } else if let Some(m) = a.get("monitor") {
        p["kind"] = json!("monitor");
        p["command"] = json!([m]);
    } else {
        p["kind"] = json!("shell");
        if !a.rest.is_empty() {
            p["command"] = json!(a.rest);
        }
    }
    let v = call("session.open", p)?;
    out(&v, &|v| println!("{}", v["id"].as_str().unwrap_or("")));
    Ok(())
}

/// `needs` (list), `needs get <id>`, `needs wait <id> [--timeout S]`.
fn needs(a: &Args, out: OutFn) -> Res {
    match a.pos.get(1).map(String::as_str) {
        None | Some("list" | "ls") => {
            a.check(&[])?;
            let v = call("needs_you.list", json!({}))?;
            out(&v, &print::needs_you);
        }
        Some(sub @ ("get" | "wait")) => {
            a.check(&["timeout"])?;
            let id = a.need(2, "needs-you id")?;
            let v = if sub == "get" {
                call("needs_you.get", json!({ "id": id }))?
            } else {
                // The daemon waits at most 600 s per call; ask again until the timeout.
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(a.num::<u64>("timeout")?.unwrap_or(300));
                loop {
                    let left = deadline.saturating_duration_since(std::time::Instant::now()).as_secs().clamp(1, 600);
                    let v = call("needs_you.get", json!({ "id": id, "wait_secs": left }))?;
                    if v["state"] != "open" || std::time::Instant::now() >= deadline {
                        break v;
                    }
                }
            };
            out(&v, &print::needs_you_state);
            if sub == "wait" && v["state"] == "open" {
                return Err(Fail::Pending(format!("{id} is still waiting on the human")));
            }
        }
        Some(other) => return Err(Fail::Usage(format!("unknown needs subcommand `{other}` (get|wait)"))),
    }
    Ok(())
}

fn parse_scope(s: &str) -> Result<Value, Fail> {
    Ok(match s {
        "once" => json!({ "kind": "once" }),
        "session" => json!({ "kind": "session" }),
        "always" => json!({ "kind": "always" }),
        m => {
            let n: u32 = m.trim_end_matches(['m', 'i', 'n']).parse().map_err(|_| Fail::Usage(format!("bad scope `{m}` (once|session|always|<N>m)")))?;
            json!({ "kind": "minutes", "minutes": n })
        }
    })
}

fn approve(a: &Args, out: OutFn) -> Res {
    a.check(&["scope", "deny", "done", "restart", "dismiss"])?;
    let id = a.need(1, "needs-you id")?;
    let resolution = if a.has("deny") {
        json!({ "kind": "deny" })
    } else if a.has("done") {
        json!({ "kind": "done" })
    } else if a.has("restart") {
        json!({ "kind": "restart" })
    } else if a.has("dismiss") {
        json!({ "kind": "dismiss" })
    } else {
        json!({ "kind": "approve", "scope": parse_scope(a.get("scope").unwrap_or("once"))? })
    };
    let v = call("needs_you.resolve", json!({ "id": id, "resolution": resolution }))?;
    out(&v, &|v| match v.get("rule") {
        Some(r) if !r.is_null() => println!("approved; added rule {}", r["id"].as_str().unwrap_or("")),
        _ => println!("{}", ["deny", "done", "restart", "dismiss"].iter().find(|f| a.has(f)).map_or("approved", |f| match *f {
            "deny" => "denied",
            "done" => "marked done",
            "restart" => "restarted",
            _ => "dismissed",
        })),
    });
    Ok(())
}

fn parse_rule_scope(s: &str) -> Result<Value, Fail> {
    match s.split_once(':') {
        None if s == "global" => Ok(json!({ "kind": "global" })),
        Some((k @ ("project" | "session"), id)) => Ok(json!({ "kind": k, "id": id })),
        _ => Err(Fail::Usage(format!("bad scope `{s}` (global|project:ID|session:ID)"))),
    }
}

fn rules(a: &Args, out: OutFn) -> Res {
    match a.pos.get(1).map(String::as_str).unwrap_or("list") {
        "list" | "ls" => {
            let v = call("rule.list", json!({}))?;
            out(&v, &print::rules);
        }
        "add" => {
            a.check(&["scope", "expires"])?;
            let effect = a.need(2, "effect (allow|ask|deny)")?;
            let kind = a.need(3, "matcher kind (command|tool|path|cli|window)")?;
            let pattern = a.text(4);
            if pattern.is_empty() {
                return Err(Fail::Usage("missing pattern".into()));
            }
            let scope = match a.get("scope") {
                Some(s) => parse_rule_scope(s)?,
                None => json!({ "kind": "global" }),
            };
            let p = json!({ "effect": effect, "matcher": { "kind": kind, "pattern": pattern }, "scope": scope, "expires_in_secs": a.num::<u64>("expires")? });
            let v = call("rule.add", p)?;
            out(&v, &|v| println!("added {}", v["id"].as_str().unwrap_or("")));
        }
        "request-removal" => {
            a.check(&["reason"])?;
            let id = a.need(2, "rule id")?;
            let reason = a.get("reason").ok_or_else(|| Fail::Usage("--reason is required".into()))?;
            let v = call("rule.request_removal", json!({ "id": id, "reason": reason }))?;
            out(&v, &|v| println!("asked the human (needs-you {})", v["id"].as_str().unwrap_or("")));
        }
        "remove" | "rm" => {
            let id = a.need(2, "rule id")?;
            let v = call("rule.remove", json!({ "id": id }))?;
            out(&v, &|_| println!("removed {id}"));
        }
        "restore" => {
            let id = a.need(2, "rule id")?;
            // The rule as it was when removed, from the log.
            let ev = call("events.list", json!({ "limit": 10000, "filter": { "kinds": ["rule.removed"] } }))?;
            let rule = ev.as_array().and_then(|a| a.iter().rev().find(|e| e["data"]["id"] == id)).map(|e| e["data"].clone());
            let rule = rule.ok_or_else(|| Fail::Usage(format!("no removed rule {id} in the event log")))?;
            let v = call("rule.restore", json!({ "rule": rule }))?;
            out(&v, &|_| println!("restored {id}"));
        }
        other => return Err(Fail::Usage(format!("unknown rules subcommand `{other}`"))),
    }
    Ok(())
}

fn commands(a: &Args, out: OutFn) -> Res {
    match a.pos.get(1).map(String::as_str).unwrap_or("list") {
        "list" | "ls" => {
            let v = call("ui.commands.list", json!({}))?;
            out(&v, &|v| {
                for c in v["commands"].as_array().into_iter().flatten() {
                    println!("{:<28} {}", c["id"].as_str().unwrap_or(""), c["title"].as_str().unwrap_or(""));
                }
                for bad in v["invalid"].as_array().into_iter().flatten() {
                    println!("invalid {}", bad.as_str().unwrap_or(""));
                }
            });
        }
        "add" => {
            a.check(&["title", "sub", "keywords", "rpc", "params", "screen", "prefill", "focus", "icon", "featured", "danger", "id", "replace"])?;
            let title = a.get("title").ok_or_else(|| Fail::Usage("--title is required".into()))?;
            let run = if let Some(m) = a.get("rpc") {
                let params: Value = match a.get("params") {
                    Some(p) => serde_json::from_str(p).map_err(|e| Fail::Usage(format!("--params must be JSON: {e}")))?,
                    None => json!({}),
                };
                json!({ "kind": "rpc", "method": m, "params": params })
            } else if let Some(s) = a.get("screen") {
                json!({ "kind": "screen", "screen": s })
            } else if let Some(t) = a.get("prefill") {
                json!({ "kind": "prefill", "text": t })
            } else if let Some(f) = a.get("focus") {
                json!({ "kind": "focus", "session": f })
            } else {
                return Err(Fail::Usage("one of --rpc, --screen, --prefill or --focus is required".into()));
            };
            let cmd = json!({ "id": a.get("id").unwrap_or(""), "title": title, "sub": a.get("sub").unwrap_or(""),
                "keywords": a.get("keywords").unwrap_or(""), "icon": a.get("icon"), "featured": a.get("featured"),
                "danger": a.get("danger"), "run": run });
            let v = call("ui.commands.add", json!({ "command": cmd, "replace": a.has("replace") }))?;
            out(&v, &|v| println!("added {}", v["id"].as_str().unwrap_or("")));
        }
        "remove" | "rm" => {
            let id = a.need(2, "command id")?;
            let v = call("ui.commands.remove", json!({ "id": id }))?;
            out(&v, &|_| println!("removed {id}"));
        }
        other => return Err(Fail::Usage(format!("unknown commands subcommand `{other}`"))),
    }
    Ok(())
}

fn themes(a: &Args, out: OutFn) -> Res {
    match a.pos.get(1).map(String::as_str).unwrap_or("list") {
        "list" | "ls" => {
            let v = call("themes.list", json!({}))?;
            out(&v, &|v| {
                let showing = v["showing"].as_str().unwrap_or("");
                for t in v["themes"].as_array().into_iter().flatten() {
                    let id = t["id"].as_str().unwrap_or("");
                    let mark = if id == showing { "*" } else { " " };
                    let src = t["path"].as_str().unwrap_or("built-in");
                    println!("{mark} {id:<18} {:<5} {:<20} {src}", t["kind"].as_str().unwrap_or(""), t["name"].as_str().unwrap_or(""));
                }
                println!("theme = {} (dark: {}, light: {})", v["setting"].as_str().unwrap_or(""), v["dark"].as_str().unwrap_or(""), v["light"].as_str().unwrap_or(""));
                for e in v["errors"].as_array().into_iter().flatten() {
                    println!("error {}", e.as_str().unwrap_or(""));
                }
            });
        }
        "use" | "set" => {
            let id = a.need(2, "theme id (or system)")?;
            let v = call("settings.set", json!({ "key": "theme", "value": id }))?;
            out(&v, &|_| println!("theme = {id}"));
        }
        "format" => {
            let v = call("themes.list", json!({}))?;
            out(&json!({ "dir": v["dir"], "format": v["format"] }), &|v| {
                println!("{}\n\n{}", v["dir"].as_str().unwrap_or(""), v["format"].as_str().unwrap_or(""));
            });
        }
        other => return Err(Fail::Usage(format!("unknown themes subcommand `{other}`"))),
    }
    Ok(())
}

fn updates(a: &Args, out: OutFn) -> Res {
    let sub = a.pos.get(1).map(String::as_str).unwrap_or("status");
    let method = match sub {
        "status" => "updates.status",
        "check" => "updates.check",
        "install" => "updates.install",
        other => return Err(Fail::Usage(format!("unknown updates subcommand `{other}`"))),
    };
    let v = call(method, json!({}))?;
    out(&v, &|v| {
        let st = if v.get("status").is_some() { &v["status"] } else { v };
        if v.get("delivered").is_some_and(|d| d == 0) {
            eprintln!("(the midna app is not running, so nothing happened)");
        }
        print::kv(st);
    });
    Ok(())
}

fn hooks(a: &Args, out: OutFn) -> Res {
    let sub = a.pos.get(1).map(String::as_str).unwrap_or("status");
    let agents: Vec<&str> = a.pos.iter().skip(2).map(String::as_str).collect();
    if let Some(bad) = agents.iter().find(|x| !matches!(**x, "claude" | "codex")) {
        return Err(Fail::Usage(format!("unknown agent `{bad}`; claude or codex")));
    }
    let print_status = |v: &Value| {
        for k in ["claude", "codex"] {
            let h = &v[k];
            let detail = h["detail"].as_str().map(|d| format!("  {d}")).unwrap_or_default();
            println!("{k:<7} {:<14} {}{detail}", h["state"].as_str().unwrap_or(""), h["path"].as_str().unwrap_or(""));
        }
    };
    match sub {
        "status" => {
            let v = call("hooks.status", json!({}))?;
            out(&v, &print_status);
        }
        "preview" => {
            let v = call("hooks.preview", json!({ "agents": agents, "uninstall": a.has("uninstall") }))?;
            out(&v, &|v| {
                for f in v["files"].as_array().into_iter().flatten() {
                    println!("{}{}", f["path"].as_str().unwrap_or(""), if f["creates"] == true { " (new file)" } else { "" });
                    if let Some(e) = f["error"].as_str() {
                        println!("  {e}");
                    } else if f["unchanged"] == true {
                        println!("  no change");
                    }
                    for l in f["lines"].as_array().into_iter().flatten() {
                        println!("{} {}", l["op"].as_str().unwrap_or(" "), l["text"].as_str().unwrap_or(""));
                    }
                }
            });
        }
        "install" | "uninstall" => {
            let v = call(&format!("hooks.{sub}"), json!({ "agents": agents }))?;
            out(&v, &print_status);
        }
        other => return Err(Fail::Usage(format!("unknown hooks subcommand `{other}`"))),
    }
    Ok(())
}

fn permissions(a: &Args, out: OutFn) -> Res {
    match a.pos.get(1).map(String::as_str).unwrap_or("status") {
        "status" => {
            let v = call("permissions.status", json!({}))?;
            out(&v, &|v| {
                for p in v["permissions"].as_array().into_iter().flatten() {
                    println!("{:<14} {:<14} {}", p["name"].as_str().unwrap_or(""), p["state"].as_str().unwrap_or(""), p["detail"].as_str().unwrap_or(""));
                }
            });
        }
        "open" => {
            let name = a.need(2, "permission (accessibility|notifications|login-items)")?;
            let url = permission_pane(name).ok_or_else(|| Fail::Usage(format!("unknown permission `{name}`; one of accessibility, notifications, login-items")))?;
            // Opening a pane grants nothing; the human decides there.
            let ok = std::process::Command::new("/usr/bin/open").arg(url).status().is_ok_and(|s| s.success());
            out(&json!({ "ok": ok, "url": url }), &|_| println!("opened System Settings → {name}"));
        }
        other => return Err(Fail::Usage(format!("unknown permissions subcommand `{other}`"))),
    }
    Ok(())
}

/// Same URLs as midnad's `permissions::pane_url`.
fn permission_pane(name: &str) -> Option<&'static str> {
    Some(match name {
        "accessibility" => "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
        "notifications" => midna_proto::paths::NOTIFICATIONS_PANE,
        "login-items" | "login" => "x-apple.systempreferences:com.apple.LoginItems-Settings.extension",
        _ => return None,
    })
}

fn settings(a: &Args, out: OutFn) -> Res {
    match a.pos.get(1).map(String::as_str).unwrap_or("list") {
        "list" | "ls" => {
            let v = call("settings.list", json!({}))?;
            out(&v, &print::settings);
        }
        "get" => {
            let v = call("settings.get", json!({ "key": a.need(2, "key")? }))?;
            out(&v, &|v| println!("{}", print::plain(&v["value"])));
        }
        "set" => {
            let key = a.need(2, "key")?;
            let raw = a.pos[3..].join(" ");
            if a.pos.len() < 4 {
                return Err(Fail::Usage("missing value".into()));
            }
            // Accept JSON (true, 42, "x") or a bare string.
            let value = serde_json::from_str::<Value>(&raw).unwrap_or(json!(raw));
            let v = call("settings.set", json!({ "key": key, "value": value }))?;
            out(&v, &|v| println!("{key} = {}", print::plain(&v["value"])));
        }
        "reset" => {
            let key = a.need(2, "key")?;
            let v = call("settings.reset", json!({ "key": key }))?;
            out(&v, &|v| println!("{key} = {}", print::plain(&v["value"])));
        }
        other => return Err(Fail::Usage(format!("unknown settings subcommand `{other}`"))),
    }
    Ok(())
}

fn events(a: &Args) -> Res {
    a.check(&["since", "limit", "kind", "follow"])?;
    let json = a.has("json");
    let filter = a.get("kind").map(|k| json!({ "kinds": k.split(',').collect::<Vec<_>>() }));
    let since = a.num::<u64>("since")?;
    let show = |e: &Value| {
        if json {
            println!("{e}");
        } else {
            print::event(e);
        }
    };
    if !a.has("follow") {
        let v = call("events.list", json!({ "since_seq": since, "limit": a.num::<u32>("limit")?, "filter": filter }))?;
        for e in v.as_array().into_iter().flatten() {
            show(e);
        }
        return Ok(());
    }
    let mut c = connect()?;
    // Default follow replays nothing; --since N replays after N.
    c.call_value("events.subscribe", json!({ "since_seq": since, "filter": filter }))?;
    loop {
        match c.next_notification()? {
            Notification::Event(e) => show(&serde_json::to_value(e).unwrap_or_default()),
            _ => continue,
        }
    }
}

fn schema(a: &Args) -> Res {
    if a.has("list") {
        if a.has("json") {
            let v: Vec<Value> = midna_proto::catalog().iter().map(|m| json!({ "method": m.name, "mutating": m.mutating, "human_only": m.human_only })).collect();
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            return Ok(());
        }
        for m in midna_proto::catalog() {
            let first = m.description.split(". ").next().unwrap_or("").trim_end_matches('.');
            let flag = if m.human_only { " [human only]" } else if !m.mutating { " [read]" } else { "" };
            println!("{:<22} {first}{flag}", m.name);
        }
        return Ok(());
    }
    let Some(name) = a.pos.get(1) else {
        println!("{}", serde_json::to_string_pretty(&midna_proto::openrpc()).unwrap_or_default());
        return Ok(());
    };
    let m = midna_proto::catalog()
        .iter()
        .find(|m| m.name == name || m.name.replace('.', "_") == *name)
        .ok_or_else(|| Fail::Usage(format!("unknown method `{name}`; `midna schema --list` lists them")))?;
    let v = json!({
        "method": m.name,
        "mcp_tool": m.name.replace('.', "_"),
        "description": m.description,
        "mutating": m.mutating,
        "human_only": m.human_only,
        "params": (m.params)(),
        "result": (m.result)(),
    });
    println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
    Ok(())
}

fn insights(a: &Args, out: OutFn) -> Res {
    a.check(&["range", "by", "bucket", "limit"])?;
    match a.pos.get(1).map(String::as_str) {
        None | Some("summary") => {
            let v = call("insights.summary", json!({ "range": a.get("range").unwrap_or("today"), "by": a.get("by") }))?;
            out(&v, &print::insights);
        }
        Some("series") => {
            let metric = a.need(2, "metric (turns|messages|spend|working|waiting|approvals|triggers)")?;
            let v = call("insights.series", json!({ "range": a.get("range").unwrap_or("today"), "metric": metric, "bucket": a.get("bucket"), "by": a.get("by") }))?;
            out(&v, &|v| {
                println!("{} {} ({}), total {} vs previous {}", s_(v, "metric"), s_(v, "range"), s_(v, "unit"), v["total"], v["previous_total"]);
                for b in v["buckets"].as_array().into_iter().flatten() {
                    println!("  {}  {}", s_(b, "start"), b["total"]);
                }
            });
        }
        Some("detail") => {
            let v = call("insights.detail", json!({ "range": a.get("range").unwrap_or("today") }))?;
            out(&v, &print::insights_detail);
        }
        Some("activity") => {
            let v = call("insights.activity", json!({ "limit": a.num::<u32>("limit")? }))?;
            if a.has("json") {
                println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            } else {
                for e in v.as_array().into_iter().flatten() {
                    print::event(e);
                }
            }
        }
        Some(other) => return Err(Fail::Usage(format!("unknown insights subcommand `{other}` (summary|series|detail|activity)"))),
    }
    Ok(())
}

fn s_(v: &Value, k: &str) -> String {
    print::plain(v.get(k).unwrap_or(&Value::Null))
}

fn links(a: &Args, out: OutFn) -> Res {
    let session = a.get("session");
    match a.pos.get(1).map(String::as_str).unwrap_or("list") {
        "list" | "ls" => {
            a.check(&["session", "kind", "pinned", "turn"])?;
            let turn = a.get("turn").map(|t| t.parse::<u32>().map_or_else(|_| json!(t), |n| json!(n)));
            let v = call("links.list", json!({ "session": session, "kind": a.get("kind"), "pinned": a.has("pinned"), "turn": turn }))?;
            out(&v, &|v| {
                let list = v["links"].as_array().cloned().unwrap_or_default();
                match (list.is_empty(), turn.is_some(), v["turn"].as_u64()) {
                    (true, true, Some(n)) => println!("no links from prompt {n} in {}", s_(v, "session")),
                    (true, true, None) => println!("no prompts yet in {}", s_(v, "session")),
                    (true, ..) => println!("no links yet in {}", s_(v, "session")),
                    _ => {}
                }
                for l in list {
                    let pin = if l["pinned"] == true { "*" } else { " " };
                    let via = l["via"].as_str().map(|v| format!(" via {v}")).unwrap_or_default();
                    let turn = l["turn"].as_u64().map(|n| format!(", prompt {n}")).unwrap_or_default();
                    println!("{pin} {}  {:<8} {}  ({}{}, {}×{turn})", s_(&l, "id"), s_(&l, "kind"), s_(&l, "title"), s_(&l, "source"), via, print::plain(&l["mentions"]));
                    println!("    {}", s_(&l, "target"));
                    if let Some(n) = l["note"].as_str() {
                        println!("    {n}");
                    }
                }
            });
        }
        verb @ ("pin" | "unpin") => {
            a.check(&["session"])?;
            let link = a.need(2, "link id, URL or path")?;
            let v = call("links.pin", json!({ "session": session, "link": link, "pinned": verb == "pin" }))?;
            out(&v, &|v| println!("{verb}ned {}  {}", s_(v, "id"), s_(v, "title")));
        }
        "remove" => {
            a.check(&["session"])?;
            let link = a.need(2, "link id, URL or path")?;
            let v = call("links.remove", json!({ "session": session, "link": link }))?;
            out(&v, &|v| println!("removed {}  {}", s_(v, "id"), s_(v, "title")));
        }
        "add" => {
            a.check(&["session", "title", "why", "no-pin"])?;
            let target = a.need(2, "URL or absolute path")?;
            let v = call("links.add", json!({ "session": session, "target": target, "title": a.get("title"), "note": a.get("why"), "pin": !a.has("no-pin") }))?;
            out(&v, &|v| println!("added {}  {}{}", s_(v, "id"), s_(v, "title"), if v["pinned"] == true { " (pinned)" } else { "" }));
        }
        other => return Err(Fail::Usage(format!("unknown `links {other}`; see midna links --help"))),
    }
    Ok(())
}

/// A notification's `response` as a line: the button's label, `clicked`, `dismissed`, or
/// `no response`.
fn response_line(r: &Value) -> String {
    match (r["kind"].as_str(), r["action"].as_str()) {
        (Some("action"), Some(a)) => a.to_string(),
        (Some(k), _) => k.to_string(),
        (None, _) => "no response".into(),
    }
}

fn notify(a: &Args, out: OutFn) -> Res {
    let print_list = |v: &Value| {
        let scope = match v["session"].as_str() {
            Some(s) if v["muted"] == true => format!("terminal {s} (muted)"),
            Some(s) => format!("terminal {s}"),
            None => "global".into(),
        };
        println!("notifications {} · {scope}", if v["enabled"] == true { "on" } else { "OFF (notify.enabled)" });
        for c in v["categories"].as_array().cloned().unwrap_or_default() {
            let mark = if c["effective"] == true { "on " } else { "off" };
            let here = match c["session"].as_bool() {
                Some(b) => format!("  (here: {}, global: {})", if b { "on" } else { "off" }, if c["global"] == true { "on" } else { "off" }),
                None => String::new(),
            };
            let sound = match s_(&c, "sound").as_str() {
                "none" | "" => "no sound".to_string(),
                snd => format!("{snd} {}%", c["volume"].as_i64().unwrap_or(100)),
            };
            let image = match s_(&c, "image").as_str() {
                "" => String::new(),
                i => format!(" · {i}"),
            };
            let stay = match c["stay"].as_i64() {
                Some(0) => "stays".to_string(),
                Some(s) => format!("{s}s"),
                None => String::new(),
            };
            println!("  {mark}  {:<11} {:<26} {stay:<6} {:<8} {sound}{image}{here}", s_(&c, "key"), s_(&c, "label"), s_(&c, "color"));
        }
    };
    let session = a.get("session");
    let global = a.has("global");
    match a.pos.get(1).map(String::as_str).unwrap_or("list") {
        "list" | "ls" => {
            a.check(&["session", "global"])?;
            let v = call("notify.list", json!({ "session": session, "global": global }))?;
            out(&v, &print_list);
        }
        verb @ ("set" | "mute" | "unmute") => {
            a.check(&["session", "global"])?;
            let (key, value) = match verb {
                "mute" => ("enabled".to_string(), json!(false)),
                "unmute" => ("enabled".to_string(), Value::Null),
                _ => {
                    let key = a.need(2, "notification key (see midna notify)")?.to_string();
                    let value = match a.need(3, "on, off or default")? {
                        "on" | "true" | "yes" => json!(true),
                        "off" | "false" | "no" => json!(false),
                        "default" | "inherit" | "null" => Value::Null,
                        other => return Err(Fail::Usage(format!("`{other}`: expected on, off or default"))),
                    };
                    (key, value)
                }
            };
            let v = call("notify.set", json!({ "session": session, "global": global, "key": key, "value": value }))?;
            out(&v, &print_list);
        }
        "send" => {
            a.check(&["session", "detail", "sound", "kind", "id", "open", "action", "wait"])?;
            let title = a.need(2, "title")?;
            let wait = a.get("wait").map(|w| crate::triggers::parse_duration(w, 1)).transpose().map_err(|e| Fail::Usage(format!("--wait: {e}")))?;
            let p = json!({
                "session": session, "title": title, "body": a.get("detail").unwrap_or(""), "sound": a.has("sound"), "category": a.get("kind"),
                "id": a.get("id"), "open": a.get("open"), "actions": a.all("action"), "wait_secs": wait,
            });
            let v = call("notify.send", p)?;
            out(&v, &|v| match v["reason"].as_str() {
                None if wait.is_some() => println!("{}", response_line(&v["response"])),
                None => println!("sent {}{}", s_(v, "id"), if v["via"] == "app" || v["via"].is_null() { String::new() } else { format!(" (via {})", s_(v, "via")) }),
                Some(r) => println!("not sent: {r}"),
            });
        }
        "withdraw" => {
            a.check(&[])?;
            let v = call("notify.withdraw", json!({ "id": a.need(2, "a notification id (from notify send --id)")? }))?;
            out(&v, &|v| match v["delivered"] == true {
                true => println!("withdrew {}", s_(v, "id")),
                false => println!("not withdrawn: the midna app isn't running"),
            });
        }
        "response" => {
            a.check(&["wait"])?;
            let wait = a.get("wait").map(|w| crate::triggers::parse_duration(w, 1)).transpose().map_err(|e| Fail::Usage(format!("--wait: {e}")))?;
            let v = call("notify.response", json!({ "id": a.need(2, "a notification id")?, "wait_secs": wait }))?;
            out(&v, &|v| println!("{}", response_line(&v["response"])));
        }
        "kinds" | "kind" => {
            let print_kinds = |v: &Value| {
                let kinds = v["kinds"].as_array().cloned().unwrap_or_default();
                if kinds.is_empty() {
                    println!("no kinds added (built-in kinds: midna notify; add one: midna notify kinds add <key> --label …)");
                }
                for k in kinds {
                    let stay = match k["stay"].as_i64() {
                        Some(0) => "stays".to_string(),
                        Some(s) => format!("{s}s"),
                        None => String::new(),
                    };
                    let on = if k["enabled"] == true { "on " } else { "off" };
                    println!("  {on}  {:<14} {:<22} {stay:<6} {:<8} {}", s_(&k, "key"), s_(&k, "label"), s_(&k, "color"), s_(&k, "sound"));
                }
            };
            match a.pos.get(2).map(String::as_str).unwrap_or("list") {
                "list" | "ls" => {
                    a.check(&[])?;
                    let v = call("notify.kinds.list", json!({}))?;
                    out(&v, &print_kinds);
                }
                verb @ ("add" | "update" | "edit") => {
                    a.check(&["label", "description", "color", "stay", "set"])?;
                    let key = a.need(3, "a kind key (deploys)")?;
                    // --stay 0 = stays until handled; --set field=value for the rest (sound=Glass, push=false, …)
                    let mut settings = serde_json::Map::new();
                    for (f, v) in [("color", a.get("color")), ("stay", a.get("stay"))] {
                        if let Some(v) = v {
                            settings.insert(f.into(), json!(v));
                        }
                    }
                    for s in a.all("set") {
                        let (f, v) = s.split_once('=').ok_or_else(|| Fail::Usage(format!("--set `{s}`: expected field=value (stay=0, sound=Glass, push=false)")))?;
                        settings.insert(f.trim().into(), json!(v.trim()));
                    }
                    let p = json!({ "key": key, "label": a.get("label"), "description": a.get("description"), "settings": settings, "replace": verb != "add" });
                    let v = call("notify.kinds.add", p)?;
                    out(&v, &|v| println!("{} kind {} ({})", if verb == "add" { "added" } else { "updated" }, s_(v, "key"), s_(v, "label")));
                }
                "remove" | "rm" => {
                    a.check(&[])?;
                    let key = a.need(3, "a kind key you added (see midna notify kinds)")?;
                    let v = call("notify.kinds.remove", json!({ "key": key }))?;
                    out(&v, &|_| println!("removed kind {key}"));
                }
                other => return Err(Fail::Usage(format!("unknown `notify kinds {other}`; list, add, update or rm"))),
            }
        }
        "media" | "sounds" | "images" => {
            a.check(&[])?;
            let v = call("notify.media", json!({}))?;
            out(&v, &|v| {
                for (title, list) in [("sounds", &v["sounds"]), ("images", &v["images"])] {
                    let list = list.as_array().cloned().unwrap_or_default();
                    println!("{title}{}", if list.is_empty() { " (none imported)" } else { "" });
                    for m in list {
                        let used: Vec<&str> = m["used_by"].as_array().map(|u| u.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
                        let tag = if m["builtin"] == true { "macOS" } else { "imported" };
                        let used = if used.is_empty() { String::new() } else { format!("  used by {}", used.join(", ")) };
                        println!("  {:<22} {tag:<8}{used}", s_(&m, "name"));
                    }
                }
                println!("imported files: {}", s_(v, "dir"));
            });
        }
        "import" => {
            a.check(&["for"])?;
            let file = a.need(2, "a sound or image file")?;
            let path = std::path::absolute(file).map_err(|e| Fail::Usage(format!("{file}: {e}")))?;
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            let image = midna_proto::notify::IMAGE_EXTS.contains(&ext.as_str());
            // --for approval = that kind's sound (or image); --for all = every notification's image
            let use_for: Vec<String> = a
                .all("for")
                .iter()
                .flat_map(|f| f.split(','))
                .map(|f| match (f.trim(), image) {
                    (f, _) if f.starts_with("notify.") => f.to_string(),
                    ("all" | "every", true) => "notify.image".into(),
                    (f, true) => midna_proto::notify::image_key(f),
                    (f, false) => midna_proto::notify::sound_key(f),
                })
                .collect();
            let v = call("notify.import", json!({ "path": path, "use_for": use_for }))?;
            out(&v, &|v| {
                let used: Vec<&str> = v["used_by"].as_array().map(|u| u.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
                println!("imported {} `{}`{}", s_(v, "kind"), s_(v, "name"), if used.is_empty() { String::new() } else { format!(", used by {}", used.join(", ")) });
            });
        }
        "remove" | "rm" => {
            a.check(&[])?;
            let v = call("notify.remove", json!({ "name": a.need(2, "an imported file's name (see midna notify media)")? }))?;
            out(&v, &|v| {
                let reset: Vec<&str> = v["reset"].as_array().map(|u| u.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
                println!("removed {}{}", s_(v, "removed"), if reset.is_empty() { String::new() } else { format!("; reset {}", reset.join(", ")) });
            });
        }
        "history" | "hist" => {
            a.check(&["session", "limit", "unread"])?;
            let limit = a.get("limit").map(|l| l.parse::<u32>().map_err(|_| Fail::Usage(format!("--limit `{l}`: expected a number")))).transpose()?;
            let unread_only = a.has("unread");
            let v = call("notify.history", json!({ "session": session, "limit": limit }))?;
            out(&v, &|v| {
                let items: Vec<&Value> = v["items"].as_array().into_iter().flatten().filter(|i| !unread_only || i["unread"] == true).collect();
                println!("{} unread · read up to {}", v["unread"].as_u64().unwrap_or(0), v["read_seq"].as_u64().unwrap_or(0));
                if items.is_empty() {
                    println!("  no {}notifications", if unread_only { "unread " } else { "" });
                }
                for i in items {
                    let n = &i["notification"];
                    let mark = if i["withdrawn"] == true { "-" } else if i["unread"] == true { "•" } else { " " };
                    let body = match s_(n, "body").as_str() {
                        "" => String::new(),
                        b => format!(" — {b}"),
                    };
                    let at = guide::ago(&s_(i, "at"));
                    println!("  {mark} {:>6}  {at:>8}  {:<11} {:<10} {}{body}", i["seq"], s_(n, "category"), s_(i, "session_id"), s_(n, "title"));
                }
            });
        }
        "read" => {
            a.check(&[])?;
            let seq = a.pos.get(2).map(|s| s.parse::<u64>().map_err(|_| Fail::Usage(format!("`{s}`: expected a seq (see midna notify history)")))).transpose()?;
            let v = call("notify.read", json!({ "seq": seq }))?;
            out(&v, &|v| println!("read up to {} · {} unread", v["read_seq"].as_u64().unwrap_or(0), v["unread"].as_u64().unwrap_or(0)));
        }
        "clear" => {
            a.check(&["session"])?;
            let v = call("notify.clear", json!({ "session": session }))?;
            out(&v, &|v| match (v["delivered"] == true, v["session"].as_str()) {
                (false, _) => println!("not cleared: the midna app isn't running"),
                (true, Some(s)) => println!("cleared terminal {s}'s notifications"),
                (true, None) => println!("cleared all notifications"),
            });
        }
        "test" => {
            a.check(&["session"])?;
            let v = call("notify.test", json!({ "session": session, "category": a.pos.get(2) }))?;
            out(&v, &|v| match v["reason"].as_str() {
                None => println!("sent a test notification"),
                Some(r) => println!("not sent: {r}"),
            });
        }
        "play" => {
            a.check(&["session", "volume"])?;
            let volume = match a.get("volume") {
                Some(v) => Some(v.trim_end_matches('%').parse::<u8>().ok().filter(|v| *v <= 100).ok_or_else(|| Fail::Usage(format!("--volume `{v}`: expected 0-100")))?),
                None => None,
            };
            let v = call("notify.play", json!({ "session": session, "sound": a.need(2, "a kind (approved, failed, …) or a sound (Glass, Pop, …)")?, "volume": volume }))?;
            out(&v, &|v| match v["reason"].as_str() {
                None => println!("played {}", s_(v, "sound")),
                Some(r) => println!("not played: {r}"),
            });
        }
        other => return Err(Fail::Usage(format!("unknown `notify {other}`; see midna notify --help"))),
    }
    Ok(())
}

fn projects(a: &Args, out: OutFn) -> Res {
    let print_projects = |v: &Value| {
        let list = v.as_array().cloned().unwrap_or_default();
        if list.is_empty() {
            println!("no projects");
        }
        for p in list {
            println!("{}  {}  {}", s_(&p, "id"), s_(&p, "name"), s_(&p, "path"));
            for c in p["commands"].as_array().into_iter().flatten() {
                println!("    {}{}: {}", s_(c, "name"), if c["pinned"] == true { " (pinned)" } else { "" }, s_(c, "run"));
            }
        }
    };
    match a.pos.get(1).map(String::as_str).unwrap_or("list") {
        "list" | "ls" => {
            let v = call("project.list", json!({}))?;
            out(&v, &print_projects);
        }
        "discover" => {
            let v = call("project.discover", json!({}))?;
            out(&v, &|v| {
                let list = v.as_array().cloned().unwrap_or_default();
                if list.is_empty() {
                    println!("nothing found; set the folders projects live in: midna settings set projects.roots ~/Development");
                }
                for c in list {
                    let open = if c["project_id"].is_string() { format!("  (project {})", s_(&c, "project_id")) } else { String::new() };
                    println!("{}  {}{}", s_(&c, "name"), s_(&c, "path"), open);
                }
            });
        }
        "add" => {
            a.check(&["name"])?;
            let path = a.need(2, "directory path")?;
            let abs = std::fs::canonicalize(path).map_err(|e| Fail::Usage(format!("{path}: {e}")))?;
            let v = call("project.add", json!({ "path": abs, "name": a.get("name") }))?;
            out(&v, &|v| println!("{}", s_(v, "id")));
        }
        "update" | "rename" => {
            a.check(&["name", "icon"])?;
            let id = a.need(2, "project id")?;
            let v = call("project.update", json!({ "id": id, "name": a.get("name"), "icon": a.get("icon") }))?;
            out(&v, &|v| print_projects(&json!([v])));
        }
        "add-command" => {
            a.check(&["name", "pinned"])?;
            let id = a.need(2, "project id")?;
            let name = a.get("name").ok_or_else(|| Fail::Usage("--name is required".into()))?;
            let run = a.text(3);
            if run.is_empty() {
                return Err(Fail::Usage("missing the command line (after `--`)".into()));
            }
            let list = call("project.list", json!({}))?;
            let p = list.as_array().into_iter().flatten().find(|p| p["id"] == id).cloned().ok_or_else(|| Fail::Other(format!("no project {id}")))?;
            let mut cmds: Vec<Value> = p["commands"].as_array().cloned().unwrap_or_default().into_iter().filter(|c| c["name"] != name).collect();
            cmds.push(json!({ "name": name, "run": run, "pinned": a.has("pinned") }));
            let v = call("project.update", json!({ "id": id, "commands": cmds }))?;
            out(&v, &|v| print_projects(&json!([v])));
        }
        "remove-command" => {
            let id = a.need(2, "project id")?;
            let name = a.text(3);
            let list = call("project.list", json!({}))?;
            let p = list.as_array().into_iter().flatten().find(|p| p["id"] == id).cloned().ok_or_else(|| Fail::Other(format!("no project {id}")))?;
            let before = p["commands"].as_array().map_or(0, Vec::len);
            let cmds: Vec<Value> = p["commands"].as_array().cloned().unwrap_or_default().into_iter().filter(|c| c["name"] != name.as_str()).collect();
            if cmds.len() == before {
                return Err(Fail::Other(format!("project {id} has no command “{name}”")));
            }
            let v = call("project.update", json!({ "id": id, "commands": cmds }))?;
            out(&v, &|v| print_projects(&json!([v])));
        }
        "remove" | "rm" => {
            let id = a.need(2, "project id")?;
            let v = call("project.remove", json!({ "id": id }))?;
            out(&v, &|_| println!("removed {id}"));
        }
        other => return Err(Fail::Usage(format!("unknown projects subcommand `{other}`"))),
    }
    Ok(())
}
