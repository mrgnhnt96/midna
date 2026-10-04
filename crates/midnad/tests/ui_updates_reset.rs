//! Rules feed defaults, rule.restore, ui.commands, window split, updates proxy, daemon.reset
//! and permissions.status against a real in-process daemon.
mod common;
use common::*;
use midna_proto::client::Notification;
use midna_proto::error::{CONFLICT, HUMAN_ONLY};
use serde_json::{Value, json};

fn kinds(h: &mut midna_proto::Client, prefix: &str) -> Vec<Value> {
    call(h, "events.list", json!({ "limit": 1000, "filter": { "kinds": [prefix] } })).as_array().unwrap().clone()
}

#[test]
fn default_decisions_are_logged_as_policy_decided() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let mut a = d.agent(Some(&sid));
    // No rule: the defaults table allows a command and has no opinion on a tool.
    let r = call(&mut a, "policy.request", json!({ "action": { "kind": "command", "value": "ls -la" } }));
    assert_eq!((r["decision"].as_str(), r["source"].as_str()), (Some("allow"), Some("default")));
    call(&mut a, "policy.request", json!({ "action": { "kind": "tool", "value": "Bash(ls)" } }));
    let ev = kinds(&mut h, "policy.decided");
    assert_eq!(ev.len(), 2, "{ev:?}");
    assert_eq!(ev[0]["data"]["action"]["value"], "ls -la");
    assert_eq!(ev[0]["data"]["decision"], "allow");
    assert_eq!(ev[0]["data"]["passthrough"], false);
    assert_eq!(ev[0]["session_id"], sid.as_str());
    assert_eq!(ev[1]["data"]["passthrough"], true);
    // A rule decision is a rule.fired, not a policy.decided.
    call(&mut h, "rule.add", json!({ "effect": "allow", "matcher": { "kind": "command", "pattern": "ls*" } }));
    call(&mut a, "policy.request", json!({ "action": { "kind": "command", "value": "ls" } }));
    assert_eq!(kinds(&mut h, "policy.decided").len(), 2);
    assert_eq!(kinds(&mut h, "rule.fired").len(), 1);
    // policy.check has no side effects.
    call(&mut a, "policy.check", json!({ "action": { "kind": "command", "value": "pwd" } }));
    assert_eq!(kinds(&mut h, "policy.decided").len(), 2);
}

#[test]
fn dismissing_a_removal_request_clears_it() {
    let d = TestDaemon::start();
    let (mut h, mut a) = (d.human(), d.agent(None));
    let r = call(&mut a, "rule.add", json!({ "effect": "deny", "matcher": { "kind": "command", "pattern": "rm -rf*" } }));
    let n = call(&mut a, "rule.request_removal", json!({ "id": r["id"], "reason": "in my way" }));
    assert!(!call(&mut h, "rule.list", json!({}))[0]["removal_request"].is_null());
    call(&mut h, "needs_you.resolve", json!({ "id": n["id"], "resolution": { "kind": "dismiss" } }));
    let rules = call(&mut h, "rule.list", json!({}));
    assert_eq!(rules.as_array().unwrap().len(), 1);
    assert!(rules[0]["removal_request"].is_null());
    // And the agent can ask again.
    let n2 = call(&mut a, "rule.request_removal", json!({ "id": r["id"], "reason": "again" }));
    assert_ne!(n2["id"], n["id"]);
}

