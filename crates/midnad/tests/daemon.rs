//! Integration tests: a real daemon (in-process) over its Unix socket.
mod common;
use common::*;
use midna_proto::error::{CONFLICT, HUMAN_ONLY, REFUSED};
use midna_proto::{Client, Event, Frame};
use serde_json::{Value, json};
use std::time::Duration;

#[test]
fn shell_session_input_read_close() {
    let d = TestDaemon::start();
    let mut c = d.human();
    let id = open_sh(&mut c);
    assert_eq!(id.len(), 8);
    let s = call(&mut c, "session.get", json!({ "id": id }));
    assert_eq!(s["status"]["state"], "idle");
    assert!(s["pid"].as_i64().unwrap() > 0);
    call(&mut c, "session.input", json!({ "id": id, "text": "echo midna-$((40+2))", "enter": true }));
    wait_for(5, "echo output", || read(&mut c, &id).contains("midna-42").then_some(()));
    // screen read returns exactly `rows` lines
    let scr = call(&mut c, "session.read", json!({ "id": id, "screen": true }));
    assert_eq!(scr["text"].as_str().unwrap().split('\n').count(), scr["rows"].as_u64().unwrap() as usize);
    call(&mut c, "session.rename", json!({ "id": id, "name": "build" }));
    call(&mut c, "session.close", json!({ "id": id }));
    let list = call(&mut c, "session.list", json!({}));
    assert!(list.as_array().unwrap().is_empty());
    let kinds: Vec<String> = call(&mut c, "events.list", json!({}))
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap().to_string())
        .collect();
    for k in ["daemon.started", "project.added", "session.opened", "session.renamed", "session.closed", "audit"] {
        assert!(kinds.contains(&k.to_string()), "missing {k} in {kinds:?}");
    }
}

#[test]
fn exit_codes_set_status() {
    let d = TestDaemon::start();
    let mut c = d.human();
    let ok = call(&mut c, "session.open", json!({ "kind": "shell", "cwd": "/tmp", "command": ["/bin/sh", "-c", "exit 0"] }));
    let bad = call(&mut c, "session.open", json!({ "kind": "monitor", "cwd": "/tmp", "command": ["/bin/sh", "-c", "echo boom; exit 3"] }));
    let state = |c: &mut Client, id: &Value| call(c, "session.get", json!({ "id": id }))["status"].clone();
    wait_for(5, "exit 0", || (state(&mut c, &ok["id"])["state"] == "exited").then_some(()));
    let st = wait_for(5, "exit 3", || Some(state(&mut c, &bad["id"])).filter(|s| s["state"] == "failed"));
    assert_eq!(st["exit_code"], 3);
    // A failed monitor raises a needs-you item with a screen excerpt.
    let items = call(&mut c, "needs_you.list", json!({}));
    let item = items.as_array().unwrap().iter().find(|n| n["kind"] == "failed").expect("failed item").clone();
    assert!(item["screen_excerpt"].to_string().contains("boom"));
    // Restart via the needs-you resolution brings it back (same id, new process).
    call(&mut c, "needs_you.resolve", json!({ "id": item["id"], "resolution": { "kind": "restart" } }));
    assert!(call(&mut c, "needs_you.list", json!({})).as_array().unwrap().iter().all(|n| n["kind"] != "failed" || n["id"] != item["id"]));
}

#[test]
fn osc_title_is_tracked() {
    let d = TestDaemon::start();
    let mut c = d.human();
    let id = open_sh(&mut c);
    call(&mut c, "session.input", json!({ "id": id, "text": "printf '\\033]0;hello title\\007'", "enter": true }));
    wait_for(5, "title", || (call(&mut c, "session.get", json!({ "id": id }))["title"] == "hello title").then_some(()));
    let ev = call(&mut c, "events.list", json!({ "filter": { "kinds": ["session.title"] } }));
    assert_eq!(ev[0]["data"]["title"], "hello title");
}

#[test]
fn events_replay_and_live() {
    let d = TestDaemon::start();
    let mut c = d.human();
    call(&mut c, "settings.set", json!({ "key": "theme", "value": "dark" }));
    call(&mut c, "settings.set", json!({ "key": "density", "value": "compact" }));
    let all: Vec<Event> = serde_json::from_value(call(&mut c, "events.list", json!({}))).unwrap();
    assert!(all.windows(2).all(|w| w[0].seq < w[1].seq), "seq must increase");
    let since = all[0].seq;
    let mut sub = d.human().subscribe(Some(since)).unwrap();
    let replayed: Vec<Event> = (0..all.len() - 1).map(|_| sub.next().unwrap().unwrap()).collect();
    assert_eq!(replayed, all[1..].to_vec());
    // live
    call(&mut c, "settings.set", json!({ "key": "theme", "value": "light" }));
    let mut live = vec![];
    while !live.iter().any(|e: &Event| e.kind == "settings.changed") {
        live.push(sub.next().unwrap().unwrap());
    }
    assert!(live[0].seq > all.last().unwrap().seq);
    // filtered list
    let only = call(&mut c, "events.list", json!({ "filter": { "kinds": ["settings."] } }));
    assert_eq!(only.as_array().unwrap().len(), 3);
}

#[test]
fn rules_agents_add_humans_remove() {
    let d = TestDaemon::start();
    let mut agent = d.agent(None);
    let mut human = d.human();
    let r = call(&mut agent, "rule.add", json!({ "effect": "deny", "matcher": { "kind": "command", "pattern": "git push --force*" } }));
    assert_eq!(r["added_by"]["kind"], "agent");
    let rid = r["id"].as_str().unwrap();
    let e = call_err(&mut agent, "rule.remove", json!({ "id": rid }));
    assert_eq!(e.code, HUMAN_ONLY);
    assert_eq!(call(&mut human, "rule.list", json!({})).as_array().unwrap().len(), 1);
    // Request removal -> needs-you item; rule shows the pending request.
    let n = call(&mut agent, "rule.request_removal", json!({ "id": rid, "reason": "too strict" }));
    assert_eq!(n["kind"], "rule_removal");
    let rules = call(&mut human, "rule.list", json!({}));
    assert_eq!(rules[0]["removal_request"]["needs_you_id"], n["id"]);
    // The agent can't approve it; the human can, which removes the rule.
    assert_eq!(call_err(&mut agent, "needs_you.resolve", json!({ "id": n["id"], "resolution": { "kind": "approve", "scope": { "kind": "once" } } })).code, HUMAN_ONLY);
    call(&mut human, "needs_you.resolve", json!({ "id": n["id"], "resolution": { "kind": "approve", "scope": { "kind": "once" } } }));
    assert!(call(&mut human, "rule.list", json!({})).as_array().unwrap().is_empty());
    assert!(call(&mut human, "needs_you.list", json!({})).as_array().unwrap().is_empty());
    // Audit records the denial.
    let audits = call(&mut human, "events.list", json!({ "filter": { "kinds": ["audit"] } }));
    assert!(audits.as_array().unwrap().iter().any(|a| a["data"]["method"] == "rule.remove" && a["data"]["outcome"] == "denied"));
}

