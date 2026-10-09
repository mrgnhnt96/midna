//! Keep-awake (`keep_awake.status|set`): the assertion is held inside the hours while there is
//! work (or always), released on low battery with hysteresis, and steered by today's override.
//! An in-process daemon never touches IOKit; it reports what it would hold.
mod common;
use common::*;
use midna_proto::keep_awake::Battery;
use midna_proto::{Client, time};
use serde_json::{Value, json};

fn status(c: &mut Client) -> Value {
    call(c, "keep_awake.status", json!({}))
}

/// Enabled, open all day every day, so the result doesn't depend on when the test runs.
fn all_day(c: &mut Client, mode: &str) -> Value {
    call(c, "keep_awake.set", json!({ "on": true, "start": "12am", "end": "midnight", "days": "daily", "mode": mode, "linger_mins": 0 }))
}

#[test]
fn off_by_default_then_held_only_with_work() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let s = status(&mut h);
    assert_eq!((s["held"].as_bool(), s["reason"].as_str()), (Some(false), Some("disabled")), "{s}");
    assert_eq!(s["settings"]["start"], "09:00");
    assert_eq!(s["settings"]["days"], json!(["mon", "tue", "wed", "thu", "fri"]));
    assert_eq!(s["schedule"], "9 AM–6 PM weekdays");

    // Agents may change it (Taskboard does, through `midna call`).
    let mut a = d.agent(None);
    let s = all_day(&mut a, "with_work");
    assert_eq!((s["held"].as_bool(), s["reason"].as_str()), (Some(false), Some("no_work")), "{s}");
    assert_eq!((s["settings"]["start"].as_str(), s["settings"]["end"].as_str()), (Some("00:00"), Some("00:00")));
    assert_eq!(s["window_open"], true);
    assert!(s["next_off"].is_null() && s["next_on"].is_null(), "always open: {s}");

    // A message queued for an hour from now is work; one queued for the day after tomorrow isn't yet.
    let sid = open_sh(&mut h);
    call(&mut h, "queue.pause", json!({ "session": sid, "paused": true }));
    let later = time::format_unix(time::now_unix() + 2 * 86_400);
    call(&mut h, "queue.add", json!({ "session": sid, "text": "echo later", "when": { "kind": "at", "at": later } }));
    call(&mut h, "queue.pause", json!({ "session": sid, "paused": false }));
    assert_eq!(status(&mut h)["reason"], "no_work");
    let soon = time::format_unix(time::now_unix() + 3600);
    call(&mut h, "queue.add", json!({ "session": sid, "text": "echo soon", "when": { "kind": "at", "at": soon }, "position": 0 }));
    let s = status(&mut h);
    assert_eq!((s["held"].as_bool(), s["reason"].as_str()), (Some(true), Some("work")), "{s}");
    assert_eq!(s["work"], json!(["1 terminal with queued input"]));
    assert!(s["held_since"].is_string());
    assert!(s["line"].as_str().unwrap().starts_with("Keeping awake: 1 terminal with queued input"), "{s}");

    // Paused input isn't work: released at once (no linger).
    call(&mut h, "queue.pause", json!({ "session": sid, "paused": true }));
    let s = status(&mut h);
    assert_eq!((s["held"].as_bool(), s["reason"].as_str()), (Some(false), Some("no_work")), "{s}");

    let evs = call(&mut h, "events.list", json!({ "filter": { "kinds": ["keep_awake.changed"] }, "limit": 50 }));
    assert!(evs.as_array().unwrap().iter().any(|e| e["data"]["held"] == true), "{evs}");
}

#[test]
fn low_battery_releases_until_five_points_above_or_plugged_in() {
    let d = TestDaemon::start();
    let mut h = d.human();
    all_day(&mut h, "always");
    let dm = d.daemon();
    let battery = |percent, on_ac| midnad::keep_awake::set_battery(&dm, Some(Battery { percent, on_ac }));
    battery(50, false);
    assert_eq!(status(&mut h)["reason"], "always");
    battery(19, false);
    let s = status(&mut h);
    assert_eq!((s["held"].as_bool(), s["reason"].as_str()), (Some(false), Some("battery_low")), "{s}");
    assert_eq!(s["battery"], json!({ "percent": 19, "on_ac": false, "low": true }));
    battery(23, false);
    assert_eq!(status(&mut h)["reason"], "battery_low", "hysteresis");
    battery(23, true);
    assert_eq!(status(&mut h)["held"], true, "plugged in");
    battery(19, false);
    assert_eq!(status(&mut h)["held"], false);
    battery(25, false);
    assert_eq!(status(&mut h)["held"], true);
    call(&mut h, "keep_awake.set", json!({ "min_battery": "0%" }));
    battery(3, false);
    assert_eq!(status(&mut h)["held"], true, "0 = no limit");
}

