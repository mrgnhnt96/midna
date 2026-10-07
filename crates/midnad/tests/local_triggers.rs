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

#[test]
fn schedule_windows_starts_ends_and_run_limits() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let send = json!({ "kind": "send_to_session", "steps": [{ "text": "echo poll" }] });
    let add = |h: &mut Client, f: Value| call_err(h, "trigger.add", json!({ "name": "x", "source": "local", "event": "schedule", "filter": f, "action": send })).message;
    let e = add(&mut h, json!({ "cron": "*/5 * * * *", "session": sid, "window": { "from": "1pm", "until": "17:00" } }));
    assert!(e.contains("window.from"), "{e}");
    let e = add(&mut h, json!({ "cron": "*/5 * * * *", "session": sid, "starts_at": "2026-10-07T00:00:00Z", "ends_at": "2026-10-06T00:00:00Z" }));
    assert!(e.contains("after filter.starts_at"), "{e}");
    let e = add(&mut h, json!({ "cron": "*/5 * * * *", "session": sid, "max_runs": 0 }));
    assert!(e.contains("at least 1"), "{e}");
    let e = call_err(&mut h, "trigger.add", json!({ "name": "x", "source": "local", "event": "hook.Stop", "filter": { "max_runs": 2 }, "action": send })).message;
    assert!(e.contains("only apply to local triggers with event schedule"), "{e}");

    // A window and an end date round-trip, and the dry run's next runs fall inside them.
    let ends = midna_proto::time::format_unix(midna_proto::time::now_unix() + 30 * 86_400);
    let t = call(
        &mut h,
        "trigger.add",
        json!({ "name": "Poll", "source": "local", "event": "schedule", "filter": { "cron": "*/5 * * * *", "session": sid, "window": { "from": "13:00", "until": "17:00" }, "ends_at": ends, "max_runs": 3 }, "action": send, "enabled": true }),
    );
    assert_eq!((t["filter"]["window"]["from"].as_str(), t["filter"]["max_runs"].as_u64()), (Some("13:00"), Some(3)), "{t}");
    let r = call(&mut h, "trigger.test", json!({ "trigger_id": t["id"], "session": sid }));
    let summary = r["summary"].as_str().unwrap();
    let hours: Vec<&str> = summary.split(" · next: ").nth(1).unwrap().split(", ").map(|x| &x[x.len() - 5..x.len() - 3]).collect();
    assert!(hours.len() == 3 && hours.iter().all(|h| ["13", "14", "15", "16"].contains(h)), "{summary}");

    // Out of runs: never runs again. A new limit counts from then.
    let sched = midna_proto::cron::Schedule::of(&serde_json::from_value(t["filter"].clone()).unwrap(), 0).unwrap();
    let at = sched.upcoming(midna_proto::time::now_unix(), 1)[0];
    for _ in 0..3 {
        midnad::local::fire_schedule_now(&d.daemon(), t["id"].as_str().unwrap(), at);
    }
    let fired = |h: &mut Client| call(h, "trigger.list", json!({})).as_array().unwrap().iter().find(|x| x["id"] == t["id"]).map(|x| x["fired"].clone());
    wait_for(10, "three firings", || fired(&mut h).filter(|f| *f == 3));
    let r = call(&mut h, "trigger.test", json!({ "trigger_id": t["id"], "session": sid }));
    assert!(r["summary"].as_str().unwrap().ends_with("never runs again"), "{r}");
    let mut f = t["filter"].clone();
    f["max_runs"] = json!(5);
    let u = call(&mut h, "trigger.update", json!({ "id": t["id"], "filter": f }));
    assert_eq!(u["fired"], 0, "{u}");

    // `@every 55m` (which `*/55` isn't): first run one interval after it's added, then 55 apart.
    let e = add(&mut h, json!({ "cron": "@every 30s", "session": sid }));
    assert!(e.contains("whole minutes"), "{e}");
    let before = midna_proto::time::now_unix();
    let t = call(&mut h, "trigger.add", json!({ "name": "Nudge", "source": "local", "event": "schedule", "filter": { "cron": "@every 55m", "session": sid }, "action": send }));
    let starts = midna_proto::time::parse_rfc3339(t["filter"]["starts_at"].as_str().unwrap_or_else(|| panic!("{t}"))).unwrap();
    assert!(starts % 60 == 0 && (before + 55 * 60..=before + 57 * 60).contains(&starts), "{t}");
    let r = call(&mut h, "trigger.test", json!({ "trigger_id": t["id"], "session": sid }));
    let next = |i: i64| midna_proto::cron::local_label(starts + i * 55 * 60);
    assert!(r["summary"].as_str().unwrap().ends_with(&format!(" · next: {}, {}, {}", next(0), next(1), next(2))), "{r}");
    // Editing it keeps its pace.
    let u = call(&mut h, "trigger.update", json!({ "id": t["id"], "filter": t["filter"], "name": "Nudge 2" }));
    assert_eq!(u["filter"]["starts_at"], t["filter"]["starts_at"], "{u}");
}