#[test]
fn rules_expire() {
    let d = TestDaemon::start();
    let mut h = d.human();
    call(&mut h, "rule.add", json!({ "effect": "deny", "matcher": { "kind": "command", "pattern": "x" }, "expires_in_secs": 1 }));
    std::thread::sleep(Duration::from_millis(1100));
    assert!(call(&mut h, "rule.list", json!({})).as_array().unwrap().is_empty());
    assert_eq!(call(&mut h, "events.list", json!({ "filter": { "kinds": ["rule.expired"] } })).as_array().unwrap().len(), 1);
}

#[test]
fn policy_check_precedence() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let p = call(&mut h, "project.add", json!({ "path": "/tmp" }));
    let pid = p["id"].as_str().unwrap();
    let sid = open_sh(&mut h);
    let add = |h: &mut Client, effect: &str, pattern: &str, scope: Value| {
        call(h, "rule.add", json!({ "effect": effect, "matcher": { "kind": "command", "pattern": pattern }, "scope": scope }))["id"].as_str().unwrap().to_string()
    };
    let g = add(&mut h, "deny", "git push*", json!({ "kind": "global" }));
    let pa = add(&mut h, "allow", "git push*", json!({ "kind": "project", "id": pid }));
    let pk = add(&mut h, "ask", "git push --force*", json!({ "kind": "project", "id": pid }));
    let check = |h: &mut Client, value: &str, session: Option<&str>, project: Option<&str>| {
        call(h, "policy.check", json!({ "action": { "kind": "command", "value": value, "session": session, "project": project } }))
    };
    // global only
    let r = check(&mut h, "git push origin", None, None);
    assert_eq!((r["decision"].as_str(), r["rule"]["id"].as_str()), (Some("deny"), Some(g.as_str())));
    // project beats global
    let r = check(&mut h, "git push origin", None, Some(pid));
    assert_eq!((r["decision"].as_str(), r["rule"]["id"].as_str()), (Some("allow"), Some(pa.as_str())));
    // within project, ask beats allow
    let r = check(&mut h, "git push --force", None, Some(pid));
    assert_eq!((r["decision"].as_str(), r["rule"]["id"].as_str()), (Some("ask"), Some(pk.as_str())));
    assert_eq!(r["trace"].as_array().unwrap().len(), 3);
    // session beats project (the session's project is filled in automatically)
    let s = add(&mut h, "allow", "*", json!({ "kind": "session", "id": sid }));
    let r = check(&mut h, "git push --force", Some(&sid), None);
    assert_eq!((r["decision"].as_str(), r["rule"]["id"].as_str()), (Some("allow"), Some(s.as_str())));
    // no match -> default (destructive cli verbs ask)
    assert_eq!(check(&mut h, "ls", None, None)["source"], "default");
    let r = call(&mut h, "policy.check", json!({ "action": { "kind": "cli", "value": "close --force abc" } }));
    assert_eq!((r["decision"].as_str(), r["source"].as_str()), (Some("ask"), Some("default")));
    // check has no side effects
    assert!(call(&mut h, "rule.list", json!({})).as_array().unwrap().iter().all(|r| r["fired"] == 0));
}

/// Block an agent's policy.request on a thread; return (join handle, needs-you item).
fn request_in_thread(d: &TestDaemon, h: &mut Client, sid: &str, value: &str) -> (std::thread::JoinHandle<Value>, Value) {
    let mut agent = d.agent(Some(sid));
    let v = value.to_string();
    let t = std::thread::spawn(move || {
        agent.call_value("policy.request", json!({ "action": { "kind": "command", "value": v }, "timeout_secs": 20 })).unwrap()
    });
    let item = wait_for(5, "approval item", || {
        call(h, "needs_you.list", json!({})).as_array().unwrap().iter().find(|n| n["kind"] == "approval").cloned()
    });
    (t, item)
}

#[test]
fn approval_scopes_create_rules() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let project = call(&mut h, "session.get", json!({ "id": sid }))["project_id"].clone();
    call(&mut h, "rule.add", json!({ "effect": "ask", "matcher": { "kind": "command", "pattern": "deploy*" } }));

    // "always" -> allow rule scoped to the project, with origin.
    let (t, item) = request_in_thread(&d, &mut h, &sid, "deploy prod");
    assert_eq!(item["approval"]["action"]["value"], "deploy prod");
    let res = call(&mut h, "needs_you.resolve", json!({ "id": item["id"], "resolution": { "kind": "approve", "scope": { "kind": "always" } } }));
    let r = t.join().unwrap();
    assert_eq!((r["decision"].as_str(), r["source"].as_str()), (Some("allow"), Some("human")));
    let rule = &res["rule"];
    assert_eq!(rule["effect"], "allow");
    assert_eq!(rule["scope"], json!({ "kind": "project", "id": project }));
    assert_eq!(rule["origin"]["needs_you_id"], item["id"]);
    assert_eq!(rule["origin"]["approval_scope"]["kind"], "always");
    // The same action is now allowed by that rule (project beats the global ask).
    let r = call(&mut h, "policy.check", json!({ "action": { "kind": "command", "value": "deploy prod", "session": sid } }));
    assert_eq!((r["decision"].as_str(), r["rule"]["id"].as_str()), (Some("allow"), rule["id"].as_str()));

    // "minutes" -> expiring session rule; "once" -> no rule; deny -> deny.
    let (t, item) = request_in_thread(&d, &mut h, &sid, "deploy staging");
    let res = call(&mut h, "needs_you.resolve", json!({ "id": item["id"], "resolution": { "kind": "approve", "scope": { "kind": "minutes", "minutes": 5 } } }));
    t.join().unwrap();
    assert_eq!(res["rule"]["scope"], json!({ "kind": "session", "id": sid }));
    assert!(res["rule"]["expires_at"].is_string());
    let (t, item) = request_in_thread(&d, &mut h, &sid, "deploy dev");
    let res = call(&mut h, "needs_you.resolve", json!({ "id": item["id"], "resolution": { "kind": "approve", "scope": { "kind": "once" } } }));
    assert_eq!(t.join().unwrap()["decision"], "allow");
    assert!(res.get("rule").is_none());
    let (t, item) = request_in_thread(&d, &mut h, &sid, "deploy qa");
    call(&mut h, "needs_you.resolve", json!({ "id": item["id"], "resolution": { "kind": "deny" } }));
    assert_eq!(t.join().unwrap()["decision"], "deny");
}

