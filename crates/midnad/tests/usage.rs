//! Claude's plan usage limits: agent_info.rate_limits, usage.get and usage.limit_reached.
mod common;
use common::*;
use midna_proto::time;
use serde_json::{Value, json};

fn statusline(a: &mut midna_proto::Client, five: f64, resets_in: i64) {
    let now = time::now_unix();
    let payload = json!({ "session_id": "c1", "model": { "id": "claude-opus-5-5" }, "cost": { "total_cost_usd": 0.1 },
        "rate_limits": { "five_hour": { "used_percentage": five, "resets_at": now + resets_in }, "seven_day": { "used_percentage": 8, "resets_at": now + 86_400 } } });
    call(a, "agent.hook", json!({ "agent": "claude", "event": "statusline", "payload": payload }));
}

fn reached(h: &mut midna_proto::Client) -> Vec<Value> {
    call(h, "events.list", json!({ "limit": 100, "filter": { "kinds": ["usage.limit_reached"] } })).as_array().unwrap().clone()
}

#[test]
fn status_line_limits_reach_session_get_and_usage_get() {
    let d = TestDaemon::start();
    let mut h = d.human();
    assert_eq!(call(&mut h, "usage.get", json!({})), json!({}), "nothing reported yet");
    let sid = open_sh(&mut h);
    let mut a = d.agent(Some(&sid));
    statusline(&mut a, 40.0, 3600);

    let info = &call(&mut h, "session.get", json!({ "id": sid }))["agent_info"];
    assert_eq!(info["rate_limits"]["five_hour"]["used_percentage"], 40.0);
    assert_eq!(info["rate_limits"]["seven_day"]["used_percentage"], 8.0);
    assert!(time::parse_rfc3339(info["rate_limits"]["five_hour"]["resets_at"].as_str().unwrap()).is_some());

    let u = call(&mut a, "usage.get", json!({}));
    let c = &u["claude"];
    assert_eq!((c["five_hour"]["used_percentage"].as_f64(), c["session"].as_str(), c["limited"].as_bool()), (Some(40.0), Some(sid.as_str()), Some(false)));
    assert!(c["observed_at"].is_string());
    assert!(reached(&mut h).is_empty());

    // The 5-hour window runs out: one event, however many status lines repeat it.
    statusline(&mut a, 100.0, 1800);
    statusline(&mut a, 100.0, 1800);
    let ev = reached(&mut h);
    assert_eq!(ev.len(), 1, "{ev:?}");
    assert_eq!((ev[0]["data"]["window"].as_str(), ev[0]["session_id"].as_str()), (Some("five_hour"), Some(sid.as_str())));
    let c = call(&mut h, "usage.get", json!({}))["claude"].clone();
    assert_eq!(c["limited"], true);
    assert_eq!(c["limited_until"], c["five_hour"]["resets_at"]);

    // The account's usage outlives the terminal that reported it.
    call(&mut h, "session.close", json!({ "id": sid, "force": true }));
    assert_eq!(call(&mut h, "usage.get", json!({}))["claude"]["five_hour"]["used_percentage"], 100.0);
}
