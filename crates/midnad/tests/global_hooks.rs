//! hooks.status / preview / install / uninstall, and the either/or with per-launch hooks.
mod common;
use common::*;
use serde_json::{Value, json};

/// A fake agent that prints whether midna added its own hooks, then waits.
fn fake_agent(home: &std::path::Path) -> String {
    let bin = home.with_extension("agent.sh");
    std::fs::write(&bin, "#!/bin/sh\necho \"ARGS:$*\"\necho \"INJECTED:${MIDNA_HOOKS_INJECTED:-no}\"\nsleep 30\n").unwrap();
    std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    bin.to_string_lossy().into_owned()
}

fn launch(d: &TestDaemon, agent: &str) -> String {
    let mut h = d.human();
    let s = call(&mut h, "session.open", json!({ "kind": "agent", "agent": agent, "cwd": "/tmp", "cols": 1000 }));
    let sid = s["id"].as_str().unwrap().to_string();
    wait_for(10, "fake agent output", || {
        let r = call(&mut h, "session.read", json!({ "id": sid }));
        let text = r["text"].as_str().unwrap_or_default().to_string();
        text.contains("INJECTED:").then_some(text)
    })
}

#[test]
fn install_flips_per_launch_hooks_and_uninstall_restores() {
    let pre = std::path::PathBuf::from(format!("/tmp/midna-gh-{}", std::process::id()));
    let bin = fake_agent(&pre);
    let d = TestDaemon::start_with(move |c| c.agent_bin = Some(bin));
    let claude_dir = d.home.join("agent-config/claude");
    let codex_dir = d.home.join("agent-config/codex");
    let mut h = d.human();

    // Neither agent set up on this "Mac".
    let st = call(&mut h, "hooks.status", json!({}));
    assert_eq!(st["claude"]["state"], "unavailable");
    assert_eq!(st["claude"]["per_session"], true);

    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::create_dir_all(&codex_dir).unwrap();
    let user = "{\n  \"model\": \"opus\",\n  \"hooks\": {\n    \"Stop\": [\n      { \"hooks\": [{ \"type\": \"command\", \"command\": \"/u/stop\" }] }\n    ]\n  }\n}\n";
    std::fs::write(claude_dir.join("settings.json"), user).unwrap();
    std::fs::write(codex_dir.join("config.toml"), "notify = [\"/apps/Other\", \"turn-ended\"]\nmodel = \"x\"\n").unwrap();
    assert_eq!(call(&mut h, "hooks.status", json!({}))["claude"]["state"], "not_installed");

    // Before installing, agents midna starts carry midna's own hooks.
    let out = launch(&d, "claude");
    assert!(out.contains("claude-settings.json") && out.contains("INJECTED:1"), "{out}");

    // Preview writes nothing.
    let p = call(&mut h, "hooks.preview", json!({}));
    assert_eq!(p["files"].as_array().unwrap().len(), 2);
    assert!(p["files"][0]["lines"].as_array().unwrap().iter().any(|l| l["op"] == "+"));
    assert_eq!(std::fs::read_to_string(claude_dir.join("settings.json")).unwrap(), user);

    // An agent may not install them.
    let e = call_err(&mut d.agent(None), "hooks.install", json!({}));
    assert_eq!(e.code, midna_proto::error::HUMAN_ONLY, "{e:?}");

    let st = call(&mut h, "hooks.install", json!({}));
    assert_eq!(st["claude"]["state"], "current", "{st}");
    assert_eq!(st["codex"]["state"], "current", "{st}");
    assert_eq!(st["claude"]["per_session"], false);
    let written: Value = serde_json::from_str(&std::fs::read_to_string(claude_dir.join("settings.json")).unwrap()).unwrap();
    assert_eq!(written["hooks"]["Stop"][0]["hooks"][0]["command"], "/u/stop", "the user's hook stays first");
    assert!(claude_dir.join("settings.json.before-midna").exists());
    let toml = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
    assert!(toml.contains("midna-notify") && toml.contains("/apps/Other"), "{toml}");

    // Now midna-started agents rely on the global hooks: no second set.
    let out = launch(&d, "claude");
    assert!(out.contains("claude-settings-base.json") && out.contains("INJECTED:no"), "{out}");
    let out = launch(&d, "codex");
    assert!(!out.contains("notify=") && out.contains("INJECTED:no"), "{out}");

    // Someone edits the file: stale, and midna injects again.
    let mut v = written.clone();
    v["hooks"].as_object_mut().unwrap().remove("PreToolUse");
    std::fs::write(claude_dir.join("settings.json"), serde_json::to_string_pretty(&v).unwrap()).unwrap();
    let st = call(&mut h, "hooks.status", json!({}));
    assert_eq!(st["claude"]["state"], "stale");
    assert!(st["claude"]["detail"].as_str().unwrap().contains("PreToolUse"));
    let out = launch(&d, "claude");
    assert!(out.contains("INJECTED:1"), "{out}");

    // Uninstall gives the user's files back.
    let st = call(&mut h, "hooks.uninstall", json!({}));
    assert_eq!(st["claude"]["state"], "not_installed");
    let back: Value = serde_json::from_str(&std::fs::read_to_string(claude_dir.join("settings.json")).unwrap()).unwrap();
    assert_eq!(back, serde_json::from_str::<Value>(user).unwrap());
    let toml = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
    assert!(toml.contains("\"/apps/Other\"") && !toml.contains("midna-notify"), "{toml}");
    let _ = std::fs::remove_file(pre.with_extension("agent.sh"));
}

#[test]
fn unparseable_settings_are_never_rewritten() {
    let d = TestDaemon::start();
    let claude_dir = d.home.join("agent-config/claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let jsonc = "{\n  // my comment\n  \"model\": \"opus\"\n}\n";
    std::fs::write(claude_dir.join("settings.json"), jsonc).unwrap();
    let mut h = d.human();
    assert_eq!(call(&mut h, "hooks.status", json!({}))["claude"]["state"], "error");
    let e = call_err(&mut h, "hooks.install", json!({ "agents": ["claude"] }));
    assert!(e.message.contains("won't rewrite"), "{e:?}");
    assert_eq!(std::fs::read_to_string(claude_dir.join("settings.json")).unwrap(), jsonc);
}
