//! The real midnad binary leaves no zombies: a command still running when a restart execs the
//! new image has no owner left to wait for it, and the daemon reaps it once it exits. Runs on a
//! temp MIDNA_HOME.
use midna_proto::Client;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Bin {
    home: PathBuf,
    child: Child,
}

impl Bin {
    fn start() -> Bin {
        let home = PathBuf::from(format!("/tmp/midna-reap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        // A copy, so a concurrent `cargo build` can't swap it under the restart's execv.
        std::fs::copy(env!("CARGO_BIN_EXE_midnad"), home.join("v1")).unwrap();
        let log = std::fs::File::create(home.join("daemon.log")).unwrap();
        let child = Command::new(home.join("v1"))
            .args(["--foreground", "--home"])
            .arg(&home)
            .env_remove("MIDNA_SOCKET")
            .env_remove("MIDNA_SESSION")
            .env_remove("MIDNA_SIGTERM")
            .env("MIDNA_APP_PATH", std::env::current_exe().unwrap())
            .env("MIDNA_NO_GH", "1")
            .env("MIDNA_NOTIFY_SYSTEM", "0")
            .env("MIDNA_UPGRADE_WATCHDOG_SECS", "5")
            .env("MIDNA_DEBUG_LOAD", "0")
            .env("MIDNA_REAP_SECS", "1")
            .env("SHELL", "/bin/sh")
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        let b = Bin { home, child };
        wait_for(10, "daemon socket", || Client::connect(b.socket()).ok());
        b
    }

    fn socket(&self) -> PathBuf {
        self.home.join("midnad.sock")
    }

    fn human(&self) -> Client {
        wait_for(15, "daemon connection", || {
            let mut c = Client::connect(self.socket()).ok()?;
            c.set_caller(None);
            c.call_value("daemon.info", json!({})).ok()?;
            Some(c)
        })
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.home.join("daemon.log")).unwrap_or_default()
    }
}

impl Drop for Bin {
    fn drop(&mut self) {
        let pid = Client::connect(self.socket()).ok().and_then(|mut c| {
            c.set_caller(None);
            let pid = c.call_value("daemon.info", json!({})).ok()?["pid"].as_i64();
            let _ = c.call_value("daemon.stop", json!({}));
            pid
        });
        let t0 = Instant::now();
        while self.socket().exists() && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(50));
        }
        if let Some(p) = pid {
            unsafe { libc::kill(p as i32, libc::SIGKILL) };
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn call(c: &mut Client, m: &str, p: Value) -> Value {
    c.call_value(m, p).unwrap_or_else(|e| panic!("{m} failed: {e}"))
}

fn wait_for<T>(secs: u64, what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let t0 = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        if t0.elapsed() > Duration::from_secs(secs) {
            panic!("timed out waiting for {what}");
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

/// `parent`'s children: (pid, ps state).
fn children(parent: i32) -> Vec<(i32, String)> {
    let out = Command::new("/bin/ps").args(["-axo", "pid=,ppid=,stat="]).output().unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            (f.len() == 3 && f[1].parse() == Ok(parent)).then(|| (f[0].parse().unwrap(), f[2].to_string()))
        })
        .collect()
}

#[test]
fn a_command_left_running_across_a_restart_is_reaped_when_it_exits() {
    let d = Bin::start();
    let mut h = d.human();
    let daemon = call(&mut h, "daemon.info", json!({}))["pid"].as_i64().unwrap() as i32;
    let dir = d.home.join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let p = call(&mut h, "project.add", json!({ "path": dir.canonicalize().unwrap() }));
    let action = json!({ "kind": "run_command", "project_id": p["id"], "command": "sleep 3; true", "headless": true });
    call(&mut h, "trigger.add", json!({ "name": "Slow", "source": "local", "event": "hook.Stop", "action": action, "enabled": true }));
    let s = call(&mut h, "session.open", json!({ "kind": "shell", "cwd": "/tmp", "command": ["/bin/sleep", "300"], "cols": 80, "rows": 24 }));
    let sid = s["id"].as_str().unwrap().to_string();
    let before: Vec<i32> = children(daemon).into_iter().map(|c| c.0).collect();

    // The trigger's command starts; the restart execs a new image while it runs.
    let mut a = Client::connect(d.socket()).unwrap().as_agent(None);
    call(&mut a, "agent.hook", json!({ "agent": "claude", "event": "Stop", "session": sid, "payload": {} }));
    let cmd = wait_for(10, "the trigger's command", || children(daemon).into_iter().map(|c| c.0).find(|p| !before.contains(p)));
    call(&mut Client::connect(d.socket()).unwrap().as_agent(None), "daemon.restart", json!({}));
    wait_for(20, "restarted image", || (d.log().matches("listening on").count() >= 2).then_some(()));
    assert!(children(daemon).iter().any(|c| c.0 == cmd), "the command should outlive the old image:\n{}", d.log());

    // Once it exits, it's reaped: no zombie stays behind.
    wait_for(20, "the command reaped", || (!children(daemon).iter().any(|c| c.0 == cmd)).then_some(()));
    assert!(d.log().contains(&format!("(pid {cmd})")), "no reap logged:\n{}", d.log());
    let zombies: Vec<_> = children(daemon).into_iter().filter(|c| c.1.starts_with('Z')).collect();
    assert!(zombies.is_empty(), "zombies under the daemon: {zombies:?}\n{}", d.log());
}