#[test]
fn policy_request_times_out_and_fires_rules() {
    let d = TestDaemon::start();
    let mut h = d.human();
    call(&mut h, "rule.add", json!({ "effect": "ask", "matcher": { "kind": "command", "pattern": "slow*" } }));
    let deny = call(&mut h, "rule.add", json!({ "effect": "deny", "matcher": { "kind": "command", "pattern": "rm -rf /*" } }));
    let mut a = d.agent(None);
    let r = call(&mut a, "policy.request", json!({ "action": { "kind": "command", "value": "slow thing" }, "timeout_secs": 1 }));
    assert_eq!((r["decision"].as_str(), r["source"].as_str()), (Some("ask"), Some("timeout")));
    assert!(call(&mut h, "needs_you.list", json!({})).as_array().unwrap().is_empty(), "timed-out approval is withdrawn");
    let r = call(&mut a, "policy.request", json!({ "action": { "kind": "command", "value": "rm -rf /tmp" } }));
    assert_eq!(r["decision"], "deny");
    let rules = call(&mut h, "rule.list", json!({}));
    let fired = rules.as_array().unwrap().iter().find(|r| r["id"] == deny["id"]).unwrap();
    assert_eq!(fired["fired"], 1);
}

#[test]
fn settings_human_only_enforced() {
    let d = TestDaemon::start();
    let mut a = d.agent(None);
    let mut h = d.human();
    let e = call_err(&mut a, "settings.set", json!({ "key": "agents.may_move_windows", "value": true }));
    assert_eq!(e.code, HUMAN_ONLY);
    let nid = e.data.unwrap()["needs_you_id"].as_str().unwrap().to_string();
    assert_eq!(call(&mut h, "settings.get", json!({ "key": "agents.may_move_windows" }))["value"], false);
    // Agents can read it and change normal settings (with coercion).
    assert_eq!(call(&mut a, "settings.get", json!({ "key": "agents.may_move_windows" }))["human_only"], true);
    assert_eq!(call(&mut a, "settings.set", json!({ "key": "webhooks.port", "value": "9000" }))["value"], 9000);
    // Any theme id is accepted (a custom theme file may come later); a malformed one is not.
    assert_eq!(call_err(&mut a, "settings.set", json!({ "key": "theme", "value": "Neon Lights!" })).code, -32602);
    assert_eq!(call_err(&mut a, "settings.set", json!({ "key": "theme.colors", "value": ["accent = pink"] })).code, -32602);
    // The human approving the confirmation applies it.
    call(&mut h, "needs_you.resolve", json!({ "id": nid, "resolution": { "kind": "approve", "scope": { "kind": "once" } } }));
    assert_eq!(call(&mut h, "settings.get", json!({ "key": "agents.may_move_windows" }))["value"], true);
    let list = call(&mut h, "settings.list", json!({}));
    let theme = list.as_array().unwrap().iter().find(|s| s["key"] == "theme").unwrap();
    assert_eq!(theme["cli"], "midna settings set theme system");
    call(&mut h, "settings.reset", json!({ "key": "agents.may_move_windows" }));
    assert_eq!(call(&mut h, "settings.get", json!({ "key": "agents.may_move_windows" }))["value"], false);
}

#[test]
fn discover_lists_every_method() {
    let d = TestDaemon::start();
    let doc = call(&mut d.agent(None), "rpc.discover", json!({}));
    let names: Vec<&str> = doc["methods"].as_array().unwrap().iter().map(|m| m["name"].as_str().unwrap()).collect();
    for m in midna_proto::catalog() {
        assert!(names.contains(&m.name), "missing {}", m.name);
    }
    assert_eq!(names.len(), midna_proto::catalog().len());
    assert_eq!(call_err(&mut d.human(), "no.such", json!({})).code, -32601);
    // An in-process daemon can't re-exec its host (tests/upgrade.rs covers the real binary).
    assert_eq!(call_err(&mut d.human(), "daemon.upgrade", json!({ "binary_path": "/x" })).code, midna_proto::error::CONFLICT);
}

#[test]
fn roles_from_peer_and_caller() {
    let d = TestDaemon::start();
    assert_eq!(call(&mut d.human(), "daemon.info", json!({}))["role"], "human");
    let info = call(&mut d.agent(Some("abcd1234")), "daemon.info", json!({}));
    assert_eq!((info["role"].as_str(), info["session"].as_str()), (Some("agent"), Some("abcd1234")));
}

#[test]
fn stream_attach_frames_roundtrip() {
    let d = TestDaemon::start();
    let mut c = d.human();
    let id = open_sh(&mut c);
    let mut st = Client::attach_stream(d.socket(), &id, 60, 20, 8, 16).unwrap();
    st.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    // Initial credit: the first frame is full and sized to the attach request.
    let f = st.next_frame().unwrap();
    assert!(f.full);
    assert_eq!((f.cols, f.rows), (60, 20));
    // Type through the stream, then pull frames with credit until the output shows up.
    st.input(b"echo streamed-$((6*7))\r").unwrap();
    let mut grid: Vec<String> = vec![String::new(); 20];
    let apply = |grid: &mut Vec<String>, f: &Frame| {
        for (y, r) in &f.changed {
            grid[*y as usize] = r.text();
        }
    };
    apply(&mut grid, &f);
    wait_for(5, "streamed frame", || {
        st.want().unwrap();
        let f = st.next_frame().unwrap();
        apply(&mut grid, &f);
        grid.iter().any(|l| l.contains("streamed-42")).then_some(())
    });
    // Resize over the stream.
    st.resize(40, 10, 8, 16).unwrap();
    let f = wait_for(5, "resized frame", || {
        st.want().unwrap();
        Some(st.next_frame().unwrap()).filter(|f| f.cols == 40)
    });
    assert_eq!(f.rows, 10);
    assert!(f.full);
    // Closing the session ends the stream.
    call(&mut c, "session.close", json!({ "id": id }));
    let _ = st.want();
    let end = wait_for(5, "stream end", || match st.next_frame() {
        Err(e) => Some(e),
        Ok(_) => {
            let _ = st.want();
            None
        }
    });
    assert_ne!(end.kind(), std::io::ErrorKind::WouldBlock);
}

