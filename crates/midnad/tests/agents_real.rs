//! Replays hook payloads recorded from the real Claude Code 2.1.288 and Codex 0.160.0 CLIs
//! (launched by midna, see docs/STATUS-agents.md) through `agent.hook` / `policy.request`.
//! Fixture rows: `{agent, event, ms, payload}` as `midna hook` received them.
mod common;
use common::*;
use midna_proto::Client;
use serde_json::{Value, json};

fn fixture(name: &str) -> Vec<Value> {
    let p = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{p}: {e}")).lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn hook(a: &mut Client, row: &Value) -> Value {
    call(a, "agent.hook", json!({ "agent": row["agent"], "event": row["event"], "payload": row["payload"] }))
}

fn state(h: &mut Client, sid: &str) -> String {
    call(h, "session.get", json!({ "id": sid }))["status"]["state"].as_str().unwrap().to_string()
}

fn items(h: &mut Client) -> Vec<Value> {
    call(h, "needs_you.list", json!({})).as_array().unwrap().clone()
}

fn count(rows: &[Value], event: &str) -> usize {
    rows.iter().filter(|r| r["event"] == event).count()
}

#[test]
fn claude_native_permission_replay() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let mut a = d.agent(Some(&sid));
    let rows = fixture("claude-2.1.288-native-permission.jsonl");
    // 2.1.288 sends PermissionRequest when its dialog opens; Notification/permission_prompt
    // only ~6s later, and only if nobody answered.
    let first = rows.iter().position(|r| r["event"] == "PermissionRequest").unwrap();
    assert_eq!(rows[first + 1]["event"], "Notification");
    assert_eq!(rows[first + 1]["payload"]["notification_type"], "permission_prompt");
    for r in &rows[..=first] {
        hook(&mut a, r);
    }
    assert_eq!(state(&mut h, &sid), "needs_you");
    let open = items(&mut h);
    assert_eq!(open.len(), 1);
    assert_eq!(open[0]["kind"], "permission_prompt");
    assert_eq!(open[0]["title"], "permission Bash(touch c.txt && ls)");
    hook(&mut a, &rows[first + 1]);
    assert_eq!(items(&mut h).len(), 1, "the late Notification must not raise a second item");
    for r in &rows[first + 2..] {
        hook(&mut a, r);
    }
    // The recording ends with Stop + SubagentStop (title generation), which must not reopen work.
    assert_eq!(state(&mut h, &sid), "done");
    let s = call(&mut h, "insights.summary", json!({ "range": "today" }));
    assert_eq!(s["totals"]["messages"].as_u64(), Some(count(&rows, "UserPromptSubmit") as u64));
    let last_total = rows.iter().rev().find_map(|r| r["payload"]["cost"]["total_cost_usd"].as_f64()).unwrap();
    // insights rounds spend to 6 decimals
    assert!((s["totals"]["spend_usd"].as_f64().unwrap() - last_total).abs() < 1e-4, "{} vs {last_total}", s["totals"]["spend_usd"]);
}

/// `policy.request` as `midna hook claude` sends it for a PreToolUse payload.
fn request_bg(d: &TestDaemon, sid: &str, row: &Value) -> std::thread::JoinHandle<Value> {
    let value = format!("Bash({})", row["payload"]["tool_input"]["command"].as_str().unwrap());
    let mut a = d.agent(Some(sid));
    std::thread::spawn(move || call(&mut a, "policy.request", json!({ "action": { "kind": "tool", "value": value } })))
}

