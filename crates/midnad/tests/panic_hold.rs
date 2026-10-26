//! After a kernel panic (#106): a report written since this boot raises a needs-you saying what
//! each terminal ran last, and agents don't start on their own until the human closes it.
mod common;
use common::*;
use midna_proto::error::REFUSED;
use serde_json::{Value, json};

fn panic_items(h: &mut midna_proto::Client) -> Vec<Value> {
    let list = call(h, "needs_you.list", json!({}));
    list.as_array().unwrap().iter().filter(|n| n["title"] == "The Mac restarted after a kernel panic").cloned().collect()
}

#[test]
fn after_a_panic_agents_wait_for_the_human() {
    let mut d = TestDaemon::start_with(|c| c.agent_bin = Some("/bin/cat".into()));
    let mut h = d.human();
    let open_agent = |c: &mut midna_proto::Client| c.call_value("session.open", json!({ "kind": "agent", "agent": "claude", "cwd": "/tmp", "name": "trailer" }));
    let sid = open_agent(&mut h).unwrap()["id"].as_str().unwrap().to_string();
    call(&mut d.agent(Some(&sid)), "policy.request", json!({ "action": { "kind": "tool", "value": "Bash(python3 make_trailer.py --tts)" } }));
    // No report: nothing happens.
    assert!(panic_items(&mut h).is_empty());

    // The Mac panics; macOS writes the report at the next boot.
    let reports = d.home.join("diagnostic-reports");
    std::fs::create_dir_all(&reports).unwrap();
    std::fs::write(
        reports.join("panic-full-2026-10-23-102800.0002.panic"),
        "{\"bug_type\":\"210\",\"timestamp\":\"2026-10-23 10:28:00.00 -0600\"}\n{\"panicString\":\"panic(cpu 0 caller 0xfe): userspace watchdog timeout: no successful checkins from WindowServer\\nmore\"}",
    )
    .unwrap();
    d.restart();
    let mut h = d.human();
    let item = wait_for(10, "panic needs-you", || panic_items(&mut h).pop());
    let detail = item["detail"].as_str().unwrap();
    assert!(detail.contains("no successful checkins from WindowServer"), "{detail}");
    assert!(detail.contains("trailer") && detail.contains("python3 make_trailer.py --tts"), "{detail}");

    // Agents, triggers and apps can't start or restart an agent; the human can.
    let mut a = d.agent(None);
    let e = call_err(&mut a, "session.open", json!({ "kind": "agent", "agent": "claude", "cwd": "/tmp" }));
    assert_eq!(e.code, REFUSED, "{e:?}");
    assert!(e.message.contains("kernel panic"), "{e:?}");
    // Refused at once, not after an approval nobody answers.
    let t = std::time::Instant::now();
    let e = call_err(&mut a, "session.restart", json!({ "id": sid }));
    assert!(e.code == REFUSED && e.message.contains("kernel panic"), "{e:?}");
    assert!(t.elapsed() < std::time::Duration::from_secs(5));
    assert!(call(&mut h, "needs_you.list", json!({})).as_array().unwrap().iter().all(|n| n["kind"] != "approval"));
    assert!(open_sh(&mut a).len() > 0, "shells aren't held");
    open_agent(&mut h).unwrap();

    // Handled once: another restart doesn't raise it again, and the hold stays.
    d.restart();
    let mut h = d.human();
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(panic_items(&mut h).len(), 1);
    let mut a = d.agent(None);
    assert!(open_agent(&mut a).is_err());
    // The human dismisses it: agents start again.
    call(&mut h, "needs_you.resolve", json!({ "id": item["id"], "resolution": { "kind": "dismiss" } }));
    open_agent(&mut a).unwrap();
}
