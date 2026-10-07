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
fn defaults_record_everything_important_but_push_only_what_needs_you() {
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
    // Recorded, but not a banner anywhere.
    assert_eq!((n["push"].as_bool(), n.get("push_focused")), (Some(false), None), "{n}");
    // Every kind plays one of midna's own (Twilight) sounds out of the box.
    assert_eq!(n["sound_file"].as_str(), d.home.join("notify/twilight/Strum.wav").to_str(), "{n}");
    assert_eq!(p[0]["session_id"].as_str(), Some(sid.as_str()));

    // A permission prompt is important (sound), pushed, and carries its needs-you id.
    hook(&mut a, "PermissionRequest", json!({ "session_id": "c1", "tool_name": "Bash", "tool_input": { "command": "rm -rf build" } }));
    let p = wait_posted(&mut h, 2);
    let n = &p[1]["data"];
    assert_eq!(n["category"], "approval");
    assert_eq!((n["sound"].as_bool(), n["push"].as_bool()), (Some(true), Some(true)), "{n}");
    assert!(n["needs_you_id"].as_str().is_some_and(|i| i.starts_with("n_")), "{n}");

    // Pushing for the terminal in front of you is its own switch (another terminal: dedupe).
    call(&mut h, "settings.set", json!({ "key": "notify.push_focused.turn_done", "value": true }));
    let mut a2 = d.agent(Some(&open_sh(&mut h)));
    hook(&mut a2, "UserPromptSubmit", json!({ "session_id": "c2", "prompt": "again" }));
    hook(&mut a2, "Stop", json!({ "session_id": "c2", "last_assistant_message": "Done." }));
    let p = wait_posted(&mut h, 3);
    assert_eq!((p[2]["data"]["push"].as_bool(), p[2]["data"]["push_focused"].as_bool()), (Some(false), Some(true)), "{}", p[2]);

    // Off by default: a clean exit.
    call(&mut h, "session.input", json!({ "id": sid, "text": "exit 0", "enter": true }));
    wait_for(5, "exit", || (call(&mut h, "session.get", json!({ "id": sid }))["status"]["state"] == "exited").then_some(()));
    assert_eq!(settle(&mut h).len(), 3);
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
    assert_eq!((n["sound_file"].as_str(), n["volume"].as_u64()), (d.home.join("notify/twilight/Portal.wav").to_str(), Some(100)), "{n}");

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
    // What macOS plays with the banner: that sound rendered at 25%, named (a full path plays the
    // default sound). Tests keep it in the daemon's home rather than ~/Library/Sounds.
    assert_eq!(n["notification_sound"], "Midna Submarine 25.wav", "{n}");
    let file = d.home.join("notify/banner/Midna Submarine 25.wav");
    assert!(std::fs::read(&file).is_ok_and(|b| b.starts_with(b"RIFF")), "{}", file.display());

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
    assert_eq!(call(&mut h, "settings.get", json!({ "key": "notify.sound.approval" }))["value"], "Portal");
    assert_eq!(call_err(&mut h, "notify.remove", json!({ "name": "Glass" })).code, -32602);
    assert_eq!(call_err(&mut h, "notify.remove", json!({ "name": "Portal" })).code, -32602);
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
    assert_eq!(p[1]["data"]["sound_file"].as_str(), d.home.join("notify/twilight/Hm.wav").to_str());
}

