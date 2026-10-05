//! Prompt fast travel: session.prompts and session.jump_prompt, against a scripted stand-in
//! for Claude Code's full-screen view (fixtures/fake_agent_view.py) and a shell's scrollback.
mod common;
use common::*;
use midna_proto::Client;
use serde_json::{Value, json};

fn hook(a: &mut Client, ev: &str, payload: Value) {
    call(a, "agent.hook", json!({ "agent": "claude", "event": ev, "payload": payload }));
}

fn screen(h: &mut Client, id: &str) -> Vec<String> {
    call(h, "session.read", json!({ "id": id, "screen": true }))["text"].as_str().unwrap().lines().map(str::to_string).collect()
}

fn jump(h: &mut Client, id: &str, how: Value) -> Value {
    let mut p = how;
    p["id"] = json!(id);
    call(h, "session.jump_prompt", p)
}

#[test]
fn prompts_list_marks_the_ones_before_a_clear() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let mut a = d.agent(Some(&sid));
    hook(&mut a, "SessionStart", json!({ "session_id": "c1", "source": "startup" }));
    hook(&mut a, "UserPromptSubmit", json!({ "session_id": "c1", "prompt": "fix the tests" }));
    hook(&mut a, "UserPromptSubmit", json!({ "session_id": "c1", "prompt": "ship it" }));
    hook(&mut a, "SessionStart", json!({ "session_id": "c2", "source": "clear" }));
    hook(&mut a, "UserPromptSubmit", json!({ "session_id": "c2", "prompt": "now the relay" }));
    let r = call(&mut h, "session.prompts", json!({ "id": sid }));
    let p = r["prompts"].as_array().unwrap();
    let got: Vec<(u64, &str, bool)> = p.iter().map(|x| (x["n"].as_u64().unwrap(), x["text"].as_str().unwrap(), x["on_screen"].as_bool().unwrap())).collect();
    assert_eq!(got, [(1, "fix the tests", false), (2, "ship it", false), (3, "now the relay", true)]);
    let e = jump(&mut h, &sid, json!({ "n": 1 }));
    assert_eq!(e["ok"], false);
    assert!(e["reason"].as_str().unwrap().contains("/clear"), "{e}");
}

#[test]
fn jumps_drive_a_full_screen_agent_view() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/fake_agent_view.py");
    let s = call(&mut h, "session.open", json!({ "kind": "shell", "cwd": "/tmp", "command": ["python3", script] }));
    let sid = s["id"].as_str().unwrap().to_string();
    call(&mut h, "session.resize", json!({ "id": sid, "cols": 80, "rows": 24 }));
    wait_for(10, "the view", || screen(&mut h, &sid).iter().any(|l| l.contains("reply 6.25")).then_some(()));
    let mut a = d.agent(Some(&sid));
    for k in 1..=6 {
        hook(&mut a, "UserPromptSubmit", json!({ "session_id": "c1", "prompt": format!("prompt {k} one two") }));
    }
    let row1 = |h: &mut Client| screen(h, &sid)[1].clone();

    // From the live end, prev is the last prompt; each jump puts the prompt on row 1.
    let r = jump(&mut h, &sid, json!({ "to": "prev" }));
    assert_eq!((r["ok"].as_bool(), r["n"].as_u64()), (Some(true), Some(6)), "{r}");
    assert_eq!(row1(&mut h), "❯ prompt 6 one two");
    for n in [5, 4] {
        let r = jump(&mut h, &sid, json!({ "to": "prev" }));
        assert_eq!(r["n"].as_u64(), Some(n), "{r}");
        assert_eq!(row1(&mut h), format!("❯ prompt {n} one two"));
    }
    let p = call(&mut h, "session.prompts", json!({ "id": sid }));
    assert_eq!((p["here"].as_u64(), p["scrolled"].as_bool()), (Some(4), Some(true)));

    // Far up, then down past where it is (PageDown, then single wheel rows).
    let r = jump(&mut h, &sid, json!({ "n": 1 }));
    assert_eq!(r["found"], true, "{r}");
    assert_eq!(row1(&mut h), "❯ prompt 1 one two");
    assert!(jump(&mut h, &sid, json!({ "to": "prev" }))["reason"].as_str().unwrap().contains("first"));
    let r = jump(&mut h, &sid, json!({ "n": 3 }));
    assert_eq!(r["found"], true, "{r}");
    assert_eq!(row1(&mut h), "❯ prompt 3 one two");
    assert_eq!(jump(&mut h, &sid, json!({ "to": "next" }))["n"], 4);
    assert_eq!(row1(&mut h), "❯ prompt 4 one two");

    // Live: back to the bottom.
    jump(&mut h, &sid, json!({ "to": "live" }));
    wait_for(5, "live", || (!screen(&mut h, &sid).iter().any(|l| l.contains("Jump to bottom"))).then_some(()));
    assert_eq!(call(&mut h, "session.prompts", json!({ "id": sid }))["scrolled"], false);
}

#[test]
fn a_shell_scrollback_is_searched() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    call(&mut h, "session.input", json!({ "id": sid, "text": "printf '❯ alpha one two\\n'; seq 1 200; printf '❯ bravo one two\\n'; seq 1 5", "enter": true }));
    wait_for(10, "output", || read(&mut h, &sid).contains("bravo one two\n1\n2").then_some(()));
    let mut a = d.agent(Some(&sid));
    hook(&mut a, "UserPromptSubmit", json!({ "session_id": "c1", "prompt": "alpha one two" }));
    hook(&mut a, "UserPromptSubmit", json!({ "session_id": "c1", "prompt": "bravo one two" }));
    let r = jump(&mut h, &sid, json!({ "n": 1 }));
    assert_eq!(r["found"], true, "{r}");
    assert_eq!(call(&mut h, "session.selection", json!({ "id": sid }))["text"], "alpha one two");
    assert_eq!(call(&mut h, "session.prompts", json!({ "id": sid }))["scrolled"], true);
}