#[test]
fn a_gui_size_undone_at_once_never_reaches_the_app() {
    let d = TestDaemon::start();
    let mut c = d.human();
    // A full-screen app that, like Claude Code, redraws only when the size it reads has changed:
    // its footer sits on the last row, and it prints each size it redraws for.
    let app = r#"import os,signal,sys,time
last = None
def draw(*_):
    global last
    n = os.get_terminal_size().lines
    if n != last:
        last = n
        sys.stdout.write(f"\x1b[2J\x1b[Hdrawn {n}\x1b[{n};1Hfooter")
        sys.stdout.flush()
sys.stdout.write("\x1b[?1049h")
draw()
signal.signal(signal.SIGWINCH, draw)
while True: time.sleep(1)"#;
    let s = call(&mut c, "session.open", json!({ "kind": "shell", "cwd": "/tmp", "command": ["python3", "-c", app] }));
    let id = s["id"].as_str().unwrap().to_string();
    let mut st = Client::attach_stream(d.socket(), &id, 60, 20, 8, 16).unwrap();
    let screen = |c: &mut Client| -> Vec<String> {
        let r = call(c, "session.read", json!({ "id": id, "screen": true }));
        r["text"].as_str().unwrap().split('\n').map(str::to_string).collect()
    };
    wait_for(5, "app drawn", || screen(&mut c).iter().any(|l| l.contains("drawn 20")).then_some(()));
    // A layout blip (a row taken and given back), faster than the app reacts: it reads 20 rows,
    // sees no change and doesn't redraw, so the engine mustn't have moved its rows.
    st.resize(60, 18, 8, 16).unwrap();
    st.resize(60, 20, 8, 16).unwrap();
    std::thread::sleep(Duration::from_millis(400));
    let rows = screen(&mut c);
    assert_eq!(rows.len(), 20);
    assert!(rows[0].contains("drawn 20") && rows[19].contains("footer"), "{rows:#?}");
    // A size that holds still does reach it.
    st.resize(60, 15, 8, 16).unwrap();
    wait_for(5, "settled size", || screen(&mut c).first().filter(|l| l.contains("drawn 15")).map(|_| ()));
}

#[test]
fn agent_hooks_drive_status_turns_cost_and_insights() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let mut a = d.agent(Some(&sid));
    let hook = |a: &mut Client, ev: &str, payload: Value| call(a, "agent.hook", json!({ "agent": "claude", "event": ev, "payload": payload }));
    let state = |h: &mut Client| call(h, "session.get", json!({ "id": sid }))["status"]["state"].as_str().unwrap().to_string();
    hook(&mut a, "SessionStart", json!({}));
    hook(&mut a, "UserPromptSubmit", json!({ "prompt": "fix the tests" }));
    assert_eq!(state(&mut h), "working");
    hook(&mut a, "Notification", json!({ "notification_type": "permission_prompt", "message": "Claude needs your permission to use Bash" }));
    assert_eq!(state(&mut h), "needs_you");
    let items = call(&mut h, "needs_you.list", json!({}));
    assert_eq!(items[0]["kind"], "permission_prompt");
    hook(&mut a, "PostToolUse", json!({ "tool_name": "Bash" }));
    assert_eq!(state(&mut h), "working");
    assert!(call(&mut h, "needs_you.list", json!({})).as_array().unwrap().is_empty(), "prompt auto-resolved");
    hook(&mut a, "statusline", json!({ "cost": { "total_cost_usd": 0.5 }, "model": { "display_name": "Opus" } }));
    hook(&mut a, "statusline", json!({ "cost": { "total_cost_usd": 0.75 } }));
    hook(&mut a, "statusline", json!({ "cost": { "total_cost_usd": 0.75 } }));
    hook(&mut a, "Stop", json!({}));
    assert_eq!(state(&mut h), "done");
    let kinds: Vec<String> = call(&mut h, "events.list", json!({ "filter": { "kinds": ["agent."] } }))
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(kinds, ["agent.prompt_submitted", "agent.turn_started", "agent.cost", "agent.cost", "agent.turn_ended"]);
    let s = call(&mut h, "insights.summary", json!({ "range": "today", "by": "terminal" }));
    assert_eq!((s["totals"]["turns"].as_i64(), s["totals"]["messages"].as_i64()), (Some(1), Some(1)));
    assert_eq!(s["totals"]["spend_usd"], 0.75);
    assert_eq!(s["rows"][0]["key"], sid.as_str());
    let act = call(&mut h, "insights.activity", json!({}));
    assert_eq!(act[0]["kind"], "session.status"); // newest first
}

#[test]
fn a_question_after_a_generic_notification_retitles_the_prompt() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let mut a = d.agent(Some(&sid));
    let hook = |a: &mut Client, ev: &str, payload: Value| call(a, "agent.hook", json!({ "agent": "claude", "event": ev, "payload": payload }));
    hook(&mut a, "UserPromptSubmit", json!({ "prompt": "pick a design" }));
    hook(&mut a, "Notification", json!({ "notification_type": "permission_prompt", "message": "Claude needs your permission" }));
    assert_eq!(call(&mut h, "needs_you.list", json!({}))[0]["title"], "Claude needs your permission");
    let ask = json!({ "tool_name": "AskUserQuestion", "tool_input": { "questions": [{ "question": "What should the bell open?", "header": "Bell opens", "options": [{ "label": "Popover", "description": "A small list" }, { "label": "Full screen", "description": "" }] }] } });
    hook(&mut a, "PermissionRequest", ask);
    let items = call(&mut h, "needs_you.list", json!({}));
    assert_eq!(items.as_array().unwrap().len(), 1, "same item, not a second one");
    assert_eq!(items[0]["title"], "question: What should the bell open?");
    assert_eq!(items[0]["question"], json!({ "text": "What should the bell open?", "header": "Bell opens", "multi_select": false, "options": [{ "label": "Popover", "description": "A small list" }, { "label": "Full screen", "description": "" }] }));
    let evs = call(&mut h, "events.list", json!({ "filter": { "kinds": ["needs_you.updated"] } }));
    assert_eq!(evs.as_array().unwrap().len(), 1);
    // Answered, then asked again with the PermissionRequest first: raised with the question.
    hook(&mut a, "PostToolUse", json!({ "tool_name": "AskUserQuestion" }));
    assert!(call(&mut h, "needs_you.list", json!({})).as_array().unwrap().is_empty());
    hook(&mut a, "PermissionRequest", json!({ "tool_name": "AskUserQuestion", "tool_input": { "questions": [{ "question": "Ship it?", "multiSelect": true }] } }));
    let items = call(&mut h, "needs_you.list", json!({}));
    assert_eq!(items[0]["question"]["text"], "Ship it?");
    assert_eq!(items[0]["question"]["multi_select"], true);
}

