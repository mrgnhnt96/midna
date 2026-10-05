//! Local triggers: hooks, midna events and idle terminals firing `send_to_session`,
//! `set_status` and `clear_status`; refused prompts (`agent.prompt_blocked`) and the built-in
//! "Prompt blocked" status.
mod common;
use common::*;
use midna_proto::Client;
use serde_json::{Value, json};
use std::io::Write;

fn hook(a: &mut Client, ev: &str, payload: Value) {
    call(a, "agent.hook", json!({ "agent": "claude", "event": ev, "payload": payload }));
}

fn session(h: &mut Client, sid: &str) -> Value {
    call(h, "session.get", json!({ "id": sid }))
}

fn custom_label(h: &mut Client, sid: &str) -> Option<String> {
    session(h, sid)["custom_status"]["label"].as_str().map(str::to_string)
}

fn events(h: &mut Client, kind: &str) -> Vec<Value> {
    call(h, "events.list", json!({ "filter": { "kinds": [kind] }, "limit": 100 })).as_array().cloned().unwrap_or_default()
}

const BLOCKED: &str = "UserPromptSubmit operation blocked by hook:\n[/x/claude-usage-guard]: Compact first: 92k tokens and the cache is cold";

#[test]
fn refused_prompt_gets_the_builtin_status_and_auto_compact_resends_it() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let mut a = d.agent(Some(&sid));
    // The built-in status trigger is there from the start.
    let list = call(&mut h, "trigger.list", json!({}));
    let builtin = list.as_array().unwrap().iter().find(|t| t["builtin"] == "prompt_blocked_status").expect("built-in trigger").clone();
    assert_eq!((builtin["enabled"].as_bool(), builtin["source"].as_str()), (Some(true), Some("local")));

    // An agent adds and enables the auto-compact trigger itself (the human asked it to).
    let t = call(
        &mut a,
        "trigger.add",
        json!({
            "name": "Compact and resend", "source": "local", "event": "agent.prompt_blocked",
            "filter": { "match": { "message": "*compact first*" } },
            "action": { "kind": "send_to_session", "steps": [{ "text": "echo step-one" }, { "text": "{{last_prompt}}" }] },
            "enabled": true
        }),
    );
    assert_eq!((t["enabled"].as_bool(), t["state"].as_str()), (Some(true), Some("active")), "{t}");
    assert!(call(&mut h, "needs_you.list", json!({})).as_array().unwrap().is_empty(), "local triggers need no secret");

    let transcript = d.home.join("transcript.jsonl");
    std::fs::write(&transcript, "").unwrap();
    hook(&mut a, "SessionStart", json!({ "session_id": "c1", "transcript_path": transcript }));
    std::thread::sleep(std::time::Duration::from_millis(1200));
    let line = json!({ "type": "system", "subtype": "informational", "level": "warning", "content": BLOCKED, "timestamp": midna_proto::time::now_rfc3339() });
    std::fs::OpenOptions::new().append(true).open(&transcript).unwrap().write_all(format!("{line}\n").as_bytes()).unwrap();
    let prompt = "echo resent-line-one\necho resent-line-two";
    hook(&mut a, "UserPromptSubmit", json!({ "session_id": "c1", "prompt": prompt }));

    // The refusal ends the turn midna opened and is announced with the whole prompt.
    let blocked = wait_for(5, "agent.prompt_blocked", || events(&mut h, "agent.prompt_blocked").pop());
    assert_eq!(blocked["data"]["hook"], "/x/claude-usage-guard");
    assert_eq!(blocked["data"]["message"], "Compact first: 92k tokens and the cache is cold");
    assert_eq!(blocked["data"]["prompt"], prompt);
    let ended = events(&mut h, "agent.turn_ended");
    assert_eq!(ended.last().unwrap()["data"]["reason"], "prompt blocked");

    // The built-in trigger labels the terminal and puts it in Needs You.
    wait_for(5, "custom status", || custom_label(&mut h, &sid).filter(|l| l == "Prompt blocked"));
    let s = session(&mut h, &sid);
    assert_eq!((s["status"]["state"].as_str(), s["custom_status"]["color"].as_str()), (Some("needs_you"), Some("amber")), "{s}");
    let items = call(&mut h, "needs_you.list", json!({}));
    let item = items.as_array().unwrap().iter().find(|n| n["kind"] == "blocked").expect("needs-you item").clone();
    assert_eq!(item["title"], "Prompt blocked");
    assert!(item["detail"].as_str().unwrap().starts_with("Compact first"), "{item}");

    // The auto-compact trigger types both steps, the second being the refused prompt.
    let screen = wait_for(20, "both steps typed", || Some(read(&mut h, &sid)).filter(|t| t.contains("resent-line-two")));
    let (one, two) = (screen.find("step-one").unwrap(), screen.find("resent-line-one").unwrap());
    assert!(one < two, "steps in order:\n{screen}");
    let fired: Vec<Value> = call(&mut h, "trigger.deliveries", json!({ "trigger_id": t["id"] })).as_array().cloned().unwrap();
    assert_eq!(fired.len(), 1, "{fired:?}");
    assert_eq!((fired[0]["source"].as_str(), fired[0]["summary"].as_str()), (Some("local"), Some("Queued 2 steps for sh")), "{:?}", fired[0]);

    // The next prompt clears the status and its needs-you item.
    hook(&mut a, "UserPromptSubmit", json!({ "session_id": "c1", "prompt": "carry on" }));
    wait_for(5, "status cleared", || custom_label(&mut h, &sid).is_none().then_some(()));
    assert!(!call(&mut h, "needs_you.list", json!({})).as_array().unwrap().iter().any(|n| n["kind"] == "blocked"));
    let changes = events(&mut h, "session.custom_status");
    assert_eq!(changes.last().unwrap()["data"]["cleared"], "prompt");
}

