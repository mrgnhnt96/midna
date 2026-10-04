//! In-place upgrade / restart / stop against the real `midnad` binary (an upgrade execs, so it
//! can't run in-process). Ports `spikes/daemon-reexec/test.sh` and `test_flood.sh`.
//!
//! Every test runs its own daemon on a temp MIDNA_HOME; this test binary is the "GUI" (human).
use midna_proto::{Client, ClientError};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

static N: AtomicU32 = AtomicU32::new(0);

struct Bin {
    home: PathBuf,
    child: Child,
}

impl Bin {
    /// Copies of the built midnad as v1/v2 (so `cargo build` can't swap them mid-test and the
    /// watchdog has a stable fallback path), started from v1.
    fn start() -> Bin {
        let home = PathBuf::from(format!("/tmp/midna-u-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        for v in ["v1", "v2"] {
            std::fs::copy(env!("CARGO_BIN_EXE_midnad"), home.join(v)).unwrap();
        }
        let log = std::fs::File::create(home.join("daemon.log")).unwrap();
        let child = Command::new(home.join("v1"))
            .args(["--foreground", "--home"])
            .arg(&home)
            .env_remove("MIDNA_SOCKET")
            .env_remove("MIDNA_SESSION")
            .env("MIDNA_APP_PATH", std::env::current_exe().unwrap())
            .env("MIDNA_NO_GH", "1")
            .env("MIDNA_UPGRADE_WATCHDOG_SECS", "5")
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

    fn path(&self, name: &str) -> String {
        self.home.join(name).to_string_lossy().into_owned()
    }

    /// A fresh human connection, retried while the daemon is between images.
    fn human(&self) -> Client {
        wait_for(15, "daemon connection", || {
            let mut c = Client::connect(self.socket()).ok()?;
            c.set_caller(None);
            c.call_value("daemon.info", json!({})).ok()?;
            Some(c)
        })
    }

    fn info(&self) -> Value {
        call(&mut self.human(), "daemon.info", json!({}))
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.home.join("daemon.log")).unwrap_or_default()
    }

    /// Wait for the n-th `daemon.upgraded` event and return its data.
    fn wait_upgraded(&self, n: usize) -> Value {
        wait_for(20, "daemon.upgraded", || {
            let mut c = Client::connect(self.socket()).ok()?;
            c.set_caller(None);
            let v = c.call_value("events.list", json!({ "since_seq": 0, "limit": 1000, "filter": { "kinds": ["daemon.upgraded"] } })).ok()?;
            let evs = v.as_array()?.clone();
            (evs.len() >= n).then(|| evs[n - 1]["data"].clone())
        })
    }
}

impl Drop for Bin {
    fn drop(&mut self) {
        // An explicit stop hangs up every terminal; then make sure nothing is left.
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

fn open(c: &mut Client, kind: &str, argv: &[&str]) -> (String, i64) {
    let s = call(c, "session.open", json!({ "kind": kind, "cwd": "/tmp", "command": argv, "cols": 100, "rows": 30 }));
    (s["id"].as_str().unwrap().to_string(), s["pid"].as_i64().unwrap())
}

fn read(c: &mut Client, id: &str, lines: u32) -> String {
    call(c, "session.read", json!({ "id": id, "lines": lines }))["text"].as_str().unwrap().to_string()
}

fn screen(c: &mut Client, id: &str) -> String {
    call(c, "session.read", json!({ "id": id, "screen": true }))["text"].as_str().unwrap().to_string()
}

fn alive(pid: i64) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// A loop printing `C <n>` every 50ms; its HUP trap records any hangup in `hup_file`.
fn counter_cmd(hup_file: &str) -> String {
    format!("trap 'echo HUP >> {hup_file}' HUP; i=0; while :; do echo C $i; i=$((i+1)); sleep 0.05; done")
}

/// The `C <n>` numbers in a session's text; panics on any gap.
fn counts(text: &str) -> Vec<u64> {
    let v: Vec<u64> = text.lines().filter_map(|l| l.strip_prefix("C ")?.trim().parse().ok()).collect();
    for w in v.windows(2) {
        assert_eq!(w[1], w[0] + 1, "gap in the counter output: {} -> {}", w[0], w[1]);
    }
    v
}

fn session(c: &mut Client, id: &str) -> Value {
    call(c, "session.get", json!({ "id": id }))
}

#[test]
fn upgrade_keeps_shells_ids_screens_and_counting() {
    let d = Bin::start();
    let mut c = d.human();
    let pid0 = d.info()["pid"].as_i64().unwrap();
    let hup = d.path("hup");
    let (loop_id, loop_pid) = open(&mut c, "monitor", &["/bin/sh", "-c", &counter_cmd(&hup)]);
    let (sh_id, sh_pid) = open(&mut c, "shell", &["/bin/zsh", "-f"]);
    wait_for(10, "prompt", || screen(&mut c, &sh_id).contains('%').then_some(()));
    call(&mut c, "session.input", json!({ "id": sh_id, "text": "echo still-$((40+2))-here", "enter": false }));
    wait_for(10, "typed text", || screen(&mut c, &sh_id).contains("echo still-").then_some(()));
    wait_for(10, "counting", || (counts(&read(&mut c, &loop_id, 500)).len() > 10).then_some(()));
    std::thread::sleep(Duration::from_millis(300));
    let before = screen(&mut c, &sh_id);
    let ids_before: Vec<Value> = call(&mut c, "session.list", json!({})).as_array().unwrap().iter().map(|s| s["id"].clone()).collect();

    let r = call(&mut c, "daemon.upgrade", json!({ "binary_path": d.path("v2") }));
    assert_eq!(r["ok"], json!(true));
    assert_eq!(r["sessions"], json!(2));
    let up = d.wait_upgraded(1);
    assert_eq!(up["sessions_kept"], json!(2), "{up}");
    assert_eq!(up["fallback"], json!(false));
    assert!(up["to"]["binary"].as_str().unwrap().ends_with("/v2"), "{up}");

    let mut c = d.human();
    assert_eq!(d.info()["pid"].as_i64().unwrap(), pid0, "same pid after execv");
    let ids_after: Vec<Value> = call(&mut c, "session.list", json!({})).as_array().unwrap().iter().map(|s| s["id"].clone()).collect();
    assert_eq!(ids_after, ids_before, "same session ids");
    for (id, pid) in [(&loop_id, loop_pid), (&sh_id, sh_pid)] {
        let s = session(&mut c, id);
        assert_eq!(s["pid"].as_i64(), Some(pid), "{s}");
        assert!(!matches!(s["status"]["state"].as_str(), Some("exited" | "failed")), "{s}");
        assert!(alive(pid));
    }
    // Screen contents preserved exactly, typed-but-unsubmitted input included.
    assert_eq!(screen(&mut c, &sh_id), before);

    // The loop kept counting with no gap and no SIGHUP.
    let n0 = *counts(&read(&mut c, &loop_id, 2000)).last().unwrap();
    wait_for(10, "more counting", || (*counts(&read(&mut c, &loop_id, 2000)).last().unwrap() > n0 + 10).then_some(()));
    let all = counts(&read(&mut c, &loop_id, 5000));
    assert_eq!(all[0], 0, "the whole history is still there");
    assert!(!Path::new(&hup).exists(), "the counter got SIGHUP");

    // Still interactive: submit the line typed before the upgrade.
    call(&mut c, "session.input", json!({ "id": sh_id, "text": "", "enter": true }));
    wait_for(10, "command output", || screen(&mut c, &sh_id).lines().any(|l| l.trim() == "still-42-here").then_some(()));

    // And again, back to v1 (a second handoff from a resumed image).
    call(&mut c, "daemon.upgrade", json!({ "binary_path": d.path("v1") }));
    let up = d.wait_upgraded(2);
    assert_eq!(up["sessions_kept"], json!(2));
    let mut c = d.human();
    counts(&read(&mut c, &loop_id, 5000));
    call(&mut c, "session.input", json!({ "id": sh_id, "text": "echo second-$((1+1))", "enter": true }));
    wait_for(10, "second output", || screen(&mut c, &sh_id).lines().any(|l| l.trim() == "second-2").then_some(()));
    assert!(!Path::new(&hup).exists());
    assert!(d.log().contains("resumed 2 session(s)"));
}

#[test]
fn upgrade_during_flood_loses_nothing() {
    let d = Bin::start();
    let mut c = d.human();
    let (flood, _) = open(&mut c, "monitor", &["/bin/sh", "-c", "yes | head -c 50000000; echo; echo FLOOD-DONE"]);
    // Numbered lines, paced to span both upgrades. Few enough to all stay in the engine's
    // scrollback (libghostty's max_scrollback is a byte budget: ~800 rows at 100 columns).
    let (nums, _) = open(&mut c, "monitor", &["/bin/sh", "-c", "i=1; while [ $i -le 500 ]; do echo N $i; [ $((i % 10)) = 0 ] && sleep 0.05; i=$((i+1)); done; echo NUMS-DONE"]);
    std::thread::sleep(Duration::from_millis(300));
    call(&mut c, "daemon.upgrade", json!({ "binary_path": d.path("v2") }));
    d.wait_upgraded(1);
    let mut c = d.human();
    call(&mut c, "daemon.upgrade", json!({ "binary_path": d.path("v1") }));
    d.wait_upgraded(2);
    let mut c = d.human();
    wait_for(120, "flood to finish", || read(&mut c, &flood, 5).contains("FLOOD-DONE").then_some(()));
    wait_for(60, "numbers to finish", || read(&mut c, &nums, 5).contains("NUMS-DONE").then_some(()));
    let text = read(&mut c, &nums, 9000);
    let v: Vec<u64> = text.lines().filter_map(|l| l.strip_prefix("N ")?.trim().parse().ok()).collect();
    assert_eq!(v.len(), 500, "every numbered line arrived exactly once");
    assert!(v.iter().enumerate().all(|(i, n)| *n == i as u64 + 1), "lines out of order or duplicated");
    // The flood's tail is intact `y` lines.
    let tail = read(&mut c, &flood, 30);
    assert!(tail.lines().filter(|l| !l.is_empty() && *l != "FLOOD-DONE").all(|l| l == "y"), "{tail}");
}

#[test]
fn selftest_failure_aborts_the_upgrade() {
    let d = Bin::start();
    let mut c = d.human();
    let pid0 = d.info()["pid"].as_i64().unwrap();
    let hup = d.path("hup");
    let (loop_id, _) = open(&mut c, "monitor", &["/bin/sh", "-c", &counter_cmd(&hup)]);
    let broken = d.path("broken");
    std::fs::write(&broken, "#!/bin/sh\necho 'v2 cannot run: simulated' >&2\nexit 3\n").unwrap();
    std::fs::set_permissions(&broken, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let err = match c.call_value("daemon.upgrade", json!({ "binary_path": broken })) {
        Err(ClientError::Rpc(e)) => e,
        other => panic!("expected a refusal, got {other:?}"),
    };
    assert_eq!(err.code, midna_proto::error::REFUSED);
    assert!(err.message.contains("selftest") && err.message.contains("simulated"), "{}", err.message);
    // Nothing changed: same connection still works, same pid, no handoff left, still counting.
    assert_eq!(call(&mut c, "daemon.info", json!({}))["pid"].as_i64().unwrap(), pid0);
    assert!(!d.home.join("handoff").exists());
    let n0 = *counts(&read(&mut c, &loop_id, 2000)).last().unwrap_or(&0);
    wait_for(10, "counting after abort", || (counts(&read(&mut c, &loop_id, 2000)).last().copied().unwrap_or(0) > n0 + 5).then_some(()));
    let failed = call(&mut c, "events.list", json!({ "since_seq": 0, "filter": { "kinds": ["daemon.upgrade_failed"] } }));
    assert_eq!(failed.as_array().unwrap().len(), 1);
    // A binary that isn't there is a bad-params error, also without side effects.
    assert!(c.call_value("daemon.upgrade", json!({ "binary_path": d.path("nope") })).is_err());
    assert!(!Path::new(&hup).exists());
}

#[test]
fn agents_get_a_needs_you_for_upgrade() {
    let d = Bin::start();
    let mut agent = Client::connect(d.socket()).unwrap().as_agent(None);
    let err = match agent.call_value("daemon.upgrade", json!({ "binary_path": d.path("v2") })) {
        Err(ClientError::Rpc(e)) => e,
        other => panic!("expected human-only, got {other:?}"),
    };
    assert_eq!(err.code, midna_proto::error::HUMAN_ONLY);
    let mut c = d.human();
    let needs = call(&mut c, "needs_you.list", json!({}));
    assert!(needs.as_array().unwrap().iter().any(|n| n["detail"].as_str().unwrap_or("").contains("daemon.upgrade")), "{needs}");
}

#[test]
fn watchdog_falls_back_when_the_new_binary_dies() {
    let d = Bin::start();
    let mut c = d.human();
    let pid0 = d.info()["pid"].as_i64().unwrap();
    let hup = d.path("hup");
    let (loop_id, loop_pid) = open(&mut c, "monitor", &["/bin/sh", "-c", &counter_cmd(&hup)]);
    let (sh_id, _) = open(&mut c, "shell", &["/bin/zsh", "-f"]);
    wait_for(10, "prompt", || screen(&mut c, &sh_id).contains('%').then_some(()));
    // Passes the selftest (delegates to the real binary), then dies instead of resuming.
    let dies = d.path("dies");
    std::fs::write(&dies, format!("#!/bin/sh\n[ \"$1\" = --selftest ] && exec {} --selftest\necho 'v2 crashed: simulated' >&2\nexit 7\n", d.path("v1"))).unwrap();
    std::fs::set_permissions(&dies, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    call(&mut c, "daemon.upgrade", json!({ "binary_path": dies }));
    let up = d.wait_upgraded(1);
    assert_eq!(up["fallback"], json!(true), "{up}");
    assert_eq!(up["sessions_kept"], json!(2), "{up}");
    let mut c = d.human();
    let pid1 = d.info()["pid"].as_i64().unwrap();
    assert_ne!(pid1, pid0, "the watchdog process became the daemon");
    assert!(alive(loop_pid));
    let n0 = *counts(&read(&mut c, &loop_id, 3000)).last().unwrap();
    wait_for(10, "counting after fallback", || (*counts(&read(&mut c, &loop_id, 3000)).last().unwrap() > n0 + 5).then_some(()));
    assert!(!Path::new(&hup).exists());
    // The shells aren't our children any more: exits are still noticed (kqueue), without a code.
    call(&mut c, "session.input", json!({ "id": sh_id, "text": "exit 3", "enter": true }));
    let s = wait_for(10, "exit noticed", || {
        let s = session(&mut c, &sh_id);
        matches!(s["status"]["state"].as_str(), Some("exited" | "failed")).then_some(s)
    });
    assert!(s["pid"].is_null(), "{s}");
    assert!(d.log().contains("falling back"), "{}", d.log());
}

#[test]
fn sigterm_restarts_in_place_and_sigint_stops_and_hangs_up() {
    let d = Bin::start();
    let mut c = d.human();
    let pid0 = d.info()["pid"].as_i64().unwrap();
    let hup = d.path("hup");
    let (loop_id, loop_pid) = open(&mut c, "monitor", &["/bin/sh", "-c", &counter_cmd(&hup)]);
    wait_for(10, "counting", || (counts(&read(&mut c, &loop_id, 500)).len() > 3).then_some(()));
    unsafe { libc::kill(pid0 as i32, libc::SIGTERM) };
    let up = d.wait_upgraded(1);
    assert_eq!(up["reason"], json!("sigterm"));
    assert_eq!(up["sessions_kept"], json!(1));
    let mut c = d.human();
    assert_eq!(d.info()["pid"].as_i64().unwrap(), pid0);
    let n0 = *counts(&read(&mut c, &loop_id, 2000)).last().unwrap();
    wait_for(10, "counting after restart", || (*counts(&read(&mut c, &loop_id, 2000)).last().unwrap() > n0 + 5).then_some(()));
    assert!(!Path::new(&hup).exists());

    // A monitor whose job-controlled child ignores SIGHUP (the reported orphan) and a
    // background job in another process group: an explicit stop must take both down.
    let (_, mon_pid) = open(&mut c, "monitor", &["/bin/sh", "-c", "set -m; (trap '' HUP; exec sleep 300) & echo BG $!; sleep 300 & wait"]);
    let bg: i64 = wait_for(10, "bg pid", || {
        let s = call(&mut c, "session.list", json!({}));
        let id = s.as_array()?.iter().find(|x| x["pid"].as_i64() == Some(mon_pid))?["id"].as_str()?.to_string();
        read(&mut c, &id, 10).lines().find_map(|l| l.strip_prefix("BG ")?.trim().parse().ok())
    });
    assert!(alive(bg));
    unsafe { libc::kill(pid0 as i32, libc::SIGINT) };
    // The daemon is this test's child: reap it rather than probing (a zombie still "exists").
    wait_for(10, "daemon exit", || (!d.socket().exists() && unsafe { libc::waitpid(pid0 as i32, std::ptr::null_mut(), libc::WNOHANG) } == pid0 as i32).then_some(()));
    wait_for(5, "sessions hung up", || (!alive(loop_pid) && !alive(mon_pid) && !alive(bg)).then_some(()));
    assert!(Path::new(&hup).exists(), "an explicit stop sends SIGHUP");
}

#[test]
fn restart_rpc_is_a_graceful_handoff() {
    let d = Bin::start();
    let mut c = d.human();
    let (sh_id, sh_pid) = open(&mut c, "shell", &["/bin/zsh", "-f"]);
    wait_for(10, "prompt", || screen(&mut c, &sh_id).contains('%').then_some(()));
    // Agents may restart (nothing is lost); the CLI is an agent.
    let mut agent = Client::connect(d.socket()).unwrap().as_agent(None);
    let r = call(&mut agent, "daemon.restart", json!({}));
    assert!(r["binary"].as_str().unwrap().ends_with("/v1"), "{r}");
    let up = d.wait_upgraded(1);
    assert_eq!(up["reason"], json!("restart"));
    let mut c = d.human();
    assert_eq!(session(&mut c, &sh_id)["pid"].as_i64(), Some(sh_pid));
    call(&mut c, "session.input", json!({ "id": sh_id, "text": "echo after-$((2+3))", "enter": true }));
    wait_for(10, "output", || screen(&mut c, &sh_id).lines().any(|l| l.trim() == "after-5").then_some(()));
    // Same pid across execv: the shell is still our child, so waitpid still reports its code.
    call(&mut c, "session.input", json!({ "id": sh_id, "text": "exit 4", "enter": true }));
    let st = wait_for(10, "exit code", || Some(session(&mut c, &sh_id)["status"].clone()).filter(|s| s["state"] == "failed"));
    assert_eq!(st["exit_code"], 4, "{st}");
}
