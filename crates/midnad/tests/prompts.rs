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
fn links_know_the_prompt_they_came_up_in() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let mut a = d.agent(Some(&sid));
    let dir = std::env::temp_dir().join(format!("midna-turns-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.jsonl");
    std::fs::write(&path, "").unwrap();
    let edit = |file: &str| {
        let e = json!({"type": "assistant", "timestamp": midna_proto::time::now_rfc3339(), "message": {"content": [{"type": "tool_use", "id": file, "name": "Edit", "input": {"file_path": file}}]}});
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        std::io::Write::write_all(&mut f, format!("{e}\n").as_bytes()).unwrap();
    };
    let files = |h: &mut Client, turn: Value| -> Vec<(String, Value)> {
        let r = call(h, "links.list", json!({ "session": sid, "kind": "file", "turn": turn }));
        r["links"].as_array().unwrap().iter().map(|l| (l["target"].as_str().unwrap().to_string(), l["turns"].clone())).collect()
    };
    let ctx = json!({ "session_id": "c1", "transcript_path": path.to_str().unwrap() });
    let submit = |a: &mut Client, prompt: &str| {
        let mut p = ctx.clone();
        p["prompt"] = json!(prompt);
        hook(a, "UserPromptSubmit", p);
    };

    submit(&mut a, "fix a");
    edit("/p/a.rs");
    hook(&mut a, "PostToolUse", ctx.clone());
    wait_for(10, "a.rs", || (files(&mut h, Value::Null).len() == 1).then_some(()));
    std::thread::sleep(std::time::Duration::from_millis(1100)); // timestamps are whole seconds
    submit(&mut a, "now b");
    edit("/p/b.rs");
    edit("/p/a.rs");
    hook(&mut a, "PostToolUse", ctx.clone());
    wait_for(10, "b.rs", || (files(&mut h, Value::Null).len() == 2).then_some(()));

    let mut last = files(&mut h, json!("last"));
    last.sort_by(|x, y| x.0.cmp(&y.0));
    assert_eq!(last, [("/p/a.rs".to_string(), json!([1, 2])), ("/p/b.rs".to_string(), json!([2]))]);
    assert_eq!(files(&mut h, json!(1)), [("/p/a.rs".to_string(), json!([1, 2]))]);
    let r = call(&mut h, "links.list", json!({ "session": sid, "turn": "last" }));
    assert_eq!(r["turn"], 2);
    assert!(r["links"].as_array().unwrap().iter().all(|l| l["turn"] == 2));
    assert!(call_err(&mut h, "links.list", json!({ "session": sid, "turn": "first" })).message.contains("prompt number"));
    let _ = std::fs::remove_dir_all(&dir);
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
