//! Queued messages (`queue.*`): typed in order once the agent is ready, held while it works,
//! while paused, until a time or until another terminal finishes; edited, moved, removed and
//! sent now.
mod common;
use common::*;
use midna_proto::Client;
use serde_json::{Value, json};
use std::time::Duration;

fn hook(a: &mut Client, ev: &str, payload: Value) {
    call(a, "agent.hook", json!({ "agent": "claude", "event": ev, "payload": payload }));
}

fn queue(h: &mut Client, sid: &str) -> Value {
    call(h, "queue.list", json!({ "session": sid }))
}

fn items(h: &mut Client, sid: &str) -> Vec<Value> {
    queue(h, sid)["items"].as_array().cloned().unwrap_or_default()
}

fn add(h: &mut Client, sid: &str, text: &str, when: Value) -> Value {
    call(h, "queue.add", json!({ "session": sid, "text": text, "when": when }))
}

fn actions(h: &mut Client, sid: &str) -> Vec<String> {
    let evs = call(h, "events.list", json!({ "filter": { "kinds": ["session.queue"] }, "limit": 200 }));
    evs.as_array().unwrap().iter().filter(|e| e["session_id"] == sid).map(|e| e["data"]["action"].as_str().unwrap_or("").to_string()).collect()
}

#[test]
fn messages_go_in_order_once_ready_and_leave_the_queue() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let one = add(&mut h, &sid, "echo q-one", json!({ "kind": "idle" }));
    assert!(one["id"].as_str().unwrap().starts_with("q_"), "{one}");
    assert_eq!((one["state"].as_str(), one["by"]["kind"].as_str()), (Some("waiting"), Some("human")));
    add(&mut h, &sid, "echo q-two", json!({ "kind": "idle" }));
    let screen = wait_for(20, "both typed", || Some(read(&mut h, &sid)).filter(|t| t.contains("q-two")));
    assert!(screen.find("echo q-one").unwrap() < screen.find("echo q-two").unwrap(), "in order:\n{screen}");
    wait_for(5, "queue empty", || items(&mut h, &sid).is_empty().then_some(()));
    let a = actions(&mut h, &sid);
    assert_eq!(a.iter().filter(|x| *x == "sent").count(), 2, "{a:?}");
    assert_eq!(a.iter().filter(|x| *x == "added").count(), 2, "{a:?}");
}

#[test]
fn holds_while_the_agent_works_and_while_paused() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let mut a = d.agent(Some(&sid));
    hook(&mut a, "UserPromptSubmit", json!({ "prompt": "long task" }));
    // An agent queues into its own terminal (no `session`).
    let m = call(&mut a, "queue.add", json!({ "text": "echo after-work" }));
    assert_eq!(m["by"]["kind"], "agent");
    let first = wait_for(5, "waiting_for", || items(&mut h, &sid).first().cloned().filter(|i| i["waiting_for"].as_array().is_some_and(|w| !w.is_empty())));
    assert!(first["waiting_for"].to_string().contains("finish"), "{first}");
    std::thread::sleep(Duration::from_secs(3));
    assert!(!read(&mut h, &sid).contains("after-work"), "typed while the agent was working");

    let q = call(&mut h, "queue.pause", json!({ "session": sid }));
    assert_eq!(q["paused"], true);
    hook(&mut a, "Stop", json!({}));
    std::thread::sleep(Duration::from_secs(4));
    assert!(!read(&mut h, &sid).contains("after-work"), "typed while paused");
    call(&mut h, "queue.pause", json!({ "session": sid, "paused": false }));
    wait_for(15, "typed after resume", || Some(read(&mut h, &sid)).filter(|t| t.contains("after-work")));
    let acts = actions(&mut h, &sid);
    assert!(acts.contains(&"paused".to_string()) && acts.contains(&"resumed".to_string()), "{acts:?}");
}

