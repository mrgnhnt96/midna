//! Resume after sleep (`agents.resume_after_sleep`): an agent that was mid-turn when the Mac
//! slept and then stops on an error gets `continue` typed in. Resume after network loss
//! (`agents.resume_after_network`): an agent whose turn dies on a connection error gets it once
//! the network is back. The agent is `cat` standing in for `claude`, so what is typed shows on
//! screen; the wake is simulated with `resume::on_wake` and the network with `resume::set_probe`.
mod common;
use common::*;
use midna_proto::Client;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

fn fake_agent(tag: &str) -> PathBuf {
    let bin = PathBuf::from(format!("/tmp/midna-resume-{}-{tag}", std::process::id()));
    std::fs::write(&bin, "#!/bin/sh\nexec cat\n").unwrap();
    std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    bin
}

fn hook(a: &mut Client, event: &str, payload: Value) {
    call(a, "agent.hook", json!({ "agent": "claude", "event": event, "payload": payload }));
}

fn queued(h: &mut Client, sid: &str) -> Vec<Value> {
    call(h, "queue.list", json!({ "session": sid }))["items"].as_array().cloned().unwrap_or_default()
}

/// A daemon whose network check answers from the returned flag (starts online).
fn daemon(tag: &str) -> (TestDaemon, PathBuf, Arc<AtomicBool>) {
    let bin = fake_agent(tag);
    let b = bin.to_string_lossy().into_owned();
    let d = TestDaemon::start_with(move |c| c.agent_bin = Some(b));
    let online = Arc::new(AtomicBool::new(true));
    let o = online.clone();
    midnad::resume::set_probe(&d.daemon(), move || o.load(Ordering::Relaxed));
    (d, bin, online)
}

/// An agent mid-turn whose turn then dies on `error_details`, with no sleep involved.
fn fail_a_turn(d: &TestDaemon, h: &mut Client, error: &str, details: &str) -> String {
    let sid = call(h, "session.open", json!({ "kind": "agent", "agent": "claude", "cwd": "/tmp" }))["id"].as_str().unwrap().to_string();
    let mut a = d.agent(Some(&sid));
    hook(&mut a, "UserPromptSubmit", json!({ "prompt": "long task" }));
    hook(&mut a, "StopFailure", json!({ "error": error, "error_details": details }));
    assert_eq!(call(h, "session.get", json!({ "id": sid }))["status"]["state"], "failed");
    sid
}

/// An agent mid-turn, then a wake, then its turn dies on a connection error.
fn sleep_through_a_turn(d: &TestDaemon, h: &mut Client) -> String {
    let sid = call(h, "session.open", json!({ "kind": "agent", "agent": "claude", "cwd": "/tmp" }))["id"].as_str().unwrap().to_string();
    let mut a = d.agent(Some(&sid));
    hook(&mut a, "UserPromptSubmit", json!({ "prompt": "long task" }));
    midnad::resume::on_wake(&d.daemon(), midna_proto::time::now_unix() - 600);
    hook(&mut a, "StopFailure", json!({ "error": "unknown", "error_details": "Connection error." }));
    assert_eq!(call(h, "session.get", json!({ "id": sid }))["status"]["state"], "failed");
    sid
}

#[test]
fn a_turn_that_died_in_sleep_is_continued() {
    let (d, bin, _) = daemon("on");
    let mut h = d.human();
    let sid = sleep_through_a_turn(&d, &mut h);
    let item = wait_for(5, "continue queued", || queued(&mut h, &sid).first().cloned());
    assert_eq!((item["text"].as_str(), item["by"]["name"].as_str()), (Some("continue"), Some("resume after sleep")), "{item}");
    wait_for(15, "continue typed", || Some(read(&mut h, &sid)).filter(|t| t.contains("continue")));
    wait_for(5, "queue empty", || queued(&mut h, &sid).is_empty().then_some(()));
    let _ = std::fs::remove_file(bin);
}

#[test]
fn nothing_is_typed_when_the_setting_is_off() {
    let (d, bin, _) = daemon("off");
    let mut h = d.human();
    call(&mut h, "settings.set", json!({ "key": "agents.resume_after_sleep", "value": false }));
    call(&mut h, "settings.set", json!({ "key": "agents.resume_after_network", "value": false }));
    let sid = sleep_through_a_turn(&d, &mut h);
    std::thread::sleep(std::time::Duration::from_secs(3));
    assert!(queued(&mut h, &sid).is_empty());
    let _ = std::fs::remove_file(bin);
}

#[test]
fn a_turn_lost_to_the_network_is_continued_once_it_is_back() {
    let (d, bin, online) = daemon("net");
    let mut h = d.human();
    online.store(false, Ordering::Relaxed);
    let sid = fail_a_turn(&d, &mut h, "unknown", "Connection error.");
    std::thread::sleep(std::time::Duration::from_secs(7));
    assert!(queued(&mut h, &sid).is_empty(), "nothing goes in while offline");
    online.store(true, Ordering::Relaxed);
    let item = wait_for(10, "continue queued", || queued(&mut h, &sid).first().cloned());
    assert_eq!((item["text"].as_str(), item["by"]["name"].as_str()), (Some("continue"), Some("resume after network loss")), "{item}");
    wait_for(15, "continue typed", || Some(read(&mut h, &sid)).filter(|t| t.contains("continue")));
    let _ = std::fs::remove_file(bin);
}

#[test]
fn other_failures_and_the_setting_off_are_left_alone() {
    let (d, bin, _) = daemon("net-off");
    let mut h = d.human();
    let limited = fail_a_turn(&d, &mut h, "rate_limit", "Rate limit reached.");
    call(&mut h, "settings.set", json!({ "key": "agents.resume_after_network", "value": false }));
    let off = fail_a_turn(&d, &mut h, "unknown", "Connection error.");
    std::thread::sleep(std::time::Duration::from_secs(3));
    assert!(queued(&mut h, &limited).is_empty());
    assert!(queued(&mut h, &off).is_empty());
    let _ = std::fs::remove_file(bin);
}

#[test]
fn connection_errors_are_told_apart() {
    use midnad::resume::is_network_error;
    assert!(is_network_error("unknown", "Connection error."));
    assert!(is_network_error("unknown", "fetch failed: getaddrinfo ENOTFOUND api.anthropic.com"));
    assert!(is_network_error("server_error", "Request timed out."));
    assert!(!is_network_error("rate_limit", "Rate limit reached."));
    assert!(!is_network_error("authentication_failed", "Invalid API key"));
    assert!(!is_network_error("", ""));
}
