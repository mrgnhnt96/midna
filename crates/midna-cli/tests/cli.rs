//! CLI tests: the real `midna` binary against an in-process daemon on a temp MIDNA_HOME.
//! The CLI is a different executable than the configured GUI, so it is an agent.
use serde_json::{Value, json};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

struct D {
    handle: Option<midnad::Handle>,
    home: PathBuf,
}

impl D {
    fn start() -> D {
        D::start_with(|_| {})
    }
    fn start_with(tweak: impl FnOnce(&mut midnad::Config)) -> D {
        let home = PathBuf::from(format!("/tmp/midna-c-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&home);
        let mut cfg = midnad::Config::for_home(home.clone());
        cfg.app_path = Some(std::env::current_exe().unwrap().to_string_lossy().into_owned());
        cfg.cli_path = env!("CARGO_BIN_EXE_midna").into();
        tweak(&mut cfg);
        D { handle: Some(midnad::start(cfg).unwrap()), home }
    }
    fn sock(&self) -> String {
        self.home.join("midnad.sock").to_string_lossy().into_owned()
    }
    fn human(&self) -> midna_proto::Client {
        let mut c = midna_proto::Client::connect(self.sock()).unwrap();
        c.set_caller(None);
        c
    }
}

impl Drop for D {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            h.shutdown();
        }
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn midna(sock: &str, args: &[&str], stdin: Option<&str>, session: Option<&str>) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_midna"));
    cmd.args(args).env("MIDNA_SOCKET", sock).env_remove("MIDNA_SESSION").env_remove("MIDNA_HOME");
    if let Some(s) = session {
        cmd.env("MIDNA_SESSION", s);
    }
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    if let Some(input) = stdin {
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    }
    drop(child.stdin.take());
    child.wait_with_output().unwrap()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn exit_codes() {
    let d = D::start();
    let s = d.sock();
    // 0: ok, and the CLI is an agent.
    let o = midna(&s, &["info", "--json"], None, None);
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(serde_json::from_str::<Value>(&stdout(&o)).unwrap()["role"], "agent");
    // 1: refused (human only).
    let r = midna(&s, &["rules", "add", "deny", "command", "git push --force*", "--json"], None, None);
    assert_eq!(code(&r), 0);
    let rid = serde_json::from_str::<Value>(&stdout(&r)).unwrap()["id"].as_str().unwrap().to_string();
    let o = midna(&s, &["rules", "remove", &rid], None, None);
    assert_eq!(code(&o), 1, "{o:?}");
    let err = String::from_utf8_lossy(&o.stderr).into_owned();
    assert!(err.contains(&format!("midna rules request-removal {rid} --reason")), "{err}");
    let o = midna(&s, &["settings", "set", "approve.from_cli", "true"], None, None);
    assert_eq!(code(&o), 1);
    // 2: bad args (usage, unknown verb, unknown method, bad params).
    assert_eq!(code(&midna(&s, &["read"], None, None)), 2);
    assert_eq!(code(&midna(&s, &["frobnicate"], None, None)), 2);
    assert_eq!(code(&midna(&s, &["call", "no.such.method"], None, None)), 2);
    assert_eq!(code(&midna(&s, &["call", "session.get", "{\"nope\":1}"], None, None)), 2);
    // 3: daemon unreachable.
    let o = midna("/tmp/midna-no-such.sock", &["list"], None, None);
    assert_eq!(code(&o), 3);
    // `schema` works without a daemon.
    let o = midna("/tmp/midna-no-such.sock", &["schema"], None, None);
    assert_eq!(code(&o), 0);
    assert!(stdout(&o).contains("\"rpc.discover\""));
}

#[test]
fn verbs_drive_a_session() {
    let d = D::start();
    let s = d.sock();
    let o = midna(&s, &["open", "--cwd", "/tmp", "--name", "t1", "--", "/bin/sh"], None, None);
    assert_eq!(code(&o), 0, "{o:?}");
    let id = stdout(&o).trim().to_string();
    assert_eq!(code(&midna(&s, &["send", &id, "echo", "cli-$((1+1))"], None, None)), 0);
    let t0 = std::time::Instant::now();
    loop {
        let o = midna(&s, &["read", &id, "--lines", "20"], None, None);
        if stdout(&o).contains("cli-2") {
            break;
        }
        assert!(t0.elapsed().as_secs() < 5, "output never appeared");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let o = midna(&s, &["list"], None, None);
    assert!(stdout(&o).contains("t1"));
    let o = midna(&s, &["attention", "need", "a", "decision", "--note"], None, Some(&id));
    assert_eq!(code(&o), 0, "{o:?}");
    let o = midna(&s, &["needs", "--json"], None, None);
    let items: Value = serde_json::from_str(&stdout(&o)).unwrap();
    assert_eq!(items[0]["kind"], "note");
    assert_eq!(items[0]["session_id"], id.as_str());
    let o = midna(&s, &["check", "cli", "close --force x"], None, None);
    assert!(stdout(&o).starts_with("ask (default)"), "{}", stdout(&o));
    let o = midna(&s, &["events", "--kind", "session.", "--json"], None, None);
    assert!(stdout(&o).lines().count() >= 1);
    assert_eq!(code(&midna(&s, &["insights"], None, None)), 0);
    assert_eq!(code(&midna(&s, &["settings", "get", "theme"], None, None)), 0);
    assert_eq!(code(&midna(&s, &["close", &id], None, None)), 0);
}

#[test]
fn hook_bridge_prints_claude_decision() {
    let d = D::start();
    let s = d.sock();
    let mut h = d.human();
    let sess = h.call_value("session.open", json!({ "kind": "shell", "cwd": "/tmp", "command": ["/bin/sh"] })).unwrap();
    let sid = sess["id"].as_str().unwrap();
    h.call_value("rule.add", json!({ "effect": "deny", "matcher": { "kind": "tool", "pattern": "Bash(rm -rf*)" } })).unwrap();
    h.call_value("rule.add", json!({ "effect": "allow", "matcher": { "kind": "tool", "pattern": "Bash(ls*)" } })).unwrap();
    let pre = |cmd: &str| json!({ "hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": { "command": cmd } }).to_string();
    let o = midna(&s, &["hook", "claude"], Some(&pre("rm -rf /")), Some(sid));
    assert_eq!(code(&o), 0);
    let v: Value = serde_json::from_str(&stdout(&o)).unwrap();
    assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
    let o = midna(&s, &["hook", "claude"], Some(&pre("ls -la")), Some(sid));
    let v: Value = serde_json::from_str(&stdout(&o)).unwrap();
    assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "allow");
    // No rule: no opinion, no output (Claude's own permission flow decides).
    let o = midna(&s, &["hook", "claude"], Some(&pre("cargo build")), Some(sid));
    assert_eq!((code(&o), stdout(&o).trim().to_string()), (0, String::new()));
    // The PreToolUse drove status to working; Stop -> done.
    let st = h.call_value("session.get", json!({ "id": sid })).unwrap();
    assert_eq!(st["status"]["state"], "working");
    midna(&s, &["hook", "claude"], Some(r#"{"hook_event_name":"Stop"}"#), Some(sid));
    assert_eq!(h.call_value("session.get", json!({ "id": sid })).unwrap()["status"]["state"], "done");
    // Codex notify (payload in argv).
    let o = midna(&s, &["hook", "codex", "notify", r#"{"type":"agent-turn-complete","input-messages":["hi"]}"#], None, Some(sid));
    assert_eq!(code(&o), 0);
    // Statusline prints a line and records cost.
    let o = midna(&s, &["hook", "claude", "statusline"], Some(r#"{"model":{"display_name":"Opus"},"cost":{"total_cost_usd":1.5}}"#), Some(sid));
    assert_eq!(stdout(&o).trim(), "Opus · $1.50");
    let ev = h.call_value("events.list", json!({ "filter": { "kinds": ["agent.cost"] } })).unwrap();
    assert_eq!(ev[0]["data"]["delta_usd"], 1.5);
    // Outside midna (no MIDNA_SESSION) or with the daemon down: silent success.
    assert_eq!(code(&midna(&s, &["hook", "claude"], Some(&pre("rm -rf /")), None)), 0);
    assert_eq!(code(&midna("/tmp/midna-no-such.sock", &["hook", "claude"], Some(&pre("x")), Some(sid))), 0);
    // The hook settings file midna passes to `claude --settings` points at this CLI.
    let settings: Value = serde_json::from_slice(&std::fs::read(d.home.join("hooks/claude-settings.json")).unwrap()).unwrap();
    let cmd = settings["hooks"]["PreToolUse"][0]["hooks"][0]["command"].as_str().unwrap();
    assert!(cmd.contains(env!("CARGO_BIN_EXE_midna")) && cmd.ends_with("hook claude"));
}

#[test]
fn global_hook_stands_down_when_midna_injected_its_own() {
    let d = D::start();
    let s = d.sock();
    let mut h = d.human();
    h.call_value("rule.add", json!({ "effect": "deny", "matcher": { "kind": "tool", "pattern": "Bash(rm -rf*)" } })).unwrap();
    let sess = h.call_value("session.open", json!({ "kind": "shell", "cwd": "/tmp", "command": ["/bin/sh"] })).unwrap();
    let sid = sess["id"].as_str().unwrap();
    let pre = json!({ "hook_event_name": "PreToolUse", "tool_name": "Bash", "tool_input": { "command": "rm -rf /" } }).to_string();
    let run = |injected: bool| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_midna"));
        cmd.args(["hook", "claude", "--global"]).env("MIDNA_SOCKET", &s).env("MIDNA_SESSION", sid).env_remove("MIDNA_HOME");
        if injected {
            cmd.env("MIDNA_HOOKS_INJECTED", "1");
        } else {
            cmd.env_remove("MIDNA_HOOKS_INJECTED");
        }
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        child.stdin.take().unwrap().write_all(pre.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    };
    // midna already passed its own hooks to this agent: the global entry does nothing.
    let o = run(true);
    assert_eq!((code(&o), stdout(&o).trim().to_string()), (0, String::new()));
    assert_eq!(h.call_value("session.get", json!({ "id": sid })).unwrap()["status"]["state"], "idle");
    // A hand-typed agent: the global entry reports and applies policy.
    let o = run(false);
    let v: Value = serde_json::from_str(&stdout(&o)).unwrap();
    assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
    assert_eq!(h.call_value("session.get", json!({ "id": sid })).unwrap()["status"]["state"], "working");
}

#[test]
fn mcp_exposes_catalog_as_tools() {
    let d = D::start();
    let input = [
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "t", "version": "0" } } }),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "daemon_info", "arguments": {} } }),
        json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": { "name": "rule_remove", "arguments": { "id": "r_x" } } }),
    ]
    .iter()
    .map(|v| v.to_string() + "\n")
    .collect::<String>();
    let o = midna(&d.sock(), &["mcp"], Some(&input), None);
    assert_eq!(code(&o), 0);
    let replies: Vec<Value> = stdout(&o).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(replies.len(), 4, "notification gets no reply");
    assert_eq!(replies[0]["result"]["serverInfo"]["name"], "midna");
    let tools = replies[1]["result"]["tools"].as_array().unwrap();
    // minus stream.attach, events.subscribe and secret.exec_env, plus capabilities, explain and guide
    assert_eq!(tools.len(), midna_proto::catalog().len() - 3 + 3);
    assert!(!tools.iter().any(|t| t["name"] == "secret_exec_env"), "secret values never go through MCP");
    let open = tools.iter().find(|t| t["name"] == "session_open").unwrap();
    assert_eq!(open["inputSchema"]["type"], "object");
    assert!(open["description"].as_str().unwrap().contains("agent"));
    assert_eq!(replies[2]["result"]["isError"], false);
    assert_eq!(replies[2]["result"]["structuredContent"]["role"], "agent");
    assert_eq!(replies[3]["result"]["isError"], true);
}

#[test]
fn discoverability() {
    let s = "/tmp/midna-no-such.sock";
    // Every verb has complete `--help` (and `help <verb>`), offline, and never runs the verb.
    let top = stdout(&midna(s, &["help"], None, None));
    for v in ["capabilities", "skill", "explain", "schema", "projects", "open", "send", "key", "restart", "queue", "attention", "rules", "triggers", "settings", "window", "mcp"] {
        assert!(top.contains(&format!("  {v}")), "help lacks {v}");
        let o = midna(s, &[v, "--help"], None, None);
        assert_eq!(code(&o), 0, "{v} --help: {o:?}");
        assert!(stdout(&o).contains(&format!("usage: midna {v}")), "{v}: {}", stdout(&o));
        assert_eq!(stdout(&midna(s, &["help", v], None, None)), stdout(&o));
    }
    let o = midna(s, &["rules", "--help"], None, None);
    assert!(stdout(&o).contains("rule.remove [rule_remove]  human only"), "{}", stdout(&o));
    assert_eq!(code(&midna(s, &["help", "frobnicate"], None, None)), 2);
    // capabilities, skill and schema work without a daemon.
    let caps = stdout(&midna(s, &["capabilities"], None, None));
    assert!(caps.contains("HUMAN ONLY") && caps.contains("rule.remove") && caps.contains("approve.from_cli"), "{caps}");
    assert!(stdout(&midna(s, &["skill"], None, None)).starts_with("---\nname: midna"));
    let one: Value = serde_json::from_str(&stdout(&midna(s, &["schema", "rule_add"], None, None))).unwrap();
    assert_eq!(one["method"], "rule.add");
    assert!(one["params"]["properties"]["matcher"].is_object());
    assert!(stdout(&midna(s, &["schema", "--list"], None, None)).lines().count() >= midna_proto::catalog().len());
    assert_eq!(code(&midna(s, &["schema", "nope.nope"], None, None)), 2);
    // explain: topics offline.
    assert!(stdout(&midna(s, &["explain", "rules"], None, None)).contains("request-removal"));
}

#[test]
fn explain_and_guidance_against_a_daemon() {
    let d = D::start();
    let s = d.sock();
    let o = midna(&s, &["open", "--cwd", "/tmp", "--name", "x", "--", "/bin/sh"], None, None);
    let id = stdout(&o).trim().to_string();
    let o = midna(&s, &["explain", &id], None, None);
    assert_eq!(code(&o), 0, "{o:?}");
    assert!(stdout(&o).contains("status: idle") && stdout(&o).contains("why: reason"), "{}", stdout(&o));
    let o = midna(&s, &["projects", "--json"], None, None);
    let pid = serde_json::from_str::<Value>(&stdout(&o)).unwrap()[0]["id"].as_str().unwrap().to_string();
    let o = midna(&s, &["projects", "add-command", &pid, "--name", "test", "--pinned", "--", "cargo", "test"], None, None);
    assert_eq!(code(&o), 0, "{o:?}");
    assert!(stdout(&midna(&s, &["explain", &pid], None, None)).contains("cargo test (pinned)"));
    // A human-only setting from an agent: refused, with the request already made and what to do.
    let o = midna(&s, &["settings", "set", "agents.may_move_windows", "true"], None, Some(&id));
    assert_eq!(code(&o), 1);
    let err = String::from_utf8_lossy(&o.stderr).into_owned();
    assert!(err.contains("Next:") && err.contains("midna explain n_"), "{err}");
    let n = err.split("needs-you item ").nth(1).unwrap().split_whitespace().next().unwrap();
    let o = midna(&s, &["explain", n], None, None);
    assert!(stdout(&o).contains("only the human"), "{}", stdout(&o));
    let o = midna(&s, &["key", &id, "ctrl-c"], None, None);
    assert_eq!(code(&o), 0, "{o:?}");
    assert_eq!(code(&midna(&s, &["explain", "nothing-like-this"], None, None)), 1);
}

#[test]
fn agents_get_mcp_skill_and_hint_without_global_config() {
    // Never launch a real agent in tests: the agent binary is /bin/echo.
    let d = D::start_with(|c| c.agent_bin = Some("/bin/echo".into()));
    let mut h = d.human();
    let home = d.home.clone();
    let mcp: Value = serde_json::from_slice(&std::fs::read(home.join("hooks/mcp.json")).unwrap()).unwrap();
    assert_eq!(mcp["mcpServers"]["midna"]["command"], env!("CARGO_BIN_EXE_midna"));
    assert_eq!(mcp["mcpServers"]["midna"]["args"], json!(["mcp"]));
    assert!(std::fs::read_to_string(home.join("hooks/SKILL.md")).unwrap().contains("Never route around a denial"));
    let sess = h.call_value("session.open", json!({ "kind": "agent", "agent": "claude", "cwd": "/tmp", "prompt": "hi" })).unwrap();
    let cmd: Vec<String> = serde_json::from_value(sess["command"].clone()).unwrap();
    let pos = |f: &str| cmd.iter().position(|a| a == f).unwrap_or_else(|| panic!("{f} missing from {cmd:?}"));
    assert_eq!(cmd[pos("--mcp-config") + 1], home.join("hooks/mcp.json").to_string_lossy());
    // --mcp-config is variadic in Claude: --settings must follow it before the prompt.
    assert_eq!(pos("--settings"), pos("--mcp-config") + 2);
    assert!(cmd[pos("--append-system-prompt") + 1].contains("midna capabilities"));
    assert_eq!(cmd.last().unwrap(), "hi");
    // `midna open --agent … --resume ID -- <agent args>`
    let o = midna(&d.sock(), &["open", "--agent", "claude", "--cwd", "/tmp", "--resume", "c9", "--prompt", "go", "--json", "--", "--model", "opus"], None, None);
    assert_eq!(code(&o), 0, "{o:?}");
    let v: Value = serde_json::from_str(&stdout(&o)).unwrap();
    assert_eq!(v["agent_args"], json!(["--model", "opus"]));
    let cmd: Vec<String> = serde_json::from_value(v["command"].clone()).unwrap();
    assert_eq!(&cmd[cmd.len() - 6..], ["--model", "opus", "--resume", "c9", "--", "go"], "{cmd:?}");
    assert_eq!(code(&midna(&d.sock(), &["open", "--resume", "c9"], None, None)), 2, "--resume without --agent");
    let sess = h.call_value("session.open", json!({ "kind": "agent", "agent": "codex", "cwd": "/tmp" })).unwrap();
    let cmd: Vec<String> = serde_json::from_value(sess["command"].clone()).unwrap();
    let joined = cmd.join(" ");
    assert!(joined.contains("mcp_servers.midna.command=") && joined.contains(r#"mcp_servers.midna.args=["mcp"]"#), "{joined}");
    assert!(joined.contains(&format!("MIDNA_SESSION=\"{}\"", sess["id"].as_str().unwrap())), "{joined}");
    assert!(joined.contains("developer_instructions="), "{joined}");
    // Both settings off: none of it.
    h.call_value("settings.set", json!({ "key": "agents.mcp", "value": false })).unwrap();
    h.call_value("settings.set", json!({ "key": "agents.system_hint", "value": false })).unwrap();
    let sess = h.call_value("session.open", json!({ "kind": "agent", "agent": "claude", "cwd": "/tmp" })).unwrap();
    let joined = serde_json::from_value::<Vec<String>>(sess["command"].clone()).unwrap().join(" ");
    assert!(!joined.contains("--mcp-config") && !joined.contains("--append-system-prompt") && joined.contains("--settings"), "{joined}");
    // Every terminal knows where the guide is.
    let sh = h.call_value("session.open", json!({ "kind": "shell", "cwd": "/tmp", "command": ["/bin/sh"] })).unwrap();
    let sid = sh["id"].as_str().unwrap();
    h.call_value("session.input", json!({ "id": sid, "text": "echo skill=$MIDNA_SKILL", "enter": true })).unwrap();
    let want = format!("skill={}", home.join("hooks/SKILL.md").display());
    let t0 = std::time::Instant::now();
    while !h.call_value("session.read", json!({ "id": sid })).unwrap()["text"].as_str().unwrap().contains(&want) {
        assert!(t0.elapsed().as_secs() < 5, "MIDNA_SKILL not set");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[test]
fn queue_add_list_move_remove() {
    let d = D::start();
    let s = d.sock();
    let o = midna(&s, &["open", "--cwd", "/tmp", "--name", "q", "--", "/bin/sh"], None, None);
    assert_eq!(code(&o), 0, "{o:?}");
    let id = stdout(&o).trim().to_string();
    let o = midna(&s, &["open", "--cwd", "/tmp", "--name", "other", "--", "/bin/sh"], None, None);
    assert_eq!(code(&o), 0, "{o:?}");
    let other = stdout(&o).trim().to_string();
    let ok = |o: &Output| assert_eq!(code(o), 0, "{}{}", stdout(o), String::from_utf8_lossy(&o.stderr));
    let json_of = |o: &Output| -> Value { serde_json::from_str(&stdout(o)).unwrap_or_else(|e| panic!("{e}: {}", stdout(o))) };
    // Paused first so nothing is typed while the test looks at the queue.
    let o = midna(&s, &["queue", "pause", "--session", &id, "--json"], None, None);
    ok(&o);
    assert_eq!(json_of(&o)["paused"], true);
    // Defaults to the caller's terminal (MIDNA_SESSION); text from args, `--`, or stdin.
    let o = midna(&s, &["queue", "add", "/compact", "--json"], None, Some(&id));
    ok(&o);
    let a = json_of(&o);
    assert_eq!(a["text"], "/compact");
    assert_eq!(a["when"], json!({ "kind": "idle" }));
    let o = midna(&s, &["queue", "add", "--session", &id, "--idle", "10m", "--no-enter", "next", "step", "--json"], None, None);
    ok(&o);
    let b = json_of(&o);
    assert_eq!(b["text"], "next step");
    assert_eq!(b["enter"], false);
    assert_eq!(b["when"], json!({ "kind": "idle_for", "minutes": 10 }));
    let o = midna(&s, &["queue", "add", "--session", &id, "--after", &other, "-", "--json"], Some("from stdin\n"), None);
    ok(&o);
    let c = json_of(&o);
    assert_eq!(c["text"], "from stdin");
    assert_eq!(c["when"], json!({ "kind": "after", "session": other }));
    let ids = |v: &Value| -> Vec<String> { v["items"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap().to_string()).collect() };
    let (a, b, c) = (a["id"].as_str().unwrap().to_string(), b["id"].as_str().unwrap().to_string(), c["id"].as_str().unwrap().to_string());
    let o = midna(&s, &["queue", "list", "--session", &id, "--json"], None, None);
    ok(&o);
    assert_eq!(ids(&json_of(&o)), [a.clone(), b.clone(), c.clone()]);
    // Human list: number, when in words, text, paused.
    let o = midna(&s, &["queue", "--session", &id], None, None);
    ok(&o);
    let text = stdout(&o);
    assert!(text.contains("(paused)") && text.contains("after 10 min idle") && text.contains("from stdin") && text.contains(" 1 "), "{text}");
    // mv is 1-based: the last one to the front.
    let o = midna(&s, &["queue", "mv", &c, "1", "--session", &id, "--json"], None, None);
    ok(&o);
    assert_eq!(ids(&json_of(&o)), [c.clone(), a.clone(), b.clone()]);
    assert_eq!(code(&midna(&s, &["queue", "mv", &c, "0", "--session", &id], None, None)), 2);
    let o = midna(&s, &["queue", "rm", &a, "--session", &id], None, None);
    ok(&o);
    let o = midna(&s, &["queue", "list", "--session", &id, "--json"], None, None);
    assert_eq!(ids(&json_of(&o)), [c.clone(), b.clone()]);
    let o = midna(&s, &["queue", "clear", "--session", &id], None, None);
    ok(&o);
    let o = midna(&s, &["queue", "--session", &id], None, None);
    assert!(stdout(&o).contains("queue is empty"), "{}", stdout(&o));
    // Usage errors never reach the daemon.
    assert_eq!(code(&midna(&s, &["queue", "add", "x", "--idle", "soon", "--session", &id], None, None)), 2);
    assert_eq!(code(&midna(&s, &["queue", "add", "x", "--idle", "5", "--at", "18:00", "--session", &id], None, None)), 2);
}

#[test]
fn get_no_wait_and_needs_get() {
    let d = D::start();
    let s = d.sock();
    let mut h = d.human();
    let open = |h: &mut midna_proto::Client| -> String {
        let v = h.call_value("session.open", json!({ "kind": "shell", "cwd": "/tmp", "command": ["/bin/sh"] })).unwrap();
        v["id"].as_str().unwrap().to_string()
    };
    let (me, target) = (open(&mut h), open(&mut h));
    // `get` = session.get.
    let o = midna(&s, &["get", &target], None, None);
    assert_eq!(code(&o), 0, "{o:?}");
    assert!(stdout(&o).contains(&target) && stdout(&o).contains("/tmp"), "{}", stdout(&o));
    let o = midna(&s, &["get", &target, "--json"], None, None);
    assert_eq!(serde_json::from_str::<Value>(&stdout(&o)).unwrap()["id"], target.as_str());

    // `close --force --no-wait`: the needs-you id on stdout, exit 4, right away.
    let t0 = std::time::Instant::now();
    let o = midna(&s, &["close", &target, "--force", "--no-wait"], None, Some(&me));
    assert_eq!(code(&o), 4, "{o:?}");
    assert!(t0.elapsed() < std::time::Duration::from_secs(5));
    let nid = stdout(&o).trim().to_string();
    assert!(nid.starts_with("n_"), "{o:?}");
    let o = midna(&s, &["needs", "get", &nid, "--json"], None, None);
    assert_eq!(serde_json::from_str::<Value>(&stdout(&o)).unwrap()["state"], "open");
    // Still open after a short wait: exit 4.
    let o = midna(&s, &["needs", "wait", &nid, "--timeout", "1"], None, None);
    assert_eq!(code(&o), 4, "{o:?}");
    // The human approves; `needs wait` reports the close went through.
    h.call_value("needs_you.resolve", json!({ "id": nid, "resolution": { "kind": "approve", "scope": { "kind": "once" } } })).unwrap();
    let o = midna(&s, &["needs", "wait", &nid, "--json"], None, None);
    assert_eq!(code(&o), 0, "{o:?}");
    let v: Value = serde_json::from_str(&stdout(&o)).unwrap();
    assert_eq!((v["state"].as_str(), v["result"]["ok"].as_bool()), (Some("resolved"), Some(true)), "{v}");
    let o = midna(&s, &["needs", "get", &nid], None, None);
    assert!(stdout(&o).contains("resolved: approve"), "{}", stdout(&o));
}

#[test]
fn notify_history_lists_unread_and_read_marks_them() {
    let d = D::start();
    let s = d.sock();
    let mut h = d.human();
    h.call_value("settings.set", json!({ "key": "notify.bell.agent", "value": true })).unwrap();
    let sess = h.call_value("session.open", json!({ "kind": "shell", "cwd": "/tmp", "command": ["/bin/sh"] })).unwrap();
    let sid = sess["id"].as_str().unwrap();
    // The first look starts the read marker: only what comes after is unread.
    assert_eq!(code(&midna(&s, &["notify", "history"], None, None)), 0);
    for title in ["first", "second"] {
        assert_eq!(code(&midna(&s, &["notify", "send", title], None, Some(sid))), 0);
    }
    let history = |args: &[&str]| -> Value {
        let o = midna(&s, &[&["notify", "history", "--json"], args].concat(), None, None);
        assert_eq!(code(&o), 0, "{o:?}");
        serde_json::from_str(&stdout(&o)).unwrap()
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while history(&[])["items"].as_array().map_or(0, Vec::len) < 2 {
        assert!(std::time::Instant::now() < deadline, "notifications never posted");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let v = history(&[]);
    assert_eq!(v["unread"], 2, "{v}");
    let items = v["items"].as_array().unwrap();
    let text = stdout(&midna(&s, &["notify", "history", "--unread"], None, None));
    assert!(text.starts_with("2 unread") && text.contains("• ") && text.contains("second"), "{text}");

    // Read up to the older one: the newer one stays unread.
    let older = items[1]["seq"].as_u64().unwrap().to_string();
    let o = midna(&s, &["notify", "read", &older], None, None);
    assert!(stdout(&o).contains("1 unread"), "{o:?}");
    assert_eq!(history(&["--unread"])["unread"], 1);
    let o = midna(&s, &["notify", "read"], None, None);
    assert!(stdout(&o).contains("0 unread"), "{o:?}");
    let text = stdout(&midna(&s, &["notify", "history", "--unread"], None, None));
    assert!(text.contains("no unread notifications"), "{text}");
    assert_eq!(code(&midna(&s, &["notify", "read", "soon"], None, None)), 2);
}