#[test]
fn rule_restore_keeps_the_original_id() {
    let d = TestDaemon::start();
    let (mut h, mut a) = (d.human(), d.agent(None));
    let r = call(&mut a, "rule.add", json!({ "effect": "ask", "matcher": { "kind": "command", "pattern": "git push*" } }));
    call(&mut a, "policy.request", json!({ "action": { "kind": "command", "value": "git push" }, "timeout_secs": 1 }));
    let fired = call(&mut h, "rule.list", json!({}))[0].clone();
    assert_eq!(fired["fired"], 1);
    call(&mut h, "rule.remove", json!({ "id": r["id"] }));
    let removed = kinds(&mut h, "rule.removed")[0]["data"].clone();
    // Agents can't restore (it becomes a human approval).
    let e = call_err(&mut a, "rule.restore", json!({ "rule": removed }));
    assert_eq!(e.code, HUMAN_ONLY);
    let back = call(&mut h, "rule.restore", json!({ "rule": removed }));
    assert_eq!((back["id"].clone(), back["fired"].clone(), back["added_by"]["kind"].clone()), (r["id"].clone(), json!(1), json!("agent")));
    assert_eq!(call(&mut h, "rule.list", json!({})).as_array().unwrap().len(), 1);
    assert_eq!(kinds(&mut h, "rule.restored").len(), 1);
    // Twice is a conflict.
    assert_eq!(call_err(&mut h, "rule.restore", json!({ "rule": removed })).code, CONFLICT);
}

#[test]
fn ask_settings_exist_and_agents_may_write_them() {
    let d = TestDaemon::start();
    let mut a = d.agent(None);
    assert_eq!(call(&mut a, "settings.get", json!({ "key": "ui.ask.agent" }))["value"], "claude");
    assert_eq!(call(&mut a, "settings.get", json!({ "key": "ui.ask.scope" }))["value"], "project");
    call(&mut a, "settings.set", json!({ "key": "ui.ask.agent", "value": "codex" }));
    call(&mut a, "settings.set", json!({ "key": "ui.ask.scope", "value": "root" }));
    assert_eq!(call(&mut a, "settings.get", json!({ "key": "ui.ask.agent" }))["value"], "codex");
    assert_eq!(call_err(&mut a, "settings.set", json!({ "key": "ui.ask.scope", "value": "moon" })).code, -32602);
}

#[test]
fn ui_commands_add_list_remove_and_live_events() {
    let d = TestDaemon::start();
    let (mut h, mut a) = (d.human(), d.agent(None));
    let empty = call(&mut a, "ui.commands.list", json!({}));
    assert_eq!(empty["commands"], json!([]));
    let c = call(&mut a, "ui.commands.add", json!({ "command": {
        "title": "Deploy staging", "keywords": "ship",
        "run": { "kind": "rpc", "method": "session.open", "params": { "kind": "monitor", "command": ["./deploy.sh"] } } } }));
    assert_eq!(c["id"], "user:deploy-staging");
    assert_eq!(c["added_by"]["kind"], "agent");
    // The file is the backing store, in the app's format.
    let file: Value = serde_json::from_str(&std::fs::read_to_string(d.home.join("commands.json")).unwrap()).unwrap();
    assert_eq!(file[0]["title"], "Deploy staging");
    // Validation.
    let bad = |a: &mut midna_proto::Client, cmd: Value| call_err(a, "ui.commands.add", json!({ "command": cmd })).code;
    assert_eq!(bad(&mut a, json!({ "title": "x", "run": { "kind": "rpc", "method": "no.such" } })), -32602);
    assert_eq!(bad(&mut a, json!({ "title": "", "run": { "kind": "screen", "screen": "rules" } })), -32602);
    assert_eq!(bad(&mut a, json!({ "title": "x" })), -32602);
    assert_eq!(bad(&mut a, json!({ "title": "Deploy staging", "run": { "kind": "screen", "screen": "rules" } })), CONFLICT);
    call(&mut a, "ui.commands.add", json!({ "replace": true, "command": { "title": "Deploy staging", "run": { "kind": "screen", "screen": "rules" } } }));
    // Human-only methods get the two-step confirm.
    let stop = call(&mut a, "ui.commands.add", json!({ "command": { "title": "Stop midnad", "run": { "kind": "rpc", "method": "daemon.stop" } } }));
    assert!(stop["danger"].is_string() && stop["human_only"] == true);
    let list = call(&mut h, "ui.commands.list", json!({}));
    assert_eq!(list["commands"].as_array().unwrap().len(), 2);
    call(&mut a, "ui.commands.remove", json!({ "id": "user:stop-midnad" }));
    assert_eq!(call_err(&mut a, "ui.commands.remove", json!({ "id": "user:stop-midnad" })).code, 3);
    assert_eq!(kinds(&mut h, "ui.commands_changed").len(), 4);
    // A hand edit is noticed too (polled about once a second), and bad entries are reported.
    std::thread::sleep(std::time::Duration::from_millis(1100)); // mtime granularity
    std::fs::write(d.home.join("commands.json"), r#"[{"title":"Hand made","run":{"kind":"prefill","text":"hi"}},{"title":7}]"#).unwrap();
    wait_for(5, "external edit event", || (kinds(&mut h, "ui.commands_changed").len() == 5).then_some(()));
    let list = call(&mut h, "ui.commands.list", json!({}));
    assert_eq!(list["commands"][0]["id"], "user:hand-made");
    assert_eq!(list["invalid"].as_array().unwrap().len(), 1);
    // A file we can't parse is never overwritten.
    std::fs::write(d.home.join("commands.json"), "{oops").unwrap();
    assert_eq!(bad(&mut a, json!({ "title": "x", "run": { "kind": "screen", "screen": "rules" } })), CONFLICT);
}

#[test]
fn split_reaches_the_gui_without_the_move_setting() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let mut gui = d.human().subscribe(None).unwrap();
    let mut a = d.agent(Some(&sid));
    let r = call(&mut a, "window.command", json!({ "action": "split", "target": sid, "value": "stacked" }));
    assert_eq!(r["delivered"], 1);
    let cmd = loop {
        if let Notification::WindowCommand(v) = gui.next_notification().unwrap() {
            break v;
        }
    };
    assert_eq!((cmd["action"].as_str(), cmd["value"].as_str()), (Some("split"), Some("stacked")));
    assert_eq!(call_err(&mut a, "window.command", json!({ "action": "split", "target": sid, "value": "diagonal" })).code, -32602);
    assert_eq!(call_err(&mut a, "window.command", json!({ "action": "split" })).code, -32602);
    call(&mut a, "window.command", json!({ "action": "split", "value": "close" }));
    // A deny rule still stops it.
    call(&mut h, "rule.add", json!({ "effect": "deny", "matcher": { "kind": "window", "pattern": "split*" } }));
    assert_eq!(call_err(&mut a, "window.command", json!({ "action": "split", "target": sid })).code, 1);
}

