//! Agent prompts that only the screen shows: Codex's approval overlay (Codex runs with `notify`
//! only, so no hook reports it) and Claude's "Review your answers" screen after a question.
//! The agent is a script that draws the screen and sets the title the real one does.
mod common;
use common::*;
use midna_proto::Client;
use midna_proto::error::HUMAN_ONLY;
use serde_json::{Value, json};

fn script(tag: &str, body: &str) -> std::path::PathBuf {
    let bin = std::path::PathBuf::from(format!("/tmp/midna-asp-{}-{tag}.sh", std::process::id()));
    std::fs::write(&bin, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    bin
}

fn state(h: &mut Client, sid: &str) -> String {
    call(h, "session.get", json!({ "id": sid }))["status"]["state"].as_str().unwrap().to_string()
}

fn prompt_item(h: &mut Client, sid: &str) -> Option<Value> {
    call(h, "needs_you.list", json!({})).as_array().unwrap().iter().find(|n| n["session_id"] == sid && n["kind"] == "permission_prompt").cloned()
}

/// codex-cli 0.163.0 with approval_policy=on-request: the title goes from its spinner to
/// "[ ! ] Action Required | <dir>" while the approval overlay is up.
const CODEX: &str = r#"printf '\033]0;\342\240\213 \342\240\213 | work\007'
sleep 0.5
printf 'Would you like to run the following command?\r\n\r\n  $ touch /tmp/x\r\n\r\n\342\200\272 1. Yes, proceed (y)\r\n  2. No, and tell Codex what to do differently (esc)\r\n\r\n  Press enter to confirm or esc to cancel\r\n'
printf '\033]0;[ ! ] Action Required | work\007'
read answer
printf '\033[2J\033[H'
printf '\033]0;\342\240\231 \342\240\231 | work\007'
echo ran it
sleep 30
"#;

#[test]
fn codex_approval_on_screen_raises_needs_you() {
    let bin = script("codex", CODEX);
    let b = bin.to_string_lossy().into_owned();
    let d = TestDaemon::start_with(move |c| c.agent_bin = Some(b));
    let mut h = d.human();
    let sid = call(&mut h, "session.open", json!({ "kind": "agent", "agent": "codex", "cwd": "/tmp", "cols": 120 }))["id"].as_str().unwrap().to_string();

    let item = wait_for(15, "a needs-you item for the approval", || prompt_item(&mut h, &sid));
    assert_eq!(item["title"], "Codex wants to run a command", "{item}");
    assert!(item["screen_excerpt"].to_string().contains("Would you like to run the following command"), "{item}");
    assert_eq!(state(&mut h, &sid), "needs_you");

    // Another agent can't answer it by typing into the terminal.
    let e = call_err(&mut d.agent(None), "session.input", json!({ "id": sid, "text": "y", "enter": true }));
    assert_eq!(e.code, HUMAN_ONLY, "{e:?}");

    // The human approves: Enter goes to the overlay, Codex runs it and works on.
    call(&mut h, "needs_you.resolve", json!({ "id": item["id"], "resolution": { "kind": "approve", "scope": { "kind": "once" } } }));
    wait_for(10, "codex to run the command", || read(&mut h, &sid).contains("ran it").then_some(()));
    wait_for(10, "the terminal back to working", || (state(&mut h, &sid) == "working").then_some(()));
    assert!(prompt_item(&mut h, &sid).is_none());
    let _ = std::fs::remove_file(bin);
}

#[test]
fn claude_question_review_screen_refuses_agent_keys() {
    let review = format!("{}/tests/fixtures/claude-2.1.301-question-review.screen.txt", env!("CARGO_MANIFEST_DIR"));
    let bin = script("claude", &format!("printf '\\033[2J\\033[H'\ncat '{review}'\nsleep 30\n"));
    let b = bin.to_string_lossy().into_owned();
    let d = TestDaemon::start_with(move |c| c.agent_bin = Some(b));
    let mut h = d.human();
    let sid = call(&mut h, "session.open", json!({ "kind": "agent", "agent": "claude", "cwd": "/tmp", "cols": 120, "rows": 40 }))["id"].as_str().unwrap().to_string();
    wait_for(10, "the review screen", || read(&mut h, &sid).contains("2. Cancel").then_some(()));

    let mut a = d.agent(None);
    assert_eq!(call_err(&mut a, "session.input", json!({ "id": sid, "text": "", "enter": true })).code, HUMAN_ONLY);
    assert_eq!(call_err(&mut a, "session.key", json!({ "id": sid, "key": "enter" })).code, HUMAN_ONLY);
    // The human still can.
    call(&mut h, "session.key", json!({ "id": sid, "key": "down" }));
    let _ = std::fs::remove_file(bin);
}
