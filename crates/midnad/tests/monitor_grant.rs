//! Monitor grants: only the human gives or revokes one, it shows on the terminal, and it ends
//! by itself at its `until`.
mod common;
use common::*;
use midna_proto::error::{CONFLICT, HUMAN_ONLY};
use midna_proto::{Client, time};
use serde_json::{Value, json};

fn events(c: &mut Client, kind: &str) -> Vec<Value> {
    call(c, "events.list", json!({ "filter": { "kinds": [kind] } })).as_array().cloned().unwrap_or_default()
}

fn approve(h: &mut Client, nid: &str) -> Value {
    call(h, "needs_you.resolve", json!({ "id": nid, "resolution": { "kind": "approve", "scope": { "kind": "once" } } }))
}

#[test]
fn the_human_grants_replaces_and_revokes_a_monitor_grant() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let id = open_sh(&mut h);
    // No grant: the field is left out.
    assert!(call(&mut h, "session.get", json!({ "id": id })).get("monitor_grant").is_none());

    let until = time::format_unix(time::now_unix() + 3600);
    let s = call(&mut h, "session.monitor_grant", json!({ "id": id, "scope": "prompts", "until": until }));
    assert_eq!((s["monitor_grant"]["scope"].clone(), s["monitor_grant"]["until"].clone()), (json!("prompts"), json!(until)));
    assert!(time::parse_rfc3339(s["monitor_grant"]["granted_at"].as_str().unwrap()).is_some());
    // It shows on the terminal in session.get and session.list.
    assert_eq!(call(&mut h, "session.get", json!({ "id": id }))["monitor_grant"], s["monitor_grant"]);
    assert_eq!(call(&mut h, "session.list", json!({}))[0]["monitor_grant"], s["monitor_grant"]);

    // Granting again replaces it; scope defaults to all and no until lasts until revoked.
    let s = call(&mut h, "session.monitor_grant", json!({ "id": id }));
    assert_eq!(s["monitor_grant"]["scope"], "all");
    assert!(s["monitor_grant"].get("until").is_none());
    let granted = events(&mut h, "session.monitor_granted");
    assert_eq!(granted.len(), 2);
    assert_eq!((granted[0]["data"]["replaced"].clone(), granted[1]["data"]["replaced"].clone()), (json!(false), json!(true)));
    assert_eq!((granted[1]["session_id"].as_str(), granted[1]["actor"]["kind"].as_str()), (Some(id.as_str()), Some("human")));

    // Revoke ends it and says which grant ended; revoking again changes nothing.
    let s = call(&mut h, "session.monitor_revoke", json!({ "id": id }));
    assert!(s.get("monitor_grant").is_none());
    call(&mut h, "session.monitor_revoke", json!({ "id": id }));
    let revoked = events(&mut h, "session.monitor_revoked");
    assert_eq!(revoked.len(), 1);
    assert_eq!(revoked[0]["data"]["monitor_grant"]["scope"], "all");

    // A past or unreadable until, or a terminal that isn't there, is refused.
    let past = time::format_unix(time::now_unix() - 60);
    assert_eq!(call_err(&mut h, "session.monitor_grant", json!({ "id": id, "until": past })).code, -32602);
    assert_eq!(call_err(&mut h, "session.monitor_grant", json!({ "id": id, "until": "2am" })).code, -32602);
    assert_eq!(call_err(&mut h, "session.monitor_grant", json!({ "id": id, "scope": "everything" })).code, -32602);
    assert!(call_err(&mut h, "session.monitor_grant", json!({ "id": "nope" })).message.contains("no session nope"));
    assert!(call(&mut h, "session.get", json!({ "id": id })).get("monitor_grant").is_none());
}

#[test]
fn an_agent_asking_for_a_grant_asks_the_human() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let id = open_sh(&mut h);
    let mut a = d.agent(Some(&id));

    // Its own terminal or another: refused, with a needs-you for the human, and nothing changes.
    let e = call_err(&mut a, "session.monitor_grant", json!({ "id": id, "scope": "approvals" }));
    assert_eq!(e.code, HUMAN_ONLY);
    let nid = e.data.unwrap()["needs_you_id"].as_str().unwrap().to_string();
    assert!(call(&mut h, "session.get", json!({ "id": id })).get("monitor_grant").is_none());
    let item = call(&mut h, "needs_you.get", json!({ "id": nid }));
    assert!(item["item"]["detail"].as_str().unwrap().contains("session.monitor_grant"), "{item}");
    // Agents can't approve it themselves, even with approve.from_cli on.
    call(&mut h, "settings.set", json!({ "key": "approve.from_cli", "value": true }));
    assert_eq!(call_err(&mut a, "needs_you.resolve", json!({ "id": nid, "resolution": { "kind": "approve", "scope": { "kind": "once" } } })).code, HUMAN_ONLY);
    assert!(events(&mut h, "session.monitor_granted").is_empty());

    // The human approving it gives the grant.
    approve(&mut h, &nid);
    assert_eq!(call(&mut h, "session.get", json!({ "id": id }))["monitor_grant"]["scope"], "approvals");

    // Revoking is human only too.
    let e = call_err(&mut a, "session.monitor_revoke", json!({ "id": id }));
    assert_eq!(e.code, HUMAN_ONLY);
    assert!(call(&mut h, "session.get", json!({ "id": id }))["monitor_grant"].is_object());

    // An approval for a terminal that closed since it was asked does nothing.
    let other = open_sh(&mut h);
    let e = call_err(&mut a, "session.monitor_grant", json!({ "id": other }));
    let nid = e.data.unwrap()["needs_you_id"].as_str().unwrap().to_string();
    call(&mut h, "session.close", json!({ "id": other, "force": true }));
    let res = call_err(&mut h, "needs_you.resolve", json!({ "id": nid, "resolution": { "kind": "approve", "scope": { "kind": "once" } } }));
    assert_eq!(res.code, CONFLICT, "{}", res.message);
}

#[test]
fn a_grant_ends_by_itself_at_its_until() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let ending = open_sh(&mut h);
    let lasting = open_sh(&mut h);
    let open_ended = open_sh(&mut h);
    let later = time::format_unix(time::now_unix() + 3600);
    for sid in [&ending, &lasting] {
        call(&mut h, "session.monitor_grant", json!({ "id": sid, "until": later }));
    }
    call(&mut h, "session.monitor_grant", json!({ "id": open_ended }));
    // Its until passes.
    {
        let dm = d.daemon();
        let mut core = dm.core();
        let g = core.state.session_mut(&ending).unwrap().monitor_grant.as_mut().unwrap();
        g.until = Some(time::format_unix(time::now_unix() - 1));
    }
    midnad::rpc::needs_you::sweep(&d.daemon());
    assert!(call(&mut h, "session.get", json!({ "id": ending })).get("monitor_grant").is_none());
    assert!(call(&mut h, "session.get", json!({ "id": lasting }))["monitor_grant"].is_object());
    assert!(call(&mut h, "session.get", json!({ "id": open_ended }))["monitor_grant"].is_object());
    let expired = events(&mut h, "session.monitor_expired");
    assert_eq!(expired.len(), 1);
    assert_eq!((expired[0]["session_id"].as_str(), expired[0]["actor"]["kind"].as_str()), (Some(ending.as_str()), Some("system")));
    assert!(events(&mut h, "session.monitor_revoked").is_empty());
    // The next sweep has nothing left to end.
    midnad::rpc::needs_you::sweep(&d.daemon());
    assert_eq!(events(&mut h, "session.monitor_expired").len(), 1);
}
