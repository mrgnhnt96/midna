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

// ------------------------------------------------------------------ honoring the grant

/// A permission prompt pending on terminal `sid`, raised the way agent.hook does (as its agent).
fn raise_prompt(d: &TestDaemon, sid: &str, title: &str, question: bool) -> String {
    let daemon = d.daemon();
    let mut n = daemon.new_needs_you(midna_proto::NeedsYouKind::PermissionPrompt, title.into(), midna_proto::Actor::agent(Some(sid.into())), Some(sid.into()));
    if question {
        n.question = Some(midna_proto::NeedsYouQuestion { text: "Which one?".into(), ..Default::default() });
    }
    daemon.raise_needs_you(n).id
}

/// An approval item terminal `sid`'s agent is waiting on (a policy request a rule asks about).
fn raise_approval(d: &TestDaemon, h: &mut Client, sid: &str, what: &str) -> String {
    call(h, "rule.add", json!({ "effect": "ask", "matcher": { "kind": "command", "pattern": what } }));
    let e = call_err(&mut d.agent(Some(sid)).no_wait(), "policy.request", json!({ "action": { "kind": "command", "value": what }, "timeout_secs": 60 }));
    assert_eq!(e.code, midna_proto::error::PENDING, "{}", e.message);
    e.data.unwrap()["needs_you_id"].as_str().unwrap().to_string()
}

fn resolve(c: &mut Client, nid: &str, kind: &str) -> Result<Value, midna_proto::RpcError> {
    let res = if kind == "approve" { json!({ "kind": "approve", "scope": { "kind": "once" } }) } else { json!({ "kind": kind }) };
    c.call_value("needs_you.resolve", json!({ "id": nid, "resolution": res })).map_err(|e| match e {
        midna_proto::ClientError::Rpc(e) => e,
        e => panic!("{e}"),
    })
}

fn refused(r: Result<Value, midna_proto::RpcError>) -> String {
    let e = r.expect_err("refused");
    assert_eq!(e.code, HUMAN_ONLY, "{}", e.message);
    e.message
}

fn is_open(h: &mut Client, nid: &str) -> bool {
    call(h, "needs_you.get", json!({ "id": nid }))["state"] == "open"
}

/// The resolution `needs_you.resolved` recorded for item `nid`.
fn resolution(h: &mut Client, nid: &str) -> Value {
    let ev = events(h, "needs_you.resolved").into_iter().find(|e| e["data"]["id"] == nid).expect("resolved event");
    ev["data"]["resolution"].clone()
}

#[test]
fn a_granted_terminal_approves_and_denies_other_terminals_prompts_and_approvals() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let monitor = open_sh(&mut h);
    let other = open_sh(&mut h);
    let mut m = d.agent(Some(&monitor));
    call(&mut h, "session.monitor_grant", json!({ "id": monitor }));

    // Another terminal's permission prompt: approved, then a new one denied.
    let p1 = raise_prompt(&d, &other, "Bash(ls)", false);
    resolve(&mut m, &p1, "approve").unwrap();
    let r = resolution(&mut h, &p1);
    assert_eq!((r["kind"].as_str(), r["monitor_grant"]["session"].as_str(), r["monitor_grant"]["scope"].as_str()), (Some("approve"), Some(monitor.as_str()), Some("all")), "{r}");
    let g = call(&mut h, "needs_you.get", json!({ "id": p1 }));
    assert_eq!((g["resolved_by"]["session"].as_str(), g["resolution"]["monitor_grant"]["session"].as_str()), (Some(monitor.as_str()), Some(monitor.as_str())), "{g}");
    let p2 = raise_prompt(&d, &other, "Bash(rm x)", false);
    resolve(&mut m, &p2, "deny").unwrap();
    assert_eq!(resolution(&mut h, &p2)["kind"], "deny");
    assert_eq!(resolution(&mut h, &p2)["monitor_grant"]["session"], monitor.as_str());

    // Another terminal's approval item, with approve.from_cli off: approved once, or denied.
    let a1 = raise_approval(&d, &mut h, &other, "deploy staging");
    resolve(&mut m, &a1, "approve").unwrap();
    assert_eq!(call(&mut h, "needs_you.get", json!({ "id": a1 }))["state"], "resolved");
    assert_eq!(resolution(&mut h, &a1)["monitor_grant"]["session"], monitor.as_str());
    let a2 = raise_approval(&d, &mut h, &other, "deploy prod");
    resolve(&mut m, &a2, "deny").unwrap();
    assert_eq!(resolution(&mut h, &a2)["kind"], "deny");
    // Approving for longer would add a rule: the human's.
    let a3 = raise_approval(&d, &mut h, &other, "deploy qa");
    let e = m.call_value("needs_you.resolve", json!({ "id": a3, "resolution": { "kind": "approve", "scope": { "kind": "always" } } })).unwrap_err();
    assert!(e.to_string().contains("approves once"), "{e}");
    // Nor may it dismiss what it didn't raise.
    assert!(refused(resolve(&mut m, &a3, "dismiss")).contains("only approves or denies"));
    assert!(is_open(&mut h, &a3));

    // `midna key` on another terminal's prompt goes through, logged with the grant.
    raise_prompt(&d, &other, "Bash(ls -la)", false);
    call(&mut m, "session.key", json!({ "id": other, "key": "escape" }));
    let ev = events(&mut h, "session.input_by_agent").into_iter().rev().find(|e| e["data"]["key"] == "escape").expect("input event");
    assert_eq!((ev["session_id"].as_str(), ev["data"]["monitor_grant"]["session"].as_str()), (Some(other.as_str()), Some(monitor.as_str())), "{ev}");
    // Typing into a terminal with no prompt records no grant.
    call(&mut m, "session.input", json!({ "id": monitor, "text": "true" }));
    let ev = events(&mut h, "session.input_by_agent").into_iter().last().unwrap();
    assert!(ev["data"].get("monitor_grant").is_none(), "{ev}");
}

