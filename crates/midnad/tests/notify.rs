//! Notifications: which daemon signals become `notify.posted`, global settings vs. a terminal's
//! overrides, and notify.send's dedupe and rate limit.
mod common;
use common::*;
use midna_proto::Client;
use serde_json::{Value, json};

fn hook(a: &mut Client, ev: &str, payload: Value) {
    call(a, "agent.hook", json!({ "agent": "claude", "event": ev, "payload": payload }));
}

fn posted(h: &mut Client) -> Vec<Value> {
    let v = call(h, "events.list", json!({ "filter": { "kinds": ["notify.posted"] }, "limit": 100 }));
    v.as_array().cloned().unwrap_or_default()
}

/// Wait until `n` notifications were posted (the notifier runs on its own thread).
fn wait_posted(h: &mut Client, n: usize) -> Vec<Value> {
    wait_for(5, &format!("{n} notifications"), || Some(posted(h)).filter(|p| p.len() >= n))
}

/// Give the notifier time to (not) act, then return what it posted.
fn settle(h: &mut Client) -> Vec<Value> {
    std::thread::sleep(std::time::Duration::from_millis(300));
    posted(h)
}

#[test]
fn defaults_notify_what_needs_you_and_finished_turns() {
    let d = TestDaemon::start();
    let mut h = d.human();
    call(&mut h, "settings.set", json!({ "key": "notify.turn_done_min_secs", "value": 0 }));
    let sid = open_sh(&mut h);
    let mut a = d.agent(Some(&sid));
    hook(&mut a, "UserPromptSubmit", json!({ "session_id": "c1", "prompt": "fix the tests" }));
    hook(&mut a, "Stop", json!({ "session_id": "c1", "last_assistant_message": "\nAll 12 tests pass now.\nDetails…" }));
    let p = wait_posted(&mut h, 1);
    let n = &p[0]["data"];
    assert_eq!((n["category"].as_str(), n["via"].as_str()), (Some("turn_done"), Some("none")), "{n}");
    assert_eq!(n["body"], "Finished: All 12 tests pass now.");
    assert_eq!(n["sound"], false);
    assert_eq!(p[0]["session_id"].as_str(), Some(sid.as_str()));

    // A permission prompt is important (sound) and carries its needs-you id.
    hook(&mut a, "PermissionRequest", json!({ "session_id": "c1", "tool_name": "Bash", "tool_input": { "command": "rm -rf build" } }));
    let p = wait_posted(&mut h, 2);
    let n = &p[1]["data"];
    assert_eq!(n["category"], "approval");
    assert_eq!(n["sound"], true);
    assert!(n["needs_you_id"].as_str().is_some_and(|i| i.starts_with("n_")), "{n}");

    // Off by default: a clean exit.
    call(&mut h, "session.input", json!({ "id": sid, "text": "exit 0", "enter": true }));
    wait_for(5, "exit", || (call(&mut h, "session.get", json!({ "id": sid }))["status"]["state"] == "exited").then_some(()));
    assert_eq!(settle(&mut h).len(), 2);
}

#[test]
fn short_turns_and_interrupts_stay_quiet() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let mut a = d.agent(Some(&sid));
    // Default minimum is 30 s: a quick turn doesn't notify.
    hook(&mut a, "UserPromptSubmit", json!({ "session_id": "c1", "prompt": "hi" }));
    hook(&mut a, "Stop", json!({ "session_id": "c1", "last_assistant_message": "hello" }));
    assert!(settle(&mut h).is_empty());
}

#[test]
fn terminal_overrides_beat_global_settings() {
    let d = TestDaemon::start();
    let mut h = d.human();
    call(&mut h, "settings.set", json!({ "key": "notify.turn_done_min_secs", "value": 0 }));
    let quiet = open_sh(&mut h);
    let loud = open_sh(&mut h);

    // Agents change their own terminal by default.
    let mut a = d.agent(Some(&quiet));
    let r = call(&mut a, "notify.set", json!({ "key": "enabled", "value": false }));
    assert_eq!((r["session"].as_str(), r["muted"].as_bool()), (Some(quiet.as_str()), Some(true)), "{r}");
    assert!(r["categories"].as_array().unwrap().iter().all(|c| c["effective"] == false));
    let s = call(&mut h, "session.get", json!({ "id": quiet }));
    assert_eq!(s["notify"], json!({ "enabled": false }));

    // Globally off, on for one terminal.
    call(&mut h, "notify.set", json!({ "global": true, "key": "turn_done", "value": false }));
    let r = call(&mut h, "notify.set", json!({ "session": loud, "key": "turn_done", "value": true }));
    let td = r["categories"].as_array().unwrap().iter().find(|c| c["key"] == "turn_done").unwrap().clone();
    assert_eq!((td["global"].as_bool(), td["session"].as_bool(), td["effective"].as_bool()), (Some(false), Some(true), Some(true)), "{td}");

    for sid in [&quiet, &loud] {
        let mut a = d.agent(Some(sid));
        hook(&mut a, "UserPromptSubmit", json!({ "session_id": "c", "prompt": "go" }));
        hook(&mut a, "Stop", json!({ "session_id": "c" }));
    }
    let p = settle(&mut h);
    assert_eq!(p.len(), 1, "{p:?}");
    assert_eq!(p[0]["session_id"].as_str(), Some(loud.as_str()));

    // null drops the override: back to the (off) global setting.
    call(&mut h, "notify.set", json!({ "session": loud, "key": "turn_done", "value": null }));
    assert_eq!(call(&mut h, "session.get", json!({ "id": loud }))["notify"], Value::Null);
    assert_eq!(call_err(&mut h, "notify.set", json!({ "session": loud, "key": "nope", "value": true })).code, -32602);
}

