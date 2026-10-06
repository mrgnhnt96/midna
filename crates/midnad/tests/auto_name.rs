//! Terminals that name themselves (`terminal.auto_name`): from the agent's title, the prompt
//! or the folder, and never over a name someone chose.
mod common;
use common::*;
use midna_proto::Client;
use serde_json::{Value, json};

/// A fake agent that turns each line typed into it into terminal output (`\033]0;…\007` sets
/// its title), like an agent rewriting its title as it works.
fn fake_agent(tag: &str) -> String {
    let bin = std::path::PathBuf::from(format!("/tmp/midna-an-{}-{tag}.sh", std::process::id()));
    std::fs::write(&bin, "#!/bin/sh\nstty -echo\nwhile read -r l; do printf '%b' \"$l\"; done\n").unwrap();
    std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    bin.to_string_lossy().into_owned()
}

fn name(h: &mut Client, sid: &str) -> String {
    call(h, "session.get", json!({ "id": sid }))["name"].as_str().unwrap().to_string()
}

fn set_title(h: &mut Client, sid: &str, title: &str) {
    call(h, "session.input", json!({ "id": sid, "text": format!("\\033]0;{title}\\007"), "enter": true }));
    wait_for(5, "title", || (call(h, "session.get", json!({ "id": sid }))["title"] == title).then_some(()));
}

fn prompt(d: &TestDaemon, sid: &str, text: &str) {
    call(&mut d.agent(Some(sid)), "agent.hook", json!({ "agent": "claude", "event": "UserPromptSubmit", "payload": { "prompt": text } }));
}

fn open_claude(h: &mut Client) -> String {
    call(h, "session.open", json!({ "kind": "agent", "agent": "claude", "cwd": "/tmp" }))["id"].as_str().unwrap().to_string()
}

#[test]
fn agents_take_their_own_summary_then_keep_a_chosen_name() {
    let bin = fake_agent("summary");
    let d = TestDaemon::start_with(move |c| c.agent_bin = Some(bin));
    let mut h = d.human();
    let sid = open_claude(&mut h);
    assert_eq!(name(&mut h, &sid), "claude");

    // Before the agent has a summary, the prompt names it.
    set_title(&mut h, &sid, "✳ Claude Code");
    prompt(&d, &sid, "can you fix the login bug in auth.rs");
    assert_eq!(name(&mut h, &sid), "Fix login bug in auth.rs");
    // The agent's summary wins over the prompt, and follows it as it changes.
    set_title(&mut h, &sid, "◑ Fix login redirect");
    assert_eq!(name(&mut h, &sid), "Fix login redirect");
    let ev = call(&mut h, "events.list", json!({ "filter": { "kinds": ["session.renamed"] } }));
    let last = ev.as_array().unwrap().last().cloned().unwrap_or(Value::Null);
    assert_eq!((last["data"]["auto"].as_str(), last["data"]["old"].as_str()), (Some("agent"), Some("Fix login bug in auth.rs")));
    set_title(&mut h, &sid, "✳ Add redirect tests");
    assert_eq!(name(&mut h, &sid), "Add redirect tests");
    // A later prompt doesn't override the agent's summary.
    prompt(&d, &sid, "now update the changelog for the release");
    assert_eq!(name(&mut h, &sid), "Add redirect tests");

    // A name someone chooses stays.
    call(&mut h, "session.rename", json!({ "id": sid, "name": "Login" }));
    set_title(&mut h, &sid, "◑ Something else entirely");
    assert_eq!(name(&mut h, &sid), "Login");
    // Renaming it back to its default lets midna name it again.
    call(&mut h, "session.rename", json!({ "id": sid, "name": "claude" }));
    set_title(&mut h, &sid, "◑ Back to auto");
    assert_eq!(name(&mut h, &sid), "Back to auto");
}

#[test]
fn first_keeps_a_name_and_off_names_nothing() {
    let bin = fake_agent("first");
    let d = TestDaemon::start_with(move |c| c.agent_bin = Some(bin));
    let mut h = d.human();
    call(&mut h, "settings.set", json!({ "key": "terminal.auto_name_updates", "value": "first" }));
    let sid = open_claude(&mut h);
    set_title(&mut h, &sid, "◑ Fix login redirect");
    set_title(&mut h, &sid, "◑ Add redirect tests");
    assert_eq!(name(&mut h, &sid), "Fix login redirect");

    call(&mut h, "settings.set", json!({ "key": "terminal.auto_name", "value": "off" }));
    let other = open_claude(&mut h);
    set_title(&mut h, &other, "◑ Fix login redirect");
    prompt(&d, &other, "can you fix the login bug in auth.rs");
    assert_eq!(name(&mut h, &other), "claude");
}

#[test]
fn prompt_mode_ignores_the_title_and_context_mode_names_shells() {
    let bin = fake_agent("modes");
    let d = TestDaemon::start_with(move |c| c.agent_bin = Some(bin));
    let mut h = d.human();
    call(&mut h, "settings.set", json!({ "key": "terminal.auto_name", "value": "prompt" }));
    let sid = open_claude(&mut h);
    set_title(&mut h, &sid, "◑ Fix login redirect");
    assert_eq!(name(&mut h, &sid), "claude");
    prompt(&d, &sid, "Add a setting for auto names");
    assert_eq!(name(&mut h, &sid), "Add setting for auto names");

    // Plain shells keep their names unless the mode is context; switching names open ones.
    let dir = std::env::temp_dir().join(format!("midna-an-{}-proj", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sh = call(&mut h, "session.open", json!({ "kind": "shell", "cwd": dir, "command": ["/bin/sh"] }))["id"].as_str().unwrap().to_string();
    assert_eq!(name(&mut h, &sh), "sh");
    call(&mut h, "settings.set", json!({ "key": "terminal.auto_name", "value": "context" }));
    assert_eq!(name(&mut h, &sh), format!("midna-an-{}-proj", std::process::id()));
    // A name from a source the mode no longer uses gives way: the agent takes its folder's.
    assert_eq!(name(&mut h, &sid), "tmp");
}
