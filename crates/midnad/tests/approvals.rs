//! Approvals that outlive what they were about, forced closes without the human, no-wait
//! calls, projects an agent's open created, and Claude's folder-trust dialog as needs-you.
mod common;
use common::*;
use midna_proto::error::{HUMAN_ONLY, PENDING, REFUSED};
use midna_proto::{Client, ClientError};
use serde_json::{Value, json};
use std::io::Write;
use std::path::PathBuf;

fn approvals(h: &mut Client) -> Vec<Value> {
    call(h, "needs_you.list", json!({})).as_array().unwrap().iter().filter(|n| n["kind"] == "approval").cloned().collect()
}

fn wait_approval(h: &mut Client) -> Value {
    wait_for(5, "approval item", || approvals(h).into_iter().next())
}

#[test]
fn approval_is_withdrawn_when_its_target_closes() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let asker = open_sh(&mut h);
    let target = open_sh(&mut h);
    let mut a = d.agent(Some(&asker));
    let t = {
        let target = target.clone();
        std::thread::spawn(move || a.call_value("session.close", json!({ "id": target, "force": true })))
    };
    let item = wait_approval(&mut h);
    assert_eq!(item["approval"]["action"]["value"], format!("close --force {target}"));
    assert_eq!(item["approval"]["target_session"], target.as_str());
    // The agent (or the human) closes the target another way: the question is moot.
    call(&mut h, "session.close", json!({ "id": target }));
    let e = match t.join().unwrap() {
        Err(ClientError::Rpc(e)) => e,
        other => panic!("expected a refusal, got {other:?}"),
    };
    assert_eq!(e.code, REFUSED);
    assert!(e.message.contains("withdrawn") && e.message.contains(&format!("terminal {target} closed")), "{}", e.message);
    assert!(approvals(&mut h).is_empty(), "nothing left in Needs you");
    let g = call(&mut h, "needs_you.get", json!({ "id": item["id"] }));
    assert_eq!(g["state"], "withdrawn");
    assert_eq!(g["resolution"]["kind"], "withdrawn");
}

#[test]
fn approval_is_withdrawn_when_the_asker_goes_away() {
    let d = TestDaemon::start();
    let mut h = d.human();
    call(&mut h, "rule.add", json!({ "effect": "ask", "matcher": { "kind": "command", "pattern": "deploy*" } }));

    // Its terminal closes: the blocked call gets a deny with source `withdrawn`.
    let asker = open_sh(&mut h);
    let mut a = d.agent(Some(&asker));
    let t = std::thread::spawn(move || a.call_value("policy.request", json!({ "action": { "kind": "command", "value": "deploy prod" }, "timeout_secs": 30 })).unwrap());
    wait_approval(&mut h);
    call(&mut h, "session.close", json!({ "id": asker }));
    let r = t.join().unwrap();
    assert_eq!((r["decision"].as_str(), r["source"].as_str()), (Some("deny"), Some("withdrawn")));
    assert!(approvals(&mut h).is_empty());

    // Its connection drops (the hook process was killed): the item goes within a second or so.
    let mut s = std::os::unix::net::UnixStream::connect(d.socket()).unwrap();
    let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "policy.request", "params": { "caller": { "role": "agent" }, "action": { "kind": "command", "value": "deploy staging" }, "timeout_secs": 30 } });
    s.write_all(format!("{req}\n").as_bytes()).unwrap();
    let item = wait_approval(&mut h);
    drop(s);
    wait_for(5, "withdrawn after disconnect", || approvals(&mut h).is_empty().then_some(()));
    let g = call(&mut h, "needs_you.get", json!({ "id": item["id"] }));
    assert_eq!(g["state"], "withdrawn");
    assert!(g["resolution"]["reason"].as_str().unwrap().contains("disconnected"), "{g}");
}

