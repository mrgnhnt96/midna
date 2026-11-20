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

#[test]
fn a_flood_is_deduped_rate_limited_cooled_down_and_listed() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let notify = json!({ "kind": "notify", "title": "{{stream.id}}" });
    // Default cooldown: once a minute per stream. Zero: every event.
    let once = call(&mut h, "trigger.add", json!({ "name": "once", "source": "local", "event": "stream.flood.*", "action": notify, "enabled": true }));
    let every = call(&mut h, "trigger.add", json!({ "name": "every", "source": "local", "event": "stream.flood.*", "action": notify, "enabled": true, "cooldown_secs": 0 }));

    // 30 at once: the burst of 20 gets in, the rest are over the rate limit.
    let mut first = None;
    for n in 0..30 {
        let p = json!({ "stream": "flood", "kind": "message", "id": format!("m{n}"), "data": { "n": n } });
        if n < 20 {
            let r = call(&mut h, "streams.publish", p);
            assert_eq!(r["verdict"], "accepted", "{n}: {r}");
            first.get_or_insert(r["seq"].as_u64().unwrap());
        } else {
            let e = call_err(&mut h, "streams.publish", p);
            assert_eq!(e.code, midna_proto::error::BUSY, "{n}: {e:?}");
            let data = e.data.clone().unwrap();
            assert_eq!(data["verdict"], "rate_limited", "{e:?}");
            assert!(data["retry_after_ms"].as_u64().is_some_and(|ms| (1..=1000).contains(&ms)), "{e:?}");
            assert!(e.message.contains("rate limit"), "{e:?}");
        }
    }
    // A repeated id is a duplicate of the first event, even over the limit, and isn't logged.
    let dup = call(&mut h, "streams.publish", json!({ "stream": "flood", "kind": "message", "id": "m0", "data": { "n": 0 } }));
    assert_eq!((dup["verdict"].as_str(), dup["seq"].as_u64()), (Some("duplicate"), first), "{dup}");
    // Too big: dropped and listed.
    let big = call_err(&mut h, "streams.publish", json!({ "stream": "flood", "kind": "message", "id": "big", "data": { "t": "x".repeat(70_000) } }));
    assert_eq!(big.data.unwrap()["verdict"], "too_large");
    // Too big with a bad id is just a bad publish, not listed.
    let e = call_err(&mut h, "streams.publish", json!({ "stream": "flood", "kind": "message", "id": "", "data": { "t": "x".repeat(70_000) } }));
    assert!(e.message.contains("id must be") && e.data.is_none(), "{e:?}");
    // Another stream has its own limit.
    assert_eq!(call(&mut h, "streams.publish", json!({ "stream": "calm", "kind": "message" }))["verdict"], "accepted");

    assert_eq!(events(&mut h, "stream.flood.message").len(), 20);
    let listed = call(&mut h, "streams.events", json!({ "stream": "flood" }));
    assert_eq!(listed["counts"], json!({ "accepted": 20, "duplicate": 1, "rate_limited": 10, "too_large": 1, "busy": 0, "refused": 0 }), "{listed}");
    let rows = listed["events"].as_array().unwrap();
    assert_eq!(rows.len(), 32);
    assert_eq!((rows[0]["verdict"].as_str(), rows[0]["id"].as_str()), (Some("too_large"), Some("big")));
    assert_eq!((rows[1]["verdict"].as_str(), rows[1]["seq"].as_u64()), (Some("duplicate"), first));
    assert_eq!((rows[2]["verdict"].as_str(), rows[2]["id"].as_str()), (Some("rate_limited"), Some("m29")));
    assert_eq!((rows[31]["verdict"].as_str(), rows[31]["id"].as_str(), rows[31]["seq"].as_u64()), (Some("accepted"), Some("m0"), first));
    assert_eq!(call(&mut h, "streams.events", json!({ "stream": "flood", "limit": 2 }))["events"].as_array().unwrap().len(), 2);
    assert_eq!(call(&mut h, "streams.events", json!({ "stream": "nobody" })), json!({ "stream": "nobody", "events": [], "counts": { "accepted": 0, "duplicate": 0, "rate_limited": 0, "too_large": 0, "busy": 0, "refused": 0 } }));
    assert!(bad(&mut h, "streams.events", json!({ "stream": "No Such" })).contains("a-z 0-9 -"));

    // The trigger with no cooldown fires for each accepted event; the default one once.
    let deliveries = |h: &mut Client, t: &Value| call(h, "trigger.deliveries", json!({ "trigger_id": t["id"], "limit": 100 })).as_array().map_or(0, Vec::len);
    wait_for(10, "a delivery per accepted event", || (deliveries(&mut h, &every) == 20).then_some(()));
    assert_eq!(deliveries(&mut h, &once), 1, "cooled down for the stream");
    // The cooldown is per stream: another stream's event fires the cooled-down trigger.
    call(&mut h, "trigger.update", json!({ "id": once["id"], "event": "stream.*" }));
    call(&mut h, "streams.publish", json!({ "stream": "other", "kind": "message" }));
    wait_for(5, "the other stream fires it", || (deliveries(&mut h, &once) == 2).then_some(()));
}
