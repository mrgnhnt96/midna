//! The load guard's hold on agents' tool calls: `guard.busy_gate` while the Mac is busy, and
//! `guard.max_subagents`. Every daemon in this binary sees a busy Mac (MIDNA_DEBUG_LOAD).
mod common;
use common::*;
use midna_proto::Client;
use serde_json::{Value, json};

fn busy_daemon() -> TestDaemon {
    unsafe { std::env::set_var("MIDNA_DEBUG_LOAD", "1000") };
    TestDaemon::start()
}

/// `policy.request` for a tool call, as `midna hook claude` sends it.
fn request(a: &mut Client, value: &str) -> Value {
    call(a, "policy.request", json!({ "action": { "kind": "tool", "value": value } }))
}

fn hook(a: &mut Client, event: &str, payload: Value) {
    call(a, "agent.hook", json!({ "agent": "claude", "event": event, "payload": payload }));
}

/// The PreToolUse of an Agent call, then midna's decision on it.
fn launch(a: &mut Client, n: u32) -> Value {
    hook(a, "PreToolUse", json!({ "tool_name": "Agent", "tool_use_id": format!("tu{n}"), "tool_input": { "subagent_type": "general-purpose", "description": format!("helper {n}") } }));
    request(a, "Agent")
}

#[test]
fn a_busy_mac_holds_heavy_work_back() {
    let d = busy_daemon();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let mut a = d.agent(Some(&sid));
    let r = request(&mut a, "Bash(source ./env.sh && cargo test --workspace)");
    assert_eq!((r["decision"].as_str(), r["source"].as_str()), (Some("deny"), Some("guard")), "{r}");
    assert!(r["reason"].as_str().unwrap().contains("the Mac is busy"), "{r}");
    let r = launch(&mut a, 1);
    assert_eq!(r["source"], "guard", "{r}");
    // Light commands go through: midna has no opinion on them.
    let r = request(&mut a, "Bash(git status)");
    assert_eq!(r["source"], "default", "{r}");
    // A rule's allow is about permission, not load.
    call(&mut h, "rule.add", json!({ "effect": "allow", "matcher": { "kind": "tool", "pattern": "Bash(cargo *)" }, "scope": { "kind": "global" } }));
    assert_eq!(request(&mut a, "Bash(cargo build)")["source"], "guard");
    // Off, heavy work goes ahead; agents can't turn it off themselves.
    assert!(call_err(&mut a, "settings.set", json!({ "key": "guard.busy_gate", "value": false })).code != 0);
    call(&mut h, "settings.set", json!({ "key": "guard.busy_gate", "value": false }));
    assert_eq!(request(&mut a, "Bash(cargo build)")["source"], "rule");
    let kinds: Vec<String> = call(&mut h, "events.list", json!({ "since_seq": 0, "limit": 1000 })).as_array().unwrap().iter().map(|e| e["kind"].as_str().unwrap().to_string()).collect();
    assert_eq!(kinds.iter().filter(|k| *k == "guard.held").count(), 3, "{kinds:?}");
}

#[test]
fn an_agent_runs_at_most_max_subagents() {
    let d = busy_daemon();
    let mut h = d.human();
    call(&mut h, "settings.set", json!({ "key": "guard.busy_gate", "value": false }));
    call(&mut h, "settings.set", json!({ "key": "guard.max_subagents", "value": 2 }));
    let sid = open_sh(&mut h);
    let mut a = d.agent(Some(&sid));
    for n in 0..2 {
        assert_eq!(launch(&mut a, n)["source"], "default");
        hook(&mut a, "SubagentStart", json!({ "agent_id": format!("a{n}"), "agent_type": "general-purpose" }));
    }
    // Twice over the limit: a denied call never starts, so it doesn't count as about to.
    for n in 2..4 {
        let r = launch(&mut a, n);
        assert_eq!(r["source"], "guard", "{r}");
        assert!(r["reason"].as_str().unwrap().contains("already runs 2 subagents"), "{r}");
    }
    hook(&mut a, "SubagentStop", json!({ "agent_id": "a0" }));
    assert_eq!(launch(&mut a, 4)["source"], "default");
    call(&mut h, "settings.set", json!({ "key": "guard.max_subagents", "value": 0 }));
    hook(&mut a, "SubagentStart", json!({ "agent_id": "a4", "agent_type": "general-purpose" }));
    assert_eq!(launch(&mut a, 5)["source"], "default", "0 = no limit");
}
