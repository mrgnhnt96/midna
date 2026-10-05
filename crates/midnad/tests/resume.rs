//! Resume after sleep (`agents.resume_after_sleep`): an agent that was mid-turn when the Mac
//! slept and then stops on an error gets `continue` typed in. The agent is `cat` standing in for
//! `claude`, so what is typed shows on screen; the wake is simulated with `resume::on_wake`.
mod common;
use common::*;
use midna_proto::Client;
use serde_json::{Value, json};
use std::path::PathBuf;

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
    let bin = fake_agent("on");
    let b = bin.to_string_lossy().into_owned();
    let d = TestDaemon::start_with(move |c| c.agent_bin = Some(b));
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
    let bin = fake_agent("off");
    let b = bin.to_string_lossy().into_owned();
    let d = TestDaemon::start_with(move |c| c.agent_bin = Some(b));
    let mut h = d.human();
    call(&mut h, "settings.set", json!({ "key": "agents.resume_after_sleep", "value": false }));
    let sid = sleep_through_a_turn(&d, &mut h);
    std::thread::sleep(std::time::Duration::from_secs(3));
    assert!(queued(&mut h, &sid).is_empty());
    let _ = std::fs::remove_file(bin);
}
