//! A `claude` typed into a midna shell terminal runs under midna (`midna shim`, setting
//! agents.adopt_typed): the real `midna` binary and midnad's shim against an in-process daemon.
//! The "claude" and "codex" are shell scripts that record their argv, answer `--help` with the
//! real help texts (Claude Code 2.1.289, Codex 0.160.0), exit 0 on SIGHUP like the agents, and
//! exit 3 once a `quit` file appears (`mcp …` and `exec …` exit at once).
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

static N: AtomicU32 = AtomicU32::new(0);

struct D {
    handle: Option<midnad::Handle>,
    home: PathBuf,
    fake: PathBuf,
}

impl D {
    fn start() -> D {
        let n = format!("{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed));
        let home = PathBuf::from(format!("/tmp/midna-a-{n}"));
        let fake = PathBuf::from(format!("/tmp/midna-a-{n}-bin"));
        for d in [&home, &fake] {
            let _ = std::fs::remove_dir_all(d);
        }
        std::fs::create_dir_all(&fake).unwrap();
        for (agent, help) in [("claude", midna_proto::agent_cli::BUILTIN_CLAUDE_HELP), ("codex", midna_proto::agent_cli::BUILTIN_CODEX_HELP)] {
            std::fs::write(fake.join(format!("{agent}-help.txt")), help).unwrap();
            let script = format!(
                "#!/bin/sh\n\
                 case \"$1\" in --help) cat '{d}/{agent}-help.txt'; exit 0;; --version) echo 'FAKE {agent} 1.0'; exit 0;; esac\n\
                 printf '%s\\n' \"$*\" >> '{d}/{agent}-argv'\n\
                 pwd -P >> '{d}/{agent}-cwd'\n\
                 case \"$1\" in mcp|exec) exit 0;; esac\n\
                 echo FAKE-UP\n\
                 trap 'exit 0' HUP\n\
                 while [ ! -f '{d}/quit' ]; do sleep 0.1; done\n\
                 exit 3\n",
                d = fake.display()
            );
            let bin = fake.join(agent);
            std::fs::write(&bin, script).unwrap();
            std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        }
        let mut cfg = midnad::Config::for_home(home.clone());
        cfg.app_path = Some(std::env::current_exe().unwrap().to_string_lossy().into_owned());
        cfg.cli_path = env!("CARGO_BIN_EXE_midna").into();
        D { handle: Some(midnad::start(cfg).unwrap()), home, fake }
    }

    fn human(&self) -> midna_proto::Client {
        let mut c = midna_proto::Client::connect(self.home.join("midnad.sock")).unwrap();
        c.set_caller(None);
        c
    }

    fn launches(&self) -> Vec<String> {
        self.launches_of("claude")
    }

    fn launches_of(&self, agent: &str) -> Vec<String> {
        std::fs::read_to_string(self.fake.join(format!("{agent}-argv"))).unwrap_or_default().lines().map(str::to_string).collect()
    }

    /// A shell terminal that runs `line` with the fake claude on PATH behind midna's shim.
    fn shell(&self, h: &mut midna_proto::Client, line: &str) -> String {
        let script = format!("PATH=\"$MIDNA_SHIMS:{}:$PATH\"; export PATH; {line}; echo SHELL-BACK $?; sleep 30", self.fake.display());
        let s = call(h, "session.open", json!({ "kind": "shell", "command": ["/bin/sh", "-c", script], "cwd": "/tmp" }));
        s["id"].as_str().unwrap().to_string()
    }
}

impl Drop for D {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            h.shutdown();
        }
        let _ = std::fs::remove_dir_all(&self.home);
        let _ = std::fs::remove_dir_all(&self.fake);
    }
}

fn call(c: &mut midna_proto::Client, m: &str, p: Value) -> Value {
    c.call_value(m, p).unwrap_or_else(|e| panic!("{m} failed: {e}"))
}