#[test]
fn agents_play_sounds_by_kind_or_name() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let mut a = d.agent(Some(&sid));
    call(&mut h, "settings.set", json!({ "key": "notify.volume", "value": 50 }));
    let played = |h: &mut Client| call(h, "events.list", json!({ "filter": { "kinds": ["notify.sound"] }, "limit": 100 })).as_array().cloned().unwrap_or_default();

    // A kind plays its sound at its volume, times the master; a name at full (or --volume).
    assert_eq!(call(&mut a, "notify.play", json!({ "sound": "approved" })), json!({ "played": false, "sound": "Rise", "reason": "no_app" }));
    call(&mut a, "notify.play", json!({ "sound": "glass", "volume": 40 }));
    let p = played(&mut h);
    let rise = d.home.join("notify/twilight/Rise.wav");
    assert_eq!((p[0]["data"]["file"].as_str(), p[0]["data"]["volume"].as_u64(), p[0]["data"]["kind"].as_str()), (rise.to_str(), Some(50), Some("approved")));
    assert_eq!((p[1]["data"]["sound"].as_str(), p[1]["data"]["volume"].as_u64(), p[1].get("session_id").and_then(Value::as_str)), (Some("Glass"), Some(20), Some(sid.as_str())));

    // A kind with no sound, unknown names, and the switches.
    call(&mut h, "settings.set", json!({ "key": "notify.sound.switched", "value": "none" }));
    assert_eq!(call(&mut a, "notify.play", json!({ "sound": "switched" }))["reason"], "no_sound");
    assert_eq!(call(&mut h, "notify.play", json!({ "sound": "uh-oh" }))["sound"], "Uh-oh", "Twilight names in any case");
    assert_eq!(call_err(&mut a, "notify.play", json!({ "sound": "nope" })).code, -32602);
    call(&mut h, "settings.set", json!({ "key": "notify.sounds", "value": false }));
    assert_eq!(call(&mut a, "notify.play", json!({ "sound": "Pop" }))["reason"], "sounds_off");
    call(&mut h, "notify.test", json!({ "category": "approval" }));
    assert_eq!(wait_posted(&mut h, 1)[0]["data"]["sound"], false, "notify.sounds off silences banners too");
    call(&mut h, "settings.set", json!({ "key": "notify.sounds", "value": true }));

    // Agents: 6 a minute (2 played above); the human isn't limited.
    let reasons: Vec<Value> = (0..5).map(|_| call(&mut a, "notify.play", json!({ "sound": "Pop" }))["reason"].clone()).collect();
    assert_eq!(reasons[3], "no_app");
    assert_eq!(reasons[4], "rate_limited");
    assert_eq!(call(&mut h, "notify.play", json!({ "sound": "Pop", "session": sid }))["reason"], "no_app");
}

#[test]
fn clear_asks_the_app_to_remove_all_or_one_terminals() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let cleared = |h: &mut Client| call(h, "events.list", json!({ "filter": { "kinds": ["notify.cleared"] }, "limit": 100 })).as_array().cloned().unwrap_or_default();

    assert_eq!(call(&mut h, "notify.clear", json!({})), json!({ "delivered": false }));
    assert_eq!(call(&mut h, "notify.clear", json!({ "session": sid })), json!({ "delivered": false, "session": sid }));
    let c = cleared(&mut h);
    assert_eq!(c.len(), 2);
    assert_eq!(c[0].get("session_id"), None, "no terminal: every notification");
    assert_eq!(c[1]["session_id"].as_str(), Some(sid.as_str()));
    call_err(&mut h, "notify.clear", json!({ "session": "s_nope" }));
}

#[test]
fn history_lists_newest_first_and_reading_clears_unread() {
    let d = TestDaemon::start();
    let mut h = d.human();
    // Nothing before the first look counts as unread.
    let r = call(&mut h, "notify.history", json!({}));
    assert_eq!((r["items"].as_array().map(Vec::len), r["unread"].as_u64()), (Some(0), Some(0)), "{r}");
    let sid = open_sh(&mut h);
    call(&mut h, "settings.set", json!({ "key": "notify.bell.agent", "value": true }));
    let mut a = d.agent(Some(&sid));
    call(&mut a, "notify.send", json!({ "title": "first" }));
    call(&mut a, "notify.send", json!({ "title": "second" }));
    call(&mut h, "notify.test", json!({ "session": sid }));
    wait_posted(&mut h, 3);
    let r = call(&mut h, "notify.history", json!({}));
    let items = r["items"].as_array().unwrap();
    // Tests are left out; newest first.
    assert_eq!(items.iter().map(|i| i["notification"]["body"].as_str().unwrap_or("")).collect::<Vec<_>>(), ["second", "first"], "{r}");
    assert!(items.iter().all(|i| i["unread"] == true && i["session_id"] == json!(sid)), "{r}");
    assert_eq!(r["unread"], 2);

    let done = call(&mut h, "notify.read", json!({}));
    assert_eq!(done["unread"], 0);
    let r = call(&mut h, "notify.history", json!({}));
    assert!(r["items"].as_array().unwrap().iter().all(|i| i["unread"] == false), "{r}");
    assert_eq!(r["read_seq"], done["read_seq"]);
    // An older seq never marks things unread again.
    assert_eq!(call(&mut h, "notify.read", json!({ "seq": 1 }))["read_seq"], done["read_seq"]);
    assert_eq!(call(&mut h, "events.list", json!({ "filter": { "kinds": ["notify.read"] } })).as_array().map(Vec::len), Some(1));
}