#[test]
fn hook_triggers_statuses_clear_rules_and_validation() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let mut a = d.agent(Some(&sid));
    let add = |c: &mut Client, v: Value| call(c, "trigger.add", v);
    add(
        &mut a,
        json!({
            "name": "Waiting", "source": "local", "event": "hook.Notification",
            "filter": { "match": { "notification_type": "idle_prompt" }, "agent": "claude" },
            "action": { "kind": "set_status", "label": "Waiting on {{session.name}}", "color": "#3366ff", "base": "done", "clear_on": "turn" },
            "enabled": true, "cooldown_secs": 0
        }),
    );
    hook(&mut a, "Notification", json!({ "notification_type": "permission_prompt", "message": "x" }));
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(custom_label(&mut h, &sid), None, "match said no");
    hook(&mut a, "Notification", json!({ "notification_type": "idle_prompt" }));
    wait_for(5, "label", || custom_label(&mut h, &sid).filter(|l| l == "Waiting on sh"));
    assert_eq!(session(&mut h, &sid)["status"]["state"], "done");
    assert!(call(&mut h, "needs_you.list", json!({})).as_array().unwrap().is_empty(), "a done base raises nothing");
    hook(&mut a, "UserPromptSubmit", json!({ "prompt": "go" }));
    wait_for(5, "cleared on the turn", || custom_label(&mut h, &sid).is_none().then_some(()));

    // A matching event can clear a status that only clears that way.
    add(&mut h, json!({ "name": "Hold", "source": "local", "event": "hook.PreCompact", "action": { "kind": "set_status", "label": "Compacting", "color": "teal", "base": "working", "clear_on": "never" }, "enabled": true }));
    add(&mut h, json!({ "name": "Unhold", "source": "local", "event": "hook.SessionStart", "filter": { "match": { "source": "compact" } }, "action": { "kind": "clear_status" }, "enabled": true }));
    hook(&mut a, "PreCompact", json!({}));
    wait_for(5, "held", || custom_label(&mut h, &sid).filter(|l| l == "Compacting"));
    hook(&mut a, "Stop", json!({}));
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(custom_label(&mut h, &sid).as_deref(), Some("Compacting"), "never: survives status changes");
    hook(&mut a, "SessionStart", json!({ "source": "compact" }));
    wait_for(5, "unheld", || custom_label(&mut h, &sid).is_none().then_some(()));

    // Dry run: evaluation line and what it would do, nothing set.
    let t = add(&mut h, json!({ "name": "Dry", "source": "local", "event": "hook.Stop", "filter": { "match": { "last_assistant_message": "*done*" } }, "action": { "kind": "send_to_session", "steps": [{ "text": "/compact" }] } }));
    let r = call(&mut h, "trigger.test", json!({ "trigger_id": t["id"], "session": sid, "payload": { "last_assistant_message": "All done." } }));
    assert_eq!(r["summary"], "Matches, but the trigger isn't enabled · would send on sh: “/compact”", "{r}");
    let r = call(&mut h, "trigger.test", json!({ "trigger_id": t["id"], "payload": { "last_assistant_message": "nope" } }));
    assert!(r["summary"].as_str().unwrap().starts_with("Doesn't match"), "{r}");

    // Agents enable and pause local triggers without asking.
    call(&mut a, "trigger.set_enabled", json!({ "id": t["id"], "enabled": true }));
    call(&mut a, "trigger.set_enabled", json!({ "id": t["id"], "enabled": false }));

    // Validation.
    let e = call_err(&mut a, "trigger.add", json!({ "name": "x", "source": "github", "event": "push", "action": { "kind": "clear_status" } }));
    assert!(e.message.contains("needs source local"), "{}", e.message);
    let e = call_err(&mut a, "trigger.add", json!({ "name": "x", "source": "local", "event": "idle", "action": { "kind": "clear_status" } }));
    assert!(e.message.contains("idle_minutes"), "{}", e.message);
    let e = call_err(&mut a, "trigger.add", json!({ "name": "x", "source": "local", "event": "hook.Stop", "action": { "kind": "set_status", "label": "x", "color": "chartreuse", "base": "done" } }));
    assert!(e.message.contains("color must be"), "{}", e.message);
    let e = call_err(&mut a, "trigger.add", json!({ "name": "x", "source": "github", "event": "push", "action": { "kind": "attention", "message": "m" }, "enabled": true }));
    assert!(e.message.contains("only local triggers"), "{}", e.message);
}

