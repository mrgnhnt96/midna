//! Test harness: an in-process midnad on a temp MIDNA_HOME. This test binary is configured as
//! the "GUI", so plain connections are human; `agent()` connections downgrade themselves.
#![allow(dead_code)]
use midna_proto::{Client, ClientError, RpcError};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

static N: AtomicU32 = AtomicU32::new(0);

pub struct TestDaemon {
    handle: Option<midnad::Handle>,
    pub home: PathBuf,
}

impl TestDaemon {
    pub fn start() -> TestDaemon {
        TestDaemon::start_with(|_| {})
    }

    /// Like `start`, with the config adjusted first (e.g. `agent_bin`).
    pub fn start_with(f: impl FnOnce(&mut midnad::Config)) -> TestDaemon {
        // Short path: Unix socket paths are limited to 104 bytes on macOS.
        let home = PathBuf::from(format!("/tmp/midna-t-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&home);
        let mut cfg = midnad::Config::for_home(home.clone());
        cfg.app_path = Some(std::env::current_exe().unwrap().to_string_lossy().into_owned());
        f(&mut cfg);
        let handle = midnad::start(cfg).expect("start daemon");
        TestDaemon { handle: Some(handle), home }
    }

    /// Stop midnad and start a new one on the same home (state.json, the event log).
    pub fn restart(&mut self) {
        if let Some(h) = self.handle.take() {
            h.shutdown();
        }
        let mut cfg = midnad::Config::for_home(self.home.clone());
        cfg.app_path = Some(std::env::current_exe().unwrap().to_string_lossy().into_owned());
        self.handle = Some(midnad::start(cfg).expect("restart daemon"));
    }

    pub fn daemon(&self) -> std::sync::Arc<midnad::daemon::Daemon> {
        self.handle.as_ref().expect("running").daemon.clone()
    }

    pub fn socket(&self) -> PathBuf {
        self.home.join("midnad.sock")
    }

    pub fn human(&self) -> Client {
        let mut c = Client::connect(self.socket()).expect("connect");
        c.set_caller(None);
        c
    }

    pub fn agent(&self, session: Option<&str>) -> Client {
        Client::connect(self.socket()).expect("connect").as_agent(session.map(str::to_string))
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            h.shutdown();
        }
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

pub fn call(c: &mut Client, m: &str, p: Value) -> Value {
    c.call_value(m, p).unwrap_or_else(|e| panic!("{m} failed: {e}"))
}

pub fn call_err(c: &mut Client, m: &str, p: Value) -> RpcError {
    match c.call_value(m, p) {
        Err(ClientError::Rpc(e)) => e,
        other => panic!("{m}: expected an RPC error, got {other:?}"),
    }
}

/// Poll `f` until it returns Some, or panic after `secs`.
pub fn wait_for<T>(secs: u64, what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let t0 = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        if t0.elapsed() > Duration::from_secs(secs) {
            panic!("timed out waiting for {what}");
        }
        std::thread::sleep(Duration::from_millis(30));
    }
}

/// Open a plain /bin/sh session in /tmp (no login shell, no user dotfiles).
pub fn open_sh(c: &mut Client) -> String {
    let s = call(c, "session.open", json!({ "kind": "shell", "cwd": "/tmp", "command": ["/bin/sh"] }));
    s["id"].as_str().unwrap().to_string()
}

pub fn read(c: &mut Client, id: &str) -> String {
    call(c, "session.read", json!({ "id": id, "lines": 200 }))["text"].as_str().unwrap().to_string()
}
