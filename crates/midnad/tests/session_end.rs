//! Why a terminal's agent ended. Closing a terminal drops it before the dying agent's
//! SessionEnd hook arrives; that hook is still accepted and `agent.session_ended` says who
//! ended it and how, next to the agent's own reason.
mod common;
use common::*;
use serde_json::{Value, json};

fn hook(d: &TestDaemon, sid: &str, event: &str, reason: &str) -> Value {
    call(&mut d.agent(Some(sid)), "agent.hook", json!({ "agent": "claude", "event": event, "payload": { "session_id": "c1", "reason": reason } }))
}

fn last(h: &mut midna_proto::Client, kind: &str, sid: &str) -> Value {
    let ev = call(h, "events.list", json!({ "filter": { "kinds": [kind], "session_id": sid } }));
    ev.as_array().unwrap().last().cloned().unwrap_or_else(|| panic!("no {kind} for {sid}: {ev}"))
}

#[test]
fn session_end_after_a_human_closes_the_terminal_records_why() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    call(&mut h, "session.close", json!({ "id": sid }));

    let r = hook(&d, &sid, "SessionEnd", "other");
    assert_eq!(r["ok"], true, "{r}");
    // Other final hooks of the dying agent are accepted too.
    assert_eq!(hook(&d, &sid, "Stop", "")["ok"], true);

    let closed = last(&mut h, "session.closed", &sid);
    assert_eq!((closed["data"]["how"].as_str(), closed["actor"]["kind"].as_str()), (Some("close"), Some("human")), "{closed}");
    let end = last(&mut h, "agent.session_ended", &sid);
    assert!(end["seq"].as_u64() > closed["seq"].as_u64());
    let data = &end["data"];
    assert_eq!(data["reason"], "other", "{end}");
    assert_eq!(data["how"], "close");
    assert_eq!(data["by"]["kind"], "human");
    assert_eq!(data["terminal_closed"], true);
    assert_eq!(data["conversation"], "c1");
    assert_eq!(end["actor"]["kind"], "agent");

    // The audit trail logs the hooks as ok, not 'no session'.
    let audit = call(&mut h, "events.list", json!({ "filter": { "kinds": ["audit"] } }));
    let hooks: Vec<&Value> = audit.as_array().unwrap().iter().filter(|e| e["data"]["method"] == "agent.hook").collect();
    assert_eq!(hooks.len(), 2, "{audit}");
    assert!(hooks.iter().all(|e| e["data"]["outcome"] == "ok"), "{audit}");
}

#[test]
fn an_agent_force_closing_its_own_terminal_is_recorded() {
    let d = TestDaemon::start();
    let mut h = d.human();
    call(&mut h, "settings.set", json!({ "key": "agents.may_force_close", "value": true }));
    let sid = open_sh(&mut h);
    call(&mut d.agent(Some(&sid)), "session.close", json!({ "id": sid, "force": true }));
    hook(&d, &sid, "SessionEnd", "other");

    let closed = last(&mut h, "session.closed", &sid);
    assert_eq!((closed["data"]["how"].as_str(), closed["data"]["force"].as_bool()), (Some("force_close"), Some(true)), "{closed}");
    let end = last(&mut h, "agent.session_ended", &sid);
    assert_eq!((end["data"]["how"].as_str(), end["data"]["by"]["kind"].as_str()), (Some("force_close"), Some("agent")), "{end}");
    assert_eq!(end["data"]["by"]["session"].as_str(), Some(sid.as_str()));
}

#[test]
fn an_agent_ending_on_its_own_is_recorded_with_its_reason() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    hook(&d, &sid, "SessionEnd", "prompt_input_exit");
    let end = last(&mut h, "agent.session_ended", &sid);
    let data = &end["data"];
    assert_eq!((data["how"].as_str(), data["reason"].as_str()), (Some("agent_exit"), Some("prompt_input_exit")), "{end}");
    assert_eq!((data["by"]["kind"].as_str(), data["terminal_closed"].as_bool()), (Some("agent"), Some(false)));
}

#[test]
fn a_hook_for_a_terminal_that_never_existed_still_fails() {
    let d = TestDaemon::start();
    let e = call_err(&mut d.agent(Some("nope")), "agent.hook", json!({ "agent": "claude", "event": "SessionEnd", "payload": {} }));
    assert!(e.message.contains("no session nope"), "{e:?}");
}

#[test]
fn a_restart_is_why_the_old_agent_ended_but_only_once() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    call(&mut h, "session.restart", json!({ "id": sid }));
    hook(&d, &sid, "SessionEnd", "other");
    let end = last(&mut h, "agent.session_ended", &sid);
    assert_eq!((end["data"]["how"].as_str(), end["data"]["by"]["kind"].as_str()), (Some("restart"), Some("human")), "{end}");
    assert_eq!(end["data"]["terminal_closed"], false);
    // The restarted agent's own end is its own.
    hook(&d, &sid, "SessionEnd", "prompt_input_exit");
    assert_eq!(last(&mut h, "agent.session_ended", &sid)["data"]["how"], "agent_exit");
}
