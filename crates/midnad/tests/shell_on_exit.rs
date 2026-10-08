//! An agent midna started that exits leaves a login shell behind in the same terminal
//! (`agents.shell_on_exit`), below the agent's last screen.
mod common;
use common::*;
use midna_proto::Client;
use serde_json::{Value, json};

/// A fake agent that prints a line and exits.
fn fake_agent(tag: &str) -> String {
    let bin = std::path::PathBuf::from(format!("/tmp/midna-soe-{}-{tag}.sh", std::process::id()));
    std::fs::write(&bin, "#!/bin/sh\necho AGENT-BYE\nsleep 0.3\n").unwrap();
    std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    bin.to_string_lossy().into_owned()
}

fn start(tag: &str) -> TestDaemon {
    // The shell left behind is $SHELL: keep it free of the user's dotfiles.
    unsafe { std::env::set_var("SHELL", "/bin/sh") };
    let bin = fake_agent(tag);
    TestDaemon::start_with(move |c| c.agent_bin = Some(bin))
}

fn get(h: &mut Client, sid: &str) -> Value {
    call(h, "session.get", json!({ "id": sid }))
}

#[test]
fn an_exited_agent_becomes_a_shell_below_its_last_screen() {
    let d = start("on");
    let mut h = d.human();
    let sid = call(&mut h, "session.open", json!({ "kind": "agent", "agent": "claude", "cwd": "/tmp" }))["id"].as_str().unwrap().to_string();
    let s = wait_for(10, "the shell", || {
        let s = get(&mut h, &sid);
        (s["kind"] == "shell" && !s["pid"].is_null()).then_some(s)
    });
    assert!(s["agent"].is_null(), "{s}");
    assert_eq!(s["command"][1], "-l", "{s}");
    assert_eq!(s["status"]["state"], "idle", "{s}");
    assert_eq!(s["status"]["reason"], "claude exited", "{s}");
    assert!(read(&mut h, &sid).contains("AGENT-BYE"), "the agent's screen carries over");
    call(&mut h, "session.input", json!({ "id": sid, "text": "echo SHELL-$((40+2))", "enter": true }));
    wait_for(10, "the shell to answer", || read(&mut h, &sid).contains("SHELL-42").then_some(()));
    assert!(read(&mut h, &sid).contains("AGENT-BYE"));
}

#[test]
fn off_leaves_the_agent_exited() {
    let d = start("off");
    let mut h = d.human();
    call(&mut h, "settings.set", json!({ "key": "agents.shell_on_exit", "value": false }));
    let sid = call(&mut h, "session.open", json!({ "kind": "agent", "agent": "claude", "cwd": "/tmp" }))["id"].as_str().unwrap().to_string();
    wait_for(10, "exit", || (get(&mut h, &sid)["status"]["state"] == "exited").then_some(()));
    std::thread::sleep(std::time::Duration::from_millis(500));
    let s = get(&mut h, &sid);
    assert_eq!(s["kind"], "agent", "{s}");
    assert!(s["pid"].is_null(), "{s}");
}
