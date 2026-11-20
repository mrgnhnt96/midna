//! Local event streams: `streams.publish` logs `stream.<name>.<kind>` with the payload as data
//! and the envelope on the event, and local triggers react to it like any midna event.
mod common;
use common::*;
use midna_proto::Client;
use serde_json::{Value, json};

fn events(h: &mut Client, kind: &str) -> Vec<Value> {
    call(h, "events.list", json!({ "filter": { "kinds": [kind] }, "limit": 100 })).as_array().cloned().unwrap_or_default()
}

fn bad(c: &mut Client, m: &str, p: Value) -> String {
    call_err(c, m, p).message
}

#[test]
fn a_published_event_is_logged_with_its_envelope_and_fires_local_triggers() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let mut a = d.agent(Some(&sid));
    let t = call(
        &mut a,
        "trigger.add",
        json!({
            "name": "CI failed", "source": "local", "event": "stream.slack.*",
            "filter": { "match": { "text": "*failed to build on pipeline roon_mobile*", "branch": "master" } },
            "action": { "kind": "notify", "title": "{{stream.name}}/{{stream.kind}}/{{stream.id}}", "body": "{{data.text}}" }, "enabled": true
        }),
    );
    assert_eq!(t["state"], "active", "{t}");

    // Dry run: the stream's name and kind come from the event; the id isn't known yet.
    let r = call(&mut h, "trigger.test", json!({ "trigger_id": t["id"], "event": "stream.slack.message", "payload": { "text": "x failed to build on pipeline roon_mobile", "branch": "master" } }));
    assert_eq!(r["summary"], "Matches · would notify: slack/message/‹id› — x failed to build on pipeline roon_mobile", "{r}");

    let text = "Build 42 failed to build on pipeline roon_mobile";
    let p = call(&mut a, "streams.publish", json!({ "stream": "slack", "kind": "message", "id": "1700000000.0001", "data": { "text": text, "branch": "master" } }));
    assert_eq!(p["event"], "stream.slack.message", "{p}");
    let logged = events(&mut h, "stream.slack.message");
    let e = logged.last().expect("logged");
    assert_eq!(e["seq"], p["seq"]);
    assert_eq!(e["data"], json!({ "text": text, "branch": "master" }), "data is the payload itself");
    assert_eq!(e["stream"], json!({ "name": "slack", "kind": "message", "id": "1700000000.0001" }));
    assert_eq!((e["actor"]["kind"].as_str(), e["actor"]["session"].as_str()), (Some("agent"), Some(sid.as_str())), "{e}");
    assert!(e.get("session_id").is_none(), "a stream event belongs to no terminal: {e}");

    let posted = wait_for(5, "notification", || events(&mut h, "notify.posted").into_iter().find(|e| e["data"]["category"] == "from_trigger"));
    let body = posted["data"]["body"].as_str().unwrap();
    assert!(body.contains("slack/message/1700000000.0001") && body.contains(text), "{posted}");
    let fired = call(&mut h, "trigger.deliveries", json!({ "trigger_id": t["id"] }));
    assert_eq!(fired.as_array().unwrap().len(), 1, "{fired}");
    assert_eq!(fired[0]["event"], "stream.slack.message");

    // The filter is on the payload: another branch, or another stream, doesn't fire it.
    call(&mut h, "streams.publish", json!({ "stream": "slack", "kind": "message", "data": { "text": text, "branch": "dev" } }));
    call(&mut h, "streams.publish", json!({ "stream": "ci", "kind": "message", "data": { "text": text, "branch": "master" } }));
    // No data: an empty payload.
    let empty = call(&mut h, "streams.publish", json!({ "stream": "ci", "kind": "build.done" }));
    assert_eq!(events(&mut h, "stream.ci.build.done").last().unwrap()["data"], json!({}), "{empty}");
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert_eq!(call(&mut h, "trigger.deliveries", json!({ "trigger_id": t["id"] })).as_array().unwrap().len(), 1);
    // Publishing is its own record, not an audit entry.
    assert!(events(&mut h, "audit").iter().all(|e| !e["data"].to_string().contains("streams.publish")));
}

#[test]
fn publishes_and_stream_triggers_are_checked() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let e = bad(&mut h, "streams.publish", json!({ "stream": "Slack", "kind": "message" }));
    assert!(e.contains("a-z 0-9 -"), "{e}");
    let e = bad(&mut h, "streams.publish", json!({ "stream": "slack", "kind": "new message" }));
    assert!(e.contains("kind"), "{e}");
    let e = bad(&mut h, "streams.publish", json!({ "stream": "slack", "kind": "m", "data": [1, 2] }));
    assert!(e.contains("JSON object"), "{e}");
    let e = bad(&mut h, "streams.publish", json!({ "stream": "slack", "kind": "m", "data": { "t": "x".repeat(70_000) } }));
    assert!(e.contains("64 KB"), "{e}");
    assert!(events(&mut h, "stream.slack.m").is_empty());

    let add = |event: &str, filter: Value, action: Value| json!({ "name": "s", "source": "local", "event": event, "filter": filter, "action": action });
    let notify = json!({ "kind": "notify", "title": "{{data.text}}" });
    let e = bad(&mut h, "trigger.add", add("stream.slack", json!({}), notify.clone()));
    assert!(e.contains("stream.slack.*"), "{e}");
    let e = bad(&mut h, "trigger.add", add("streams.slack.message", json!({}), notify.clone()));
    assert!(e.contains("stream.<name>.<kind>"), "{e}");
    let e = bad(&mut h, "trigger.add", add("stream.slack.*", json!({ "session": "abcd1234" }), notify.clone()));
    assert!(e.contains("no terminal"), "{e}");
    let e = bad(&mut h, "trigger.add", add("stream.slack.*", json!({}), json!({ "kind": "send_to_session", "steps": [{ "text": "hi" }] })));
    assert!(e.contains("send_to_session") && e.contains("have none"), "{e}");
    let t = call(&mut h, "trigger.add", add("stream.*", json!({}), notify));
    let e = bad(&mut h, "trigger.update", json!({ "id": t["id"], "event": "stream.Sl_ack.*" }));
    assert!(e.contains("can't match a stream event"), "{e}");
}
