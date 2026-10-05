//! Tracking what an agent has in flight, and restarting it into the same conversation once
//! nothing would be lost. The agent is a shell script standing in for `claude`: it records its
//! argv, draws Claude's input box the way 2.1.289 does (grey rules, a faint placeholder), answers `--version` with the "installed" version, and its
//! hooks are the payload shapes recorded from Claude Code 2.1.289.
mod common;
use common::*;
use midna_proto::Client;
use serde_json::{Value, json};
use std::path::PathBuf;

struct FakeClaude {
    dir: PathBuf,
}

impl FakeClaude {
    fn new(tag: &str) -> FakeClaude {
        let dir = PathBuf::from(format!("/tmp/midna-fake-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let rule = "─".repeat(40);
        let script = format!(
            "#!/bin/sh\n\
             case \"$1\" in --version) cat '{d}/installed'; exit 0;; esac\n\
             echo \"$@\" >> '{d}/argv'\n\
             [ -f '{d}/notice' ] && printf '%30s\\342\\234\\224 Update installed \\302\\267 Restart to update\\n' ''\n\
             printf '\\033[38;2;136;136;136m%s\\033[39m\\n\\342\\235\\257\\302\\240\\033[2mTry \"fix lint errors\"\\033[22m\\n\\033[38;2;136;136;136m%s\\033[39m\\n' '{rule}' '{rule}'\n\
             trap 'exit 0' HUP TERM\n\
             sleep 300 & wait $!\n",
            d = dir.display()
        );
        let bin = dir.join("claude");
        std::fs::write(&bin, script).unwrap();
        std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        std::fs::write(dir.join("installed"), "2.1.289 (Claude Code)\n").unwrap();
        FakeClaude { dir }
    }

    fn bin(&self) -> String {
        self.dir.join("claude").to_string_lossy().into_owned()
    }

    fn launches(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("argv")).unwrap_or_default().lines().map(str::to_string).collect()
    }

    /// Draw Claude's "Restart to update" notice above the input box from the next launch on.
    fn show_notice(&self) {
        std::fs::write(self.dir.join("notice"), "").unwrap();
    }

    fn install(&self, version: &str) {
        std::fs::write(self.dir.join("installed"), format!("{version} (Claude Code)\n")).unwrap();
    }
}

impl Drop for FakeClaude {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn hook(a: &mut Client, event: &str, payload: Value) {
    call(a, "agent.hook", json!({ "agent": "claude", "event": event, "payload": payload }));
}

fn info(h: &mut Client, sid: &str) -> Value {
    call(h, "session.get", json!({ "id": sid }))["agent_info"].clone()
}

fn stop(a: &mut Client, background: Value) {
    hook(a, "Stop", json!({ "session_id": "conv-1", "hook_event_name": "Stop", "stop_hook_active": false, "background_tasks": background, "session_crons": [] }));
}

#[test]
fn queued_restart_waits_for_background_work_then_resumes() {
    let fake = FakeClaude::new("q");
    let bin = fake.bin();
    let d = TestDaemon::start_with(move |c| c.agent_bin = Some(bin));
    let mut h = d.human();
    call(&mut h, "settings.set", json!({ "key": "agents.restart_idle_secs", "value": 0 }));
    let sid = call(&mut h, "session.open", json!({ "kind": "agent", "agent": "claude", "cwd": "/tmp", "prompt": "do the thing" }))["id"].as_str().unwrap().to_string();
    wait_for(5, "fake claude started", || (fake.launches().len() == 1).then_some(()));
    assert!(fake.launches()[0].ends_with("do the thing"), "{:?}", fake.launches());
    let mut a = d.agent(Some(&sid));
    hook(&mut a, "SessionStart", json!({ "session_id": "conv-1", "source": "startup", "permission_mode": "acceptEdits", "model": "claude-opus-5-5" }));
    hook(&mut a, "statusline", json!({ "session_id": "conv-1", "version": "2.1.289", "model": { "id": "claude-opus-5-5", "display_name": "Opus" }, "cost": { "total_cost_usd": 0.25 } }));
    hook(&mut a, "UserPromptSubmit", json!({ "session_id": "conv-1", "prompt": "start a dev server" }));
    hook(&mut a, "PostToolUse", json!({ "session_id": "conv-1", "tool_name": "Bash", "tool_input": { "command": "sleep 400", "run_in_background": true }, "tool_response": { "backgroundTaskId": "bsh1" } }));
    assert_eq!(info(&mut h, &sid)["background"][0]["id"], "bsh1", "tracked live, before the turn ends");
    stop(&mut a, json!([{ "id": "bsh1", "type": "shell", "status": "running", "description": "dev", "command": "sleep 400" }]));
    let i = info(&mut h, &sid);
    assert_eq!(i["conversation_id"], "conv-1");
    assert_eq!(i["version"], "2.1.289");

    // Now would kill the dev server: refused unless forced.
    let e = call_err(&mut h, "session.restart", json!({ "id": sid, "when": "now" }));
    assert!(e.message.contains("bsh1") && e.message.contains("when=idle"), "{}", e.message);

    let s = call(&mut h, "session.restart", json!({ "id": sid, "when": "idle", "reason": "pick up settings" }));
    let waiting = s["agent_info"]["restart"]["waiting_for"].as_array().unwrap().clone();
    assert!(waiting.iter().any(|w| w.as_str().unwrap().contains("bsh1")), "{waiting:?}");
    std::thread::sleep(std::time::Duration::from_secs(3));
    assert_eq!(fake.launches().len(), 1, "must wait while the shell runs");

    // The shell finished: Claude's next Stop no longer lists it.
    stop(&mut a, json!([]));
    wait_for(10, "restart", || (fake.launches().len() == 2).then_some(()));
    let argv = &fake.launches()[1];
    assert!(argv.ends_with("--model claude-opus-5-5 --permission-mode acceptEdits --resume conv-1"), "{argv}");
    assert!(!argv.contains("do the thing"), "the initial prompt is not sent again: {argv}");
    let s = call(&mut h, "session.get", json!({ "id": sid }));
    assert_eq!(s["status"]["reason"], "restarted (resumed)");
    assert!(s["agent_info"]["restart"].is_null());
    let ev = call(&mut h, "events.list", json!({ "filter": { "kinds": ["session.restarted"] } }));
    let ev = &ev.as_array().unwrap()[0];
    assert_eq!((ev["data"]["resume"].as_bool(), ev["data"]["reason"].as_str()), (Some(true), Some("pick up settings")));

    // The queued restart is gone from the last session.agent event too (event-driven clients).
    let ev = call(&mut h, "events.list", json!({ "filter": { "kinds": ["session.agent"] } }));
    assert!(ev.as_array().unwrap().last().unwrap()["data"]["restart"].is_null());

    // The resumed agent reports the conversation's running total: not spent again.
    let mut a = d.agent(Some(&sid));
    hook(&mut a, "statusline", json!({ "session_id": "conv-1", "version": "2.1.289", "cost": { "total_cost_usd": 0.25 } }));
    let costs = call(&mut h, "events.list", json!({ "filter": { "kinds": ["agent.cost"] } }));
    assert_eq!(costs.as_array().unwrap().len(), 1, "{costs}");

    // Processes under the terminal: the script and its sleep.
    let procs = call(&mut h, "session.processes", json!({ "id": sid }));
    let procs = procs.as_array().unwrap();
    assert!(procs.iter().any(|p| p["depth"] == 1 && p["command"].as_str().unwrap().contains("sleep 300")), "{procs:?}");

    // An update is installed: when_idle queues the restart without asking.
    call(&mut h, "settings.set", json!({ "key": "agents.restart_on_update", "value": "when_idle" }));
    let mut a = d.agent(Some(&sid));
    hook(&mut a, "SessionStart", json!({ "session_id": "conv-1", "source": "resume" }));
    hook(&mut a, "statusline", json!({ "session_id": "conv-1", "version": "2.1.289", "model": { "id": "claude-opus-5-5" } }));
    fake.install("2.1.290");
    midnad::restart::check_updates(&d.daemon());
    wait_for(10, "update restart", || (fake.launches().len() == 3).then_some(()));
    assert!(fake.launches()[2].ends_with("--resume conv-1"));
    let ev = call(&mut h, "events.list", json!({ "filter": { "kinds": ["session.restarted"] } }));
    assert_eq!(ev.as_array().unwrap().last().unwrap()["data"]["reason"], "Claude 2.1.289 → 2.1.290");

    // ask: recorded for the terminal's prompt, no needs-you item; "When idle" queues it.
    call(&mut h, "settings.set", json!({ "key": "agents.restart_on_update", "value": "ask" }));
    let mut a = d.agent(Some(&sid));
    hook(&mut a, "statusline", json!({ "session_id": "conv-1", "version": "2.1.290" }));
    fake.install("2.1.291");
    midnad::restart::check_updates(&d.daemon());
    assert_eq!(call(&mut h, "needs_you.list", json!({})), json!([]), "the status never changes for an update");
    assert_eq!(info(&mut h, &sid)["update_available"], "2.1.291");
    std::thread::sleep(std::time::Duration::from_secs(3));
    assert_eq!(fake.launches().len(), 3, "ask never restarts on its own");
    call(&mut h, "session.restart", json!({ "id": sid, "when": "idle" }));
    wait_for(10, "restart when idle", || (fake.launches().len() == 4).then_some(()));
}

#[test]
fn on_screen_update_notice_is_offered_without_a_status_line() {
    let fake = FakeClaude::new("n");
    let bin = fake.bin();
    let d = TestDaemon::start_with(move |c| c.agent_bin = Some(bin));
    let mut h = d.human();
    call(&mut h, "settings.set", json!({ "key": "agents.restart_idle_secs", "value": 0 }));
    fake.show_notice();
    fake.install("2.1.290");
    let sid = call(&mut h, "session.open", json!({ "kind": "agent", "agent": "claude", "cwd": "/tmp" }))["id"].as_str().unwrap().to_string();
    wait_for(5, "fake claude started", || (fake.launches().len() == 1).then_some(()));
    let mut a = d.agent(Some(&sid));
    // The user's own status line: midna never hears a version.
    hook(&mut a, "SessionStart", json!({ "session_id": "conv-1", "source": "startup" }));
    stop(&mut a, json!([]));
    wait_for(5, "notice drawn", || call(&mut h, "session.read", json!({ "id": sid, "screen": true })).to_string().contains("Restart to update").then_some(()));

    // Default (ask): recorded for the prompt, no needs-you item.
    midnad::restart::check_updates(&d.daemon());
    assert_eq!(call(&mut h, "needs_you.list", json!({})), json!([]));
    assert_eq!(info(&mut h, &sid)["update_available"], "2.1.290");

    // Not now: hidden for this update, and a later check doesn't bring it back.
    let s = call(&mut h, "session.update_decline", json!({ "id": sid }));
    assert_eq!(s["agent_info"]["update_declined"], "2.1.290");
    midnad::restart::check_updates(&d.daemon());
    assert_eq!(info(&mut h, &sid)["update_declined"], "2.1.290");
    assert_eq!(fake.launches().len(), 1, "ask never restarts on its own");

    // A newer update asks again.
    fake.install("2.1.291");
    midnad::restart::check_updates(&d.daemon());
    let i = info(&mut h, &sid);
    assert_eq!((i["update_available"].as_str(), i["update_declined"].as_str()), (Some("2.1.291"), Some("2.1.290")));

    // Restart (now): same conversation, and the update state is gone.
    call(&mut h, "session.restart", json!({ "id": sid }));
    wait_for(10, "restart", || (fake.launches().len() == 2).then_some(()));
    let i = info(&mut h, &sid);
    assert!(i["update_available"].is_null() && i["update_declined"].is_null(), "{i}");
    assert!(fake.launches()[1].ends_with("--resume conv-1"), "{:?}", fake.launches());
}

#[test]
fn cancel_and_exit_drop_a_queued_restart() {
    let fake = FakeClaude::new("c");
    let bin = fake.bin();
    let d = TestDaemon::start_with(move |c| c.agent_bin = Some(bin));
    let mut h = d.human();
    let sid = call(&mut h, "session.open", json!({ "kind": "agent", "agent": "claude", "cwd": "/tmp" }))["id"].as_str().unwrap().to_string();
    wait_for(5, "fake claude started", || (fake.launches().len() == 1).then_some(()));
    let mut a = d.agent(Some(&sid));
    hook(&mut a, "SessionStart", json!({ "session_id": "conv-1", "source": "startup" }));
    // The default quiet period (60s) holds it.
    let s = call(&mut h, "session.restart", json!({ "id": sid, "when": "idle" }));
    assert!(s["agent_info"]["restart"]["waiting_for"].to_string().contains("since the terminal was last active"), "{s}");
    let s = call(&mut h, "session.restart_cancel", json!({ "id": sid }));
    assert!(s["agent_info"]["restart"].is_null());
    // A plain shell has nothing to report: idle needs an agent.
    let sh = open_sh(&mut h);
    let e = call_err(&mut h, "session.restart", json!({ "id": sh, "when": "idle" }));
    assert!(e.message.contains("agent"), "{}", e.message);
    // resume=true without a conversation id is refused.
    let e = call_err(&mut h, "session.restart", json!({ "id": sh, "resume": true }));
    assert!(e.message.contains("no conversation"), "{}", e.message);
    // Queued, then the agent exits: the queue goes with it.
    call(&mut h, "session.restart", json!({ "id": sid, "when": "idle" }));
    let pid = call(&mut h, "session.get", json!({ "id": sid }))["pid"].as_i64().unwrap() as i32;
    unsafe { libc::kill(pid, libc::SIGTERM) };
    wait_for(10, "exit", || call(&mut h, "session.get", json!({ "id": sid }))["pid"].is_null().then_some(()));
    assert!(info(&mut h, &sid)["restart"].is_null());
}