#[test]
fn may_force_close_lets_agents_close_without_asking() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let me = open_sh(&mut h);
    let mut a = d.agent(Some(&me));
    // Human only: an agent can't turn it on for itself.
    assert_eq!(call_err(&mut a, "settings.set", json!({ "key": "agents.may_force_close", "value": true })).code, HUMAN_ONLY);
    for n in approvals(&mut h) {
        call(&mut h, "needs_you.resolve", json!({ "id": n["id"], "resolution": { "kind": "deny" } }));
    }
    assert_eq!(call(&mut h, "settings.get", json!({ "key": "agents.may_force_close" }))["value"], false);
    call(&mut h, "settings.set", json!({ "key": "agents.may_force_close", "value": true }));

    // A working terminal, closed with force: no approval.
    let busy = open_sh(&mut h);
    call(&mut d.agent(Some(&busy)), "agent.hook", json!({ "agent": "claude", "event": "UserPromptSubmit", "payload": {} }));
    call(&mut a, "session.close", json!({ "id": busy, "force": true }));
    assert!(approvals(&mut h).is_empty());
    assert!(call_err(&mut h, "session.get", json!({ "id": busy })).message.contains(&busy));
    // Plain close of someone else's idle terminal, even with may_close_idle off.
    call(&mut h, "settings.set", json!({ "key": "agents.may_close_idle", "value": false }));
    let idle = open_sh(&mut h);
    call(&mut a, "session.close", json!({ "id": idle }));
    // A rule still wins.
    call(&mut h, "rule.add", json!({ "effect": "deny", "matcher": { "kind": "cli", "pattern": "close --force*" } }));
    let other = open_sh(&mut h);
    assert_eq!(call_err(&mut a, "session.close", json!({ "id": other, "force": true })).code, REFUSED);
}

#[test]
fn no_wait_returns_the_needs_you_id_and_finishes_later() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let me = open_sh(&mut h);
    let target = open_sh(&mut h);
    let mut a = d.agent(Some(&me)).no_wait();
    let t0 = std::time::Instant::now();
    let e = call_err(&mut a, "session.close", json!({ "id": target, "force": true }));
    assert!(t0.elapsed() < std::time::Duration::from_secs(3), "returned at once");
    assert_eq!(e.code, PENDING, "{}", e.message);
    let nid = e.data.unwrap()["needs_you_id"].as_str().unwrap().to_string();
    let g = call(&mut a, "needs_you.get", json!({ "id": nid }));
    assert_eq!((g["state"].as_str(), g["item"]["id"].as_str()), (Some("open"), Some(nid.as_str())));
    // Calls that need nobody answer normally.
    assert_eq!(call(&mut a, "session.rename", json!({ "id": me, "name": "board" }))["name"], "board");

    // The human approves later; the close happens then, and needs_you.get says how it went.
    let waiter = {
        let mut a = d.agent(Some(&me));
        let nid = nid.clone();
        std::thread::spawn(move || call(&mut a, "needs_you.get", json!({ "id": nid, "wait_secs": 20 })))
    };
    std::thread::sleep(std::time::Duration::from_millis(600));
    call(&mut h, "needs_you.resolve", json!({ "id": nid, "resolution": { "kind": "approve", "scope": { "kind": "once" } } }));
    let g = waiter.join().unwrap();
    assert_eq!(g["state"], "resolved");
    assert_eq!(g["resolution"]["kind"], "approve");
    assert_eq!(g["result"], json!({ "ok": true }), "{g}");
    wait_for(5, "target closed", || call(&mut h, "session.list", json!({})).as_array().unwrap().iter().all(|s| s["id"] != target.as_str()).then_some(()));

    // A denial ends up as the call's error.
    let target = open_sh(&mut h);
    let e = call_err(&mut a, "session.close", json!({ "id": target, "force": true }));
    let nid = e.data.unwrap()["needs_you_id"].as_str().unwrap().to_string();
    call(&mut h, "needs_you.resolve", json!({ "id": nid, "resolution": { "kind": "deny" } }));
    let g = call(&mut a, "needs_you.get", json!({ "id": nid, "wait_secs": 10 }));
    assert_eq!(g["state"], "resolved");
    assert_eq!(g["error"]["code"], REFUSED, "{g}");
    assert!(call(&mut h, "session.get", json!({ "id": target }))["id"].is_string(), "still open");
    assert_eq!(call_err(&mut a, "needs_you.get", json!({ "id": "n_nope" })).code, 3);
}