#[test]
fn today_overrides_and_per_day_hours() {
    let d = TestDaemon::start();
    let mut h = d.human();
    // Today needs keep-awake on.
    assert_eq!(call_err(&mut h, "keep_awake.set", json!({ "today": "off" })).code, -32602);
    all_day(&mut h, "always");
    let s = call(&mut h, "keep_awake.set", json!({ "today": "off" }));
    assert_eq!((s["held"].as_bool(), s["reason"].as_str()), (Some(false), Some("today_off")), "{s}");
    assert_eq!(s["today"]["on"], false);
    assert_eq!(s["today"]["date"], midna_proto::keep_awake::local_date(time::now_unix()));
    assert!(s["next_on"].is_string(), "back on at midnight: {s}");
    let s = call(&mut h, "keep_awake.set", json!({ "today": "clear" }));
    assert_eq!((s["held"].as_bool(), s["today"].is_null()), (Some(true), true), "{s}");
    // The override survives a daemon restart.
    call(&mut h, "keep_awake.set", json!({ "today": { "on": false } }));
    drop(h);
    let mut d = d;
    d.restart();
    let mut h = d.human();
    assert_eq!(status(&mut h)["reason"], "today_off");
    call(&mut h, "keep_awake.set", json!({ "today": null }));
    assert_eq!(status(&mut h)["reason"], "today_off", "null leaves it alone");
    call(&mut h, "keep_awake.set", json!({ "today": "normal" }));
    // On for a while runs past midnight (24 hours always does).
    let s = call(&mut h, "keep_awake.set", json!({ "today": "for 24 hours" }));
    let tomorrow = midna_proto::keep_awake::local_date(time::now_unix() + 86_400);
    assert_eq!((s["today"]["until_date"].as_str(), s["held"].as_bool()), (Some(tomorrow.as_str()), Some(true)), "{s}");
    assert!(s["today"]["line"].as_str().unwrap().ends_with("tomorrow"), "{s}");
    assert!(call_err(&mut h, "keep_awake.set", json!({ "today": "for 25 hours" })).message.contains("24 hours"));
    call(&mut h, "keep_awake.set", json!({ "today": "clear" }));

    // Per-day hours merge by day; null puts a day back on the schedule.
    call(&mut h, "keep_awake.set", json!({ "hours": { "fri": "9am-3pm" } }));
    let s = call(&mut h, "keep_awake.set", json!({ "hours": { "sat": "off", "sun": "all day" } }));
    assert_eq!(s["settings"]["hours"], json!({ "fri": "09:00-15:00", "sat": "off", "sun": "all day" }));
    let s = call(&mut h, "keep_awake.set", json!({ "hours": { "fri": null } }));
    assert_eq!(s["settings"]["hours"], json!({ "sat": "off", "sun": "all day" }));
    let s = call(&mut h, "keep_awake.set", json!({ "hours": "" }));
    assert_eq!(s["settings"]["hours"], json!({}));

    // One bad field saves nothing.
    let e = call_err(&mut h, "keep_awake.set", json!({ "start": "7am", "end": "25:00" }));
    assert!(e.message.contains("keep_awake.end"), "{}", e.message);
    assert_eq!(status(&mut h)["settings"]["start"], "00:00");
    // The settings are ordinary settings too.
    let e = call(&mut h, "settings.set", json!({ "key": "keep_awake.days", "value": "mon-wed, fri" }));
    assert_eq!(e["value"], json!(["mon", "tue", "wed", "fri"]));
    let e = call(&mut h, "settings.set", json!({ "key": "keep_awake.start", "value": "8:30 PM" }));
    assert_eq!(e["value"], "20:30");
}

