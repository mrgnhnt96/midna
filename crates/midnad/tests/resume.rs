//! Resume after sleep (`agents.resume_after_sleep`): an agent that was mid-turn when the Mac
//! slept and then stops on an error gets `continue` typed in. Resume after network loss
//! (`agents.resume_after_network`): an agent whose turn dies on a connection error gets it once
//! the network is back. Resume after a usage limit (`agents.resume_after_limit`): an agent whose
//! turn dies on Claude's usage limit gets it once that window resets. The agent is `cat` standing
//! in for `claude`, so what is typed shows on screen; the wake is simulated with
//! `resume::on_wake`, the network with `resume::set_probe`, and the limit's timing shortened with
//! `resume::set_limit_timing`.
mod common;
use common::*;
use midna_proto::{Client, time};
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
fn a_failure_stays_pending_when_idle_prompt_marks_it_done_before_the_network_is_back() {
    // On wake Claude's delayed idle_prompt turns `failed` into `done` before Wi-Fi reconnects.
    let (d, bin, online) = daemon("idle");
    let mut h = d.human();
    online.store(false, Ordering::Relaxed);
    let sid = sleep_through_a_turn(&d, &mut h);
    std::thread::sleep(std::time::Duration::from_secs(2));
    hook(&mut d.agent(Some(&sid)), "Notification", json!({ "notification_type": "idle_prompt", "message": "Claude is waiting for your input" }));
    assert_eq!(call(&mut h, "session.get", json!({ "id": sid }))["status"]["state"], "done");
    std::thread::sleep(std::time::Duration::from_secs(3));
    assert!(queued(&mut h, &sid).is_empty(), "nothing goes in while offline");
    online.store(true, Ordering::Relaxed);
    let item = wait_for(10, "continue queued", || queued(&mut h, &sid).first().cloned());
    assert_eq!(item["text"].as_str(), Some("continue"), "{item}");
    wait_for(15, "continue typed", || Some(read(&mut h, &sid)).filter(|t| t.contains("continue")));
    let _ = std::fs::remove_file(bin);
}