/// Fire a no-terminal `@daily` schedule trigger now.
fn fire_daily(d: &TestDaemon, id: &Value) {
    let at = midna_proto::cron::Cron::parse("@daily").unwrap().next_after(midna_proto::time::now_unix()).unwrap();
    midnad::local::fire_schedule_now(&d.daemon(), id.as_str().unwrap(), at);
}

/// A trigger's latest delivery once its headless run has finished.
fn finished_run(h: &mut Client, trigger: &Value, secs: u64) -> Value {
    wait_for(secs, "headless run finished", || {
        let list = call(h, "trigger.deliveries", json!({ "trigger_id": trigger }));
        list.as_array().and_then(|a| a.first().cloned()).filter(|d| !d["command_runs"][0]["finished_at"].is_null())
    })
}

#[test]
fn run_command_in_background_or_with_no_terminal() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let dir = d.home.join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    let p = call(&mut h, "project.add", json!({ "path": dir }));
    let pid = p["id"].as_str().unwrap().to_string();
    let add = |h: &mut Client, name: &str, action: Value| {
        call(h, "trigger.add", json!({ "name": name, "source": "local", "event": "schedule", "filter": { "cron": "@daily" }, "action": action, "enabled": true }))
    };
    let run = |cmd: &str, extra: Value| {
        let mut a = json!({ "kind": "run_command", "project_id": pid, "command": cmd });
        a.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        a
    };

    // background and headless don't mix; a timeout is for headless runs.
    let bad = |h: &mut Client, action: Value| call_err(h, "trigger.add", json!({ "name": "x", "source": "local", "event": "hook.Stop", "action": action })).message;
    let e = bad(&mut h, run("true", json!({ "background": true, "headless": true })));
    assert!(e.contains("pick one"), "{e}");
    let e = bad(&mut h, run("true", json!({ "timeout_secs": 5 })));
    assert!(e.contains("timeout_secs is for headless"), "{e}");
    let e = bad(&mut h, run("true", json!({ "headless": true, "timeout_secs": 0 })));
    assert!(e.contains("timeout_secs must be"), "{e}");

    // Background: a monitor terminal in the Background group.
    let bg = add(&mut h, "Compaction", run("echo compacting; sleep 5", json!({ "background": true })));
    let r = call(&mut h, "trigger.test", json!({ "trigger_id": bg["id"] }));
    assert!(r["summary"].as_str().unwrap().starts_with("Matches · would run in Background: echo compacting"), "{r}");
    fire_daily(&d, &bg["id"]);
    let del = wait_for(5, "background firing", || call(&mut h, "trigger.deliveries", json!({ "trigger_id": bg["id"] })).as_array().and_then(|a| a.first().cloned()));
    assert_eq!(del["summary"], "Ran command in Background › Compaction", "{del}");
    let s = call(&mut h, "session.get", json!({ "id": del["session_started"] }));
    assert_eq!((s["kind"].as_str(), s["background"].as_bool(), s["project_id"].as_str()), (Some("monitor"), Some(true), Some(pid.as_str())), "{s}");

    // Headless: no terminal; the exit code and output end up on the delivery.
    let before = call(&mut h, "session.list", json!({})).as_array().unwrap().len();
    let hl = add(&mut h, "Housekeeping", run("echo from $MIDNA_TRIGGER in $(pwd); echo oops >&2; test -z \"$MIDNA_SESSION\" && exit 3", json!({ "headless": true })));
    let r = call(&mut h, "trigger.test", json!({ "trigger_id": hl["id"] }));
    assert!(r["summary"].as_str().unwrap().starts_with("Matches · would run with no terminal: "), "{r}");
    fire_daily(&d, &hl["id"]);
    let del = finished_run(&mut h, &hl["id"], 10);
    assert_eq!(del["summary"], "Running with no terminal › Housekeeping", "{del}");
    assert!(del["session_started"].is_null(), "{del}");
    let ran = &del["command_runs"][0];
    assert_eq!((ran["exit_code"].as_i64(), ran["trigger_id"].as_str()), (Some(3), hl["id"].as_str()), "{ran}");
    assert_eq!(ran["output"].as_str().unwrap(), format!("from {} in {}\noops", hl["id"].as_str().unwrap(), dir.display()), "{ran}");
    assert_eq!(call(&mut h, "session.list", json!({})).as_array().unwrap().len(), before, "no terminal opened");
    let done = wait_for(5, "command_finished event", || events(&mut h, "trigger.command_finished").pop());
    assert_eq!((done["data"]["delivery_id"].as_str(), done["data"]["run"]["exit_code"].as_i64()), (del["id"].as_str(), Some(3)), "{done}");

    // Past its timeout the run is stopped.
    let slow = add(&mut h, "Slow", run("echo started; sleep 30", json!({ "headless": true, "timeout_secs": 1 })));
    fire_daily(&d, &slow["id"]);
    let del = finished_run(&mut h, &slow["id"], 15);
    let ran = &del["command_runs"][0];
    assert_eq!((ran["timed_out"].as_bool(), ran["exit_code"].as_i64(), ran["output"].as_str()), (Some(true), None, Some("started")), "{ran}");
}