#[test]
fn a_grant_never_reaches_human_only_items_or_its_own_terminal() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let monitor = open_sh(&mut h);
    let other = open_sh(&mut h);
    let mut m = d.agent(Some(&monitor));
    call(&mut h, "session.monitor_grant", json!({ "id": monitor }));

    // A deferred human-only call, including a grant for another terminal (or itself).
    let mut o = d.agent(Some(&other));
    let e = call_err(&mut o, "session.monitor_grant", json!({ "id": other }));
    let grant_ask = e.data.unwrap()["needs_you_id"].as_str().unwrap().to_string();
    assert!(refused(resolve(&mut m, &grant_ask, "approve")).contains("human-only"));
    assert!(refused(resolve(&mut m, &grant_ask, "deny")).contains("human-only"));
    let e = call_err(&mut o, "session.monitor_revoke", json!({ "id": monitor }));
    let revoke_ask = e.data.unwrap()["needs_you_id"].as_str().unwrap().to_string();
    refused(resolve(&mut m, &revoke_ask, "deny"));
    // A rule removal.
    let r = call(&mut h, "rule.add", json!({ "effect": "deny", "matcher": { "kind": "command", "pattern": "rm -rf*" } }));
    let removal = call(&mut o, "rule.request_removal", json!({ "id": r["id"], "reason": "in my way" }))["id"].as_str().unwrap().to_string();
    refused(resolve(&mut m, &removal, "approve"));
    refused(resolve(&mut m, &removal, "deny"));
    // A folder-trust prompt and an agent's question.
    let trust = raise_prompt(&d, &other, "Trust this folder? /tmp/x", false);
    assert!(refused(resolve(&mut m, &trust, "approve")).contains("trusting a folder"));
    let question = raise_prompt(&d, &other, "Which one?", true);
    assert!(refused(resolve(&mut m, &question, "approve")).contains("question"));
    // Nor keys into a terminal showing a question or a folder-trust prompt.
    assert_eq!(call_err(&mut m, "session.key", json!({ "id": other, "key": "enter" })).code, HUMAN_ONLY);
    for nid in [&grant_ask, &revoke_ask, &removal, &trust, &question] {
        assert!(is_open(&mut h, nid), "{nid}");
    }
    assert!(call(&mut h, "session.get", json!({ "id": other })).get("monitor_grant").is_none());

    // Its own terminal's items follow the ordinary rules (approve.from_cli is off).
    let own = raise_approval(&d, &mut h, &monitor, "deploy own");
    assert!(refused(resolve(&mut m, &own, "approve")).contains("approve.from_cli"));
    raise_prompt(&d, &monitor, "Bash(ls)", false);
    assert_eq!(call_err(&mut m, "session.key", json!({ "id": monitor, "key": "enter" })).code, HUMAN_ONLY);
}