#[test]
fn updates_are_proxied_to_the_gui() {
    let d = TestDaemon::start();
    let (mut h, mut a) = (d.human(), d.agent(None));
    let st = call(&mut a, "updates.status", json!({}));
    assert_eq!((st["state"].as_str(), st["gui_connected"].as_bool()), (Some("unknown"), Some(false)));
    assert_eq!(call(&mut a, "updates.check", json!({}))["delivered"], 0);
    let mut gui = d.human().subscribe(None).unwrap();
    let r = call(&mut a, "updates.check", json!({}));
    assert_eq!(r["delivered"], 1);
    let n = loop {
        if let Notification::Other { method, params } = gui.next_notification().unwrap()
            && method == "updates.command" {
                break params;
            }
    };
    assert_eq!((n["action"].as_str(), n["by"]["kind"].as_str()), (Some("check"), Some("agent")));
    // The GUI reports; agents read it. Agents can't report.
    call(&mut h, "updates.report", json!({ "status": { "state": "ready", "current_version": "0.1.0", "available_version": "0.2.0" } }));
    let st = call(&mut a, "updates.status", json!({}));
    assert_eq!((st["state"].as_str(), st["available_version"].as_str()), (Some("ready"), Some("0.2.0")));
    assert_eq!(kinds(&mut h, "updates.status").len(), 1);
    call(&mut h, "updates.report", json!({ "status": { "state": "ready", "current_version": "0.1.0", "available_version": "0.2.0" } }));
    assert_eq!(kinds(&mut h, "updates.status").len(), 1, "an unchanged report is not an event");
    assert_eq!(call_err(&mut a, "updates.report", json!({ "status": { "state": "idle" } })).code, HUMAN_ONLY);
    // Installing is human only: the agent's call waits for the human.
    let e = call_err(&mut a, "updates.install", json!({}));
    assert_eq!(e.code, HUMAN_ONLY);
    let nid = e.data.unwrap()["needs_you_id"].as_str().unwrap().to_string();
    call(&mut h, "needs_you.resolve", json!({ "id": nid, "resolution": { "kind": "approve", "scope": { "kind": "once" } } }));
    let n = loop {
        if let Notification::Other { method, params } = gui.next_notification().unwrap()
            && method == "updates.command" {
                break params;
            }
    };
    assert_eq!((n["action"].as_str(), n["by"]["kind"].as_str()), (Some("install"), Some("human")));
}

