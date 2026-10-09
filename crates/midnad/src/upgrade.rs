//! In-place daemon upgrade / graceful restart (proven in `spikes/daemon-reexec`).
//!
//! 1. Preflight: `<new> --selftest` must print `{"ok":true,"handoff_versions":[..]}` listing
//!    our `HANDOFF_VERSION`, or the upgrade is refused and nothing changes.
//! 2. PTY readers pause (unread output stays in the kernel buffer), every engine snapshots
//!    its terminal (libghostty VT export + our own title/cursor/half-escape state), state.json
//!    is flushed and `$MIDNA_HOME/handoff/` gets `handoff.json` plus one `.vt` file per screen.
//! 3. FD_CLOEXEC is cleared on the PTY masters and the listening socket, a watchdog process
//!    (`midnad --upgrade-watchdog`) is started holding copies of them, and the daemon
//!    `execv`s the new binary with `--resume <handoff.json>`: same pid, so the shells stay our
//!    children (waitpid keeps working) and never see a hangup.
//! 4. The new image restores every session under its id, writes `handoff/resumed`, and emits
//!    `daemon.upgraded`. Client connections (CLOEXEC) closed at the exec and reconnect.
//!
//! Busy Mac: `daemon.upgrade` / `daemon.restart` are refused with error BUSY while the load is at
//! or above `system.busy_load` (guard.rs), unless `force`; the app then offers to wait.
//!
//! Watchdog: if the new image dies, or hasn't written `resumed` within `watchdog_secs`
//! (more when the Mac is loaded; and while the new image keeps using CPU, up to
//! WATCHDOG_MAX), or is killed then, the watchdog itself execs the old binary with `--resume --fallback` on the
//! same handoff. It holds the fds, so the terminals survive; the shells are no longer the
//! daemon's children, so exits are tracked with kqueue and carry no exit code.
use crate::daemon::{Config, Daemon};
use crate::engine::{Engine, TermSnap};
use crate::term::{self, EngineMsg};
use midna_proto::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::ffi::CString;
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

pub const HANDOFF_VERSION: u32 = 1;
const SELFTEST_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_WATCHDOG_SECS: u64 = 10;
/// The watchdog never waits longer than this, however busy the new image looks.
const WATCHDOG_MAX: Duration = Duration::from_secs(120);

