//! The real midnad binary, started the way launchd starts it (soft open-file limit 256), raises
//! that limit and keeps it across a graceful restart's execv. Terminals inherit the raised limit,
//! so a terminal's `ulimit -n` shows what the daemon has. Runs on a temp MIDNA_HOME.
use midna_proto::Client;
use serde_json::{Value, json};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const LAUNCHD_SOFT: u64 = 256;

struct Bin {
    home: PathBuf,
    child: Child,
}

impl Bin {
    fn start() -> Bin {
        let home = PathBuf::from(format!("/tmp/midna-fdd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        // A copy, so a concurrent `cargo build` can't swap it under the restart's execv.
        std::fs::copy(env!("CARGO_BIN_EXE_midnad"), home.join("v1")).unwrap();
        let log = std::fs::File::create(home.join("daemon.log")).unwrap();
        let mut cmd = Command::new(home.join("v1"));
        cmd.args(["--foreground", "--home"])
            .arg(&home)
            .env_remove("MIDNA_SOCKET")
            .env_remove("MIDNA_SESSION")
            .env_remove("MIDNA_SIGTERM")
            .env("MIDNA_APP_PATH", std::env::current_exe().unwrap())
            .env("MIDNA_NO_GH", "1")
            .env("MIDNA_NOTIFY_SYSTEM", "0")
            .env("MIDNA_UPGRADE_WATCHDOG_SECS", "5")
            .env("MIDNA_DEBUG_LOAD", "0")
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log);
        unsafe {
            cmd.pre_exec(|| {
                let mut r = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
                libc::getrlimit(libc::RLIMIT_NOFILE, &mut r);
                r.rlim_cur = LAUNCHD_SOFT as libc::rlim_t;
                libc::setrlimit(libc::RLIMIT_NOFILE, &r);
                Ok(())
            });
        }
        let b = Bin { home, child: cmd.spawn().unwrap() };
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

/// The soft limit a fresh terminal sees (the daemon's: terminals inherit it).
fn terminal_limit(c: &mut Client) -> String {
    let s = call(c, "session.open", json!({ "kind": "shell", "cwd": "/tmp", "command": ["/bin/sh", "-c", "echo NOFILE=$(ulimit -n); sleep 30"], "cols": 100, "rows": 30 }));
    let id = s["id"].as_str().unwrap().to_string();
    wait_for(10, "ulimit output", || {
        let t = call(c, "session.read", json!({ "id": id, "lines": 20 }))["text"].as_str().unwrap().to_string();
        t.lines().find_map(|l| l.trim().strip_prefix("NOFILE=").map(str::to_string))
    })
}

fn hard_limit() -> u64 {
    let mut r = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut r) };
    r.rlim_max
}

#[test]
fn a_started_daemon_raises_its_open_file_limit_and_keeps_it_across_a_restart() {
    let target = midnad::fd_limit::WANT.min(hard_limit());
    let d = Bin::start();
    let raised = format!("open-file limit {LAUNCHD_SOFT} -> {target}");
    assert!(d.log().contains(&raised), "no '{raised}' in the log:\n{}", d.log());
    let mut c = d.human();
    assert_eq!(terminal_limit(&mut c), target.to_string());

    let mut agent = Client::connect(d.socket()).unwrap().as_agent(None);
    call(&mut agent, "daemon.restart", json!({}));
    wait_for(20, "restarted image", || (d.log().matches("listening on").count() >= 2).then_some(()));
    let mut c = d.human();
    assert_eq!(terminal_limit(&mut c), target.to_string());
}