#[test]
fn window_commands_reach_gui_and_are_gated() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let mut gui = d.human().subscribe(None).unwrap();
    let mut a = d.agent(Some(&sid));
    // Moving windows needs the human-only setting.
    let e = call_err(&mut a, "window.command", json!({ "action": "keep_on_top", "target": sid, "value": true }));
    assert_eq!(e.code, REFUSED);
    let r = call(&mut a, "session.focus", json!({ "id": sid }));
    assert_eq!(r["delivered"], 1);
    let cmd = loop {
        if let midna_proto::client::Notification::WindowCommand(v) = gui.next_notification().unwrap() {
            break v;
        }
    };
    assert_eq!((cmd["action"].as_str(), cmd["target"].as_str()), (Some("front"), Some(sid.as_str())));
    // Human may keep on top; it is reflected in window.list.
    call(&mut h, "window.command", json!({ "action": "keep_on_top", "target": sid, "value": true }));
    let w = call(&mut h, "window.list", json!({}));
    assert_eq!(w["keep_on_top"], json!([sid]));
    assert_eq!(w["gui_connected"], true);
}

#[test]
fn agent_close_policy() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let mut a = d.agent(Some(&sid));
    call(&mut a, "agent.hook", json!({ "agent": "claude", "event": "UserPromptSubmit", "payload": {} }));
    // Working session: plain close is a conflict for agents.
    assert_eq!(call_err(&mut a, "session.close", json!({ "id": sid })).code, CONFLICT);
    // A deny rule on `close --force*` refuses the forced close.
    call(&mut h, "rule.add", json!({ "effect": "deny", "matcher": { "kind": "cli", "pattern": "close --force*" } }));
    assert_eq!(call_err(&mut a, "session.close", json!({ "id": sid, "force": true })).code, REFUSED);
    call(&mut h, "session.close", json!({ "id": sid })); // humans aren't gated
}

#[test]
fn needs_you_raise_and_agent_resolve_own() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let mut a = d.agent(Some(&sid));
    let n = call(&mut a, "needs_you.raise", json!({ "kind": "blocked", "message": "need the API key" }));
    assert_eq!((n["session_id"].as_str(), n["asked_by"]["kind"].as_str()), (Some(sid.as_str()), Some("agent")));
    assert_eq!(call_err(&mut a, "needs_you.resolve", json!({ "id": n["id"], "resolution": { "kind": "dismiss" } })).code, HUMAN_ONLY);
    call(&mut a, "needs_you.resolve", json!({ "id": n["id"], "resolution": { "kind": "done" } }));
    assert!(call(&mut h, "needs_you.list", json!({})).as_array().unwrap().is_empty());
    assert_eq!(call_err(&mut a, "needs_you.raise", json!({ "kind": "approval", "message": "x" })).code, -32602);
}

#[test]
fn needs_you_raise_replaces_the_terminals_earlier_ones() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let (sid, other) = (open_sh(&mut h), open_sh(&mut h));
    let mut a = d.agent(Some(&sid));
    let mut b = d.agent(Some(&other));
    call(&mut a, "needs_you.raise", json!({ "kind": "blocked", "message": "first" }));
    call(&mut b, "needs_you.raise", json!({ "kind": "note", "message": "elsewhere" }));
    call(&mut a, "needs_you.raise", json!({ "kind": "note", "message": "second" }));
    let titles = |h: &mut Client| -> Vec<String> {
        let mut t: Vec<String> = call(h, "needs_you.list", json!({})).as_array().unwrap().iter().map(|n| n["title"].as_str().unwrap().to_string()).collect();
        t.sort();
        t
    };
    assert_eq!(titles(&mut h), ["elsewhere", "second"]);
    // Off: they pile up.
    call(&mut h, "settings.set", json!({ "key": "needs_you.replace", "value": false }));
    call(&mut a, "needs_you.raise", json!({ "kind": "blocked", "message": "third" }));
    assert_eq!(titles(&mut h), ["elsewhere", "second", "third"]);
}

#[test]
fn script_run_builtins_and_custom() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    // /tmp is not a git repo: github renders nothing.
    let r = call(&mut h, "script.run", json!({ "session_id": sid, "slot": "header" }));
    assert_eq!(r["segments"], json!([]));
    let script = d.home.join("seg.sh");
    std::fs::write(&script, "#!/bin/sh\necho '[{\"text\":\"hi '$MIDNA_SLOT'\",\"tone\":\"ok\"}]'\n").unwrap();
    std::os::unix::fs::PermissionsExt::set_mode(&mut std::fs::metadata(&script).unwrap().permissions(), 0o755);
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    call(&mut h, "settings.set", json!({ "key": "ui.row.script", "value": script }));
    let r = call(&mut h, "script.run", json!({ "session_id": sid, "slot": "row" }));
    assert_eq!(r["segments"], json!([{ "text": "hi row", "tone": "ok" }]));
}