/// DEFAULT_WATCHDOG_SECS, stretched by how loaded the Mac is (load per core above 1), up to 60s.
pub fn watchdog_secs(load1: f64, cpus: u32) -> u64 {
    let per_core = if cpus == 0 { 1. } else { load1 / cpus as f64 };
    ((DEFAULT_WATCHDOG_SECS as f64 * per_core.max(1.)).ceil() as u64).min(60)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Handoff {
    pub version: u32,
    /// `upgrade`, `restart` or `sigterm`.
    pub reason: String,
    pub created_at: Timestamp,
    pub daemon_pid: i32,
    pub from_version: String,
    pub from_binary: String,
    pub to_binary: String,
    /// What the watchdog execs if the new image doesn't come up.
    pub fallback_binary: String,
    pub home: String,
    pub socket: String,
    #[serde(default)]
    pub app_path: Option<String>,
    pub listener_fd: RawFd,
    pub event_seq: u64,
    pub watchdog_secs: u64,
    pub sessions: Vec<HandoffSession>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HandoffSession {
    pub id: Id,
    pub pid: i32,
    /// The process is still running (exited sessions keep their screen but get no reaper).
    pub alive: bool,
    pub fd: RawFd,
    /// st_rdev of the PTY master, so a resume can tell an inherited fd from a reused number.
    pub rdev: u64,
    pub term: TermSnap,
    pub primary_file: String,
    #[serde(default)]
    pub alt_file: Option<String>,
    #[serde(default)]
    pub in_turn: bool,
    #[serde(default)]
    pub last_cost: f64,
}

pub fn handoff_dir(home: &Path) -> PathBuf {
    home.join("handoff")
}

fn log(msg: &str) {
    eprintln!("midnad[{}]: {msg}", std::process::id());
}

fn set_cloexec(fd: RawFd, on: bool) {
    unsafe {
        let f = libc::fcntl(fd, libc::F_GETFD);
        if f >= 0 {
            libc::fcntl(fd, libc::F_SETFD, if on { f | libc::FD_CLOEXEC } else { f & !libc::FD_CLOEXEC });
        }
    }
}

fn fd_rdev(fd: RawFd) -> Option<(u16, u64)> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    (unsafe { libc::fstat(fd, &mut st) } == 0).then_some((st.st_mode & libc::S_IFMT, st.st_rdev as u64))
}

// ---------------------------------------------------------------- selftest

/// `midnad --selftest`: prove this binary can run a terminal engine and round-trip a
/// snapshot, and say which handoff formats it can resume.
pub fn selftest() -> Result<Value, String> {
    let mut e = Engine::new(40, 6, None);
    e.feed(b"selftest \x1b[1;32mok\x1b[0m\r\n\x1b]2;midna\x07\x1b[?1049halt\x1b[2;3H\x1b[3", 0);
    let snap = e.snapshot();
    let r = Engine::restored(&snap, None);
    if r.plain_lines() != e.plain_lines() || r.cursor() != e.cursor() || r.title() != "midna" {
        return Err("terminal snapshot round-trip mismatch".into());
    }
    Ok(json!({ "ok": true, "version": midna_proto::VERSION, "pid": std::process::id(), "handoff_versions": [HANDOFF_VERSION] }))
}

/// Run `<bin> --selftest` (bounded) and check it can resume our handoff format.
pub fn run_selftest(bin: &Path) -> Result<Value, String> {
    let mut child = std::process::Command::new(bin)
        .arg("--selftest")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run {} --selftest: {e}", bin.display()))?;
    let t0 = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if t0.elapsed() > SELFTEST_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("--selftest did not finish within {}s", SELFTEST_TIMEOUT.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(format!("--selftest: {e}")),
        }
    };
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if !status.success() {
        return Err(format!("--selftest failed ({status}){}", if stderr.is_empty() { String::new() } else { format!(": {stderr}") }));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: Value = stdout.lines().rev().find_map(|l| serde_json::from_str(l).ok()).ok_or_else(|| format!("--selftest printed no JSON: {stdout:?}"))?;
    if v["ok"] != json!(true) {
        return Err(format!("--selftest reported failure: {v}"));
    }
    let supported = v["handoff_versions"].as_array().is_some_and(|a| a.iter().any(|x| x.as_u64() == Some(HANDOFF_VERSION as u64)));
    if !supported {
        return Err(format!("new binary can't resume handoff format {HANDOFF_VERSION}: {v}"));
    }
    Ok(v)
}

// ---------------------------------------------------------------- upgrade (old image)

/// The binary `daemon.restart` / SIGTERM re-exec: `MIDNA_HOME/bin/current/midnad` when we run
/// from the installed location, else our own executable (which may have been rebuilt).
pub fn restart_binary(cfg: &Config) -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("midnad"));
    let current = crate::install::current_binary(&cfg.home);
    let installed_root = std::fs::canonicalize(cfg.home.join("bin")).unwrap_or_else(|_| cfg.home.join("bin"));
    let exe_canon = std::fs::canonicalize(&exe).unwrap_or_else(|_| exe.clone());
    if current.is_file() && exe_canon.starts_with(&installed_root) {
        return current;
    }
    exe
}