#[test]
fn only_kinds_that_count_on_the_bell_are_unread() {
    let d = TestDaemon::start();
    let mut h = d.human();
    call(&mut h, "notify.history", json!({}));
    let sid = open_sh(&mut h);
    let mut a = d.agent(Some(&sid));
    call(&mut a, "notify.send", json!({ "title": "one" }));
    call(&mut a, "notify.send", json!({ "title": "two" }));
    wait_posted(&mut h, 2);
    // Agent messages don't count out of the box: still listed (and unread), just not counted.
    let r = call(&mut h, "notify.history", json!({}));
    assert_eq!((r["unread"].as_u64(), r["items"].as_array().map(Vec::len)), (Some(0), Some(2)), "{r}");

    call(&mut h, "settings.set", json!({ "key": "notify.bell.agent", "value": true }));
    assert_eq!(call(&mut h, "notify.history", json!({}))["unread"], 2);
}

#[test]
fn each_post_carries_how_long_it_stays_and_its_color() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let mut a = d.agent(Some(&open_sh(&mut h)));
    call(&mut a, "notify.send", json!({ "title": "one" }));
    let n = &wait_posted(&mut h, 1)[0]["data"];
    assert_eq!((n["stay_secs"].as_u64(), n["color"].as_str(), n.get("label")), (Some(6), Some("accent"), None), "{n}");

    call(&mut h, "settings.set", json!({ "key": "notify.stay.agent", "value": 0 }));
    call(&mut h, "settings.set", json!({ "key": "notify.color.agent", "value": "#FF8800" }));
    call(&mut a, "notify.send", json!({ "title": "two" }));
    let n = &wait_posted(&mut h, 2)[1]["data"];
    assert_eq!((n["stay_secs"].as_u64(), n["color"].as_str()), (Some(0), Some("#ff8800")), "{n}");
    assert_eq!(call_err(&mut h, "settings.set", json!({ "key": "notify.color.agent", "value": "pink" })).code, -32602);
    assert_eq!(call_err(&mut h, "settings.set", json!({ "key": "notify.stay.agent", "value": 4000 })).code, -32602);
}