#[test]
fn idle_trigger_fires_once_per_idle_stretch() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let mut a = d.agent(Some(&sid));
    let t = call(
        &mut h,
        "trigger.add",
        json!({
            "name": "Keep warm", "source": "local", "event": "idle", "filter": { "idle_minutes": 55, "session": sid },
            "action": { "kind": "send_to_session", "steps": [{ "text": "echo warm-{{idle_minutes}}" }] }, "enabled": true
        }),
    );
    let fired = |h: &mut Client| call(h, "trigger.deliveries", json!({ "trigger_id": t["id"] })).as_array().unwrap().len();
    // No turn yet: no idle clock, nothing to keep warm.
    midnad::local::backdate_activity(&d.daemon(), "nobody", 0);
    assert_eq!(fired(&mut h), 0);
    hook(&mut a, "UserPromptSubmit", json!({ "prompt": "hi" }));
    hook(&mut a, "Stop", json!({}));
    std::thread::sleep(std::time::Duration::from_millis(300));
    midnad::local::backdate_activity(&d.daemon(), &sid, 30 * 60);
    assert_eq!(fired(&mut h), 0, "not idle long enough");
    midnad::local::backdate_activity(&d.daemon(), &sid, 56 * 60);
    assert_eq!(fired(&mut h), 1);
    wait_for(10, "keep-warm typed", || Some(read(&mut h, &sid)).filter(|t| t.contains("warm-56")));
    midnad::local::backdate_activity(&d.daemon(), &sid, 56 * 60);
    assert_eq!(fired(&mut h), 1, "once per idle stretch");
    // A new turn restarts the clock; the next stretch fires again.
    hook(&mut a, "UserPromptSubmit", json!({ "prompt": "again" }));
    hook(&mut a, "Stop", json!({}));
    std::thread::sleep(std::time::Duration::from_millis(300));
    midnad::local::backdate_activity(&d.daemon(), &sid, 60 * 60);
    assert_eq!(fired(&mut h), 2);
}

#[test]
fn notify_action_posts_in_its_own_category() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let mut a = d.agent(Some(&sid));
    let t = call(
        &mut a,
        "trigger.add",
        json!({
            "name": "Tell me", "source": "local", "event": "agent.prompt_blocked",
            "action": { "kind": "notify", "title": "{{session.name}}: prompt blocked", "body": "{{message}}" }, "enabled": true
        }),
    );
    // Dry run first: nothing posted.
    let r = call(&mut h, "trigger.test", json!({ "trigger_id": t["id"], "session": sid, "payload": { "message": "Compact first" } }));
    assert_eq!(r["summary"], "Matches · would notify on sh: sh: prompt blocked — Compact first", "{r}");
    assert!(events(&mut h, "notify.posted").is_empty());

    let transcript = d.home.join("transcript.jsonl");
    let line = json!({ "type": "system", "subtype": "informational", "level": "warning", "content": BLOCKED, "timestamp": midna_proto::time::now_rfc3339() });
    std::fs::write(&transcript, format!("{line}\n")).unwrap();
    hook(&mut a, "UserPromptSubmit", json!({ "session_id": "c1", "transcript_path": transcript, "prompt": "big prompt" }));
    let posted = wait_for(5, "notification", || events(&mut h, "notify.posted").into_iter().find(|e| e["data"]["category"] == "from_trigger"));
    assert_eq!(posted["data"]["body"], "sh: prompt blocked\nCompact first: 92k tokens and the cache is cold", "{posted}");
    assert_eq!(posted["session_id"], json!(sid));
    let fired = call(&mut h, "trigger.deliveries", json!({ "trigger_id": t["id"] }));
    assert_eq!(fired[0]["summary"], "Notified “sh: prompt blocked”");

    // Its own switch: off means nothing is posted, and the firing says why.
    call(&mut h, "settings.set", json!({ "key": "notify.from_trigger", "value": false }));
    call(&mut h, "trigger.update", json!({ "id": t["id"], "cooldown_secs": 0 }));
    std::fs::OpenOptions::new().append(true).open(&transcript).unwrap().write_all(format!("{line}\n").as_bytes()).unwrap();
    hook(&mut a, "UserPromptSubmit", json!({ "session_id": "c1", "prompt": "again" }));
    let last = wait_for(5, "second firing", || call(&mut h, "trigger.deliveries", json!({ "trigger_id": t["id"] })).as_array().filter(|a| a.len() == 2).map(|a| a[1].clone()));
    assert!(last["summary"].as_str().unwrap().starts_with("Notification not shown"), "{last}");

    // Webhook triggers can notify too; an empty title is refused.
    let e = call_err(&mut a, "trigger.add", json!({ "name": "x", "source": "local", "event": "hook.Stop", "action": { "kind": "notify", "title": " " } }));
    assert!(e.message.contains("notify needs a title"), "{}", e.message);
    call(&mut a, "trigger.add", json!({ "name": "PR opened", "source": "github", "event": "pull_request.opened", "action": { "kind": "notify", "title": "PR #{{pr.number}}" } }));
}