#[test]
fn daemon_reset_clears_state_but_keeps_rules_and_log() {
    let d = TestDaemon::start();
    let (mut h, mut a) = (d.human(), d.agent(None));
    open_sh(&mut h);
    open_sh(&mut h);
    call(&mut h, "settings.set", json!({ "key": "theme", "value": "light" }));
    call(&mut h, "rule.add", json!({ "effect": "deny", "matcher": { "kind": "command", "pattern": "x*" } }));
    call(&mut a, "needs_you.raise", json!({ "kind": "note", "message": "fyi" }));
    call(&mut a, "trigger.add", json!({ "name": "t", "source": "github", "event": "push", "action": { "kind": "attention", "message": "m" } }));
    let before = call(&mut h, "events.list", json!({ "limit": 10000 })).as_array().unwrap().len();
    // Agents only get to ask.
    assert_eq!(call_err(&mut a, "daemon.reset", json!({})).code, HUMAN_ONLY);
    let pending = call(&mut h, "needs_you.list", json!({})).as_array().unwrap().len();
    assert!(pending >= 2);
    let r = call(&mut h, "daemon.reset", json!({}));
    assert_eq!((r["sessions_closed"].as_u64(), r["triggers_removed"].as_u64(), r["settings_reset"].as_u64()), (Some(2), Some(1), Some(1)));
    assert!(r["projects_removed"].as_u64().unwrap() >= 1);
    for (m, empty) in [("session.list", true), ("project.list", true), ("trigger.list", true), ("needs_you.list", true), ("rule.list", false)] {
        assert_eq!(call(&mut h, m, json!({})).as_array().unwrap().is_empty(), empty, "{m}");
    }
    assert_eq!(call(&mut h, "settings.get", json!({ "key": "theme" }))["value"], "system");
    assert!(call(&mut h, "events.list", json!({ "limit": 10000 })).as_array().unwrap().len() > before, "the log is kept");
    assert_eq!(kinds(&mut h, "daemon.reset").len(), 1);
    let r = call(&mut h, "daemon.reset", json!({ "keep_rules": false }));
    assert_eq!(r["rules_removed"], 1);
    assert!(call(&mut h, "rule.list", json!({})).as_array().unwrap().is_empty());
}

#[test]
fn permissions_status_lists_the_three_panes() {
    let d = TestDaemon::start();
    let mut a = d.agent(None);
    let p = call(&mut a, "permissions.status", json!({}));
    let names: Vec<&str> = p["permissions"].as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["accessibility", "notifications", "login-items"]);
    assert_eq!(p["permissions"][0]["open_cli"], "midna permissions open accessibility");
}

#[test]
fn session_clear_drops_the_scrollback() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let id = open_sh(&mut h);
    call(&mut h, "session.input", json!({ "id": id, "text": "seq 1 300", "enter": true }));
    wait_for(5, "seq output", || read(&mut h, &id).contains("\n300").then_some(()));
    let r = call(&mut h, "session.clear", json!({ "id": id }));
    assert_eq!(r["scrollback_cleared"], true);
    wait_for(5, "scrollback gone", || {
        let text = call(&mut h, "session.read", json!({ "id": id, "lines": 400 }))["text"].as_str().unwrap().to_string();
        (!text.lines().any(|l| l.trim() == "5")).then_some(())
    });
}