#[test]
fn script_parts_show_the_worktree_and_branch() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let git = |cwd: &std::path::Path, args: &[&str]| {
        let ok = std::process::Command::new("git").args(args).current_dir(cwd).env("GIT_CONFIG_GLOBAL", "/dev/null").output().unwrap().status.success();
        assert!(ok, "git {args:?}");
    };
    let main = d.home.join("repo");
    std::fs::create_dir_all(&main).unwrap();
    git(&main, &["init", "-q", "-b", "main"]);
    git(&main, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "x"]);
    git(&main, &["worktree", "add", "-q", "../repo-fix", "-b", "fix/login"]);
    let open = |h: &mut midna_proto::Client, cwd: &std::path::Path| {
        call(h, "session.open", json!({ "kind": "shell", "cwd": cwd, "command": ["/bin/sh"] }))["id"].as_str().unwrap().to_string()
    };
    let (in_main, in_wt) = (open(&mut h, &main), open(&mut h, &d.home.join("repo-fix")));
    // status bar default: worktree+branch; the main checkout has no worktree segment
    let r = call(&mut h, "script.run", json!({ "session_id": in_main, "slot": "status" }));
    assert_eq!(r["segments"], json!([{ "text": "main", "tone": "accent", "icon": "branch" }]));
    let r = call(&mut h, "script.run", json!({ "session_id": in_wt, "slot": "status" }));
    assert_eq!(r["segments"], json!([{ "text": "", "tone": "work", "icon": "worktree", "tooltip": "repo-fix" }, { "text": "fix/login", "tone": "accent", "icon": "branch" }]));
    // header default (github) leads with the worktree too
    let r = call(&mut h, "script.run", json!({ "session_id": in_wt, "slot": "header" }));
    assert_eq!(r["segments"][0]["icon"], "worktree");
    // agents may combine built-in parts but not point a slot at an executable
    let mut a = d.agent(Some(&in_wt));
    call(&mut a, "settings.set", json!({ "key": "ui.status.script", "value": "branch+worktree" }));
    let r = call(&mut h, "script.run", json!({ "session_id": in_wt, "slot": "status" }));
    assert_eq!(r["segments"][0]["text"], "fix/login");
    assert!(call_err(&mut a, "settings.set", json!({ "key": "ui.status.script", "value": "branch+/bin/date" })).code != 0);
}

#[test]
fn status_bar_script_items() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let script = d.home.join("ci.sh");
    std::fs::write(&script, "#!/bin/sh\necho '[{\"text\":\"CI\",\"tone\":\"ok\",\"icon\":\"check\",\"tooltip\":\"all green\",\"mono\":true}]'\n").unwrap();
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let path = script.to_str().unwrap();
    // only a path listed in ui.status.items runs
    let run = json!({ "session_id": sid, "slot": "status", "script": path });
    assert!(call_err(&mut h, "script.run", run.clone()).code != 0);
    // an agent can't add a script path; the human can
    let mut a = d.agent(Some(&sid));
    assert!(call_err(&mut a, "settings.set", json!({ "key": "ui.status.items", "value": ["daemon", path] })).code != 0);
    call(&mut h, "settings.set", json!({ "key": "ui.status.items", "value": ["daemon", path, "keys"] }));
    let r = call(&mut h, "script.run", run);
    assert_eq!(r["segments"], json!([{ "text": "CI", "tone": "ok", "icon": "check", "tooltip": "all green", "mono": true }]));
    // but it may reorder or hide items, keeping the human's path
    call(&mut a, "settings.set", json!({ "key": "ui.status.items", "value": [path, "daemon"] }));
    call(&mut a, "settings.set", json!({ "key": "ui.status.items", "value": ["keys"] }));
    call(&mut a, "settings.reset", json!({ "key": "ui.status.items" }));
}

#[test]
fn custom_header_buttons_run_and_click() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let script = d.home.join("deploy.sh");
    std::fs::write(&script, "#!/bin/sh\nif [ -n \"$MIDNA_CLICK\" ]; then echo '[{\"text\":\"Deployed\",\"icon\":\"check\"}]'; else echo '[{\"text\":\"Deploy\",\"icon\":\"play\"}]'; fi\n").unwrap();
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let path = script.to_str().unwrap();
    let look = json!({ "session_id": sid, "slot": "button", "script": path });
    let click = json!({ "session_id": sid, "script": path });
    assert!(call_err(&mut h, "script.run", look.clone()).code != 0);
    assert!(call_err(&mut h, "script.click", click.clone()).code != 0);
    let mut a = d.agent(Some(&sid));
    assert!(call_err(&mut a, "settings.set", json!({ "key": "ui.header.buttons", "value": [path, "ide"] })).code != 0);
    call(&mut h, "settings.set", json!({ "key": "ui.header.buttons", "value": ["links", path, "ide", "split"] }));
    assert_eq!(call(&mut h, "script.run", look)["segments"], json!([{ "text": "Deploy", "icon": "play" }]));
    assert_eq!(call(&mut h, "script.click", click.clone())["segments"], json!([{ "text": "Deployed", "icon": "check" }]));
    // clicking is the human's; an agent's click becomes a request
    assert!(call_err(&mut a, "script.click", click).code != 0);
    // agents may reorder and hide buttons, keeping the human's script
    call(&mut a, "settings.set", json!({ "key": "ui.header.buttons", "value": format!("ide, {path}, links") }));
    assert_eq!(call(&mut h, "settings.get", json!({ "key": "ui.header.buttons" }))["value"], json!(["ide", path, "links"]));
    assert!(call_err(&mut a, "settings.set", json!({ "key": "ui.header.buttons", "value": ["more"] })).code != 0);
}

#[test]
fn state_persists_across_restart() {
    let home = {
        let d = TestDaemon::start();
        let mut h = d.human();
        open_sh(&mut h);
        call(&mut h, "rule.add", json!({ "effect": "allow", "matcher": { "kind": "tool", "pattern": "Read(*)" } }));
        call(&mut h, "settings.set", json!({ "key": "theme", "value": "dark" }));
        // Keep the files: copy the home before the harness deletes it.
        let copy = std::path::PathBuf::from(format!("{}-copy", d.home.display()));
        std::thread::sleep(Duration::from_millis(400)); // saver interval
        let _ = std::fs::remove_dir_all(&copy);
        std::fs::create_dir_all(&copy).unwrap();
        for f in ["state.json", "events.jsonl"] {
            std::fs::copy(d.home.join(f), copy.join(f)).unwrap();
        }
        copy
    };
    let state: Value = serde_json::from_slice(&std::fs::read(home.join("state.json")).unwrap()).unwrap();
    assert_eq!(state["rules"].as_array().unwrap().len(), 1);
    assert_eq!(state["settings"]["theme"], "dark");
    // A new daemon on that home keeps rules/settings; old sessions are marked exited; seq continues.
    let mut cfg = midnad::Config::for_home(home.clone());
    cfg.app_path = Some(std::env::current_exe().unwrap().to_string_lossy().into_owned());
    let handle = midnad::start(cfg).unwrap();
    let mut h = Client::connect(home.join("midnad.sock")).unwrap();
    h.set_caller(None);
    assert_eq!(call(&mut h, "rule.list", json!({})).as_array().unwrap().len(), 1);
    assert_eq!(call(&mut h, "settings.get", json!({ "key": "theme" }))["value"], "dark");
    let s = call(&mut h, "session.list", json!({}));
    assert_eq!(s[0]["status"]["state"], "exited");
    let ev: Vec<Event> = serde_json::from_value(call(&mut h, "events.list", json!({ "limit": 10000 }))).unwrap();
    assert!(ev.windows(2).all(|w| w[1].seq == w[0].seq + 1));
    assert_eq!(ev.iter().filter(|e| e.kind == "daemon.started").count(), 2);
    handle.shutdown();
    let _ = std::fs::remove_dir_all(&home);
}

