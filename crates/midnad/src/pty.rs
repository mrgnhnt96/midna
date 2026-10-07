//! PTY spawn / resize / write. Spawning uses `std::process::Command` (PATH lookup, env, cwd)
//! with a `pre_exec` that makes the slave the controlling terminal of a new session.
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

/// Inherited variables that would confuse programs in a fresh terminal (e.g. Claude Code
/// refusing to start "nested" because midnad was launched from inside a Claude session).
/// Verified against Claude Code 2.1.288 run from inside another Claude session: an inherited
/// `CLAUDE_CODE_CHILD_SESSION` turns transcript saving off, and the parent's session id,
/// messaging socket and pid leak into the child. Host-terminal markers (Saggar, iTerm,
/// Terminal.app) would make the user's global hooks attribute a midna agent to the wrong
/// terminal. User configuration (e.g. `CLAUDE_CODE_ENABLE_TODO_TOOLS`) is kept.
pub(crate) const ENV_SCRUB: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SSE_PORT",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
    "AI_AGENT",
    "CLAUDE_HANDOFF_BATON",
    "CLAUDE_HANDOFF_WRAPPER",
    "CLAUDE_HANDOFF_ARGV",
    "CODEX_THREAD_ID",
    "CODEX_SANDBOX",
    "CODEX_SANDBOX_NETWORK_DISABLED",
    "SAGGAR",
    "SAGGAR_SESSION",
    "SAGGAR_CLI_DIR",
    "TERM_SESSION_ID",
    "ITERM_SESSION_ID",
];

/// Serializes openpty+spawn so a concurrently spawning child can't inherit another's fds.
static SPAWN_LOCK: Mutex<()> = Mutex::new(());

/// Hold off PTY spawns (an upgrade clears FD_CLOEXEC on every master while it hands off; a
/// child spawned meanwhile would inherit them).
pub fn spawn_lock() -> std::sync::MutexGuard<'static, ()> {
    SPAWN_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct Spawned {
    pub master: RawFd,
    pub child: Child,
}

fn set_cloexec(fd: RawFd) {
    unsafe {
        let f = libc::fcntl(fd, libc::F_GETFD);
        libc::fcntl(fd, libc::F_SETFD, f | libc::FD_CLOEXEC);
    }
}

pub fn spawn(argv: &[String], cwd: &str, env: &[(String, String)], cols: u16, rows: u16) -> std::io::Result<Spawned> {
    let Some((prog, args)) = argv.split_first() else {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty command"));
    };
    let _g = SPAWN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (mut m, mut s) = (0, 0);
    let mut ws = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
    if unsafe { libc::openpty(&mut m, &mut s, std::ptr::null_mut(), std::ptr::null_mut(), &mut ws) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    set_cloexec(m);
    set_cloexec(s);
    // Command dup2s these onto 0/1/2 (dup2 clears CLOEXEC on the copies).
    let slave = unsafe { OwnedFd::from_raw_fd(s) };
    let mut cmd = Command::new(prog);
    cmd.args(args)
        .current_dir(if std::path::Path::new(cwd).is_dir() { cwd } else { "/" })
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    for k in ENV_SCRUB {
        cmd.env_remove(k);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    unsafe {
        cmd.pre_exec(|| {
            // Only async-signal-safe calls here.
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            libc::ioctl(0, libc::TIOCSCTTY as _, 0);
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
            Ok(())
        });
    }
    match cmd.spawn() {
        Ok(child) => Ok(Spawned { master: m, child }),
        Err(e) => {
            unsafe { libc::close(m) };
            Err(e)
        }
    }
}

pub fn resize(fd: RawFd, cols: u16, rows: u16) {
    let ws = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
    unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &ws) };
}

/// Write everything (the master fd is blocking; retries on EINTR/EAGAIN).
pub fn write_all(fd: RawFd, mut b: &[u8]) -> bool {
    while !b.is_empty() {
        let n = unsafe { libc::write(fd, b.as_ptr() as *const _, b.len()) };
        if n <= 0 {
            let e = std::io::Error::last_os_error();
            if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted) {
                std::thread::sleep(std::time::Duration::from_micros(200));
                continue;
            }
            return false;
        }
        b = &b[n as usize..];
    }
    true
}

/// Resolve a program name against PATH (for error messages before spawning).
pub fn which(prog: &str, path: &str) -> Option<String> {
    if prog.contains('/') {
        return std::path::Path::new(prog).exists().then(|| prog.to_string());
    }
    path.split(':').map(|d| format!("{d}/{prog}")).find(|p| {
        std::fs::metadata(p).map(|m| std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o111 != 0).unwrap_or(false)
    })
}
