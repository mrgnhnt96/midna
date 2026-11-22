//! While the Mac is overloaded (`guard.hold_agents_at`), agent terminals others open wait their
//! turn and start as it calms; the human's own start anyway. The load is faked with
//! MIDNA_DEBUG_LOAD and MIDNA_DEBUG_CPU, process-wide, so this binary has one test.
mod common;
use common::*;
use midna_proto::Client;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// An agent that leaves a file named after its terminal once it really starts.
fn fake_agent(marks: &Path) -> String {
    std::fs::create_dir_all(marks).unwrap();
    let bin = marks.join("agent.sh");
    std::fs::write(&bin, format!("#!/bin/sh\ntouch '{}'/\"$MIDNA_SESSION\"\nexec cat\n", marks.display())).unwrap();
    std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    bin.to_string_lossy().into_owned()
}

fn open_agent(c: &mut Client) -> Value {
    call(c, "session.open", json!({ "kind": "agent", "agent": "claude", "cwd": "/tmp" }))
}

fn status(h: &mut Client, sid: &str) -> Value {
    call(h, "session.get", json!({ "id": sid }))["custom_status"].clone()
}

fn overloaded(on: bool) {
    unsafe {
        std::env::set_var("MIDNA_DEBUG_LOAD", if on { "10000" } else { "0" });
        std::env::set_var("MIDNA_DEBUG_CPU", if on { "100:100" } else { "10:5" });
    }
}

#[test]
fn agents_others_open_wait_for_the_mac_to_calm() {
    unsafe { std::env::set_var("MIDNA_DEBUG_MEMORY", "normal") };
    overloaded(true);
    let marks: PathBuf = std::env::temp_dir().join(format!("midna-admit-test-{}", std::process::id()));
    let bin = fake_agent(&marks);
    let d = TestDaemon::start_with(move |c| c.agent_bin = Some(bin));
    let started = |sid: &str| marks.join(sid).exists();
    let mut h = d.human();
    let mut a = d.agent(None);
    call(&mut h, "settings.set", json!({ "key": "guard.hold_agents_at", "value": 300 }));

    // An agent's open succeeds at once, but its agent doesn't start.
    let first = open_agent(&mut a);
    let s1 = first["id"].as_str().unwrap().to_string();
    assert_eq!(first["custom_status"]["label"], "Waiting for the Mac", "{first}");
    let screen = wait_for(5, "the waiting message", || Some(read(&mut h, &s1)).filter(|s| s.contains("Ctrl-C")));
    assert!(screen.contains("overloaded"), "{screen}");
    // Its queued messages wait too: nothing is typed into the wrapper.
    call(&mut h, "queue.add", json!({ "session": s1, "text": "hello", "enter": true }));

    // The human's own start anyway, with a warning.
    let mine = open_agent(&mut h);
    let mine = mine["id"].as_str().unwrap().to_string();
    wait_for(5, "the human's agent", || started(&mine).then_some(()));
    assert!(status(&mut h, &mine).is_null());
    let screen = read(&mut h, &mine);
    assert!(screen.contains("midna: the Mac is overloaded"), "{screen}");

    // Another joins the line; one more is started by Ctrl-C in its terminal.
    let s2 = open_agent(&mut a)["id"].as_str().unwrap().to_string();
    let s3 = open_agent(&mut a)["id"].as_str().unwrap().to_string();
    std::thread::sleep(std::time::Duration::from_millis(500));
    call(&mut h, "session.input", json!({ "id": s3, "text": "\u{3}" }));
    wait_for(5, "Ctrl-C starts it", || started(&s3).then_some(()));
    wait_for(5, "its status cleared", || status(&mut h, &s3).is_null().then_some(()));
    std::thread::sleep(std::time::Duration::from_secs(3));
    assert!(!started(&s1) && !started(&s2), "still held while overloaded");
    let q = call(&mut h, "queue.list", json!({ "session": s1 }));
    assert_eq!(q["items"].as_array().map(|a| a.len()), Some(1), "{q}");

    // Calm: the first in line starts, then the next one a gap later.
    overloaded(false);
    wait_for(8, "the first in line starts", || started(&s1).then_some(()));
    assert!(status(&mut h, &s1).is_null());
    assert!(!started(&s2), "one at a time");
    assert_eq!(status(&mut h, &s2)["label"], "Waiting for the Mac");
    wait_for(20, "the next one starts", || started(&s2).then_some(()));
    let _ = std::fs::remove_dir_all(&marks);
}