/// `daemon.upgrade` / `daemon.restart`: selftest now (errors go to the caller), then hand off
/// on a separate thread shortly after, so the caller's answer is flushed before the exec.
/// Refused with BUSY while the Mac is busy, unless `force`.
pub fn request(d: &Arc<Daemon>, bin: &Path, reason: &str, force: bool) -> Result<Value, RpcError> {
    if !d.cfg.owns_process {
        return Err(RpcError::conflict("this daemon runs inside another process (tests); it can't re-exec"));
    }
    let bin = std::fs::canonicalize(bin).map_err(|e| RpcError::bad_params(format!("{}: {e}", bin.display())))?;
    if !force && let Some(l) = crate::guard::busy(d) {
        let msg = format!(
            "the Mac is busy (load {:.1} on {} cores, {}% of system.busy_load {}%): the {reason} waits until it calms down, \
             because under this load the handoff can time out and hang up every terminal. Pass force to {reason} anyway",
            l.load1, l.cpus, l.load_percent, l.busy_at
        );
        d.emit(kinds::DAEMON_UPGRADE_POSTPONED, Actor::system(), None, None, json!({ "to": bin, "reason": reason, "load1": l.load1, "cpus": l.cpus, "load_percent": l.load_percent, "busy_at": l.busy_at }));
        return Err(RpcError::new(midna_proto::error::BUSY, msg).with_data(json!(l)));
    }
    if d.upgrading.swap(true, Ordering::SeqCst) {
        return Err(RpcError::conflict("an upgrade is already in progress"));
    }
    if let Err(e) = run_selftest(&bin) {
        d.upgrading.store(false, Ordering::SeqCst);
        let msg = format!("upgrade aborted, nothing changed: {e}");
        d.emit(kinds::DAEMON_UPGRADE_FAILED, Actor::system(), None, None, json!({ "to": bin, "reason": reason, "error": msg }));
        return Err(RpcError::refused(msg));
    }
    let sessions = d.core().rt.len() as u32;
    let (d2, bin2, reason) = (d.clone(), bin.clone(), reason.to_string());
    std::thread::Builder::new()
        .name("upgrade".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            let e = handoff_and_exec(&d2, &bin2, &reason);
            log(&format!("upgrade to {} failed, old daemon keeps running: {e}", bin2.display()));
            d2.emit(kinds::DAEMON_UPGRADE_FAILED, Actor::system(), None, None, json!({ "to": bin2, "reason": reason, "error": e }));
            d2.upgrading.store(false, Ordering::SeqCst);
        })
        .map_err(|e| RpcError::internal(e.to_string()))?;
    Ok(json!(UpgradeResult { ok: true, binary: bin.to_string_lossy().into_owned(), sessions }))
}

/// Graceful restart on SIGTERM (synchronous). Returns only on failure.
pub fn restart_now(d: &Arc<Daemon>, reason: &str) -> String {
    if d.upgrading.swap(true, Ordering::SeqCst) {
        return "an upgrade is already in progress".into();
    }
    let bin = restart_binary(&d.cfg);
    let e = match run_selftest(&bin) {
        Ok(_) => handoff_and_exec(d, &bin, reason),
        Err(e) => e,
    };
    d.upgrading.store(false, Ordering::SeqCst);
    e
}