#[test]
fn a_turn_finished_by_hand_after_a_failure_is_left_alone() {
    let (d, bin, online) = daemon("by-hand");
    let mut h = d.human();
    online.store(false, Ordering::Relaxed);
    let sid = sleep_through_a_turn(&d, &mut h);
    std::thread::sleep(std::time::Duration::from_secs(2));
    let mut a = d.agent(Some(&sid));
    hook(&mut a, "UserPromptSubmit", json!({ "prompt": "continue" }));
    hook(&mut a, "Stop", json!({}));
    online.store(true, Ordering::Relaxed);
    std::thread::sleep(std::time::Duration::from_secs(7));
    assert!(queued(&mut h, &sid).is_empty(), "the human already continued it");
    assert_eq!(midnad::resume::watching(&d.daemon()), 0);
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
fn a_turn_whose_connection_error_is_only_in_its_message_is_continued() {
    // What Claude sent when DNS failed on a network change: no details, the error in the message.
    let (d, bin, online) = daemon("net-msg");
    let mut h = d.human();
    online.store(false, Ordering::Relaxed);
    let sid = call(&mut h, "session.open", json!({ "kind": "agent", "agent": "claude", "cwd": "/tmp" }))["id"].as_str().unwrap().to_string();
    let mut a = d.agent(Some(&sid));
    hook(&mut a, "UserPromptSubmit", json!({ "prompt": "long task" }));
    let message = "API Error: Can't reach the API server — check your internet or DNS (ENOTFOUND)";
    hook(&mut a, "StopFailure", json!({ "error": "server_error", "error_details": "", "last_assistant_message": message }));
    assert_eq!(call(&mut h, "session.get", json!({ "id": sid }))["status"]["state"], "failed");
    online.store(true, Ordering::Relaxed);
    let item = wait_for(10, "continue queued", || queued(&mut h, &sid).first().cloned());
    assert_eq!((item["text"].as_str(), item["by"]["name"].as_str()), (Some("continue"), Some("resume after network loss")), "{item}");
    let _ = std::fs::remove_file(bin);
}

#[test]
fn a_failure_whose_message_only_mentions_the_network_is_left_alone() {
    // The last message is the agent's own prose, not an API error: no resume.
    let (d, bin, _) = daemon("net-prose");
    let mut h = d.human();
    let sid = call(&mut h, "session.open", json!({ "kind": "agent", "agent": "claude", "cwd": "/tmp" }))["id"].as_str().unwrap().to_string();
    let mut a = d.agent(Some(&sid));
    hook(&mut a, "UserPromptSubmit", json!({ "prompt": "long task" }));
    hook(&mut a, "StopFailure", json!({ "error": "server_error", "error_details": "", "last_assistant_message": "I fixed the socket connection timeout." }));
    std::thread::sleep(std::time::Duration::from_secs(3));
    assert!(queued(&mut h, &sid).is_empty());
    let _ = std::fs::remove_file(bin);
}

#[test]
fn other_failures_and_the_setting_off_are_left_alone() {
    let (d, bin, _) = daemon("net-off");
    let mut h = d.human();
    let limited = fail_a_turn(&d, &mut h, "authentication_failed", "Invalid API key");
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
    assert!(is_network_error("", "API Error: Can't reach the API server — check your internet or DNS (ENOTFOUND)"));
    assert!(is_network_error("unknown", "API Error: Your computer went to sleep mid-response. The response above may be incomplete."));
    assert!(!is_network_error("rate_limit", "Rate limit reached."));
    assert!(!is_network_error("authentication_failed", "Invalid API key"));
    assert!(!is_network_error("", ""));
}

/// A status line from `sid` reporting the 5-hour window at `five`% resetting in `resets_in` secs.
fn statusline(d: &TestDaemon, sid: &str, five: f64, resets_in: i64) {
    let now = time::now_unix();
    let payload = json!({ "session_id": "c1", "model": { "id": "claude-opus-5-5" }, "cost": { "total_cost_usd": 0.1 },
        "rate_limits": { "five_hour": { "used_percentage": five, "resets_at": now + resets_in }, "seven_day": { "used_percentage": 40, "resets_at": now + 86_400 } } });
    hook(&mut d.agent(Some(sid)), "statusline", payload);
}

fn open_agent(h: &mut Client) -> String {
    call(h, "session.open", json!({ "kind": "agent", "agent": "claude", "cwd": "/tmp" }))["id"].as_str().unwrap().to_string()
}

/// An agent whose turn dies on the usage limit.
fn hit_the_limit(d: &TestDaemon, h: &mut Client, sid: &str, details: &str) {
    let mut a = d.agent(Some(sid));
    hook(&mut a, "UserPromptSubmit", json!({ "prompt": "long task" }));
    hook(&mut a, "StopFailure", json!({ "error": "rate_limit", "error_details": details, "last_assistant_message": "You've hit your limit · resets 3pm" }));
    assert_eq!(call(h, "session.get", json!({ "id": sid }))["status"]["state"], "failed");
}

fn queued_at(item: &Value) -> i64 {
    time::parse_rfc3339(item["when"]["at"].as_str().unwrap()).unwrap()
}

#[test]
fn a_turn_stopped_by_a_usage_limit_is_continued_once_it_resets() {
    let (d, bin, _) = daemon("limit");
    midnad::resume::set_limit_timing(&d.daemon(), 0, 2);
    let mut h = d.human();
    let sid = open_agent(&mut h);
    statusline(&d, &sid, 100.0, 5);
    hit_the_limit(&d, &mut h, &sid, "");
    std::thread::sleep(std::time::Duration::from_secs(2));
    assert!(queued(&mut h, &sid).is_empty(), "nothing goes in before the window resets");
    assert_eq!(midnad::resume::watching(&d.daemon()), 1);
    let now = time::now_unix();
    assert_eq!(midnad::resume::watching_before(&d.daemon(), now + 1), 0, "keep-awake needn't hold the Mac for a reset not due yet");
    assert_eq!(midnad::resume::watching_before(&d.daemon(), now + 60), 1);
    let item = wait_for(10, "continue queued", || queued(&mut h, &sid).first().cloned());
    assert_eq!((item["text"].as_str(), item["by"]["name"].as_str()), (Some("continue"), Some("resume after usage limit")), "{item}");
    assert!(queued_at(&item) >= now + 3, "queued once the window reset: {item}");
    wait_for(15, "continue typed", || Some(read(&mut h, &sid)).filter(|t| t.contains("continue")));
    let _ = std::fs::remove_file(bin);
}

#[test]
fn the_reset_in_the_limit_error_is_used_when_no_status_line_reported_it() {
    let (d, bin, _) = daemon("limit-msg");
    midnad::resume::set_limit_timing(&d.daemon(), 0, 2);
    let mut h = d.human();
    let sid = open_agent(&mut h);
    let reset = time::now_unix() + 4;
    hit_the_limit(&d, &mut h, &sid, &format!("Claude AI usage limit reached|{reset}"));
    std::thread::sleep(std::time::Duration::from_secs(2));
    assert!(queued(&mut h, &sid).is_empty(), "nothing goes in before the window resets");
    let item = wait_for(10, "continue queued", || queued(&mut h, &sid).first().cloned());
    assert_eq!(item["by"]["name"].as_str(), Some("resume after usage limit"), "{item}");
    assert!(queued_at(&item) >= reset, "{item}");
    let _ = std::fs::remove_file(bin);
}

#[test]
fn agents_one_limit_stopped_are_resumed_apart() {
    let (d, bin, _) = daemon("limit-many");
    midnad::resume::set_limit_timing(&d.daemon(), 0, 4);
    let mut h = d.human();
    let (one, two) = (open_agent(&mut h), open_agent(&mut h));
    statusline(&d, &one, 100.0, 3);
    hit_the_limit(&d, &mut h, &one, "");
    hit_the_limit(&d, &mut h, &two, "");
    let a = wait_for(10, "first queued", || queued(&mut h, &one).first().cloned());
    let b = wait_for(10, "second queued", || queued(&mut h, &two).first().cloned());
    assert!((queued_at(&a) - queued_at(&b)).abs() >= 4, "{a} {b}");
    let _ = std::fs::remove_file(bin);
}

#[test]
fn a_limited_agent_continued_by_hand_is_left_alone() {
    let (d, bin, _) = daemon("limit-hand");
    midnad::resume::set_limit_timing(&d.daemon(), 0, 2);
    let mut h = d.human();
    let sid = open_agent(&mut h);
    statusline(&d, &sid, 100.0, 4);
    hit_the_limit(&d, &mut h, &sid, "");
    let mut a = d.agent(Some(&sid));
    hook(&mut a, "UserPromptSubmit", json!({ "prompt": "carry on" }));
    hook(&mut a, "Stop", json!({}));
    std::thread::sleep(std::time::Duration::from_secs(8));
    assert!(queued(&mut h, &sid).is_empty(), "the human already continued it");
    assert_eq!(midnad::resume::watching(&d.daemon()), 0);
    let _ = std::fs::remove_file(bin);
}

#[test]
fn a_limit_with_no_reset_known_waits_and_the_setting_off_drops_it() {
    let (d, bin, _) = daemon("limit-unknown");
    let mut h = d.human();
    let sid = open_agent(&mut h);
    hit_the_limit(&d, &mut h, &sid, "Rate limit reached.");
    std::thread::sleep(std::time::Duration::from_secs(3));
    assert!(queued(&mut h, &sid).is_empty(), "no reset known: the first retry is an hour out");
    assert_eq!(midnad::resume::watching(&d.daemon()), 1);
    call(&mut h, "settings.set", json!({ "key": "agents.resume_after_limit", "value": false }));
    wait_for(5, "watch dropped", || (midnad::resume::watching(&d.daemon()) == 0).then_some(()));
    let off = open_agent(&mut h);
    hit_the_limit(&d, &mut h, &off, "");
    std::thread::sleep(std::time::Duration::from_secs(2));
    assert_eq!(midnad::resume::watching(&d.daemon()), 0, "the setting is off");
    let _ = std::fs::remove_file(bin);
}

#[test]
fn usage_limit_errors_are_told_apart() {
    use midnad::resume::is_limit_error;
    assert!(is_limit_error("rate_limit", ""));
    assert!(is_limit_error("unknown", "Claude AI usage limit reached|1748886000"));
    assert!(is_limit_error("", "You've hit your limit · resets 3pm (Europe/London)"));
    assert!(is_limit_error("", "5-hour limit reached ∙ resets 3pm"));
    assert!(!is_limit_error("unknown", "Connection error."));
    assert!(!is_limit_error("authentication_failed", "Invalid API key"));
    assert!(!is_limit_error("", ""));
}