#[test]
fn kinds_you_add_get_their_own_settings_and_can_be_sent_to() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let mut a = d.agent(Some(&sid));
    // Built-in keys, global setting names and bad keys are refused.
    for key in ["approval", "enabled", "badge", "9lives", "has space", ""] {
        assert_eq!(call_err(&mut h, "notify.kinds.add", json!({ "key": key })).code, -32602, "{key}");
    }
    // A bad starting setting leaves no half-made kind behind.
    assert_eq!(call_err(&mut h, "notify.kinds.add", json!({ "key": "deploys", "settings": { "color": "pink" } })).code, -32602);
    assert_eq!(call(&mut h, "notify.kinds.list", json!({}))["kinds"], json!([]));

    let k = call(&mut a, "notify.kinds.add", json!({ "key": "deploys", "label": "Deploys", "settings": { "stay": 0, "color": "ok", "sound": "Glass" } }));
    assert_eq!(k, json!({ "key": "deploys", "label": "Deploys", "description": "", "enabled": true, "stay": 0, "color": "ok", "sound": "Glass", "push": true }));
    assert_eq!(call_err(&mut a, "notify.kinds.add", json!({ "key": "deploys" })).code, -32602);
    assert_eq!(call(&mut a, "notify.kinds.add", json!({ "key": "deploys", "label": "Ships", "replace": true }))["label"], "Ships");
    let changed = call(&mut h, "events.list", json!({ "filter": { "kinds": ["notify.kinds_changed"] } }));
    assert_eq!(changed.as_array().map(Vec::len), Some(2));

    // Its settings are ordinary settings: listed, typed and defaulted like a built-in kind's.
    let all = call(&mut h, "settings.list", json!({}));
    let entry = |k: &str| all.as_array().unwrap().iter().find(|e| e["key"] == k).cloned().unwrap_or_else(|| panic!("no {k} in settings.list"));
    assert_eq!((entry("notify.deploys")["value"].clone(), entry("notify.stay.deploys")["value"].clone()), (json!(true), json!(0)));
    assert_eq!(entry("notify.volume.deploys")["value"], 100);
    assert_eq!(call(&mut h, "settings.get", json!({ "key": "notify.sound.deploys" }))["value"], "Glass");
    assert_eq!(call_err(&mut h, "settings.set", json!({ "key": "notify.volume.deploys", "value": 101 })).code, -32602);
    let unknown = call_err(&mut h, "settings.get", json!({ "key": "notify.stay.nope" })).code;
    let listed = call(&mut h, "notify.list", json!({ "global": true }));
    assert!(listed["categories"].as_array().unwrap().iter().any(|c| c["key"] == "deploys" && c["custom"] == true), "{listed}");

    // Agents send to it by name; unknown kinds are refused.
    assert_eq!(call(&mut a, "notify.send", json!({ "title": "staging is up", "category": "deploys" })), json!({ "posted": true }));
    let n = &wait_posted(&mut h, 1)[0]["data"];
    assert_eq!((n["category"].as_str(), n["label"].as_str(), n["color"].as_str(), n["stay_secs"].as_u64()), (Some("deploys"), Some("Ships"), Some("ok"), Some(0)), "{n}");
    assert_eq!(call_err(&mut a, "notify.send", json!({ "title": "x", "category": "nope" })).code, -32602);
    assert_eq!(call(&mut h, "notify.test", json!({ "category": "deploys", "session": sid }))["posted"], true);

    // Its switch turns it off; a terminal can override it.
    call(&mut h, "settings.set", json!({ "key": "notify.deploys", "value": false }));
    assert_eq!(call(&mut a, "notify.send", json!({ "title": "prod is up", "category": "deploys" }))["reason"], "category_off");
    call(&mut h, "notify.set", json!({ "session": sid, "key": "deploys", "value": true }));
    assert_eq!(call(&mut a, "notify.send", json!({ "title": "prod is up", "category": "deploys" }))["posted"], true);

    // A trigger's notify action can post as it; an unknown kind is refused up front.
    let action = |k: &str| json!({ "kind": "notify", "title": "Shipped", "category": k });
    let trigger = |action: Value| json!({ "name": "Ship", "source": "local", "event": "agent.prompt_blocked", "action": action, "enabled": false });
    assert!(call(&mut h, "trigger.add", trigger(action("deploys")))["id"].is_string());
    assert_eq!(call_err(&mut h, "trigger.add", trigger(action("nope"))).code, -32602);

    // Removing it drops its settings and overrides; built-in kinds can't be removed.
    assert_eq!(call_err(&mut h, "notify.kinds.remove", json!({ "key": "approval" })).code, unknown);
    call(&mut h, "notify.kinds.remove", json!({ "key": "deploys" }));
    assert_eq!(call(&mut h, "notify.kinds.list", json!({}))["kinds"], json!([]));
    assert_eq!(call_err(&mut h, "settings.get", json!({ "key": "notify.deploys" })).code, unknown);
    assert!(call(&mut h, "session.get", json!({ "id": sid }))["notify"].get("deploys").is_none());
    // Added again, it starts from the defaults.
    call(&mut h, "notify.kinds.add", json!({ "key": "deploys" }));
    assert_eq!(call(&mut h, "settings.get", json!({ "key": "notify.deploys" }))["value"], true);
}
