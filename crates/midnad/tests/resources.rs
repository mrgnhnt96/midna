//! Resource bounds: no fd or thread leaks over 200 terminal open/close cycles, and bounded
//! memory while a terminal prints 100 MB. One test in its own binary, so the process-wide
//! counts it measures aren't disturbed by other tests running in parallel.
mod common;
use common::{TestDaemon, call, wait_for};
use serde_json::json;
use std::time::{Duration, Instant};

fn fd_count() -> usize {
    std::fs::read_dir("/dev/fd").map(|d| d.count()).unwrap_or(0)
}

fn task_info() -> libc::proc_taskinfo {
    let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_taskinfo>() as i32;
    let n = unsafe { libc::proc_pidinfo(std::process::id() as i32, libc::PROC_PIDTASKINFO, 0, &mut info as *mut _ as *mut _, size) };
    assert_eq!(n, size);
    info
}

fn threads() -> i32 {
    task_info().pti_threadnum
}

fn rss_mb() -> u64 {
    task_info().pti_resident_size / (1 << 20)
}

fn cycle(h: &mut midna_proto::Client) {
    let s = call(h, "session.open", json!({ "kind": "shell", "cwd": "/tmp", "command": ["/bin/cat"] }));
    let id = s["id"].as_str().unwrap().to_string();
    call(h, "session.input", json!({ "id": id, "text": "x" }));
    call(h, "session.close", json!({ "id": id, "force": true }));
}

/// Wait until `f() <= limit` (resources are released asynchronously), returning the last value.
fn settle(what: &str, limit: i64, f: impl Fn() -> i64) -> i64 {
    let t0 = Instant::now();
    loop {
        let v = f();
        if v <= limit || t0.elapsed() > Duration::from_secs(15) {
            eprintln!("{what}: {v} (limit {limit})");
            return v;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn terminals_do_not_leak_and_floods_stay_bounded() {
    let d = TestDaemon::start();
    let mut h = d.human();
    for _ in 0..5 {
        cycle(&mut h);
    }
    std::thread::sleep(Duration::from_millis(500));
    let (fd0, th0) = (fd_count() as i64, threads() as i64);
    for _ in 0..200 {
        cycle(&mut h);
    }
    assert_eq!(call(&mut h, "session.list", json!({})).as_array().unwrap().len(), 0);
    let fd1 = settle("fds", fd0 + 4, || fd_count() as i64);
    let th1 = settle("threads", th0 + 4, || threads() as i64);
    assert!(fd1 <= fd0 + 4, "fd leak: {fd0} -> {fd1}");
    assert!(th1 <= th0 + 4, "thread leak: {th0} -> {th1}");

    // 100 MB through one terminal: memory stays bounded (scrollback is capped) and the
    // daemon keeps answering other calls while it runs.
    let rss0 = rss_mb();
    let s = call(
        &mut h,
        "session.open",
        json!({ "kind": "shell", "cwd": "/tmp", "command": ["/bin/sh", "-c", "head -c 75000000 /dev/urandom | base64 -b 120; echo FLOOD-DONE; exec cat"] }),
    );
    let id = s["id"].as_str().unwrap().to_string();
    let mut peak = rss0;
    let mut other = d.human();
    let t0 = Instant::now();
    wait_for(240, "flood to finish", || {
        peak = peak.max(rss_mb());
        let t = Instant::now();
        call(&mut other, "daemon.info", json!({}));
        assert!(t.elapsed() < Duration::from_secs(2), "daemon.info took {:?} during the flood", t.elapsed());
        let screen = call(&mut h, "session.read", json!({ "id": id, "screen": true }))["text"].as_str().unwrap_or("").to_string();
        std::thread::sleep(Duration::from_millis(200));
        screen.contains("FLOOD-DONE").then_some(())
    });
    eprintln!("flood: {:?}, rss {rss0} MB -> peak {peak} MB", t0.elapsed());
    assert!(peak < rss0 + 300, "memory grew {rss0} -> {peak} MB during a 100 MB flood");
    call(&mut h, "session.close", json!({ "id": id, "force": true }));
}
