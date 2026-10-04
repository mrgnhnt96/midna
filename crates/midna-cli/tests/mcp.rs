//! `midna mcp` driven over stdio like a real MCP client, against an in-process daemon on a
//! temp MIDNA_HOME. The MCP server runs "inside" a midna terminal (MIDNA_SESSION set), as it
//! does for agents midna launches, and does a realistic agent workflow through MCP only.
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

static N: AtomicU32 = AtomicU32::new(0);

struct D {
    handle: Option<midnad::Handle>,
    home: PathBuf,
}

impl D {
    fn start() -> D {
        let home = PathBuf::from(format!("/tmp/midna-m-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&home);
        let mut cfg = midnad::Config::for_home(home.clone());
        cfg.app_path = Some(std::env::current_exe().unwrap().to_string_lossy().into_owned());
        cfg.cli_path = env!("CARGO_BIN_EXE_midna").into();
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

/// A running `midna mcp` with a line reader that can time out.
struct Mcp {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
}

impl Mcp {
    fn spawn(sock: &str, session: Option<&str>) -> Mcp {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_midna"));
        cmd.arg("mcp").env("MIDNA_SOCKET", sock).env_remove("MIDNA_SESSION").env_remove("MIDNA_HOME");
        if let Some(s) = session {
            cmd.env("MIDNA_SESSION", s);
        }
        let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for l in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(l).is_err() {
                    break;
                }
            }
        });
        Mcp { child, stdin, lines, next_id: 1 }
    }

    fn send_raw(&mut self, line: &str) {
        writeln!(self.stdin, "{line}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn recv(&self) -> Value {
        let l = self.lines.recv_timeout(Duration::from_secs(20)).expect("midna mcp did not answer");
        serde_json::from_str(&l).unwrap_or_else(|e| panic!("not JSON ({e}): {l}"))
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send_raw(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string());
        let r = self.recv();
        assert_eq!(r["jsonrpc"], "2.0");
        assert_eq!(r["id"], id, "{r}");
        r
    }

    /// tools/call; returns the tool result (panics on a JSON-RPC error).
    fn tool(&mut self, name: &str, args: Value) -> Value {
        let r = self.request("tools/call", json!({ "name": name, "arguments": args }));
        assert!(r.get("error").is_none(), "{name}: {r}");
        r["result"].clone()
    }

    fn ok(&mut self, name: &str, args: Value) -> Value {
        let r = self.tool(name, args);
        assert_eq!(r["isError"], false, "{name} failed: {}", text(&r));
        r["structuredContent"].clone()
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn text(r: &Value) -> String {
    r["content"][0]["text"].as_str().unwrap_or("").to_string()
}

fn init(m: &mut Mcp) -> Value {
    let r = m.request("initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "0" } }));
    m.send_raw(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string());
    r["result"].clone()
}

#[test]
fn protocol_conformance() {
    let d = D::start();
    let mut m = Mcp::spawn(&d.sock(), None);
    let init = init(&mut m);
    assert_eq!(init["protocolVersion"], "2025-06-18");
    assert_eq!(init["serverInfo"]["name"], "midna");
    assert!(init["capabilities"]["tools"].is_object());
    assert!(init["instructions"].as_str().unwrap().contains("capabilities"));
    // An unknown protocol version gets our latest instead of an echo.
    let r = m.request("initialize", json!({ "protocolVersion": "1999-01-01", "capabilities": {} }));
    assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(m.request("ping", json!({}))["result"], json!({}));

    let tools = m.request("tools/list", json!({}))["result"]["tools"].as_array().unwrap().clone();
    // Every catalog method except the two streaming ones, plus capabilities/explain/guide.
    assert_eq!(tools.len(), midna_proto::catalog().len() - 2 + 3);
    for t in &tools {
        let name = t["name"].as_str().unwrap();
        assert!(name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'), "{name}");
        assert_eq!(t["inputSchema"]["type"], "object", "{name}");
        assert!(t["description"].as_str().unwrap().len() > 20, "{name}");
        assert!(t["annotations"]["readOnlyHint"].is_boolean(), "{name}");
    }
    let by = |n: &str| tools.iter().find(|t| t["name"] == n).unwrap().clone();
    assert!(by("session_open")["inputSchema"]["required"].as_array().unwrap().contains(&json!("kind")));
    assert!(by("project_remove")["description"].as_str().unwrap().contains("HUMAN ONLY"));
    assert!(by("rule_remove")["description"].as_str().unwrap().contains("rule_request_removal"));
    assert_eq!(by("session_list")["annotations"]["readOnlyHint"], true);
    assert_eq!(by("session_close")["annotations"]["destructiveHint"], true);

    // JSON-RPC errors.
    let r = m.request("tools/call", json!({ "name": "no_such_tool", "arguments": {} }));
    assert_eq!(r["error"]["code"], -32602);
    let r = m.request("tools/call", json!({ "name": "session_list", "arguments": [1] }));
    assert_eq!(r["error"]["code"], -32602);
    let r = m.request("tools/call", json!({}));
    assert_eq!(r["error"]["code"], -32602);
    let r = m.request("prompts/list", json!({}));
    assert_eq!(r["error"]["code"], -32601);
    m.send_raw("{not json");
    assert_eq!(m.recv()["error"]["code"], -32700);
    m.send_raw(r#"{"jsonrpc":"2.0","id":99}"#);
    assert_eq!(m.recv()["error"]["code"], -32600);
    // Notifications and stray responses get no reply: the next answer is for the next request.
    m.send_raw(r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}"#);
    m.send_raw(r#"{"jsonrpc":"2.0","id":5,"result":{}}"#);
    assert_eq!(m.request("ping", json!({}))["result"], json!({}));

    // Bad params from the daemon are a tool error (the model should read it), with a next step.
    let r = m.tool("session_get", json!({ "nope": 1 }));
    assert_eq!(r["isError"], true);
    assert!(text(&r).contains("Next:"), "{}", text(&r));
    // Arguments may be omitted.
    let r = m.request("tools/call", json!({ "name": "daemon_info" }));
    assert_eq!(r["result"]["isError"], false);
    assert_eq!(r["result"]["structuredContent"]["role"], "agent");

    // The guide is a tool and a resource.
    assert!(text(&m.tool("guide", json!({}))).contains("Never route around a denial"));
    let res = m.request("resources/list", json!({}))["result"]["resources"].clone();
    assert_eq!(res[0]["uri"], "midna://skill");
    let r = m.request("resources/read", json!({ "uri": "midna://skill" }));
    assert!(r["result"]["contents"][0]["text"].as_str().unwrap().contains("# midna"));
    assert_eq!(m.request("resources/read", json!({ "uri": "midna://nope" }))["error"]["code"], -32002);
    let caps = m.tool("capabilities", json!({}));
    assert!(text(&caps).contains("HUMAN ONLY"));
    assert!(caps["structuredContent"]["methods"].as_array().unwrap().len() >= midna_proto::catalog().len());
}

/// The end-to-end agent workflow, through MCP only.
#[test]
fn agent_workflow_through_mcp_only() {
    let d = D::start();
    let mut human = d.human();
    // The human has a project, and the agent lives in a terminal in it.
    let dir = d.home.join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let project = human.call_value("project.add", json!({ "path": dir, "name": "demo" })).unwrap();
    let pid = project["id"].as_str().unwrap().to_string();
    let me = human.call_value("session.open", json!({ "project_id": pid, "kind": "shell", "command": ["/bin/sh"], "name": "agent" })).unwrap();
    let me = me["id"].as_str().unwrap().to_string();

    let mut m = Mcp::spawn(&d.sock(), Some(&me));
    init(&mut m);

    // Orient: who am I, which projects exist.
    let info = m.ok("daemon_info", json!({}));
    assert_eq!(info["role"], "agent");
    assert_eq!(info["session"], me.as_str());
    let projects = m.ok("project_list", json!({}));
    assert_eq!(projects["result"][0]["id"], pid.as_str());

    // Open a shell (defaults to my project), run a command, read the output.
    let sh = m.ok("session_open", json!({ "kind": "shell", "command": ["/bin/sh"], "name": "build" }));
    let sid = sh["id"].as_str().unwrap().to_string();
    assert_eq!(sh["project_id"], pid.as_str());
    m.ok("session_input", json!({ "id": sid, "text": "echo mcp-$((2+3))", "enter": true }));
    let t0 = Instant::now();
    loop {
        let r = m.ok("session_read", json!({ "id": sid, "lines": 20 }));
        if r["text"].as_str().unwrap().lines().any(|l| l.trim() == "mcp-5") {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(10), "output never appeared: {}", r["text"]);
        std::thread::sleep(Duration::from_millis(50));
    }

    // Add a rule, try to remove it (refused, with the request path), then request removal.
    let rule = m.ok("rule_add", json!({ "effect": "deny", "matcher": { "kind": "command", "pattern": "rm -rf /*" }, "scope": { "kind": "project", "id": pid } }));
    let rid = rule["id"].as_str().unwrap().to_string();
    let r = m.tool("rule_remove", json!({ "id": rid }));
    assert_eq!(r["isError"], true);
    let msg = text(&r);
    assert!(msg.contains("human") && msg.contains("rule_request_removal") && msg.contains(&rid), "{msg}");
    assert_eq!(r["structuredContent"]["error"]["code"], 2);
    let item = m.ok("rule_request_removal", json!({ "id": rid, "reason": "too broad for the build dir" }));
    assert_eq!(item["kind"], "rule_removal");
    let rules = human.call_value("rule.list", json!({})).unwrap();
    assert_eq!(rules[0]["id"], rid.as_str(), "the rule stays until the human removes it");
    assert!(rules[0]["removal_request"].is_object());
    let why = text(&m.tool("explain", json!({ "target": rid })));
    assert!(why.contains("removal requested") && why.contains("too broad"), "{why}");
    let why = text(&m.tool("explain", json!({ "target": "command", "value": "rm -rf /tmp/x" })));
    assert!(why.contains(&rid), "{why}");

    // Draft a trigger: it waits for the human's secret.
    let t = m.ok(
        "trigger_add",
        json!({ "name": "PR review", "source": "github", "event": "pull_request.opened", "filter": { "repo": "me/demo" },
                "action": { "kind": "start_agent", "project_id": pid, "agent": "claude", "prompt_template": "Review PR {{pr.number}}" } }),
    );
    assert_eq!(t["state"], "needs_secret");
    let why = text(&m.tool("explain", json!({ "target": t["id"] })));
    assert!(why.contains("paste") && why.contains("Review PR"), "{why}");

    // Settings: a normal key changes; a human-only key becomes a request.
    let s = m.ok("settings_set", json!({ "key": "theme", "value": "light" }));
    assert_eq!(s["value"], "light");
    let r = m.tool("settings_set", json!({ "key": "approve.from_cli", "value": true }));
    assert_eq!(r["isError"], true);
    assert!(text(&r).contains("needs-you") && text(&r).contains("Don't retry"), "{}", text(&r));
    assert_eq!(human.call_value("settings.get", json!({ "key": "approve.from_cli" })).unwrap()["value"], false);

    // Raise attention.
    let n = m.ok("needs_you_raise", json!({ "kind": "blocked", "message": "need the staging password" }));
    assert_eq!(n["session_id"], me.as_str());

    // The human sees everything the agent asked for.
    let needs = human.call_value("needs_you.list", json!({})).unwrap();
    let kinds: Vec<&str> = needs.as_array().unwrap().iter().map(|n| n["kind"].as_str().unwrap()).collect();
    for k in ["rule_removal", "secret_needed", "approval", "blocked"] {
        assert!(kinds.contains(&k), "missing {k} in {kinds:?}");
    }
    // `explain` on my own terminal shows what it is waiting on.
    let why = text(&m.tool("explain", json!({ "target": me })));
    assert!(why.contains("waiting on the human") && why.contains("staging password"), "{why}");

    m.ok("session_close", json!({ "id": sid }));
}