/// Structured keys over the stream and `session.key` are encoded for the app's keyboard mode;
/// wheel scrolling moves the viewport; a mouse drag selects and `session.selection` copies it.
#[test]
fn stream_keys_scroll_and_selection() {
    use midna_proto::frame::{ClientMsg, KeyAction, KeyMsg, MOD_CTRL, MouseAction, MouseMsg, ScrollKind, ScrollMsg};
    let d = TestDaemon::start();
    let mut c = d.human();
    let id = open_sh(&mut c);
    let mut st = Client::attach_stream(d.socket(), &id, 60, 10, 8, 16).unwrap();
    st.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut last = st.next_frame().unwrap();
    // A reader that pushes the kitty "disambiguate" flag, then dumps the next 7 bytes it gets.
    let probe = "printf '\\033[>1u'; stty raw -echo; dd bs=1 count=7 2>/dev/null | od -An -c; stty sane; printf '\\033[<u'\r";
    st.input(probe.as_bytes()).unwrap();
    std::thread::sleep(Duration::from_millis(400));
    st.send(&ClientMsg::Key(KeyMsg { action: KeyAction::Press, mods: MOD_CTRL, key: "a".into(), text: String::new() })).unwrap();
    wait_for(5, "kitty ctrl-a", || read(&mut c, &id).contains("[   9   7   ;   5   u").then_some(()));
    // Same through the RPC, in legacy mode now: ctrl-c is ETX.
    call(&mut c, "session.input", json!({ "id": id, "text": "stty raw -echo; dd bs=1 count=1 2>/dev/null | od -An -tx1; stty sane", "enter": true }));
    std::thread::sleep(Duration::from_millis(400));
    call(&mut c, "session.key", json!({ "id": id, "key": "ctrl-c" }));
    wait_for(5, "legacy ctrl-c", || read(&mut c, &id).contains(" 03").then_some(()));
    assert_eq!(call_err(&mut c, "session.key", json!({ "id": id, "key": "hyper-q" })).code, -32602);

    // Scrollback: fill it, scroll up with the wheel, then back to the bottom.
    call(&mut c, "session.input", json!({ "id": id, "text": "clear; i=0; while [ $i -lt 60 ]; do echo row-$i; i=$((i+1)); done", "enter": true }));
    let pump = |st: &mut midna_proto::client::AttachStream, last: &mut Frame| {
        st.want().unwrap();
        *last = st.next_frame().unwrap();
    };
    wait_for(5, "rows", || {
        pump(&mut st, &mut last);
        (last.ext.scroll_total >= 60).then_some(())
    });
    st.send(&ClientMsg::Scroll(ScrollMsg { kind: ScrollKind::Wheel, amount: -20, x: 0.0, y: 0.0, mods: 0 })).unwrap();
    wait_for(5, "scrolled", || {
        pump(&mut st, &mut last);
        (!last.ext.at_bottom()).then_some(())
    });
    call(&mut c, "session.scroll", json!({ "id": id, "to": "bottom" }));
    wait_for(5, "bottom", || {
        pump(&mut st, &mut last);
        last.ext.at_bottom().then_some(())
    });

    // Select "row-5" on screen by dragging over it, then read the selection back.
    call(&mut c, "session.input", json!({ "id": id, "text": "clear; echo pick-me-please", "enter": true }));
    let mut grid: Vec<String> = vec![String::new(); 10];
    let y = wait_for(5, "pick", || {
        pump(&mut st, &mut last);
        for (y, r) in &last.changed {
            grid[*y as usize] = r.text();
        }
        grid.iter().position(|l| l.starts_with("pick-me-please"))
    }) as f32;
    let m = |action, x| ClientMsg::Mouse(MouseMsg { action, button: 1, mods: 0, x, y: y + 0.5 });
    st.send(&m(MouseAction::Press, 0.1)).unwrap();
    st.send(&m(MouseAction::Motion, 6.9)).unwrap();
    st.send(&m(MouseAction::Release, 6.9)).unwrap();
    let sel = wait_for(5, "selection", || call(&mut c, "session.selection", json!({ "id": id }))["text"].as_str().map(str::to_string));
    assert_eq!(sel, "pick-me");
    wait_for(5, "selection in frame", || {
        pump(&mut st, &mut last);
        (last.ext.selection == vec![(y as u16, 0, 6)]).then_some(())
    });
    // `clear` ran first, so only the output line matches.
    let f = call(&mut c, "session.find", json!({ "id": id, "query": "please" }));
    assert_eq!((f["total"].as_u64(), f["index"].as_u64()), (Some(1), Some(1)));
    let f = call(&mut c, "session.find", json!({ "id": id, "query": "please", "backwards": true }));
    assert_eq!(f["index"].as_u64(), Some(1));
    assert_eq!(call(&mut c, "session.selection", json!({ "id": id }))["text"], "please");
    let link = call(&mut c, "session.link_at", json!({ "id": id, "col": 0, "row": y as u16 }));
    assert_eq!(link["kind"], "none");
}

#[test]
fn no_project_opens_at_root() {
    let d = TestDaemon::start();
    let mut c = d.human();
    let home = std::env::var("HOME").unwrap();
    let s = call(&mut c, "session.open", json!({ "kind": "shell", "command": ["/bin/sh"] }));
    assert_eq!(s["project_id"], "root");
    assert_eq!(s["cwd"], home.as_str());
    // No project is created for root, and "root" can be passed back explicitly.
    assert!(call(&mut c, "project.list", json!({})).as_array().unwrap().is_empty());
    let again = call(&mut c, "session.open", json!({ "kind": "shell", "project_id": "root", "command": ["/bin/sh"] }));
    assert_eq!(again["project_id"], "root");
    // An agent standing in a root terminal opens its siblings at root too.
    let sid = s["id"].as_str().unwrap();
    let mut a = d.agent(Some(sid));
    let child = call(&mut a, "session.open", json!({ "kind": "shell", "command": ["/bin/sh"] }));
    assert_eq!(child["project_id"], "root");
}

