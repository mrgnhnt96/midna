//! PTY harness: runs an agent TUI in a pty we own, feeds output into a vt100 screen,
//! scans raw output for OSC sequences, runs midnad-lite in-process, and drives scenarios.
//!
//!   harness <scenario> <workdir>
//!   scenarios: claude-perm | claude-plain | claude-interrupt | claude-slowhook | claude-failhook
//!              | claude-osc | codex-perm
use agent_status_spike::{AgentState, Daemon, now_ms};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use regex::Regex;
use serde_json::json;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const TERM_ID: &str = "T1";
const SPIKE: &str = env!("CARGO_MANIFEST_DIR");

struct Session {
    screen: Arc<Mutex<vt100::Parser>>,
    oscs: Arc<Mutex<Vec<(f64, String)>>>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    d: Daemon,
}

impl Session {
    fn contents(&self) -> String {
        self.screen.lock().unwrap().screen().contents()
    }
    fn dump(&self, label: &str) {
        println!("----- screen [{label}] @{:.3}s -----", self.d.elapsed());
        for l in self.contents().lines().filter(|l| !l.trim().is_empty()) {
            println!("| {l}");
        }
        println!("-----");
    }
    fn send(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }
    fn type_line(&mut self, text: &str) {
        self.send(text.as_bytes());
        std::thread::sleep(Duration::from_millis(400));
        self.send(b"\r");
    }
    /// Wait until the screen matches; returns elapsed (daemon clock) when it did.
    fn wait_screen(&self, re: &str, secs: u64) -> Option<f64> {
        let re = Regex::new(re).unwrap();
        let end = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < end {
            if re.is_match(&self.contents()) {
                return Some(self.d.elapsed());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    }
    fn wait_state(&self, pred: impl Fn(&AgentState) -> bool, secs: u64) -> Option<f64> {
        let end = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < end {
            if pred(&self.d.state(TERM_ID)) {
                return Some(self.d.elapsed());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }
    fn report(&self, label: &str, hook: Option<f64>, screen: Option<f64>) {
        match (hook, screen) {
            (Some(h), Some(s)) => println!(">>> {label}: hook-state @{h:.3}s, screen @{s:.3}s, hook leads screen by {:+.0}ms", (s - h) * 1000.0),
            _ => println!(">>> {label}: hook={hook:?} screen={screen:?}  (MISSING)"),
        }
    }
}

fn spawn(argv: &[&str], cwd: &str, d: Daemon, sock: &str, ctl: &str) -> Session {
    let pty = native_pty_system();
    let pair = pty.openpty(PtySize { rows: 40, cols: 120, pixel_width: 0, pixel_height: 0 }).unwrap();
    let mut cmd = CommandBuilder::new(argv[0]);
    for a in &argv[1..] {
        cmd.arg(a);
    }
    cmd.cwd(cwd);
    // Isolate from the surrounding Saggar/Claude session.
    for (k, _) in std::env::vars() {
        if k.starts_with("SAGGAR") || k.starts_with("CLAUDE") || k == "AI_AGENT" || k == "TERM_PROGRAM" {
            cmd.env_remove(k);
        }
    }
    cmd.env("TERM", "xterm-256color");
    cmd.env("MIDNA_SOCK", sock);
    cmd.env("MIDNA_TERM_ID", TERM_ID);
    cmd.env("MIDNA_HOOK_CTL", ctl);
    let child = pair.slave.spawn_command(cmd).unwrap();
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let writer = pair.master.take_writer().unwrap();
    let screen = Arc::new(Mutex::new(vt100::Parser::new(40, 120, 0)));
    let oscs = Arc::new(Mutex::new(Vec::new()));
    let (scr, os, t0) = (screen.clone(), oscs.clone(), d.t0);
    std::thread::spawn(move || {
        let _keep_master = pair.master;
        let mut buf = [0u8; 8192];
        let mut tail: Vec<u8> = Vec::new();
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    scr.lock().unwrap().process(&buf[..n]);
                    // OSC scan: ESC ] ... (BEL | ESC \)
                    tail.extend_from_slice(&buf[..n]);
                    let mut i = 0;
                    let mut keep_from = tail.len().saturating_sub(1);
                    while i + 1 < tail.len() {
                        if tail[i] == 0x1b && tail[i + 1] == b']' {
                            let rest = &tail[i + 2..];
                            let end = rest.iter().position(|&b| b == 0x07).map(|p| (p, 1)).or_else(|| {
                                rest.windows(2).position(|w| w == b"\x1b\\").map(|p| (p, 2))
                            });
                            match end {
                                Some((p, _)) => {
                                    let body = String::from_utf8_lossy(&rest[..p]).to_string();
                                    let num = body.split(';').next().unwrap_or("");
                                    // ignore title/colour chatter, keep notification/status-ish ones
                                    if !matches!(num, "0" | "1" | "2" | "4" | "10" | "11" | "12" | "104" | "8") {
                                        os.lock().unwrap().push((t0.elapsed().as_secs_f64(), body.clone()));
                                    } else if num == "0" || num == "2" {
                                        os.lock().unwrap().push((t0.elapsed().as_secs_f64(), format!("title:{body}")));
                                    }
                                    i += 2 + p;
                                    continue;
                                }
                                None => {
                                    keep_from = i;
                                    break;
                                }
                            }
                        }
                        i += 1;
                    }
                    tail.drain(..keep_from.min(tail.len()));
                    if tail.len() > 65536 {
                        tail.clear();
                    }
                }
            }
        }
    });
    Session { screen, oscs, writer, child, d }
}

fn claude_settings(dir: &str, hookbin: &str, timeout: Option<u64>) -> String {
    let events = [
        "SessionStart", "SessionEnd", "UserPromptSubmit", "PreToolUse", "PostToolUse",
        "PostToolUseFailure", "PermissionRequest", "PermissionDenied", "Notification", "Stop",
        "StopFailure", "SubagentStart", "SubagentStop", "PreCompact", "PostCompact",
        "Elicitation", "TaskCreated", "TaskCompleted",
    ];
    let mut hooks = serde_json::Map::new();
    for e in events {
        let mut h = json!({"type": "command", "command": format!("{hookbin} claude")});
        if let Some(t) = timeout {
            h["timeout"] = json!(t);
        }
        hooks.insert(e.into(), json!([{"matcher": "*", "hooks": [h]}]));
    }
    let settings = json!({
        "hooks": hooks,
        "statusLine": {"type": "command", "command": format!("{hookbin} claude statusline"), "padding": 0},
    });
    let path = format!("{dir}/spike-claude-settings.json");
    std::fs::write(&path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();
    path
}

// Claude Code screen heuristics (2.1.x TUI)
const RE_PROMPT_READY: &str = r"(\? for shortcuts|mode on|❯|shift\+tab|bypass permissions|auto-accept)";
const RE_TRUST: &str = r"(?i)(trust (the files in )?this folder|Yes, I trust)";
const RE_PERM: &str = r"(?s)(Do you want to proceed\?|Yes, and don.t ask again).*(1\.|❯)";
const RE_WORKING: &str = r"(?i)(esc to interrupt|\(\d+s ·|tokens\))";

fn boot_claude(s: &mut Session) {
    let t = s.wait_screen(&format!("{RE_TRUST}|{RE_PROMPT_READY}"), 40);
    if t.is_none() {
        s.dump("boot timeout");
    }
    if Regex::new(RE_TRUST).unwrap().is_match(&s.contents()) {
        s.dump("trust dialog");
        s.send(b"\r");
        s.wait_screen(RE_PROMPT_READY, 30);
    }
    std::thread::sleep(Duration::from_millis(1500));
    s.dump("ready");
}

fn finish(mut s: Session, scratch: &str) {
    // /exit -> SessionEnd
    s.type_line("/exit");
    std::thread::sleep(Duration::from_millis(500));
    s.send(b"\r");
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        if let Ok(Some(_)) = s.child.try_wait() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = s.child.kill();
    std::thread::sleep(Duration::from_millis(800));
    println!("=== transitions ===");
    for t in s.d.transitions.lock().unwrap().iter() {
        println!(
            "  @{:7.3}s {:?} -> {:?} via {} (hook proc start->daemon {}ms)",
            t.at.duration_since(s.d.t0).as_secs_f64(), t.from, t.to, t.event, t.recv_ms.saturating_sub(t.hook_started_ms)
        );
    }
    println!("=== OSC seen on pty ===");
    for (t, o) in s.oscs.lock().unwrap().iter() {
        println!("  @{t:7.3}s {o:?}");
    }
    let n = s.d.events.lock().unwrap().len();
    println!("=== {n} hook events; raw log in {scratch}/events.jsonl ===");
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let scenario = a[1].clone();
    let workdir = a[2].clone();
    let scratch = std::env::var("SPIKE_SCRATCH").unwrap_or_else(|_| format!("{SPIKE}/run"));
    std::fs::create_dir_all(&scratch).unwrap();
    let sock = format!("{scratch}/midnad.sock");
    let ctl = format!("{scratch}/hook-ctl.json");
    let hookbin = format!("{SPIKE}/target/debug/midna-hook");
    let d = Daemon::start(&sock, &format!("{scratch}/events.jsonl"), false);

    let ctl_json = match scenario.as_str() {
        "claude-slowhook" => json!({"sleep_ms": {"UserPromptSubmit": 6000, "PreToolUse": 4000}}),
        "claude-failhook" => json!({"exit": {"UserPromptSubmit": 1, "PreToolUse": 1}}),
        "claude-osc" | "codex-perm" => json!({"osc": true}),
        _ => json!({}),
    };
    std::fs::write(&ctl, ctl_json.to_string()).unwrap();

    if scenario.starts_with("codex") {
        return codex(scenario, workdir, scratch, sock, ctl, hookbin, d);
    }

    let timeout = if scenario == "claude-slowhook" { None } else { Some(10) };
    let settings = claude_settings(&scratch, &hookbin, timeout);
    let claude = format!("{}/.local/bin/claude", std::env::var("HOME").unwrap());
    let argv = [
        claude.as_str(), "--settings", &settings, "--setting-sources", "project",
        "--model", "haiku", "--permission-mode", "manual",
    ];
    let mut s = spawn(&argv, &workdir, d, &sock, &ctl);
    boot_claude(&mut s);

    match scenario.as_str() {
        "claude-plain" => {
            let t0 = s.d.elapsed();
            s.type_line("What is 2+2? Answer with just the number, no tools.");
            let w = s.wait_state(|st| matches!(st, AgentState::Working(_)), 20);
            println!(">>> typed @{t0:.3}s -> working @{w:?}");
            let h = s.wait_state(|st| *st == AgentState::Done, 60);
            let sc = s.wait_screen(r"(?m)^\s*⏺\s*4\s*$", 5);
            s.report("done", h, sc);
            s.dump("after plain");
        }
        "claude-perm" | "claude-slowhook" | "claude-failhook" | "claude-osc" => {
            let t0 = s.d.elapsed();
            s.type_line("Use the Bash tool to run exactly `touch c.txt && ls`, then tell me how many entries there are.");
            println!(">>> typed @{t0:.3}s");
            let h = s.wait_state(|st| matches!(st, AgentState::NeedsYou(_)), 90);
            let sc = s.wait_screen(RE_PERM, 30);
            s.report("needs-you(permission)", h, sc);
            s.dump("permission prompt");
            if sc.is_some() {
                // Answer: "1" selects "Yes" in Claude's select list.
                let ta = s.d.elapsed();
                s.send(b"1");
                let w = s.wait_state(|st| !matches!(st, AgentState::NeedsYou(_)), 20);
                println!(">>> answered '1' @{ta:.3}s -> left needs-you @{w:?} state={:?}", s.d.state(TERM_ID));
            }
            let h = s.wait_state(|st| *st == AgentState::Done, 90);
            println!(">>> done @{h:?}");
            s.dump("after perm");
        }
        "claude-interrupt" => {
            // Phase A: Esc while a foreground tool runs (after approving it).
            s.type_line("Use the Bash tool in the foreground (not background) to run exactly `sleep 20; touch d.txt`. Nothing else.");
            let h = s.wait_state(|st| matches!(st, AgentState::NeedsYou(_)), 60);
            let sc = s.wait_screen(RE_PERM, 20);
            s.report("needs-you", h, sc);
            if sc.is_some() { s.send(b"1"); }
            std::thread::sleep(Duration::from_millis(3000));
            s.dump("tool running");
            let ti = s.d.elapsed();
            s.send(b"\x1b"); // Esc
            println!(">>> sent Esc @{ti:.3}s (state before: {:?})", s.d.state(TERM_ID));
            let sc = s.wait_screen(r"(?i)interrupted", 10);
            std::thread::sleep(Duration::from_secs(3));
            println!(">>> after Esc: screen 'Interrupted' @{sc:?}; state now {:?}", s.d.state(TERM_ID));
            s.dump("after esc");
            // Phase B: deny a permission prompt with Esc.
            s.type_line("Use the Bash tool to run exactly `touch e.txt`.");
            let h = s.wait_state(|st| matches!(st, AgentState::NeedsYou(_)), 60);
            let sc = s.wait_screen(RE_PERM, 20);
            s.report("needs-you #2", h, sc);
            let ti = s.d.elapsed();
            s.send(b"\x1b");
            println!(">>> sent Esc on permission prompt @{ti:.3}s");
            std::thread::sleep(Duration::from_secs(4));
            println!(">>> after deny: state now {:?}", s.d.state(TERM_ID));
            s.dump("after deny");
        }
        _ => panic!("unknown scenario"),
    }
    finish(s, &scratch);
}

fn codex(scenario: String, workdir: String, scratch: String, sock: String, ctl: String, hookbin: String, d: Daemon) {
    let _ = scenario;
    let codex = format!("{}/.local/bin/codex", std::env::var("HOME").unwrap());
    let h = |ev: &str| format!("[{{hooks=[{{type=\"command\",command=\"{hookbin} codex {ev}\",timeout=5}}]}}]");
    let mut argv: Vec<String> = vec![codex];
    let evs: &[&str] = if std::env::var("CODEX_NO_HOOKS").is_ok() { &[] } else { &["SessionStart", "UserPromptSubmit", "PreToolUse", "PostToolUse", "PermissionRequest", "Stop", "SessionEnd"] };
    for ev in evs {
        argv.push("-c".into());
        argv.push(format!("hooks.{ev}={}", h(ev)));
    }
    argv.push("-c".into());
    argv.push(format!("notify=[\"{hookbin}\",\"codex\",\"notify\"]"));
    for kv in ["model_reasoning_effort=\"low\"", "approval_policy=\"on-request\"", "sandbox_mode=\"read-only\""] {
        argv.push("-c".into());
        argv.push(kv.into());
    }
    if let Ok(m) = std::env::var("CODEX_MODEL") {
        argv.push("-m".into());
        argv.push(m);
    }
    let argv_ref: Vec<&str> = argv.iter().map(String::as_str).collect();
    println!("codex argv: {argv:?}");
    let mut s = spawn(&argv_ref, &workdir, d, &sock, &ctl);
    let t = s.wait_screen(r"(?i)(›|trust|hook|Ask Codex|context left)", 40);
    println!("boot @{t:?}");
    std::thread::sleep(Duration::from_millis(2500));
    s.dump("codex boot");
    if Regex::new(r"(?i)review.*hook|untrusted hook|trust.*hook").unwrap().is_match(&s.contents()) {
        println!(">>> codex asks to trust hooks; not accepting (would persist trust to ~/.codex/config.toml)");
        s.send(b"\x1b");
        std::thread::sleep(Duration::from_millis(1500));
        s.dump("after declining hook trust");
    }
    if Regex::new(r"need review").unwrap().is_match(&s.contents()) {
        println!(">>> hooks-review panel: closing with Esc (trusting would write ~/.codex/config.toml)");
        s.send(b"\x1b");
        std::thread::sleep(Duration::from_millis(1500));
    }
    if Regex::new(r"Update available").unwrap().is_match(&s.contents()) {
        println!(">>> update prompt: choosing Skip");
        s.send(b"2");
        std::thread::sleep(Duration::from_millis(500));
        s.send(b"\r");
        std::thread::sleep(Duration::from_millis(2500));
        s.dump("after skip");
    }
    if !Regex::new(r"›").unwrap().is_match(&s.contents()) || Regex::new(r"(?i)error loading|update").unwrap().is_match(&s.contents()) {
        println!(">>> codex not at a prompt; aborting before typing anything");
        let _ = s.child.kill();
        return;
    }
    let t0 = s.d.elapsed();
    s.type_line("Run the shell command `touch c.txt` in this directory, then say done.");
    println!(">>> typed @{t0:.3}s");
    let h = s.wait_state(|st| matches!(st, AgentState::NeedsYou(_)), 60);
    let sc = s.wait_screen(r"(?i)(Would you like to run|Yes, proceed|Allow command)", 60);
    s.report("codex needs-you", h, sc);
    s.dump("codex approval");
    if sc.is_some() {
        s.send(b"y");
        let w = s.wait_state(|st| *st == AgentState::Done, 60);
        let sc2 = s.wait_screen(r"(?i)done", 5);
        println!(">>> screen shows done @{sc2:?}"); std::thread::sleep(Duration::from_secs(20));
        println!(">>> after 'y': done @{w:?} state={:?}", s.d.state(TERM_ID));
    }
    std::thread::sleep(Duration::from_secs(3));
    s.dump("codex after");
    s.send(b"\x03");
    std::thread::sleep(Duration::from_millis(500));
    s.send(b"\x03");
    std::thread::sleep(Duration::from_secs(2));
    let _ = s.child.kill();
    std::thread::sleep(Duration::from_millis(800));
    let _ = (scratch, sock, ctl);
    println!("=== transitions ===");
    for t in s.d.transitions.lock().unwrap().iter() {
        println!("  @{:7.3}s {:?} -> {:?} via {}", t.at.duration_since(s.d.t0).as_secs_f64(), t.from, t.to, t.event);
    }
    println!("=== OSC ===");
    for (t, o) in s.oscs.lock().unwrap().iter() {
        println!("  @{t:7.3}s {o:?}");
    }
    let _ = now_ms();
}
