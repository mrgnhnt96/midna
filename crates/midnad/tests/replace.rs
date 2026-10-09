//! `session.replace`: a fresh terminal in the old one's place (⌘T then ⌘W in one step). The
//! agent is a script that prints its argv, so the test can see the first prompt is dropped.
mod common;
use common::*;
use serde_json::{Value, json};

fn fake_agent() -> std::path::PathBuf {
    let bin = std::path::PathBuf::from(format!("/tmp/midna-replace-{}.sh", std::process::id()));
    std::fs::write(&bin, "#!/bin/sh\nfor a; do echo \"ARG:$a\"; done\necho DONE\nsleep 30\n").unwrap();
    std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    bin
}

fn argv(h: &mut midna_proto::Client, sid: &str) -> Vec<String> {
    let text = wait_for(10, "fake agent output", || {
        let t = call(h, "session.read", json!({ "id": sid, "lines": 500 }))["text"].as_str().unwrap_or_default().to_string();
        t.contains("DONE").then_some(t)
    });
    text.lines().filter_map(|l| l.strip_prefix("ARG:")).map(str::to_string).collect()
}

fn ids(h: &mut midna_proto::Client) -> Vec<String> {
    call(h, "session.list", json!({})).as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap().to_string()).collect()
}

#[test]
fn an_agent_is_replaced_in_place_with_a_new_conversation() {
    let bin = fake_agent();
    let b = bin.to_string_lossy().into_owned();
    let d = TestDaemon::start_with(move |c| c.agent_bin = Some(b));
    let mut h = d.human();
    let dir = d.home.join("work");
    std::fs::create_dir_all(&dir).unwrap();
    let open = |h: &mut midna_proto::Client, extra: Value| {
        let mut p = json!({ "kind": "agent", "agent": "claude", "cwd": dir, "cols": 1000 });
        p.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        call(h, "session.open", p)["id"].as_str().unwrap().to_string()
    };
    let first = open(&mut h, json!({}));
    let old = open(&mut h, json!({ "agent_args": ["--model", "opus"], "prompt": "go", "background": true }));
    let last = open(&mut h, json!({}));
    call(&mut h, "session.rename", json!({ "id": old, "name": "mine" }));
    assert_eq!(argv(&mut h, &old).last().map(String::as_str), Some("go"));

    let s = call(&mut h, "session.replace", json!({ "id": old }));
    let new = s["id"].as_str().unwrap().to_string();
    assert_ne!(new, old);
    assert_eq!(ids(&mut h), [first.clone(), new.clone(), last.clone()], "it takes the old one's place");
    assert_eq!(s["name"], "claude", "a fresh name: {s}");
    assert_eq!(s["background"], true);
    assert_eq!(s["cwd"].as_str(), dir.to_str());
    assert_eq!(s["agent_args"], json!(["--model", "opus"]));
    let args = argv(&mut h, &new);
    assert!(args.windows(2).any(|w| w == ["--model", "opus"]), "{args:?}");
    assert!(!args.iter().any(|a| a == "go"), "the first prompt isn't sent again: {args:?}");
    let _ = std::fs::remove_file(bin);
}

#[test]
fn a_shell_is_replaced_and_an_agent_needs_force_while_it_works() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let old = open_sh(&mut h);
    let mut a = d.agent(Some(&old));
    call(&mut a, "agent.hook", json!({ "agent": "claude", "event": "UserPromptSubmit", "payload": { "prompt": "fix the tests" } }));
    let e = call_err(&mut a, "session.replace", json!({ "id": old }));
    assert!(e.message.contains("force"), "{e:?}");
    let s = call(&mut h, "session.replace", json!({ "id": old }));
    assert_eq!(s["kind"], "shell");
    assert_eq!(s["command"], json!(["/bin/sh"]));
    assert_eq!(ids(&mut h), [s["id"].as_str().unwrap().to_string()]);
}
