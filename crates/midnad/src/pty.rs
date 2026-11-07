//! PTY spawn / resize / write. Spawning uses `std::process::Command` (PATH lookup, env, cwd)
//! with a `pre_exec` that makes the slave the controlling terminal of a new session.
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The most unread input a terminal holds. macOS stops taking a write here, wherever in the
/// write that falls.
const INPUT_ROOM: usize = 1022;
/// How long a write waits for an app that has stopped reading its input.
const ROOM_WAIT: Duration = Duration::from_secs(2);

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

/// Bytes the app hasn't read from its input yet.
fn unread_input(fd: RawFd) -> Option<usize> {
    let mut n: libc::c_int = 0;
    (unsafe { libc::ioctl(fd, libc::FIONREAD, &mut n) } == 0).then_some(n as usize)
}

/// Wait for room for all of `len` bytes. A key's escape sequence that doesn't fit is cut where
/// the room ends: an app that's behind reads `ESC [` of `ESC [ 3 ~`, gives up on it and takes the
/// `3~` that follows for typing.
fn wait_for_room(fd: RawFd, len: usize) {
    let by = Instant::now() + ROOM_WAIT;
    while unread_input(fd).is_some_and(|n| n + len > INPUT_ROOM) && Instant::now() < by {
        std::thread::sleep(Duration::from_micros(200));
    }
}

/// Write everything (the master fd is blocking; retries on EINTR/EAGAIN), in one piece if it can
/// be.
pub fn write_all(fd: RawFd, mut b: &[u8]) -> bool {
    if b.len() <= INPUT_ROOM {
        wait_for_room(fd, b.len());
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    /// A pty in raw mode, as a full-screen app sets it: (master, slave).
    fn raw_pty() -> (OwnedFd, OwnedFd) {
        let (mut master, mut slave) = (-1, -1);
        let made = unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) };
        assert_eq!(made, 0);
        let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            libc::tcgetattr(slave.as_raw_fd(), &mut t);
            libc::cfmakeraw(&mut t);
            libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &t);
        }
        (master, slave)
    }

    #[test]
    fn a_burst_of_keys_reaches_a_slow_app_whole() {
        const KEY: &[u8] = b"\x1b[3~";
        const PRESSES: usize = 2000;
        let (master, slave) = raw_pty();
        let fd = master.as_raw_fd();
        let writer = std::thread::spawn(move || (0..PRESSES).all(|_| write_all(fd, KEY)));
        let (mut buf, mut total) = ([0u8; 4096], 0);
        while total < PRESSES * KEY.len() {
            std::thread::sleep(Duration::from_millis(2));
            let n = unsafe { libc::read(slave.as_raw_fd(), buf.as_mut_ptr() as *mut _, buf.len()) };
            assert!(n > 0);
            assert_eq!(n as usize % KEY.len(), 0, "a read ended inside a key, {total} bytes in");
            total += n as usize;
        }
        assert!(writer.join().unwrap());
    }

    #[test]
    fn an_app_that_stops_reading_doesnt_hold_a_write_forever() {
        let (master, _slave) = raw_pty();
        let fd = master.as_raw_fd();
        assert!(write_all(fd, &[b'x'; INPUT_ROOM - 2]));
        let start = Instant::now();
        wait_for_room(fd, 4);
        assert!((ROOM_WAIT..ROOM_WAIT * 2).contains(&start.elapsed()));
    }
}
