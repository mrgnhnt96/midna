//! `session.open {agent_args, resume}`: the caller's own agent arguments, merged with midna's
//! per-launch flags, kept across a restart into the same conversation. The agent is a script
//! that prints its argv (one word per line) and whether midna's hooks were injected.
mod common;
use common::*;
use serde_json::{Value, json};

fn fake_agent(tag: &str) -> std::path::PathBuf {
    let bin = std::path::PathBuf::from(format!("/tmp/midna-aa-{}-{tag}.sh", std::process::id()));
    std::fs::write(&bin, "#!/bin/sh\nfor a; do echo \"ARG:$a\"; done\necho \"INJECTED:${MIDNA_HOOKS_INJECTED:-no}\"\nsleep 30\n").unwrap();
    std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    bin
}

/// The fake agent's argv and `MIDNA_HOOKS_INJECTED`, once it has printed them.
fn launched(h: &mut midna_proto::Client, sid: &str) -> (Vec<String>, String) {
    let text = wait_for(10, "fake agent output", || {
        let t = call(h, "session.read", json!({ "id": sid, "lines": 500 }))["text"].as_str().unwrap_or_default().to_string();
        t.contains("INJECTED:").then_some(t)
    });
    let (args, injected) = text.split_once("INJECTED:").unwrap();
    (args.lines().filter_map(|l| l.strip_prefix("ARG:")).map(str::to_string).collect(), injected.lines().next().unwrap_or_default().trim().to_string())
}

#[test]
fn claude_gets_the_callers_arguments_with_midnas_hooks_kept() {
    let bin = fake_agent("claude");
    let b = bin.to_string_lossy().into_owned();
    let d = TestDaemon::start_with(move |c| c.agent_bin = Some(b));
    let mut h = d.human();
    let dir = d.home.join("board");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("board.json"), json!({ "hooks": { "Stop": [{ "hooks": [{ "type": "command", "command": "/board/stop" }] }] } }).to_string()).unwrap();
    let args = ["--append-system-prompt", "You work card 12.", "--settings", "board.json", "--add-dir", "/tmp"];
    let s = call(&mut h, "session.open", json!({ "kind": "agent", "agent": "claude", "cwd": dir, "cols": 1000, "agent_args": args, "resume": "conv-7", "prompt": "go" }));
    let sid = s["id"].as_str().unwrap().to_string();
    assert_eq!(s["agent_args"], json!(args));
    let (argv, injected) = launched(&mut h, &sid);
    assert_eq!(injected, "1", "midna's hooks are in the merged settings: {argv:?}");
    let at = |f: &str| argv.iter().position(|a| a == f).unwrap_or_else(|| panic!("{f} missing from {argv:?}"));
    assert_eq!(argv.iter().filter(|a| *a == "--settings").count(), 1, "{argv:?}");
    assert_eq!(argv.iter().filter(|a| *a == "--append-system-prompt").count(), 1, "{argv:?}");
    assert_eq!(at("--settings"), at("--mcp-config") + 2, "--settings still ends --mcp-config");
    let merged: Value = serde_json::from_str(&std::fs::read_to_string(&argv[at("--settings") + 1]).unwrap()).unwrap();
    let stops: Vec<&str> = merged["hooks"]["Stop"].as_array().unwrap().iter().map(|e| e["hooks"][0]["command"].as_str().unwrap()).collect();
    assert!(stops.len() == 2 && stops[0].ends_with("hook claude") && stops[1] == "/board/stop", "{stops:?}");
    // One word per line: the joined text shows as its first line, then midna's hint.
    assert_eq!(argv[at("--append-system-prompt") + 1], "You work card 12.");
    assert_eq!(&argv[argv.len() - 6..], ["--add-dir", "/tmp", "--resume", "conv-7", "--", "go"]);

    // A restart into the conversation keeps the caller's arguments, without the prompt or the old id.
    let transcript = d.home.join("t.jsonl");
    std::fs::write(&transcript, "{}\n").unwrap();
    call(&mut d.agent(Some(&sid)), "agent.hook", json!({ "agent": "claude", "event": "SessionStart", "payload": { "session_id": "conv-8", "transcript_path": transcript } }));
    let r = call(&mut h, "session.restart", json!({ "id": sid }));
    assert_eq!(r["status"]["reason"], "restarted (resumed)", "{r}");
    let (argv, injected) = launched(&mut h, &sid);
    assert_eq!(injected, "1");
    assert!(argv.iter().any(|a| a.starts_with("You work card 12.")), "{argv:?}");
    assert_eq!(&argv[argv.len() - 4..], ["--add-dir", "/tmp", "--resume", "conv-8"], "{argv:?}");
    let _ = std::fs::remove_file(bin);
}

#[test]
fn codex_resume_and_notify_override() {
    let bin = fake_agent("codex");
    let b = bin.to_string_lossy().into_owned();
    let d = TestDaemon::start_with(move |c| c.agent_bin = Some(b));
    let mut h = d.human();
    let s = call(&mut h, "session.open", json!({ "kind": "agent", "agent": "codex", "cwd": "/tmp", "cols": 1000, "agent_args": ["-c", "notify=[\"/board/n\"]"], "resume": "th-1" }));
    let (argv, injected) = launched(&mut h, s["id"].as_str().unwrap());
    assert_eq!(injected, "1", "{argv:?}");
    assert_eq!((argv[0].as_str(), argv.last().unwrap().as_str()), ("resume", "th-1"), "{argv:?}");
    let notify: Vec<&String> = argv.iter().filter(|a| a.starts_with("notify=")).collect();
    assert!(notify.len() == 1 && notify[0].contains("midna-notify-launch") && notify[0].contains("/board/n"), "{argv:?}");
    let _ = std::fs::remove_file(bin);
}