#[test]
fn edit_move_remove_clear_and_validation() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    call(&mut h, "queue.pause", json!({ "session": sid }));
    let later = json!({ "kind": "at", "at": "2999-01-01T00:00:00Z" });
    let a = add(&mut h, &sid, "echo a", later.clone());
    let b = add(&mut h, &sid, "echo b", json!({ "kind": "idle_for", "minutes": 10 }));
    let c = call(&mut h, "queue.add", json!({ "session": sid, "text": "echo c", "position": 0 }));
    let order = |h: &mut Client| items(h, &sid).iter().map(|i| i["text"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert_eq!(order(&mut h), ["echo c", "echo a", "echo b"]);

    let q = call(&mut h, "queue.move", json!({ "session": sid, "id": b["id"], "to": 0 }));
    assert_eq!(q["items"][0]["id"], b["id"]);
    let e = call(&mut h, "queue.update", json!({ "session": sid, "id": a["id"], "text": "echo a2", "when": { "kind": "idle" } }));
    assert_eq!((e["text"].as_str(), e["when"]["kind"].as_str()), (Some("echo a2"), Some("idle")));
    call(&mut h, "queue.remove", json!({ "session": sid, "id": c["id"] }));
    assert_eq!(order(&mut h), ["echo b", "echo a2"]);

    // Validation.
    assert_eq!(call_err(&mut h, "queue.add", json!({ "session": sid, "text": "" })).code, -32602);
    assert_eq!(call_err(&mut h, "queue.add", json!({ "session": sid, "text": "x", "when": { "kind": "after", "session": sid } })).code, -32602);
    assert_eq!(call_err(&mut h, "queue.add", json!({ "session": sid, "text": "x", "when": { "kind": "at", "at": "tomorrow" } })).code, -32602);
    assert_eq!(call_err(&mut h, "queue.add", json!({ "session": sid, "text": "x", "when": { "kind": "idle_for", "minutes": 0 } })).code, -32602);
    call_err(&mut h, "queue.remove", json!({ "session": sid, "id": "q_nope" }));

    let q = call(&mut h, "queue.clear", json!({ "session": sid }));
    assert!(q["items"].as_array().unwrap().is_empty(), "{q}");
    // The queue survives in state (persisted with the session).
    add(&mut h, &sid, "echo kept", later);
    let s = call(&mut h, "session.get", json!({ "id": sid }));
    assert_eq!((s["queue"].as_array().map(Vec::len), s["queue_paused"].as_bool()), (Some(1), Some(true)), "{s}");
}

#[test]
fn waits_for_a_time_and_send_now_skips_the_wait() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let m = add(&mut h, &sid, "echo not-yet", json!({ "kind": "at", "at": "2999-01-01T00:00:00Z" }));
    let first = wait_for(5, "waiting_for", || items(&mut h, &sid).first().cloned().filter(|i| i["waiting_for"].as_array().is_some_and(|w| !w.is_empty())));
    assert!(first["waiting_for"].to_string().contains("2999"), "{first}");
    std::thread::sleep(Duration::from_secs(2));
    assert!(!read(&mut h, &sid).contains("not-yet"));
    // The first message holds the ones behind it.
    add(&mut h, &sid, "echo behind", json!({ "kind": "idle" }));
    std::thread::sleep(Duration::from_secs(3));
    assert!(!read(&mut h, &sid).contains("behind"), "a later message jumped the queue");

    call(&mut h, "queue.send_now", json!({ "session": sid, "id": m["id"] }));
    wait_for(5, "sent now", || Some(read(&mut h, &sid)).filter(|t| t.contains("not-yet")));
    wait_for(15, "the next one follows", || Some(read(&mut h, &sid)).filter(|t| t.contains("echo behind")));
    wait_for(5, "queue empty", || items(&mut h, &sid).is_empty().then_some(()));
}

#[test]
fn after_another_terminal_finishes() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let web = open_sh(&mut h);
    let api = open_sh(&mut h);
    let mut a = d.agent(Some(&api));
    hook(&mut a, "UserPromptSubmit", json!({ "prompt": "refactor" }));
    add(&mut h, &web, "echo pick-up-api", json!({ "kind": "after", "session": api }));
    let first = wait_for(5, "waiting_for", || items(&mut h, &web).first().cloned().filter(|i| i["waiting_for"].as_array().is_some_and(|w| !w.is_empty())));
    assert!(first["waiting_for"].to_string().contains("to finish"), "{first}");
    std::thread::sleep(Duration::from_secs(3));
    assert!(!read(&mut h, &web).contains("pick-up-api"));
    hook(&mut a, "Stop", json!({}));
    wait_for(15, "typed once api finished", || Some(read(&mut h, &web)).filter(|t| t.contains("pick-up-api")));
}

#[test]
fn a_closed_terminal_fails_the_message_and_raises_a_note() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    call(&mut h, "queue.pause", json!({ "session": sid }));
    let m = add(&mut h, &sid, "echo never", json!({ "kind": "idle" }));
    call(&mut h, "session.input", json!({ "id": sid, "text": "exit", "enter": true }));
    wait_for(10, "process ended", || {
        let s = call(&mut h, "session.get", json!({ "id": sid }));
        matches!(s["status"]["state"].as_str(), Some("exited" | "failed" | "stopped")).then_some(())
    });
    // send_now reports the failure; the message stays first, marked failed.
    let e = call_err(&mut h, "queue.send_now", json!({ "session": sid, "id": m["id"] }));
    assert!(e.message.contains("ended") || e.message.contains("input"), "{e:?}");
    let it = items(&mut h, &sid);
    assert_eq!((it[0]["state"].as_str(), it.len()), (Some("failed"), 1), "{it:?}");
    let notes = call(&mut h, "needs_you.list", json!({}));
    assert!(notes.as_array().unwrap().iter().any(|n| n["title"].as_str().is_some_and(|t| t.starts_with("Couldn't send a queued message"))), "{notes}");
    // Retry puts it back to waiting.
    let r = call(&mut h, "queue.update", json!({ "session": sid, "id": m["id"], "retry": true }));
    assert_eq!(r["state"], "waiting");
}