#[test]
fn project_can_be_named_instead_of_an_id() {
    let d = TestDaemon::start();
    let mut c = d.human();
    let base = std::fs::canonicalize(std::env::temp_dir()).unwrap().join(format!("midna-named-{}", std::process::id()));
    let (a, b) = (base.join("kass"), base.join("other"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let kass = call(&mut c, "project.add", json!({ "path": a, "name": "kass" }));
    let s = call(&mut c, "session.open", json!({ "kind": "shell", "project_id": "Kass", "command": ["/bin/sh"] }));
    assert_eq!(s["project_id"], kass["id"]);
    let listed = call(&mut c, "session.list", json!({ "project_id": "kass" }));
    assert_eq!(listed.as_array().unwrap().len(), 1);
    // An unknown name is refused, not an empty list; a shared name asks for an id.
    assert!(call_err(&mut c, "session.list", json!({ "project_id": "nope" })).message.contains("no project nope"));
    call(&mut c, "project.add", json!({ "path": b, "name": "kass" }));
    let e = call_err(&mut c, "session.open", json!({ "kind": "shell", "project_id": "kass", "command": ["/bin/sh"] }));
    assert!(e.message.contains("use an id"), "{}", e.message);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn slash_is_never_a_project() {
    let d = TestDaemon::start();
    let mut c = d.human();
    let dir = std::fs::canonicalize(std::env::temp_dir()).unwrap().join(format!("midna-rel-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let dir_s = dir.to_string_lossy().into_owned();
    let s = call(&mut c, "session.open", json!({ "kind": "shell", "cwd": dir_s, "command": ["/bin/sh"] }));
    let pid = s["project_id"].as_str().unwrap().to_string();
    // A relative cwd is the caller's terminal's, not the daemon's (which is / under launchd).
    let mut a = d.agent(Some(s["id"].as_str().unwrap()));
    let here = call(&mut a, "session.open", json!({ "kind": "shell", "cwd": ".", "command": ["/bin/sh"] }));
    assert_eq!((here["project_id"].as_str(), here["cwd"].as_str()), (Some(pid.as_str()), Some(dir_s.as_str())));
    let sub = call(&mut a, "session.open", json!({ "kind": "shell", "cwd": "sub", "command": ["/bin/sh"] }));
    assert_eq!(sub["cwd"], format!("{dir_s}/sub"));
    // With no terminal to be relative to, it's refused.
    assert!(call_err(&mut c, "session.open", json!({ "kind": "shell", "cwd": ".", "command": ["/bin/sh"] })).message.contains("relative"));
    // `/` opens at root instead of becoming a project, and can't be added as one.
    let slash = call(&mut a, "session.open", json!({ "kind": "shell", "cwd": "/", "command": ["/bin/sh"] }));
    assert_eq!((slash["project_id"].as_str(), slash["cwd"].as_str()), (Some("root"), Some("/")));
    call_err(&mut c, "project.add", json!({ "path": "/" }));
    let paths: Vec<String> = call(&mut c, "project.list", json!({})).as_array().unwrap().iter().map(|p| p["path"].as_str().unwrap().to_string()).collect();
    assert_eq!(paths, [dir_s.clone()]);
    // A `/` project an older daemon stored is dropped on load; its terminals move to root.
    let mut state = midnad::state::State::default();
    let mut old: midna_proto::Session = serde_json::from_value(slash.clone()).unwrap();
    old.project_id = "p_slash".into();
    state.sessions.push(old);
    state.projects.push(serde_json::from_value(json!({ "id": "p_slash", "name": "root", "path": "/", "order": 0, "commands": [] })).unwrap());
    let file = dir.join("state.json");
    state.save(&file).unwrap();
    let loaded = midnad::state::State::load(&file);
    assert!(loaded.projects.is_empty());
    assert_eq!(loaded.sessions[0].project_id, "root");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn background_terminals_open_and_move() {
    let d = TestDaemon::start();
    let mut c = d.human();
    let s = call(&mut c, "session.open", json!({ "kind": "shell", "command": ["/bin/sh"], "background": true }));
    let id = s["id"].as_str().unwrap().to_string();
    assert_eq!(s["background"], true);
    // Still a normal terminal: listed and readable.
    let list = call(&mut c, "session.list", json!({}));
    assert_eq!(list[0]["background"], true);
    call(&mut c, "session.input", json!({ "id": id, "text": "echo bg-$((40+2))", "enter": true }));
    wait_for(5, "echo output", || read(&mut c, &id).contains("bg-42").then_some(()));
    // Moving it back drops the field (it is only serialized when set) and logs one event.
    let s = call(&mut c, "session.set_background", json!({ "id": id, "background": false }));
    assert!(s.get("background").is_none());
    call(&mut c, "session.set_background", json!({ "id": id, "background": false }));
    let moves = call(&mut c, "events.list", json!({ "filter": { "kinds": ["session.background"] } }));
    assert_eq!(moves.as_array().unwrap().len(), 1);
    assert_eq!(moves[0]["data"]["background"], false);
    // Agents may move terminals too.
    let mut a = d.agent(Some(&id));
    assert_eq!(call(&mut a, "session.set_background", json!({ "id": id, "background": true }))["background"], true);
}

#[test]
fn close_on_exit_closes_a_clean_exit_and_keeps_a_failure() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let open = |h: &mut Client, script: &str| {
        let s = call(h, "session.open", json!({ "kind": "monitor", "cwd": "/tmp", "command": ["/bin/sh", "-c", script], "close_on_exit": true }));
        s["id"].as_str().unwrap().to_string()
    };
    let clean = open(&mut h, "sleep 0.3; exit 0");
    let failed = open(&mut h, "sleep 0.3; exit 3");
    let plain = call(&mut h, "session.open", json!({ "kind": "monitor", "cwd": "/tmp", "command": ["/bin/sh", "-c", "exit 0"] }))["id"].as_str().unwrap().to_string();
    wait_for(10, "clean exit to close", || h.call_value("session.get", json!({ "id": clean })).is_err().then_some(()));
    wait_for(10, "failure to be marked", || (call(&mut h, "session.get", json!({ "id": failed }))["status"]["state"] == "failed").then_some(()));
    wait_for(10, "plain exit to be marked", || (call(&mut h, "session.get", json!({ "id": plain }))["status"]["state"] == "exited").then_some(()));
    let closed = call(&mut h, "events.list", json!({ "filter": { "kinds": ["session.closed"] } }));
    assert!(closed.to_string().contains(&clean) && closed.to_string().contains("\"system\""), "{closed}");
}