#[test]
fn claude_ask_rule_replay() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let mut a = d.agent(Some(&sid));
    call(&mut h, "rule.add", json!({ "effect": "ask", "matcher": { "kind": "tool", "pattern": "Bash(echo midna-test*)" }, "scope": { "kind": "global" } }));
    let rows = fixture("claude-2.1.288-ask-rule.jsonl");
    let mut answers = vec![json!({ "kind": "deny" }), json!({ "kind": "approve", "scope": { "kind": "always" } })].into_iter();
    let mut decisions = vec![];
    for r in &rows {
        hook(&mut a, r);
        if r["event"] != "PreToolUse" {
            continue;
        }
        let t = request_bg(&d, &sid, r);
        let resolution = answers.next();
        if let Some(res) = resolution {
            let item = wait_for(5, "approval item", || items(&mut h).into_iter().find(|n| n["kind"] == "approval"));
            // The hook blocks while Claude's title keeps spinning: midna shows needs_you.
            assert_eq!(state(&mut h, &sid), "needs_you");
            call(&mut h, "needs_you.resolve", json!({ "id": item["id"], "resolution": res }));
        }
        let out = t.join().unwrap();
        decisions.push((out["decision"].as_str().unwrap().to_string(), out["source"].as_str().unwrap().to_string()));
        assert_eq!(state(&mut h, &sid), "working");
    }
    // deny -> Claude got a deny; always -> allowed and a rule; the third call matches that rule.
    let want = [("deny", "human"), ("allow", "human"), ("allow", "rule")];
    assert_eq!(decisions, want.map(|(a, b)| (a.to_string(), b.to_string())));
    let rules = call(&mut h, "rule.list", json!({}));
    let added = rules.as_array().unwrap().iter().find(|r| r["effect"] == "allow").expect("always rule");
    assert_eq!(added["matcher"]["pattern"], "Bash(echo midna-test three)");
    assert_eq!(added["scope"]["kind"], "project");
    assert_eq!(state(&mut h, &sid), "idle", "SessionEnd");
    let s = call(&mut h, "insights.summary", json!({ "range": "today" }));
    assert_eq!(s["totals"]["turns"].as_u64(), Some(3));
    assert_eq!(s["totals"]["messages"].as_u64(), Some(3));
    assert_eq!(s["totals"]["approvals"].as_u64(), Some(1));
}

#[test]
fn codex_notify_replay_skips_title_turn() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let mut a = d.agent(Some(&sid));
    let rows = fixture("codex-0.160.0-notify.jsonl");
    // First notify is Codex's own task-title generation (separate thread), then the real turn.
    assert!(rows[0]["payload"]["last-assistant-message"].as_str().unwrap().starts_with("{\"title\""));
    hook(&mut a, &rows[0]);
    assert_eq!(state(&mut h, &sid), "idle");
    hook(&mut a, &rows[1]);
    assert_eq!(state(&mut h, &sid), "done");
    let s = call(&mut h, "insights.summary", json!({ "range": "today" }));
    assert_eq!((s["totals"]["turns"].as_u64(), s["totals"]["messages"].as_u64()), (Some(1), Some(1)));
    let ev = call(&mut h, "events.list", json!({ "filter": { "kinds": ["agent.prompt_submitted"] } }));
    assert!(ev[0]["data"]["prompt"].as_str().unwrap().contains("echo midna-codex-two"));
}

/// Claude Code turns on the kitty keyboard protocol; from then on only `CSI 13 u` submits
/// (a bare CR inserts a newline). `session.input {enter}` must encode Enter for the app's mode.
#[test]
fn input_enter_follows_kitty_keyboard_mode() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let open = |h: &mut Client, kitty: bool| {
        let push = if kitty { r"printf '\033[>1u';" } else { "" };
        let script = format!("{push} stty raw -echo; dd bs=1 count=6 2>/dev/null | od -An -c; sleep 5");
        let s = call(h, "session.open", json!({ "kind": "shell", "cwd": "/tmp", "command": ["/bin/sh", "-c", script] }));
        s["id"].as_str().unwrap().to_string()
    };
    let kitty = open(&mut h, true);
    let legacy = open(&mut h, false);
    std::thread::sleep(std::time::Duration::from_millis(500));
    call(&mut h, "session.input", json!({ "id": kitty, "text": "a", "enter": true }));
    call(&mut h, "session.input", json!({ "id": legacy, "text": "abcde", "enter": true }));
    wait_for(5, "kitty enter", || read(&mut h, &kitty).contains("033   [   1   3   u").then_some(()));
    wait_for(5, "legacy enter", || read(&mut h, &legacy).contains("e  \\r").then_some(()));
}