#[test]
fn agents_send_with_dedupe_and_a_rate_limit() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let mut a = d.agent(Some(&sid));
    let r = call(&mut a, "notify.send", json!({ "title": "Build is green", "body": "main @ 3f2a" }));
    assert_eq!(r, json!({ "posted": true }));
    assert_eq!(call(&mut a, "notify.send", json!({ "title": "Build is green", "body": "main @ 3f2a" }))["reason"], "duplicate");
    for i in 0..5 {
        assert_eq!(call(&mut a, "notify.send", json!({ "title": format!("step {i}") }))["posted"], true);
    }
    assert_eq!(call(&mut a, "notify.send", json!({ "title": "one too many" }))["reason"], "rate_limited");
    let p = posted(&mut h);
    assert_eq!(p.len(), 6);
    assert_eq!(p[0]["data"]["body"], "Build is green\nmain @ 3f2a");
    assert_eq!(p[0]["data"]["category"], "agent");

    call(&mut h, "settings.set", json!({ "key": "notify.agent", "value": false }));
    let mut a2 = d.agent(Some(&open_sh(&mut h)));
    assert_eq!(call(&mut a2, "notify.send", json!({ "title": "hello" }))["reason"], "category_off");
}

#[test]
fn each_kind_has_its_sound_and_volume() {
    let d = TestDaemon::start();
    let mut h = d.human();
    call(&mut h, "settings.set", json!({ "key": "notify.turn_done_min_secs", "value": 0 }));
    let sid = open_sh(&mut h);
    let mut a = d.agent(Some(&sid));
    hook(&mut a, "PermissionRequest", json!({ "session_id": "c1", "tool_name": "Bash", "tool_input": { "command": "ls" } }));
    let p = wait_posted(&mut h, 1);
    let n = &p[0]["data"];
    assert_eq!((n["sound_file"].as_str(), n["volume"].as_u64()), (Some("/System/Library/Sounds/Glass.aiff"), Some(100)), "{n}");

    // Another sound, 50% of a 50% master: plays at 25.
    call(&mut h, "settings.set", json!({ "key": "notify.sound.turn_done", "value": "Submarine" }));
    call(&mut h, "settings.set", json!({ "key": "notify.volume.turn_done", "value": 50 }));
    call(&mut h, "settings.set", json!({ "key": "notify.volume", "value": "50%" }));
    hook(&mut a, "UserPromptSubmit", json!({ "session_id": "c1", "prompt": "go" }));
    hook(&mut a, "Stop", json!({ "session_id": "c1" }));
    let p = wait_posted(&mut h, 2);
    let n = &p[1]["data"];
    assert_eq!((n["category"].as_str(), n["sound"].as_bool(), n["volume"].as_u64()), (Some("turn_done"), Some(true), Some(25)), "{n}");
    assert_eq!(n["sound_file"], "/System/Library/Sounds/Submarine.aiff");
    // What macOS plays with the banner: that sound rendered at 25%, in the daemon's home.
    let file = n["notification_sound"].as_str().unwrap_or_default();
    assert!(std::path::Path::new(file).starts_with(d.home.join("notify/cache")) && file.ends_with("-25.wav"), "{n}");
    assert!(std::fs::read(file).is_ok_and(|b| b.starts_with(b"RIFF")), "{file}");

    // Master at 0: silent everywhere.
    call(&mut h, "settings.set", json!({ "key": "notify.volume", "value": 0 }));
    hook(&mut a, "UserPromptSubmit", json!({ "session_id": "c2", "prompt": "go" }));
    std::thread::sleep(std::time::Duration::from_millis(3100));
    hook(&mut a, "Stop", json!({ "session_id": "c2" }));
    let p = wait_posted(&mut h, 3);
    assert_eq!((p[2]["data"]["sound"].as_bool(), p[2]["data"].get("sound_file"), p[2]["data"].get("notification_sound")), (Some(false), None, None), "{}", p[2]);
    assert_eq!(call_err(&mut h, "settings.set", json!({ "key": "notify.volume.failed", "value": 120 })).code, -32602);
    assert_eq!(call_err(&mut h, "settings.set", json!({ "key": "notify.sound.failed", "value": "/etc/passwd" })).code, -32602);
}