#[test]
fn without_a_live_grant_in_scope_everything_is_refused_as_before() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let monitor = open_sh(&mut h);
    let other = open_sh(&mut h);
    let mut m = d.agent(Some(&monitor));
    let prompt = raise_prompt(&d, &other, "Bash(ls)", false);
    let approval = raise_approval(&d, &mut h, &other, "deploy staging");
    let all_refused = |m: &mut Client, h: &mut Client| {
        refused(resolve(m, &prompt, "approve"));
        refused(resolve(m, &prompt, "deny"));
        refused(resolve(m, &approval, "approve"));
        refused(resolve(m, &approval, "deny"));
        assert_eq!(call_err(m, "session.key", json!({ "id": other, "key": "enter" })).code, HUMAN_ONLY);
        assert!(is_open(h, &prompt) && is_open(h, &approval));
    };
    // No grant.
    all_refused(&mut m, &mut h);

    // Out of scope: prompts doesn't reach approvals, approvals doesn't reach prompts.
    call(&mut h, "session.monitor_grant", json!({ "id": monitor, "scope": "prompts" }));
    assert!(refused(resolve(&mut m, &approval, "approve")).contains("doesn't cover approval items"));
    refused(resolve(&mut m, &approval, "deny"));
    call(&mut h, "session.monitor_grant", json!({ "id": monitor, "scope": "approvals" }));
    assert!(refused(resolve(&mut m, &prompt, "approve")).contains("doesn't cover permission prompts"));
    assert_eq!(call_err(&mut m, "session.key", json!({ "id": other, "key": "enter" })).code, HUMAN_ONLY);
    assert!(is_open(&mut h, &prompt) && is_open(&mut h, &approval));

    // Revoked.
    call(&mut h, "session.monitor_grant", json!({ "id": monitor }));
    call(&mut h, "session.monitor_revoke", json!({ "id": monitor }));
    all_refused(&mut m, &mut h);

    // Past its until, even before the sweep ends it.
    call(&mut h, "session.monitor_grant", json!({ "id": monitor, "until": time::format_unix(time::now_unix() + 3600) }));
    {
        let dm = d.daemon();
        let mut core = dm.core();
        core.state.session_mut(&monitor).unwrap().monitor_grant.as_mut().unwrap().until = Some(time::format_unix(time::now_unix() - 1));
    }
    all_refused(&mut m, &mut h);
    midnad::rpc::needs_you::sweep(&d.daemon());
    all_refused(&mut m, &mut h);

    // An agent outside any terminal has no grant to use.
    call(&mut h, "session.monitor_grant", json!({ "id": monitor }));
    refused(resolve(&mut d.agent(None), &prompt, "approve"));
    // With the grant back, the same calls go through.
    resolve(&mut m, &prompt, "deny").unwrap();
    resolve(&mut m, &approval, "deny").unwrap();
}

// ------------------------------------------------------------------ driving the monitor

/// An agent terminal (a shell marked as Claude's), with its queue paused so what
/// midna queues for it stays there to look at.
fn agent_tab(d: &TestDaemon, h: &mut Client) -> String {
    let sid = open_sh(h);
    d.daemon().core().state.session_mut(&sid).unwrap().agent = Some(midna_proto::AgentKind::Claude);
    call(h, "queue.pause", json!({ "session": sid }));
    sid
}

fn queued(h: &mut Client, sid: &str) -> Vec<String> {
    call(h, "queue.list", json!({ "session": sid }))["items"].as_array().unwrap().iter().map(|m| m["text"].as_str().unwrap().to_string()).collect()
}