/// Snapshot, write the handoff, start the watchdog and execv. Returns only on failure, with
/// everything put back (readers resumed, CLOEXEC restored, watchdog killed, handoff removed).
pub fn handoff_and_exec(d: &Arc<Daemon>, bin: &Path, reason: &str) -> String {
    let t0 = Instant::now();
    let _spawns = crate::pty::spawn_lock();
    let dir = handoff_dir(&d.cfg.home);
    let mut fds: Vec<RawFd> = vec![];
    let listener = d.listener_fd.load(Ordering::SeqCst);
    let mut watchdog: Option<std::process::Child> = None;
    let res = (|| -> Result<std::convert::Infallible, String> {
        if listener < 0 {
            return Err("no listening socket fd".into());
        }
        if !term::pause_readers(Duration::from_secs(2)) {
            return Err("PTY readers did not pause".into());
        }
        let rts: Vec<(Id, term::RtHandle)> = d.core().rt.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let mut sessions = vec![];
        for (sid, rt) in rts {
            let (tx, rx) = std::sync::mpsc::channel();
            if rt.tx.send(EngineMsg::Snapshot(tx)).is_err() {
                continue; // engine already stopped (session closing)
            }
            let snap = rx.recv_timeout(Duration::from_secs(5)).map_err(|_| format!("session {sid}: engine did not snapshot"))?;
            let Some((_, rdev)) = fd_rdev(rt.fd.0) else { continue };
            let (alive, in_turn, last_cost) = {
                let core = d.core();
                let alive = core.state.session(&sid).is_some_and(|s| s.pid.is_some());
                let a = core.agents.get(&sid);
                (alive, a.is_some_and(|a| a.in_turn), a.map(|a| a.last_cost).unwrap_or(0.0))
            };
            let primary_file = format!("{sid}.primary.vt");
            std::fs::write(dir.join(&primary_file), &snap.primary_vt).map_err(|e| e.to_string())?;
            let alt_file = match &snap.alt_vt {
                Some(a) => {
                    let f = format!("{sid}.alt.vt");
                    std::fs::write(dir.join(&f), a).map_err(|e| e.to_string())?;
                    Some(f)
                }
                None => None,
            };
            fds.push(rt.fd.0);
            sessions.push(HandoffSession { id: sid, pid: rt.pid, alive, fd: rt.fd.0, rdev, term: snap, primary_file, alt_file, in_turn, last_cost });
        }
        d.save_now();
        let from_binary = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        let h = Handoff {
            version: HANDOFF_VERSION,
            reason: reason.to_string(),
            created_at: time::now_rfc3339(),
            daemon_pid: std::process::id() as i32,
            from_version: midna_proto::VERSION.into(),
            fallback_binary: from_binary.clone(),
            from_binary,
            to_binary: bin.to_string_lossy().into_owned(),
            home: d.cfg.home.to_string_lossy().into_owned(),
            socket: d.cfg.socket.to_string_lossy().into_owned(),
            app_path: d.cfg.app_path.clone(),
            listener_fd: listener,
            event_seq: d.log.seq(),
            watchdog_secs: std::env::var("MIDNA_UPGRADE_WATCHDOG_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or_else(|| {
                let (load1, _, _) = crate::guard::loadavg();
                watchdog_secs(load1, crate::guard::cpus())
            }),
            sessions,
        };
        let path = dir.join("handoff.json");
        let tmp = dir.join("handoff.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&h).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
        let snap_ms = t0.elapsed().as_millis();

        for &fd in fds.iter().chain(std::iter::once(&listener)) {
            set_cloexec(fd, false);
        }
        // The watchdog (the old binary, known good) inherits the fds at the same numbers.
        let wd = std::process::Command::new(&h.fallback_binary)
            .arg("--upgrade-watchdog")
            .arg(&path)
            .stdin(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("could not start the upgrade watchdog: {e}"))?;
        let _ = std::fs::write(dir.join("watchdog.pid"), wd.id().to_string());
        watchdog = Some(wd);

        log(&format!(
            "{reason}: {} session(s) snapshotted in {snap_ms}ms, event seq {}; execv {}",
            h.sessions.len(),
            h.event_seq,
            bin.display()
        ));
        let cs = |s: &str| CString::new(s).unwrap_or_default();
        let prog = CString::new(bin.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
        let args = [prog.clone(), cs("--foreground"), cs("--resume"), CString::new(path.as_os_str().as_bytes()).unwrap_or_default()];
        let mut argv: Vec<*const libc::c_char> = args.iter().map(|a| a.as_ptr()).collect();
        argv.push(std::ptr::null());
        // An IOKit assertion isn't promised to outlive or follow an exec; the new image takes it again.
        crate::keep_awake::release(d);
        unsafe { libc::execv(prog.as_ptr(), argv.as_ptr()) };
        Err(format!("execv {}: {}", bin.display(), std::io::Error::last_os_error()))
    })();
    let Err(e) = res;
    // Put everything back.
    for &fd in fds.iter().chain(std::iter::once(&listener)) {
        if fd >= 0 {
            set_cloexec(fd, true);
        }
    }
    if let Some(mut w) = watchdog {
        let _ = w.kill();
        let _ = w.wait();
    }
    let _ = std::fs::remove_dir_all(&dir);
    term::resume_readers();
    e
}

// ---------------------------------------------------------------- resume (new image)

/// A handoff read back, with the VT blobs loaded and every fd checked.
pub struct Resume {
    pub handoff: Handoff,
    pub path: PathBuf,
    /// We are the watchdog's fallback, not the exec'd image.
    pub fallback: bool,
}

pub fn load(path: &Path, fallback: bool) -> Result<Resume, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut h: Handoff = serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    if h.version != HANDOFF_VERSION {
        return Err(format!("handoff format {} (this binary resumes {HANDOFF_VERSION})", h.version));
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    for s in &mut h.sessions {
        s.term.primary_vt = std::fs::read(dir.join(&s.primary_file)).unwrap_or_default();
        s.term.alt_vt = s.alt_file.as_ref().map(|f| std::fs::read(dir.join(f)).unwrap_or_default());
    }
    Ok(Resume { handoff: h, path: path.to_path_buf(), fallback })
}

/// Apply the handoff's paths to the config (the exec'd image got only `--resume`).
pub fn apply_config(r: &Resume, cfg: &mut Config) {
    cfg.home = PathBuf::from(&r.handoff.home);
    cfg.socket = PathBuf::from(&r.handoff.socket);
    // The dev-only GUI override survives a re-exec in debug builds. A release image never
    // takes it from the (same-user writable) handoff file.
    if cfg!(debug_assertions) && cfg.app_path.is_none() {
        cfg.app_path = r.handoff.app_path.clone();
    }
}

/// Session ids that survive (fd inherited and still the same PTY) -> live pid.
pub fn kept_sessions(r: &Resume) -> HashMap<Id, Option<i32>> {
    r.handoff
        .sessions
        .iter()
        .filter(|s| fd_ok(s))
        .map(|s| (s.id.clone(), s.alive.then_some(s.pid)))
        .collect()
}

fn fd_ok(s: &HandoffSession) -> bool {
    matches!(fd_rdev(s.fd), Some((mode, rdev)) if mode == libc::S_IFCHR && rdev == s.rdev)
}

/// The inherited listening socket, if it really is one.
pub fn inherited_listener(r: &Resume) -> Option<std::os::unix::net::UnixListener> {
    use std::os::fd::FromRawFd;
    let fd = r.handoff.listener_fd;
    match fd_rdev(fd) {
        Some((mode, _)) if mode == libc::S_IFSOCK => {
            set_cloexec(fd, true);
            Some(unsafe { std::os::unix::net::UnixListener::from_raw_fd(fd) })
        }
        _ => None,
    }
}

/// Re-adopt every surviving session and finish the handoff (marker, watchdog, event).
pub fn adopt_all(d: &Arc<Daemon>, r: &Resume) {
    let mut kept = 0u32;
    let mut lost = vec![];
    for s in &r.handoff.sessions {
        let known = d.core().state.session(&s.id).cloned();
        if !fd_ok(s) || known.is_none() {
            lost.push(s.id.clone());
            continue;
        }
        set_cloexec(s.fd, true);
        let rt = term::adopt(d, &s.id, s.fd, s.pid, s.alive, &s.term);
        let mut core = d.core();
        core.rt.insert(s.id.clone(), rt);
        if known.is_some_and(|k| k.agent.is_some()) {
            let a = core.agents.entry(s.id.clone()).or_default();
            a.in_turn = s.in_turn;
            a.last_cost = s.last_cost;
        }
        kept += 1;
    }
    for sid in &lost {
        let mut core = d.core();
        if let Some(sess) = core.state.session_mut(sid) {
            sess.pid = None;
            sess.status = Status { state: StatusState::Exited, reason: Some("daemon restarted (pty lost)".into()), exit_code: None, since: time::now_rfc3339() };
        }
    }
    d.mark_dirty();
    let dir = r.path.parent().map(Path::to_path_buf).unwrap_or_default();
    let _ = std::fs::write(dir.join("resumed"), std::process::id().to_string());
    let seq_now = d.log.seq();
    if seq_now != r.handoff.event_seq {
        log(&format!("event log seq {seq_now} differs from the handoff's {}", r.handoff.event_seq));
    }
    let me = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    d.emit(
        kinds::DAEMON_UPGRADED,
        Actor::system(),
        None,
        None,
        json!({
            "from": { "version": r.handoff.from_version, "binary": r.handoff.from_binary, "pid": r.handoff.daemon_pid },
            "to": { "version": midna_proto::VERSION, "binary": me, "pid": std::process::id() },
            "sessions_kept": kept,
            "sessions_lost": lost,
            "reason": r.handoff.reason,
            "fallback": r.fallback,
        }),
    );
    log(&format!("resumed {kept} session(s) from {} ({}){}", r.handoff.from_binary, r.handoff.reason, if r.fallback { " [watchdog fallback]" } else { "" }));
    // Let the watchdog see the marker and exit, reap it, then clean up.
    let wd: Option<i32> = std::fs::read_to_string(dir.join("watchdog.pid")).ok().and_then(|s| s.trim().parse().ok());
    std::thread::spawn(move || {
        if let Some(pid) = wd.filter(|&p| p != std::process::id() as i32) {
            let t0 = Instant::now();
            loop {
                let mut st = 0;
                let r = unsafe { libc::waitpid(pid, &mut st, libc::WNOHANG) };
                if r != 0 || t0.elapsed() > Duration::from_secs(10) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    });
}

/// A plain start found a handoff nobody resumed (the fds died with the process that held
/// them): keep it for inspection and start clean.
pub fn set_aside_stale(home: &Path) {
    let dir = handoff_dir(home);
    if dir.join("handoff.json").exists() && !dir.join("resumed").exists() {
        let to = home.join(format!("handoff.stale-{}", time::now_unix()));
        let _ = std::fs::rename(&dir, &to);
        log(&format!("found an unresumed handoff (its terminals are gone); moved to {}", to.display()));
    } else {
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ---------------------------------------------------------------- watchdog

/// `midnad --upgrade-watchdog <handoff.json>`: wait for the new image to write `resumed`;
/// if it dies or stalls, become the old daemon again on the same handoff. Never returns.
pub fn watchdog_main(path: &Path) -> ! {
    let h: Handoff = match std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()) {
        Some(h) => h,
        None => std::process::exit(0),
    };
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let t0 = Instant::now();
    // The new image's CPU time at the last check: one still using CPU is resuming, just slowly
    // (a loaded Mac), so it gets more time, up to WATCHDOG_MAX.
    let (mut cpu, mut cpu_at) = (crate::procs::cpu_ns(h.daemon_pid).unwrap_or(0), Instant::now());
    let mut working = true;
    let why = loop {
        if dir.join("resumed").exists() || !path.exists() {
            std::process::exit(0);
        }
        if unsafe { libc::getppid() } != h.daemon_pid {
            break "the new daemon exited before resuming";
        }
        if cpu_at.elapsed() >= Duration::from_secs(2) {
            let now = crate::procs::cpu_ns(h.daemon_pid).unwrap_or(cpu);
            working = now > cpu;
            (cpu, cpu_at) = (now, Instant::now());
        }
        let late = t0.elapsed() > Duration::from_secs(h.watchdog_secs);
        if late && working && t0.elapsed() < WATCHDOG_MAX {
            std::thread::sleep(Duration::from_millis(25));
            continue;
        }
        if late {
            unsafe { libc::kill(h.daemon_pid, libc::SIGKILL) };
            let t1 = Instant::now();
            while unsafe { libc::getppid() } == h.daemon_pid && t1.elapsed() < Duration::from_secs(3) {
                std::thread::sleep(Duration::from_millis(10));
            }
            break "the new daemon did not resume in time (killed)";
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    if dir.join("resumed").exists() {
        std::process::exit(0);
    }
    log(&format!("upgrade watchdog: {why}; falling back to {}", h.fallback_binary));
    let _ = std::fs::write(dir.join("watchdog.pid"), std::process::id().to_string());
    let cs = |s: &str| CString::new(s).unwrap_or_default();
    let prog = cs(&h.fallback_binary);
    let args = [prog.clone(), cs("--foreground"), cs("--resume"), CString::new(path.as_os_str().as_bytes()).unwrap_or_default(), cs("--fallback")];
    let mut argv: Vec<*const libc::c_char> = args.iter().map(|a| a.as_ptr()).collect();
    argv.push(std::ptr::null());
    unsafe { libc::execv(prog.as_ptr(), argv.as_ptr()) };
    log(&format!("upgrade watchdog: execv {} failed: {}", h.fallback_binary, std::io::Error::last_os_error()));
    std::process::exit(1)
}