#[test]
fn agents_may_remove_projects_their_open_created() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let dir = PathBuf::from(format!("/tmp/midna-auto-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut a = d.agent(None);
    let s = call(&mut a, "session.open", json!({ "kind": "shell", "cwd": dir, "command": ["/bin/sh"] }));
    let (sid, pid) = (s["id"].as_str().unwrap().to_string(), s["project_id"].as_str().unwrap().to_string());
    let p = call(&mut h, "project.list", json!({})).as_array().unwrap().iter().find(|p| p["id"] == pid.as_str()).cloned().unwrap();
    assert_eq!(p["auto_created"], true);
    // Not while a terminal runs in it (that becomes the usual human approval).
    assert_eq!(call_err(&mut a, "project.remove", json!({ "id": pid })).code, HUMAN_ONLY);
    call(&mut a, "session.close", json!({ "id": sid }));
    call(&mut a, "project.remove", json!({ "id": pid }));
    assert!(call(&mut h, "project.list", json!({})).as_array().unwrap().iter().all(|p| p["id"] != pid.as_str()));

    // A project the human added stays human only.
    let mine = call(&mut h, "project.add", json!({ "path": dir }));
    assert!(mine.get("auto_created").is_none());
    assert_eq!(call_err(&mut a, "project.remove", json!({ "id": mine["id"] })).code, HUMAN_ONLY);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A stand-in for `claude` that draws Claude Code 2.1.288's folder-trust dialog, records every
/// byte typed at it, and once Enter picks a choice draws an input box ("Yes": a down arrow came
/// first) or exits ("No, exit" is highlighted), as Claude does.
struct FakeTrust {
    dir: PathBuf,
}

impl FakeTrust {
    fn new(tag: &str) -> FakeTrust {
        let dir = PathBuf::from(format!("/tmp/midna-trust-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fixture = format!("{}/tests/fixtures/claude-2.1.288-trust-folder.screen.txt", env!("CARGO_MANIFEST_DIR"));
        let script = format!(
            "#!/usr/bin/env python3\n\
             import os, sys, tty\n\
             screen = [l.rstrip() for l in open('{fixture}').read().splitlines()]\n\
             while screen and not screen[-1]: screen.pop()\n\
             sys.stdout.write('\\x1b[H\\x1b[2J' + '\\r\\n'.join(screen)); sys.stdout.flush()\n\
             tty.setraw(0)\n\
             while True:\n\
             \x20   b = os.read(0, 64)\n\
             \x20   if not b: break\n\
             \x20   open('{d}/keys', 'ab').write(b)\n\
             \x20   if b'\\r' in b and b'\\x1b[B' not in open('{d}/keys', 'rb').read(): break\n\
             \x20   if b'\\r' in b:\n\
             \x20       sys.stdout.write('\\x1b[H\\x1b[2J  welcome\\r\\n> '); sys.stdout.flush()\n",
            d = dir.display()
        );
        let bin = dir.join("claude");
        std::fs::write(&bin, script).unwrap();
        std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        FakeTrust { dir }
    }

    fn keys(&self) -> Vec<u8> {
        std::fs::read(self.dir.join("keys")).unwrap_or_default()
    }
}

impl Drop for FakeTrust {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn folder_trust_dialog_is_a_needs_you_item() {
    let fake = FakeTrust::new("item");
    let bin = fake.dir.join("claude").to_string_lossy().into_owned();
    let d = TestDaemon::start_with(move |c| c.agent_bin = Some(bin));
    let mut h = d.human();
    let sid = call(&mut h, "session.open", json!({ "kind": "agent", "agent": "claude", "cwd": "/tmp" }))["id"].as_str().unwrap().to_string();
    let item = wait_for(10, "trust item", || {
        call(&mut h, "needs_you.list", json!({ "session_id": sid })).as_array().unwrap().iter().find(|n| n["kind"] == "permission_prompt").cloned()
    });
    assert!(item["title"].as_str().unwrap().starts_with("Trust this folder?"), "{item}");
    let s = call(&mut h, "session.get", json!({ "id": sid }));
    assert_eq!((s["status"]["state"].as_str(), s["status"]["reason"].as_str()), (Some("needs_you"), Some("folder trust")));
    // Agents can't type the answer themselves.
    assert_eq!(call_err(&mut d.agent(None), "session.input", json!({ "id": sid, "text": "" })).code, HUMAN_ONLY);

    // Approve picks "Yes, I trust this folder": one down from the highlighted "No, exit", then Enter.
    call(&mut h, "needs_you.resolve", json!({ "id": item["id"], "resolution": { "kind": "approve", "scope": { "kind": "once" } } }));
    wait_for(5, "keys typed", || (fake.keys() == b"\x1b[B\r").then_some(()));
    let s = call(&mut h, "session.get", json!({ "id": sid }));
    assert_eq!(s["status"]["state"], "idle");
    std::thread::sleep(std::time::Duration::from_millis(3500));
    assert!(call(&mut h, "needs_you.list", json!({ "session_id": sid })).as_array().unwrap().is_empty(), "not raised again once answered");
    // Trusting it saves the folder.
    assert_eq!(call(&mut h, "settings.get", json!({ "key": "agents.trust_folders" }))["value"], json!(["/tmp"]));
}

#[test]
fn trust_folders_answers_yes_without_asking() {
    let fake = FakeTrust::new("auto");
    let bin = fake.dir.join("claude").to_string_lossy().into_owned();
    let cwd = fake.dir.to_string_lossy().into_owned();
    let d = TestDaemon::start_with(move |c| c.agent_bin = Some(bin));
    let mut h = d.human();
    // Agents can't widen it themselves: that asks the human.
    let err = call_err(&mut d.agent(None), "settings.set", json!({ "key": "agents.trust_folders", "value": "/tmp" }));
    assert_eq!(err.code, HUMAN_ONLY);
    call(&mut h, "settings.set", json!({ "key": "agents.trust_folders", "value": "/tmp/midna-trust-auto-*" }));
    let sid = call(&mut h, "session.open", json!({ "kind": "agent", "agent": "claude", "cwd": cwd }))["id"].as_str().unwrap().to_string();
    wait_for(10, "keys typed", || (fake.keys() == b"\x1b[B\r").then_some(()));
    let items = call(&mut h, "needs_you.list", json!({ "session_id": sid }));
    assert!(items.as_array().unwrap().iter().all(|n| n["kind"] != "permission_prompt"), "{items}");
    let audit = call(&mut h, "events.list", json!({ "limit": 1000 }));
    assert!(audit.to_string().contains("folder_trusted"), "{audit}");
}

#[test]
fn denying_trust_saves_nothing() {
    let fake = FakeTrust::new("deny");
    let bin = fake.dir.join("claude").to_string_lossy().into_owned();
    let cwd = fake.dir.to_string_lossy().into_owned();
    let d = TestDaemon::start_with(move |c| c.agent_bin = Some(bin));
    let mut h = d.human();
    let sid = call(&mut h, "session.open", json!({ "kind": "agent", "agent": "claude", "cwd": cwd }))["id"].as_str().unwrap().to_string();
    let item = wait_for(10, "trust item", || {
        call(&mut h, "needs_you.list", json!({ "session_id": sid })).as_array().unwrap().iter().find(|n| n["kind"] == "permission_prompt").cloned()
    });
    call(&mut h, "needs_you.resolve", json!({ "id": item["id"], "resolution": { "kind": "deny" } }));
    wait_for(5, "keys typed", || (fake.keys() == b"\r").then_some(()));
    std::thread::sleep(std::time::Duration::from_millis(2500));
    assert_eq!(call(&mut h, "settings.get", json!({ "key": "agents.trust_folders" }))["value"], json!([]));
}

/// Opens the fake in its own folder, waits for its trust item and types `keys` at it as the
/// human, the way answering in the terminal does; returns the saved agents.trust_folders after
/// the agent has had time to exit (or not).
fn answer_in_terminal(tag: &str, keys: &str) -> (Value, Value) {
    let fake = FakeTrust::new(tag);
    let bin = fake.dir.join("claude").to_string_lossy().into_owned();
    let cwd = fake.dir.to_string_lossy().into_owned();
    let d = TestDaemon::start_with(move |c| c.agent_bin = Some(bin));
    let mut h = d.human();
    let sid = call(&mut h, "session.open", json!({ "kind": "agent", "agent": "claude", "cwd": cwd }))["id"].as_str().unwrap().to_string();
    wait_for(10, "trust item", || {
        call(&mut h, "needs_you.list", json!({ "session_id": sid })).as_array().unwrap().iter().find(|n| n["kind"] == "permission_prompt").cloned()
    });
    for k in keys.split_inclusive('\r') {
        call(&mut h, "session.input", json!({ "id": sid, "text": k }));
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    std::thread::sleep(std::time::Duration::from_millis(3500));
    let cwd = call(&mut h, "session.get", json!({ "id": sid }))["cwd"].clone();
    (call(&mut h, "settings.get", json!({ "key": "agents.trust_folders" }))["value"].clone(), cwd)
}

#[test]
fn yes_typed_in_the_terminal_saves_the_folder() {
    let (saved, cwd) = answer_in_terminal("typed-yes", "\x1b[B\r");
    assert_eq!(saved, json!([cwd]));
}

#[test]
fn no_typed_in_the_terminal_saves_nothing() {
    let (saved, _) = answer_in_terminal("typed-no", "\r");
    assert_eq!(saved, json!([]));
}

/// An agent that said it was blocked and then got a new prompt (the human's answer, or a
/// message from elsewhere) moved on: its own `blocked` item closes. Notes, and items other
/// terminals raised about it, stay.
#[test]
fn a_new_prompt_closes_the_agents_own_blocked_item() {
    let d = TestDaemon::start();
    let mut h = d.human();
    // Without replacing, so its blocked item and note are both open.
    call(&mut h, "settings.set", json!({ "key": "needs_you.replace", "value": false }));
    let sid = open_sh(&mut h);
    let other = open_sh(&mut h);
    let mut a = d.agent(Some(&sid));
    let mut b = d.agent(Some(&other));
    let hook = |c: &mut Client, ev: &str| call(c, "agent.hook", json!({ "agent": "claude", "event": ev, "payload": { "session_id": "c1", "prompt": "go" } }));
    hook(&mut a, "UserPromptSubmit");
    let blocked = call(&mut a, "needs_you.raise", json!({ "kind": "blocked", "message": "which option?" }))["id"].as_str().unwrap().to_string();
    let note = call(&mut a, "needs_you.raise", json!({ "kind": "note", "message": "fyi" }))["id"].as_str().unwrap().to_string();
    let elsewhere = call(&mut b, "needs_you.raise", json!({ "kind": "blocked", "message": "mine" }))["id"].as_str().unwrap().to_string();
    hook(&mut a, "Stop");
    let open = |h: &mut Client| call(h, "needs_you.list", json!({})).as_array().unwrap().iter().map(|n| n["id"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert!(open(&mut h).contains(&blocked), "a finished turn keeps the question open");

    hook(&mut a, "UserPromptSubmit");
    let ids = open(&mut h);
    assert!(!ids.contains(&blocked), "{ids:?}");
    assert!(ids.contains(&note) && ids.contains(&elsewhere), "{ids:?}");
    let got = call(&mut h, "needs_you.get", json!({ "id": blocked }));
    assert_eq!(got["resolution"], json!({ "kind": "done", "auto": true, "reason": "prompt" }), "{got}");
}

#[test]
fn a_setting_change_shows_both_values_and_saves_the_humans_edit() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let me = open_sh(&mut h);
    let mut a = d.agent(Some(&me));
    let asked = json!(["daemon", "/Users/me/bin/ci.sh", "keys"]);
    assert_eq!(call_err(&mut a, "settings.set", json!({ "key": "ui.status.items", "value": asked })).code, HUMAN_ONLY);
    let n = wait_approval(&mut h);
    let was = call(&mut h, "settings.get", json!({ "key": "ui.status.items" }))["value"].clone();
    assert_eq!(n["setting"], json!({ "key": "ui.status.items", "from": was, "to": asked }), "{n}");
    assert!(n["detail"].as_str().unwrap().contains("ci.sh"), "the value is shown, not hidden: {n}");

    // The human keeps the script but drops keys.
    let mine = json!(["/Users/me/bin/ci.sh", "daemon"]);
    let ok = json!({ "id": n["id"], "resolution": { "kind": "approve", "scope": { "kind": "once" } }, "value": mine });
    call(&mut h, "needs_you.resolve", ok);
    assert_eq!(call(&mut h, "settings.get", json!({ "key": "ui.status.items" }))["value"], mine);
    let g = call(&mut a, "needs_you.get", json!({ "id": n["id"] }));
    assert!(g.to_string().contains("ci.sh"), "the agent can see what was saved: {g}");

    // A value on any other approval is refused.
    let other = open_sh(&mut h);
    let e = call_err(&mut d.agent(Some(&me)).no_wait(), "session.close", json!({ "id": other, "force": true }));
    let close = e.data.unwrap()["needs_you_id"].clone();
    let bad = json!({ "id": close, "resolution": { "kind": "approve", "scope": { "kind": "once" } }, "value": 1 });
    assert!(call_err(&mut h, "needs_you.resolve", bad).message.contains("setting change"));
    call(&mut h, "needs_you.resolve", json!({ "id": close, "resolution": { "kind": "deny" } }));

    // A reset proposes the default.
    call(&mut h, "settings.set", json!({ "key": "agents.may_force_close", "value": true }));
    call_err(&mut a, "settings.reset", json!({ "key": "agents.may_force_close" }));
    let n = wait_approval(&mut h);
    assert_eq!(n["setting"], json!({ "key": "agents.may_force_close", "from": true, "to": false }), "{n}");
}
