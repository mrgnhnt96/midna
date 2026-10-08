//! A night asleep isn't idle time: the checks that wait for "N minutes of nobody touching it"
//! count awake time only (`clock.rs`). A sleep is simulated the way the Mac makes one: the
//! wall clock moves on and the monotonic one doesn't (`Clock::simulate_sleep`), with the
//! terminal's last activity moved back by as much, as if it happened before the sleep.
mod common;
use common::*;
use midna_proto::{Client, time};
use serde_json::{Value, json};
use std::sync::atomic::Ordering;
use std::time::Duration;

const NIGHT: i64 = 10 * 3600;

/// The terminal's last output and status change were `secs` ago.
fn backdate(d: &TestDaemon, sid: &str, secs: i64) {
    let at = time::now_unix() - secs;
    let dm = d.daemon();
    dm.rt(sid).unwrap().activity.store(at, Ordering::Relaxed);
    dm.core().state.session_mut(sid).unwrap().status.since = time::format_unix(at);
}

/// Sleep `NIGHT`, having last seen `sid` active just before.
fn sleep_through_the_night(d: &TestDaemon, sid: &str) {
    backdate(d, sid, NIGHT);
    d.daemon().clock.simulate_sleep(NIGHT);
}

#[test]
fn the_night_is_kept_in_the_sleep_log() {
    let d = TestDaemon::start();
    let mut h = d.human();
    assert_eq!(call(&mut h, "clock.sleeps", json!({})), json!({ "sleeps": [] }));
    d.daemon().clock.simulate_sleep(NIGHT);
    let s = call(&mut h, "clock.sleeps", json!({}))["sleeps"].clone();
    assert_eq!(s.as_array().unwrap().len(), 1, "{s}");
    assert!((s[0]["secs"].as_i64().unwrap() - NIGHT).abs() <= 2, "{s}");
    assert_eq!(call(&mut d.agent(None), "clock.sleeps", json!({ "since": time::now_rfc3339() })), json!({ "sleeps": [] }));
    assert_eq!(call_err(&mut h, "clock.sleeps", json!({ "since": "last night" })).code, -32602);
}

#[test]
fn an_idle_trigger_doesnt_fire_on_wake() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let t = call(
        &mut h,
        "trigger.add",
        json!({
            "name": "Keep warm", "source": "local", "event": "idle", "filter": { "idle_minutes": 55, "session": sid },
            "action": { "kind": "send_to_session", "steps": [{ "text": "echo warm-{{idle_minutes}}" }] }, "enabled": true
        }),
    );
    let fired = |h: &mut Client| call(h, "trigger.deliveries", json!({ "trigger_id": t["id"] })).as_array().unwrap().len();
    d.daemon().clock.simulate_sleep(NIGHT);
    midnad::local::backdate_activity(&d.daemon(), &sid, NIGHT + 5 * 60);
    assert_eq!(fired(&mut h), 0, "5 awake minutes and a night asleep aren't 55 idle minutes");
    midnad::local::backdate_activity(&d.daemon(), &sid, NIGHT + 56 * 60);
    assert_eq!(fired(&mut h), 1, "56 awake minutes are");
}

fn queued(h: &mut Client, sid: &str) -> Vec<Value> {
    call(h, "queue.list", json!({ "session": sid }))["items"].as_array().cloned().unwrap_or_default()
}

#[test]
fn a_message_waiting_for_quiet_isnt_sent_on_wake() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    wait_for(5, "prompt", || Some(read(&mut h, &sid)).filter(|t| !t.trim().is_empty()));
    call(&mut h, "queue.add", json!({ "session": sid, "text": "echo after-quiet", "when": { "kind": "idle_for", "minutes": 10 } }));
    sleep_through_the_night(&d, &sid);
    std::thread::sleep(Duration::from_secs(3));
    let item = queued(&mut h, &sid).first().cloned().expect("still queued");
    let waiting = item["waiting_for"].to_string();
    assert!(waiting.contains("10 min of quiet (0 min so far)"), "{item}");
    // Ten awake minutes before the night: now it goes.
    backdate(&d, &sid, NIGHT + 11 * 60);
    wait_for(15, "typed", || Some(read(&mut h, &sid)).filter(|t| t.contains("after-quiet")));
}

#[test]
fn a_queued_restarts_grace_counts_awake_time() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let grace = |d: &TestDaemon| midnad::restart::blockers(&d.daemon(), &sid).iter().any(|b| b.contains("since the terminal was last active"));
    sleep_through_the_night(&d, &sid);
    assert!(grace(&d), "just woke: the human may be about to type");
    backdate(&d, &sid, NIGHT + 2 * 60);
    assert!(!grace(&d));
}

#[test]
fn needs_you_items_dont_expire_overnight() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let n = call(&mut d.agent(Some(&sid)), "needs_you.raise", json!({ "kind": "blocked", "message": "need the API key" }));
    let id = n["id"].as_str().unwrap().to_string();
    call(&mut h, "settings.set", json!({ "key": "needs_you.expire_hours", "value": 4 }));
    // Raised 3 awake hours before a night asleep.
    let open = |h: &mut Client| call(h, "needs_you.list", json!({})).as_array().unwrap().iter().any(|n| n["id"] == id.as_str());
    let raised = |d: &TestDaemon, secs: i64| {
        let dm = d.daemon();
        dm.core().state.needs_you.iter_mut().find(|n| n.id == id).unwrap().created_at = time::format_unix(time::now_unix() - secs);
    };
    d.daemon().clock.simulate_sleep(NIGHT);
    raised(&d, NIGHT + 3 * 3600);
    midnad::rpc::needs_you::sweep(&d.daemon());
    assert!(open(&mut h), "13h on the wall clock, but only 3h awake");
    raised(&d, NIGHT + 5 * 3600);
    midnad::rpc::needs_you::sweep(&d.daemon());
    assert!(!open(&mut h), "5 awake hours is past expire_hours");
}
