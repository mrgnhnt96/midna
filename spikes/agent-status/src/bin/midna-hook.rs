//! midna-hook: tiny forwarder. Invoked by an agent's hook system.
//!
//!   midna-hook <agent> [event]            payload JSON on stdin (Claude Code / Codex hooks)
//!   midna-hook <agent> statusline         payload on stdin, prints a status line to stdout
//!   midna-hook codex notify '<json>'      Codex legacy `notify` (payload is the last argv)
//!
//! Wraps the payload in an envelope with MIDNA_TERM_ID and a send timestamp and writes
//! one JSON line to the Unix socket at MIDNA_SOCK. Never fails the agent: any error -> exit 0.
//! Spike-only knobs (read from the file at MIDNA_HOOK_CTL, if present):
//!   {"sleep_ms": {"PreToolUse": 6000}, "exit": {"UserPromptSubmit": 1}, "osc": true}
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis()
}

fn main() {
    let started = now_ms();
    let args: Vec<String> = std::env::args().collect();
    let agent = args.get(1).cloned().unwrap_or_default();
    let arg_event = args.get(2).cloned();
    let notify_mode = arg_event.as_deref() == Some("notify");

    let raw = if notify_mode {
        args.last().cloned().unwrap_or_default()
    } else {
        let mut s = String::new();
        let _ = std::io::stdin().read_to_string(&mut s);
        s
    };
    let payload: Value = serde_json::from_str(&raw).unwrap_or(Value::String(raw.clone()));
    let event = payload
        .get("hook_event_name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| payload.get("type").and_then(Value::as_str).map(str::to_string))
        .or(arg_event.clone())
        .unwrap_or_else(|| "unknown".into());

    let ctl: Value = std::env::var("MIDNA_HOOK_CTL")
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);

    // OSC self-report: try writing to the controlling terminal directly.
    let mut tty_ok = Value::Null;
    if ctl.get("osc").and_then(Value::as_bool).unwrap_or(false) {
        tty_ok = match std::fs::OpenOptions::new().write(true).open("/dev/tty") {
            Ok(mut f) => {
                let seq = format!("\x1b]6973;midna;agent={agent};event={event}\x07\x1b]9;midna {event}\x07");
                json!(f.write_all(seq.as_bytes()).is_ok())
            }
            Err(e) => {
                // No controlling tty (hooks are detached). Walk up the process tree to
                // find an ancestor's tty and open the device node directly.
                let mut pid = std::os::unix::process::parent_id();
                let mut res = json!(format!("/dev/tty err: {e}; no ancestor tty"));
                for _ in 0..6 {
                    let out = std::process::Command::new("ps").args(["-o", "tty=,ppid=", "-p", &pid.to_string()]).output();
                    let Ok(out) = out else { break };
                    let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    let mut it = line.split_whitespace();
                    let (tty, ppid) = (it.next().unwrap_or("??"), it.next().and_then(|p| p.parse().ok()).unwrap_or(1));
                    if tty != "??" && !tty.is_empty() {
                        let dev = format!("/dev/{tty}");
                        res = match std::fs::OpenOptions::new().write(true).open(&dev) {
                            Ok(mut f) => {
                                let seq = format!("\x1b]6973;midna;agent={agent};event={event}\x07\x1b]9;midna {event}\x07");
                                json!(format!("via {dev} (pid {pid}): write ok={}", f.write_all(seq.as_bytes()).is_ok()))
                            }
                            Err(e2) => json!(format!("open {dev} failed: {e2}")),
                        };
                        break;
                    }
                    if ppid <= 1 { break }
                    pid = ppid;
                }
                res
            }
        };
    }

    let env = json!({
        "v": 1,
        "agent": agent,
        "arg": arg_event,
        "event": event,
        "term": std::env::var("MIDNA_TERM_ID").ok(),
        "ppid": unsafe_ppid(),
        "started_ms": started as u64,
        "sent_ms": now_ms() as u64,
        "tty_ok": tty_ok,
        "payload": payload,
    });
    if let Ok(sock) = std::env::var("MIDNA_SOCK") {
        if let Ok(mut s) = UnixStream::connect(sock) {
            let _ = s.set_write_timeout(Some(Duration::from_millis(200)));
            let mut line = env.to_string();
            line.push('\n');
            let _ = s.write_all(line.as_bytes());
        }
    }

    if arg_event.as_deref() == Some("statusline") {
        let model = env["payload"]["model"]["display_name"].as_str().unwrap_or("?");
        let cost = env["payload"]["cost"]["total_cost_usd"].as_f64().unwrap_or(0.0);
        println!("midna-spike | {model} | ${cost:.4}");
    }

    if let Some(ms) = ctl["sleep_ms"][&event].as_u64() {
        std::thread::sleep(Duration::from_millis(ms));
    }
    if let Some(code) = ctl["exit"][&event].as_i64() {
        eprintln!("midna-hook: spike-induced failure for {event}");
        std::process::exit(code as i32);
    }
}

fn unsafe_ppid() -> u32 {
    std::os::unix::process::parent_id()
}
