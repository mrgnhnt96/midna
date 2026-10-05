//! Socket listener, background loops (state saver, rule expiry, git refresh) and shutdown.
use crate::daemon::{Config, Daemon};
use midna_proto::*;
use serde_json::json;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

pub struct Handle {
    pub daemon: Arc<Daemon>,
    accept: Option<std::thread::JoinHandle<()>>,
}

impl Handle {
    /// Stop accepting, kill every session, flush state, remove the socket.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        stop_daemon(&self.daemon);
        if let Some(t) = self.accept.take() {
            let _ = t.join();
        }
    }

    pub fn wait(mut self) {
        if let Some(t) = self.accept.take() {
            let _ = t.join();
        }
        self.stop();
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Explicit stop (`daemon.stop`, SIGINT, an in-process Handle dropping): stop accepting, hang
/// up every terminal (see `hangup_all`), flush state, remove the socket. An upgrade or restart
/// never comes through here. Returns false if a stop was already under way.
pub fn stop_daemon(d: &Daemon) -> bool {
    if d.shutting_down.swap(true, Ordering::SeqCst) {
        return false;
    }
    let rts: Vec<_> = d.core().rt.drain().map(|(_, rt)| rt).collect();
    d.emit(kinds::DAEMON_STOPPING, Actor::system(), None, None, json!({ "sessions": rts.len() }));
    hangup_all(&rts);
    for rt in rts {
        let _ = rt.tx.send(crate::term::EngineMsg::Stop);
    }
    d.save_now();
    let _ = UnixStream::connect(&d.cfg.socket); // wake the accept loop
    let _ = std::fs::remove_file(&d.cfg.socket);
    if d.cfg.owns_process {
        // The binary is done; don't depend on the accept loop waking (the socket path may
        // already be gone).
        std::process::exit(0);
    }
    true
}

/// Stop policy: SIGHUP (+SIGCONT, for stopped jobs) every process group in each terminal's
/// session — not just the shell's own group, so background jobs and a monitor's children go
/// too — then SIGKILL the groups still alive after a grace period. Processes that left the
/// session (setsid, daemons) are not ours to kill. Synchronous, so it finishes before exit.
pub fn hangup_all(rts: &[crate::term::RtHandle]) {
    const GRACE: Duration = Duration::from_millis(1500);
    let me = unsafe { libc::getpgrp() };
    let leaders: Vec<i32> = rts.iter().map(|r| r.pid).filter(|&p| p > 1).collect();
    let mut groups: Vec<i32> = vec![];
    for rt in rts {
        let fg = unsafe { libc::tcgetpgrp(rt.fd.0) };
        if fg > 1 {
            groups.push(fg);
        }
    }
    for pid in crate::procs::all_pids() {
        let sid = unsafe { libc::getsid(pid) };
        if sid > 1 && leaders.contains(&sid) {
            let pg = unsafe { libc::getpgid(pid) };
            if pg > 1 {
                groups.push(pg);
            }
        }
    }
    groups.sort_unstable();
    groups.dedup();
    groups.retain(|&g| g != me);
    for &g in &groups {
        unsafe {
            libc::kill(-g, libc::SIGHUP);
            libc::kill(-g, libc::SIGCONT);
        }
    }
    let t0 = std::time::Instant::now();
    while t0.elapsed() < GRACE && groups.iter().any(|&g| unsafe { libc::kill(-g, 0) } == 0) {
        std::thread::sleep(Duration::from_millis(20));
    }
    for &g in &groups {
        if unsafe { libc::kill(-g, 0) } == 0 {
            unsafe { libc::kill(-g, libc::SIGKILL) };
        }
    }
}

/// Make `dir` private to this user (0700), creating it if needed.
pub fn secure_dir(dir: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// Bind the control socket with mode 0600. The socket's directory is made 0700 when it is
/// MIDNA_HOME or one we create (never a shared dir like /tmp that MIDNA_SOCKET may point into).
/// The umask is tightened around bind() so the socket never exists with looser permissions.
fn bind(socket: &std::path::Path, home: &std::path::Path) -> std::io::Result<UnixListener> {
    if socket.exists() {
        if UnixStream::connect(socket).is_ok() {
            return Err(std::io::Error::new(std::io::ErrorKind::AddrInUse, format!("a daemon is already listening on {}", socket.display())));
        }
        std::fs::remove_file(socket)?; // stale socket from a crash
    }
    if let Some(dir) = socket.parent()
        && (dir == home || !dir.exists()) {
            secure_dir(dir)?;
        }
    let old = unsafe { libc::umask(0o077) };
    let l = UnixListener::bind(socket);
    unsafe { libc::umask(old) };
    let l = l?;
    std::fs::set_permissions(socket, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    Ok(l)
}

/// Concurrent client connections (control + stream). Beyond this, new ones are closed at once.
pub const MAX_CONNS: usize = 512;
static OPEN_CONNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Start the daemon in this process (threads) and return immediately.
pub fn start(cfg: Config) -> std::io::Result<Handle> {
    start_with(cfg, None)
}

/// Start, optionally resuming an upgrade handoff (inherited listener + PTYs).
pub fn start_with(mut cfg: Config, resume: Option<crate::upgrade::Resume>) -> std::io::Result<Handle> {
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
    let mut kept = std::collections::HashMap::new();
    if let Some(r) = &resume {
        crate::upgrade::apply_config(r, &mut cfg);
        kept = crate::upgrade::kept_sessions(r);
    } else if cfg.owns_process {
        crate::upgrade::set_aside_stale(&cfg.home);
    }
    let _ = secure_dir(&cfg.home);
    let d = Daemon::new_keeping(cfg, &kept)?;
    let listener = match resume.as_ref().and_then(crate::upgrade::inherited_listener) {
        Some(l) => l,
        None => bind(&d.cfg.socket, &d.cfg.home)?,
    };
    d.listener_fd.store(std::os::fd::AsRawFd::as_raw_fd(&listener), Ordering::SeqCst);
    crate::hooks::write_claude_settings(&d);
    crate::adopt::write_files(&d);
    match &resume {
        Some(r) => crate::upgrade::adopt_all(&d, r),
        None => {
            d.emit(
                kinds::DAEMON_STARTED,
                Actor::system(),
                None,
                None,
                json!({ "pid": std::process::id(), "version": midna_proto::VERSION, "socket": d.cfg.socket }),
            );
        }
    }
    background(&d);
    let ad = d.clone();
    let accept = std::thread::Builder::new().name("accept".into()).spawn(move || {
        for conn in listener.incoming() {
            if ad.shutting_down.load(Ordering::SeqCst) {
                break;
            }
            let Ok(conn) = conn else { continue };
            // Each connection costs two threads; a runaway client can't exhaust them.
            if OPEN_CONNS.fetch_add(1, Ordering::AcqRel) >= MAX_CONNS {
                OPEN_CONNS.fetch_sub(1, Ordering::AcqRel);
                drop(conn);
                continue;
            }
            let (cd, id) = (ad.clone(), ad.conn_id());
            let spawned = std::thread::Builder::new().name(format!("conn-{id}")).spawn(move || {
                crate::conn::serve(cd, conn, id);
                OPEN_CONNS.fetch_sub(1, Ordering::AcqRel);
            });
            if spawned.is_err() {
                OPEN_CONNS.fetch_sub(1, Ordering::AcqRel);
            }
        }
    })?;
    Ok(Handle { daemon: d, accept: Some(accept) })
}

fn background(d: &Arc<Daemon>) {
    let w = Arc::downgrade(d);
    std::thread::spawn(move || {
        let mut ticks = 0u64;
        loop {
            std::thread::sleep(Duration::from_millis(200));
            let Some(d) = w.upgrade() else { return };
            if d.shutting_down.load(Ordering::Relaxed) {
                return;
            }
            d.save_if_dirty();
            ticks += 1;
            if ticks.is_multiple_of(5) {
                crate::rpc::ui::poll_file(&d);
            }
            if ticks.is_multiple_of(25) {
                crate::rpc::policy::expire_rules(&d);
                crate::global_hooks::poll(&d);
            }
        }
    });
    let w = Arc::downgrade(d);
    std::thread::spawn(move || crate::git::refresh_loop(w));
    crate::restart::start(d);
    crate::webhooks::start(d);
    crate::notify::start(d);
    crate::local::start(d);
    crate::queue::start(d);
    crate::resume::start(d);
}