#[test]
fn a_grant_briefs_its_agent_wakes_it_for_other_tabs_and_says_when_it_ends() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let monitor = agent_tab(&d, &mut h);
    let worker = agent_tab(&d, &mut h);
    let shell = open_sh(&mut h);
    let early = raise_prompt(&d, &worker, "Bash: rm -rf build", false);

    let until = time::format_unix(time::now_unix() + 3600);
    call(&mut h, "session.monitor_grant", json!({ "id": monitor, "until": until }));
    // A briefing, then a note with what already waits.
    let q = wait_for(5, "briefing and note", || Some(queued(&mut h, &monitor)).filter(|q| q.len() == 2));
    assert!(q[0].starts_with("[midna monitor] The human made this terminal the monitor") && q[0].contains("every 30s"), "{q:?}");
    assert!(q[1].starts_with(midnad::monitor::NOTE_HEAD) && q[1].contains(&format!("`{early}` permission prompt")), "{q:?}");

    // A new prompt joins the waiting note; a question never does.
    let later = raise_prompt(&d, &worker, "Bash: cargo publish", false);
    let asked = raise_prompt(&d, &worker, "Which one?", true);
    let q = wait_for(5, "joined", || Some(queued(&mut h, &monitor)).filter(|q| q.len() == 2 && q[1].contains(&later)));
    assert!(q[1].contains(&early) && !q[1].contains(&asked), "{q:?}");
    // Answered meanwhile: its line goes, and with nothing left, the note.
    approve(&mut h, &early);
    wait_for(5, "line gone", || Some(queued(&mut h, &monitor)).filter(|q| q.len() == 2 && !q[1].contains(&early)));
    approve(&mut h, &later);
    wait_for(5, "note gone", || Some(queued(&mut h, &monitor)).filter(|q| q.len() == 1));

    // A shell with a grant gets nothing typed into it.
    call(&mut h, "session.monitor_grant", json!({ "id": shell }));
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert!(queued(&mut h, &shell).is_empty());

    // Revoked: a note that it ended.
    call(&mut h, "session.monitor_revoke", json!({ "id": monitor }));
    let q = wait_for(5, "ended", || Some(queued(&mut h, &monitor)).filter(|q| q.len() == 2));
    assert!(q[1].contains("Your monitor grant ended (the human revoked it)"), "{q:?}");
}

#[test]
fn an_agent_left_idle_is_noted_once_per_idle_stretch() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let monitor = agent_tab(&d, &mut h);
    let worker = agent_tab(&d, &mut h);
    let mut w = d.agent(Some(&worker));
    call(&mut h, "session.monitor_grant", json!({ "id": monitor }));
    wait_for(5, "briefing", || Some(queued(&mut h, &monitor)).filter(|q| q.len() == 1));
    // Idle a minute ago.
    let backdate = |d: &TestDaemon| {
        let dm = d.daemon();
        dm.core().state.session_mut(&worker).unwrap().status.since = time::format_unix(time::now_unix() - 60);
    };
    call(&mut w, "agent.hook", json!({ "agent": "claude", "event": "UserPromptSubmit", "payload": { "prompt": "go" } }));
    call(&mut w, "agent.hook", json!({ "agent": "claude", "event": "Stop", "payload": {} }));
    backdate(&d);
    midnad::monitor::check(&d.daemon());
    let q = queued(&mut h, &monitor);
    assert!(q.len() == 2 && q[1].contains(&format!("({worker}) finished its turn at")) && q[1].contains(&format!("midna close {worker}")), "{q:?}");

    // The note goes out: the same idle stretch isn't noted again.
    let id = call(&mut h, "queue.list", json!({ "session": monitor }))["items"][1]["id"].as_str().unwrap().to_string();
    call(&mut h, "queue.remove", json!({ "session": monitor, "id": id }));
    midnad::monitor::check(&d.daemon());
    assert_eq!(queued(&mut h, &monitor).len(), 1);
    // It works and stops again: noted again.
    call(&mut w, "agent.hook", json!({ "agent": "claude", "event": "UserPromptSubmit", "payload": { "prompt": "more" } }));
    midnad::monitor::check(&d.daemon());
    call(&mut w, "agent.hook", json!({ "agent": "claude", "event": "Stop", "payload": {} }));
    backdate(&d);
    midnad::monitor::check(&d.daemon());
    assert_eq!(queued(&mut h, &monitor).len(), 2);
}

/// An agent in terminal `sid` asks for human-only `method`: the approval it raised.
fn ask_human_only(d: &TestDaemon, sid: &str, method: &str, params: Value) -> String {
    let e = call_err(&mut d.agent(Some(sid)), method, params);
    assert_eq!(e.code, HUMAN_ONLY, "{}", e.message);
    e.data.unwrap()["needs_you_id"].as_str().unwrap().to_string()
}

