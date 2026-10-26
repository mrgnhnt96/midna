//! The load guard on memory (#106): macOS's memory pressure at `system.busy_memory` holds new
//! heavy work back and raises a needs-you, and local model runs wait for a model slot. Every
//! daemon in this binary sees warning pressure (MIDNA_DEBUG_MEMORY) and a calm CPU.
mod common;
use common::*;
use midna_proto::Client;
use serde_json::{Value, json};

fn short_of_memory() -> TestDaemon {
    unsafe {
        std::env::set_var("MIDNA_DEBUG_MEMORY", "warning");
        std::env::set_var("MIDNA_DEBUG_CPU", "10:5");
    }
    TestDaemon::start()
}

/// `policy.request` for a tool call, as `midna hook claude` sends it.
fn request(a: &mut Client, value: &str) -> Value {
    call(a, "policy.request", json!({ "action": { "kind": "tool", "value": value } }))
}

#[test]
fn memory_pressure_holds_heavy_work_back_and_says_so() {
    let d = short_of_memory();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let mut a = d.agent(Some(&sid));
    let l = call(&mut h, "system.load", json!({}));
    assert_eq!((l["memory_pressure"].as_str(), l["memory_held"].as_bool(), l["held"].as_bool()), (Some("warning"), Some(true), Some(false)), "{l}");
    assert!(l["memory_total_bytes"].as_u64().unwrap() > 0, "{l}");
    let r = request(&mut a, "Bash(cargo build)");
    assert_eq!((r["decision"].as_str(), r["source"].as_str()), (Some("deny"), Some("guard")), "{r}");
    assert!(r["reason"].as_str().unwrap().contains("short of memory"), "{r}");
    assert!(r["reason"].as_str().unwrap().contains("`midna system wait`"), "{r}");
    let r = request(&mut a, "Bash(uv run mflux-generate --prompt cat)");
    assert!(r["reason"].as_str().unwrap().contains("Don't start model runs"), "{r}");
    assert_eq!(request(&mut a, "Bash(git status)")["source"], "default");
    // The needs-you says so, once per spell.
    let item = wait_for(15, "memory needs-you", || {
        let list = call(&mut h, "needs_you.list", json!({}));
        list.as_array().unwrap().iter().find(|n| n["title"].as_str().is_some_and(|t| t.contains("short of memory"))).cloned()
    });
    assert!(item["detail"].as_str().unwrap().contains("pressure is warning"), "{item}");
    let events = call(&mut h, "events.list", json!({ "filter": { "kinds": ["system.memory_pressure"] } }));
    assert_eq!(events.as_array().unwrap().len(), 1, "{events}");
    // Only at critical: warning goes ahead.
    call(&mut h, "settings.set", json!({ "key": "system.busy_memory", "value": "critical" }));
    assert_eq!(request(&mut a, "Bash(cargo build)")["source"], "default");
}

#[test]
fn model_runs_wait_for_a_model_slot_apart_from_builds() {
    let d = short_of_memory();
    let mut h = d.human();
    call(&mut h, "settings.set", json!({ "key": "system.busy_memory", "value": "off" }));
    call(&mut h, "settings.set", json!({ "key": "guard.build_slots", "value": 1 }));
    let (s1, s2) = (open_sh(&mut h), open_sh(&mut h));
    let mut a = d.agent(Some(&s1));
    assert_eq!(request(&mut a, "Bash(mflux-generate --prompt cat)")["source"], "default");
    // Builds have their own line: a model run doesn't take their slot.
    assert_eq!(request(&mut a, "Bash(cargo build)")["source"], "default");
    let mut b = d.agent(Some(&s2));
    let waiting = std::thread::spawn(move || request(&mut b, "Bash(python3 -m mlx_audio.tts.generate --text hi)"));
    let l = wait_for(5, "a model run in line", || {
        let l = call(&mut h, "system.load", json!({}));
        (l["builds"].as_array().unwrap().iter().filter(|b| b["model"] == true).count() == 2).then_some(l)
    });
    assert_eq!(l["model_slots"], 1, "{l}");
    assert!(!waiting.is_finished());
    // The first run returned and nothing it started runs: the next one goes.
    call(&mut a, "agent.hook", json!({ "agent": "claude", "event": "PostToolUse", "payload": { "tool_name": "Bash", "tool_input": { "command": "mflux-generate --prompt cat" }, "tool_response": {} } }));
    assert_eq!(waiting.join().unwrap()["source"], "default");
}