#[test]
fn imported_sounds_and_images() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let src = d.home.join("downloads");
    std::fs::create_dir_all(&src).unwrap();
    let wav = src.join("ding.wav");
    std::fs::write(&wav, b"RIFF\x24\0\0\0WAVEfmt \x10\0\0\0").unwrap();
    let png = src.join("logo.png");
    std::fs::write(&png, b"\x89PNG\r\n\x1a\n").unwrap();
    std::fs::write(src.join("fake.wav"), b"just text").unwrap();

    let m = call(&mut h, "notify.import", json!({ "path": wav, "use_for": ["notify.sound.approval", "notify.sound.failed"] }));
    assert_eq!((m["kind"].as_str(), m["name"].as_str()), (Some("sound"), Some("ding.wav")), "{m}");
    assert_eq!(m["used_by"], json!(["notify.sound.approval", "notify.sound.failed"]));
    assert!(std::path::Path::new(m["path"].as_str().unwrap()).starts_with(d.home.join("notify/sounds")));
    // The same file again is the same import; a different one with that name gets -2.
    assert_eq!(call(&mut h, "notify.import", json!({ "path": wav }))["name"], "ding.wav");
    let other = src.join("x").join("ding.wav");
    std::fs::create_dir_all(other.parent().unwrap()).unwrap();
    std::fs::write(&other, b"RIFF\x24\0\0\0WAVEfmt \x11\0\0\0").unwrap();
    assert_eq!(call(&mut h, "notify.import", json!({ "path": other }))["name"], "ding-2.wav");
    assert_eq!(call_err(&mut h, "notify.import", json!({ "path": src.join("fake.wav") })).code, -32602);
    assert_eq!(call_err(&mut h, "notify.import", json!({ "path": png, "use_for": ["notify.sound.approval"] })).code, -32602);

    call(&mut h, "notify.import", json!({ "path": png, "use_for": ["notify.image"] }));
    call(&mut h, "settings.set", json!({ "key": "notify.image.failed", "value": "none" }));
    let media = call(&mut h, "notify.media", json!({}));
    assert_eq!(media["sounds"].as_array().unwrap().iter().filter(|s| s["builtin"] == false).count(), 2);
    assert_eq!(media["images"][0]["name"], "logo.png");

    // A test shows a kind's style even while that kind is off.
    call(&mut h, "settings.set", json!({ "key": "notify.approval", "value": false }));
    let r = call(&mut h, "notify.test", json!({ "category": "approval" }));
    assert_eq!(r, json!({ "posted": true }));
    call(&mut h, "notify.test", json!({ "category": "failed" }));
    let p = wait_posted(&mut h, 2);
    let (t, f) = (&p[0]["data"], &p[1]["data"]);
    assert_eq!((t["test"].as_bool(), t["sound_file"].as_str().is_some_and(|f| f.ends_with("notify/sounds/ding.wav"))), (Some(true), true), "{t}");
    assert!(t["image"].as_str().is_some_and(|i| i.ends_with("notify/images/logo.png")), "{t}");
    assert_eq!(f.get("image"), None, "{f}");

    // Removing it puts its settings back.
    let r = call(&mut h, "notify.remove", json!({ "name": "ding.wav" }));
    assert_eq!(r["reset"], json!(["notify.sound.approval", "notify.sound.failed"]));
    assert_eq!(call(&mut h, "settings.get", json!({ "key": "notify.sound.approval" }))["value"], "Glass");
    assert_eq!(call_err(&mut h, "notify.remove", json!({ "name": "Glass" })).code, -32602);
    assert_eq!(call_err(&mut h, "notify.remove", json!({ "name": "../state.json" })).code, midna_proto::error::NOT_FOUND);
}

#[test]
fn agents_sound_only_when_they_ask() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let mut a = d.agent(Some(&sid));
    call(&mut a, "notify.send", json!({ "title": "quiet" }));
    call(&mut a, "notify.send", json!({ "title": "loud", "sound": true }));
    let p = posted(&mut h);
    assert_eq!(p[0]["data"]["sound"], false);
    assert_eq!(p[1]["data"]["sound_file"], "/System/Library/Sounds/Ping.aiff");
}