#[test]
fn schedule_triggers_validate_fire_per_terminal_and_dry_run() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let mut a = d.agent(Some(&sid));
    let add_err = |a: &mut Client, v: Value| call_err(a, "trigger.add", v).message;
    let send = json!({ "kind": "send_to_session", "steps": [{ "text": "echo tick-{{local_time}}" }] });
    let e = add_err(&mut a, json!({ "name": "x", "source": "local", "event": "schedule", "action": send }));
    assert!(e.contains("need filter.cron"), "{e}");
    let e = add_err(&mut a, json!({ "name": "x", "source": "local", "event": "schedule", "filter": { "cron": "0 25 * * *", "session": sid }, "action": send }));
    assert!(e.contains("hour `25`: 25 is outside 0-23"), "{e}");
    let e = add_err(&mut a, json!({ "name": "x", "source": "local", "event": "schedule", "filter": { "cron": "@hourly" }, "action": send }));
    assert!(e.contains("needs filter.session"), "{e}");
    let e = add_err(&mut a, json!({ "name": "x", "source": "local", "event": "hook.Stop", "filter": { "cron": "@hourly" }, "action": send }));
    assert!(e.contains("event schedule"), "{e}");

    // Typed into one terminal on a schedule.
    let t = call(
        &mut a,
        "trigger.add",
        json!({ "name": "Tick", "source": "local", "event": "schedule", "filter": { "cron": "30 9 * * mon-fri", "session": sid }, "action": send, "enabled": true }),
    );
    let r = call(&mut h, "trigger.test", json!({ "trigger_id": t["id"], "session": sid }));
    let summary = r["summary"].as_str().unwrap();
    assert!(summary.starts_with("Matches · would send on sh: “echo tick-09:30” · next: "), "{summary}");
    assert_eq!(summary.matches(" 09:30").count(), 3, "three upcoming runs: {summary}");
    let at = midna_proto::cron::Cron::parse("30 9 * * mon-fri").unwrap().next_after(midna_proto::time::now_unix()).unwrap();
    midnad::local::fire_schedule_now(&d.daemon(), t["id"].as_str().unwrap(), at);
    wait_for(10, "scheduled send typed", || Some(read(&mut h, &sid)).filter(|t| t.contains("tick-09:30")));
    let del = call(&mut h, "trigger.deliveries", json!({ "trigger_id": t["id"] }));
    assert_eq!((del[0]["event"].as_str(), del[0]["subject"].as_str()), (Some("schedule"), Some("sh")), "{del}");

    // No terminal filter: fires once, about no terminal.
    let n = call(
        &mut a,
        "trigger.add",
        json!({ "name": "Standup", "source": "local", "event": "schedule", "filter": { "cron": "@daily" }, "action": { "kind": "attention", "message": "Daily at {{local_time}}" }, "enabled": true }),
    );
    let at = midna_proto::cron::Cron::parse("@daily").unwrap().next_after(midna_proto::time::now_unix()).unwrap();
    midnad::local::fire_schedule_now(&d.daemon(), n["id"].as_str().unwrap(), at);
    let items = call(&mut h, "needs_you.list", json!({}));
    let item = items.as_array().unwrap().iter().find(|i| i["title"] == "Daily at 00:00").unwrap_or_else(|| panic!("{items}"));
    assert!(item["session_id"].is_null(), "{item}");
}