fn wait_for<T>(secs: u64, what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let t0 = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(t0.elapsed() < Duration::from_secs(secs), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn screen(h: &mut midna_proto::Client, sid: &str) -> String {
    call(h, "session.read", json!({ "id": sid, "screen": true })).to_string()
}

#[test]
fn typed_claude_runs_under_midna_and_restarts_in_the_same_shell() {
    let d = D::start();
    let mut h = d.human();
    let sid = d.shell(&mut h, "claude --model opus 'hello there'");
    wait_for(10, "fake claude started", || (d.launches().len() == 1).then_some(()));

    // midna's flags first, then the arguments as typed; the terminal is an agent terminal now.
    let first = &d.launches()[0];
    assert!(first.contains("--settings ") && first.ends_with("--model opus hello there"), "{first}");
    let s = call(&mut h, "session.get", json!({ "id": sid }));
    assert_eq!((s["kind"].as_str(), s["agent"].as_str()), (Some("shell"), Some("claude")), "{s}");
    assert!(s["adopted"]["pid"].as_i64().is_some(), "{s}");

    // Claude reports its conversation through midna's hooks (sent here by hand).
    let mut a = midna_proto::Client::connect(d.home.join("midnad.sock")).unwrap().as_agent(Some(sid.clone()));
    call(&mut a, "agent.hook", json!({ "agent": "claude", "event": "SessionStart", "payload": { "session_id": "conv-9", "source": "startup" } }));

    // Restart: the same conversation, inside the same shell, without the first prompt.
    let r = call(&mut h, "session.restart", json!({ "id": sid }));
    assert_eq!(r["status"]["reason"], "restarted (resumed)", "{r}");
    wait_for(10, "relaunch", || (d.launches().len() == 2).then_some(()));
    let second = &d.launches()[1];
    assert!(second.ends_with("--model opus --resume conv-9") && !second.contains("hello there"), "{second}");
    assert!(!screen(&mut h, &sid).contains("SHELL-BACK"), "the shell never got control back");

    // Claude exits on its own: its exit code reaches the shell, and the terminal is a shell again.
    std::fs::write(d.fake.join("quit"), "").unwrap();
    wait_for(10, "back at the shell", || screen(&mut h, &sid).contains("SHELL-BACK 3").then_some(()));
    let s = wait_for(5, "released", || {
        let s = call(&mut h, "session.get", json!({ "id": sid }));
        s["adopted"].is_null().then_some(s)
    });
    assert!(s["agent"].is_null() && s["agent_info"].is_null(), "{s}");
}

#[test]
fn anything_but_an_interactive_session_runs_as_typed() {
    let d = D::start();
    let mut h = d.human();
    let sid = d.shell(&mut h, "claude --version; claude mcp list");
    wait_for(10, "both ran", || (d.launches().len() == 1).then_some(()));
    assert_eq!(d.launches()[0], "mcp list", "a subcommand gets exactly what was typed");
    wait_for(10, "back at the shell", || screen(&mut h, &sid).contains("SHELL-BACK").then_some(()));
    assert!(screen(&mut h, &sid).contains("FAKE claude 1.0"));
    assert!(call(&mut h, "session.get", json!({ "id": sid }))["adopted"].is_null());

    // Off: even an interactive session runs as typed.
    call(&mut h, "settings.set", json!({ "key": "agents.adopt_typed", "value": false }));
    let sid = d.shell(&mut h, "claude hi");
    wait_for(10, "ran", || (d.launches().len() == 2).then_some(()));
    assert_eq!(d.launches()[1], "hi");
    assert!(call(&mut h, "session.get", json!({ "id": sid }))["adopted"].is_null());
}

#[test]
fn typed_codex_runs_under_midna_and_resumes_its_thread() {
    let d = D::start();
    let mut h = d.human();
    let sid = d.shell(&mut h, "codex exec 'one shot'; codex -m gpt-5 'fix it'");
    wait_for(10, "fake codex started", || (d.launches_of("codex").len() == 2).then_some(()));
    let l = d.launches_of("codex");
    assert_eq!(l[0], "exec one shot", "exec runs as typed");
    assert!(l[1].contains("notify=") && l[1].ends_with("-m gpt-5 fix it"), "{}", l[1]);
    let s = call(&mut h, "session.get", json!({ "id": sid }));
    assert_eq!(s["agent"], "codex", "{s}");

    // Codex names its thread when a turn ends (notify).
    let mut a = midna_proto::Client::connect(d.home.join("midnad.sock")).unwrap().as_agent(Some(sid.clone()));
    call(&mut a, "agent.hook", json!({ "agent": "codex", "event": "agent-turn-complete", "payload": { "type": "agent-turn-complete", "thread-id": "019a-thread", "input-messages": ["fix it"], "last-assistant-message": "done" } }));
    call(&mut h, "session.restart", json!({ "id": sid }));
    wait_for(10, "relaunch", || (d.launches_of("codex").len() == 3).then_some(()));
    let r = &d.launches_of("codex")[2];
    assert!(r.starts_with("resume ") && r.ends_with("-m gpt-5 019a-thread") && !r.contains("fix it"), "{r}");

    std::fs::write(d.fake.join("quit"), "").unwrap();
    wait_for(10, "back at the shell", || screen(&mut h, &sid).contains("SHELL-BACK 3").then_some(()));
    wait_for(5, "released", || call(&mut h, "session.get", json!({ "id": sid }))["adopted"].is_null().then_some(()));

    // `codex resume --last` opens the TUI: adopted too.
    let _ = std::fs::remove_file(d.fake.join("quit"));
    let sid = d.shell(&mut h, "codex resume --last");
    wait_for(10, "resume picker", || (d.launches_of("codex").len() == 4).then_some(()));
    assert_eq!(call(&mut h, "session.get", json!({ "id": sid }))["agent"], "codex");
}

#[test]
fn the_binary_an_alias_names_is_the_one_midna_runs() {
    let d = D::start();
    let mut h = d.human();
    // What midna's shell integration turns `alias claude=~/elsewhere/claude` into.
    let elsewhere = d.fake.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::copy(d.fake.join("claude"), elsewhere.join("claude")).unwrap();
    let line = format!("PATH=/usr/bin:/bin MIDNA_SHIM_REAL='{}' \"$MIDNA_SHIMS/claude\" hi", elsewhere.join("claude").display());
    let script = format!("{line}; echo SHELL-BACK $?; sleep 30");
    let sid = call(&mut h, "session.open", json!({ "kind": "shell", "command": ["/bin/sh", "-c", script], "cwd": "/tmp" }))["id"].as_str().unwrap().to_string();
    wait_for(10, "adopted", || call(&mut h, "session.get", json!({ "id": sid }))["adopted"]["bin"].as_str().map(str::to_string)).ends_with("elsewhere/claude")
        .then_some(())
        .expect("runs the aliased binary");
    wait_for(10, "launched", || (d.launches().len() == 1).then_some(()));
    assert!(d.launches()[0].ends_with(" hi"), "{:?}", d.launches());
}

#[test]
fn a_restart_comes_back_in_the_worktree_the_agent_moved_into() {
    let d = D::start();
    let mut h = d.human();
    let sid = d.shell(&mut h, "codex --worktree");
    wait_for(10, "fake codex started", || (d.launches_of("codex").len() == 1).then_some(()));
    let mut a = midna_proto::Client::connect(d.home.join("midnad.sock")).unwrap().as_agent(Some(sid.clone()));
    // Codex keeps its process where it started and reports the worktree as its cwd.
    let wt = d.fake.join("codex-wt");
    std::fs::create_dir_all(&wt).unwrap();
    call(&mut a, "agent.hook", json!({ "agent": "codex", "event": "agent-turn-complete", "payload": { "type": "agent-turn-complete", "thread-id": "019a-wt", "cwd": wt, "input-messages": ["hi"] } }));
    call(&mut h, "session.restart", json!({ "id": sid }));
    wait_for(10, "relaunch", || (d.launches_of("codex").len() == 2).then_some(()));
    let r = &d.launches_of("codex")[1];
    assert!(r.starts_with("resume ") && r.ends_with("019a-wt") && !r.contains("--worktree"), "no second worktree: {r}");
    let cwds: Vec<String> = std::fs::read_to_string(d.fake.join("codex-cwd")).unwrap().lines().map(str::to_string).collect();
    assert_eq!(cwds[1], std::fs::canonicalize(&wt).unwrap().to_string_lossy(), "{cwds:?}");
}