/// keep_awake.wake: once the (fake) grant is in, midnad keeps one wake scheduled 2 minutes
/// before the next work due inside the hours, skipping runs outside them, and moves or cancels
/// it as the work changes.
#[test]
fn wakes_the_mac_before_work_due_inside_the_hours() {
    let d = TestDaemon::start();
    let dm = d.daemon();
    midnad::wake::set_fake(&dm);
    let mut h = d.human();
    // Hours open 3 to 5 hours from now (local, whole hours), every day.
    let now = time::now_unix();
    let (y, mo, day, hr, ..) = time::local_parts(now);
    let opens = time::local_unix(y, mo, day, hr, 0) + 3 * 3600;
    let hm = |t: i64| {
        let (.., h, m, _) = time::local_parts(t);
        format!("{h:02}:{m:02}")
    };
    call(&mut h, "keep_awake.set", json!({ "on": true, "start": hm(opens), "end": hm(opens + 2 * 3600), "days": "daily", "wake": true }));
    let s = status(&mut h);
    assert_eq!((s["wake"]["ready"].as_bool(), s["settings"]["wake"].as_bool()), (Some(false), Some(true)), "{s}");
    assert!(s["wake"]["line"].as_str().unwrap().contains("wake setup"), "{s}");

    // The grant is the human's to give.
    let mut a = d.agent(None);
    let e = call_err(&mut a, "keep_awake.wake_setup", json!({}));
    assert_eq!(e.code, midna_proto::error::HUMAN_ONLY, "{e:?}");
    let s = call(&mut h, "keep_awake.wake_setup", json!({}));
    assert_eq!(s["wake"]["ready"], true, "{s}");
    assert!(s["wake"]["next"].is_null() && s["wake"]["line"] == "Nothing scheduled inside the hours to wake for", "{s}");

    // An hourly trigger: its first run inside the hours is when they open.
    let t = call(&mut h, "trigger.add", json!({ "name": "Morning kickoff", "source": "local", "event": "schedule", "filter": { "cron": "@hourly" }, "action": { "kind": "attention", "message": "go" }, "enabled": true }));
    let s = status(&mut h);
    assert_eq!(s["wake"]["next"].as_str().and_then(time::parse_rfc3339), Some(opens - 120), "{s}");
    assert!(s["wake"]["reason"].as_str().unwrap().starts_with("Morning kickoff at "), "{s}");
    assert!(s["wake"]["line"].as_str().unwrap().starts_with("Waking the Mac "), "{s}");
    let date = |t: i64| {
        let (y, mo, d, h, mi, _) = time::local_parts(t);
        format!("{mo:02}/{d:02}/{:02} {h:02}:{mi:02}:00", y % 100)
    };
    assert_eq!(midnad::wake::fake_calls(&dm), vec!["setup".to_string(), format!("wake {}", date(opens - 120))]);
    status(&mut h);
    assert_eq!(midnad::wake::fake_calls(&dm).len(), 2, "scheduled once");

    // A message queued for half an hour after they open doesn't move it; disabling the trigger does.
    let sid = open_sh(&mut h);
    call(&mut h, "queue.add", json!({ "session": sid, "text": "echo later", "when": { "kind": "at", "at": time::format_unix(opens + 1800) } }));
    call(&mut h, "trigger.set_enabled", json!({ "id": t["id"], "enabled": false }));
    let s = status(&mut h);
    assert_eq!(s["wake"]["next"].as_str().and_then(time::parse_rfc3339), Some(opens + 1800 - 120), "{s}");
    let calls = midnad::wake::fake_calls(&dm);
    assert_eq!(calls[2..], [format!("cancel {}", date(opens - 120)), format!("wake {}", date(opens + 1800 - 120))]);

    // Turning it off cancels; removing the grant turns it off.
    call(&mut h, "keep_awake.set", json!({ "wake": false }));
    assert!(status(&mut h)["wake"]["next"].is_null());
    assert_eq!(midnad::wake::fake_calls(&dm).last().unwrap(), &format!("cancel {}", date(opens + 1800 - 120)));
    let s = call(&mut h, "keep_awake.wake_setup", json!({ "remove": true }));
    assert_eq!((s["wake"]["ready"].as_bool(), s["settings"]["wake"].as_bool()), (Some(false), Some(false)), "{s}");
}

/// Woken for work due as the hours open, keep-awake holds before they do, so the work runs.
#[test]
fn holds_from_the_wake_until_work_due_as_the_hours_open() {
    let d = TestDaemon::start();
    midnad::wake::set_fake(&d.daemon());
    let mut h = d.human();
    call(&mut h, "keep_awake.wake_setup", json!({}));
    // Due 1-2 minutes from now (inside the wake's lead: the Mac is awake for it), which is also
    // when the hours open.
    let due = (time::now_unix() / 60 + 2) * 60;
    let (.., hr, mi, _) = time::local_parts(due);
    let (.., end_h, end_m, _) = time::local_parts(due + 3600);
    call(&mut h, "keep_awake.set", json!({ "on": true, "start": format!("{hr:02}:{mi:02}"), "end": format!("{end_h:02}:{end_m:02}"), "days": "daily", "linger_mins": 0 }));
    let s = status(&mut h);
    assert_eq!((s["held"].as_bool(), s["reason"].as_str()), (Some(false), Some("outside_hours")), "{s}");
    call(&mut h, "trigger.add", json!({ "name": "Kickoff", "source": "local", "event": "schedule", "filter": { "cron": format!("{mi} {hr} * * *") }, "action": { "kind": "attention", "message": "go" }, "enabled": true }));
    let s = status(&mut h);
    assert_eq!((s["held"].as_bool(), s["reason"].as_str()), (Some(true), Some("work")), "{s}");
    assert_eq!(s["window_open"], false);
    assert!(s["wake"]["line"].as_str().unwrap().starts_with("Awake for Kickoff at "), "{s}");

    // Without keep_awake.wake it waits for the hours as before.
    call(&mut h, "keep_awake.set", json!({ "wake": false }));
    assert_eq!(status(&mut h)["reason"], "outside_hours");
}