#[test]
fn with_approve_human_only_a_grant_approves_other_agents_human_only_asks_but_never_about_grants() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let monitor = open_sh(&mut h);
    let other = open_sh(&mut h);
    let mut m = d.agent(Some(&monitor));
    call(&mut h, "session.monitor_grant", json!({ "id": monitor }));

    // Off (the default): still the human's.
    let cli = ask_human_only(&d, &other, "settings.set", json!({ "key": "approve.from_cli", "value": true }));
    assert!(refused(resolve(&mut m, &cli, "approve")).contains("monitor.approve_human_only"));
    // Agents can't turn it on for themselves either.
    let on = ask_human_only(&d, &monitor, "settings.set", json!({ "key": "monitor.approve_human_only", "value": true }));
    assert!(is_open(&mut h, &on));

    call(&mut h, "settings.set", json!({ "key": "monitor.approve_human_only", "value": true }));
    resolve(&mut m, &cli, "approve").unwrap();
    assert_eq!(call(&mut h, "settings.get", json!({ "key": "approve.from_cli" }))["value"], true);
    assert_eq!(resolution(&mut h, &cli)["monitor_grant"]["session"], monitor.as_str());
    let force = ask_human_only(&d, &other, "settings.set", json!({ "key": "agents.may_force_close", "value": true }));
    resolve(&mut m, &force, "deny").unwrap();
    assert_eq!(call(&mut h, "settings.get", json!({ "key": "agents.may_force_close" }))["value"], false);

    // Never one about grants or monitor.* settings, nor its own ask.
    let grant = ask_human_only(&d, &other, "session.monitor_grant", json!({ "id": other }));
    let rules = ask_human_only(&d, &other, "settings.set", json!({ "key": "monitor.approve_human_only", "value": false }));
    for nid in [&grant, &rules] {
        assert!(refused(resolve(&mut m, nid, "approve")).contains("monitor grants or monitor.* settings"), "{nid}");
    }
    assert!(refused(resolve(&mut m, &on, "approve")).contains("human-only action"));
    // A prompts-only grant answers no approvals, human-only or not.
    call(&mut h, "session.monitor_grant", json!({ "id": monitor, "scope": "prompts" }));
    let again = ask_human_only(&d, &other, "settings.set", json!({ "key": "agents.may_force_close", "value": true }));
    assert!(refused(resolve(&mut m, &again, "approve")).contains("doesn't cover approval items"));
    for nid in [&grant, &rules, &on, &again] {
        assert!(is_open(&mut h, nid), "{nid}");
    }
}

#[test]
fn the_humans_rules_ride_in_the_briefing_and_every_note_and_human_only_asks_join_when_allowed() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let monitor = agent_tab(&d, &mut h);
    let worker = agent_tab(&d, &mut h);
    std::fs::write(d.home.join(midnad::monitor::RULES_FILE), "- Everything except Slack messages sent straight to a person.\n").unwrap();
    let ask = ask_human_only(&d, &worker, "settings.set", json!({ "key": "agents.may_force_close", "value": true }));
    let p = raise_prompt(&d, &worker, "Bash: cargo test", false);

    call(&mut h, "session.monitor_grant", json!({ "id": monitor }));
    let q = wait_for(5, "briefing and note", || Some(queued(&mut h, &monitor)).filter(|q| q.len() == 2));
    let rules = "The human's rules for you, ahead of everything else here (`midna monitor rules`):\n- Everything except Slack messages sent straight to a person.\nWhat they rule out";
    assert!(q[0].contains(rules) && q[0].contains("human-only actions, approving"), "{q:?}");
    // Off: the human-only ask isn't the monitor's to see.
    assert!(q[1].contains(rules) && q[1].contains(&p) && !q[1].contains(&ask), "{q:?}");

    // On: a fresh grant's briefing says so, and the ask joins the note.
    call(&mut h, "settings.set", json!({ "key": "monitor.approve_human_only", "value": true }));
    call(&mut h, "session.monitor_grant", json!({ "id": monitor }));
    let q = wait_for(5, "rebriefed", || Some(queued(&mut h, &monitor)).filter(|q| q.len() == 3 && q[2].contains(&ask)));
    assert!(q[1].contains("confirm another agent's human-only action") && q[2].contains(rules), "{q:?}");
}
