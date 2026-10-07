//! Session runtime: PTY reader thread -> engine thread (owns the !Send Terminal) -> attached
//! stream clients with credit ("B-snap"), plus a reaper thread that reports the exit status.
use crate::daemon::Daemon;
use crate::engine::{Engine, now_ns};
use crate::pty;
use midna_proto::Frame;
use std::collections::HashMap;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError, sync_channel};
use std::time::{Duration, Instant};
use std::sync::{Arc, Weak};

/// Owns the PTY master fd; closed when the last holder (reader, engine, writers) drops it.
pub struct PtyFd(pub RawFd);
impl Drop for PtyFd {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}

pub enum EngineMsg {
    Bytes(Vec<u8>, u64),
    Resize(u16, u16, u32, u32),
    /// A size from the GUI's layout, applied once it holds still (`settle`). A full-screen app
    /// told to shrink and grow back faster than it reacts reads its old size, sees no change and
    /// doesn't redraw, while the engine has already moved its rows: Claude Code's caret and its
    /// partial redraws then land rows off.
    SettleResize(u16, u16, u32, u32),
    Attach(u64, SyncSender<Frame>),
    Detach(u64),
    Want(u64),
    /// Reply with (lines, cols, rows): the visible screen or all scrollback+screen.
    Read { screen: bool, reply: Sender<(Vec<String>, u16, u16)> },
    /// Capture the terminal for a daemon upgrade (see `upgrade.rs`). Sent after the PTY
    /// readers are paused, so every byte read so far is already in the snapshot.
    Snapshot(Sender<crate::engine::TermSnap>),
    /// Key / scroll / mouse / focus / paste from a stream client (or an RPC); whatever the
    /// engine encodes for the app is written to the PTY.
    Client(midna_proto::frame::ClientMsg),
    /// Run a closure on the engine thread (selection text, links, find, ...).
    With(Box<dyn FnOnce(&mut Engine) + Send>),
    Stop,
}

#[derive(Clone)]
pub struct RtHandle {
    pub tx: SyncSender<EngineMsg>,
    pub fd: Arc<PtyFd>,
    pub pid: i32,
    /// Restart generation: exits/titles from an older generation are ignored.
    pub generation: u64,
    /// Unix seconds of the last PTY output.
    pub activity: Arc<AtomicI64>,
    /// The app's current kitty keyboard protocol flags (0 = legacy keys), mirrored by the
    /// engine thread so daemon-side key injection can encode keys the way a real terminal would.
    pub kitty: Arc<AtomicU8>,
}

impl RtHandle {
    pub fn write(&self, bytes: &[u8]) -> bool {
        pty::write_all(self.fd.0, bytes)
    }

    /// Send a named key, encoded for the app's keyboard mode.
    pub fn key(&self, k: Key) -> bool {
        self.write(&encode_key(k, self.kitty.load(Ordering::Relaxed)))
    }

    pub fn resize(&self, cols: u16, rows: u16, cw: u32, ch: u32) {
        let _ = self.tx.send(EngineMsg::Resize(cols, rows, cw, ch));
    }

    /// Hand a client message (key, scroll, mouse, focus, paste) to the engine thread.
    pub fn client(&self, m: midna_proto::frame::ClientMsg) -> bool {
        self.tx.send(EngineMsg::Client(m)).is_ok()
    }

    /// Run `f` on the engine thread and wait (briefly) for its answer.
    pub fn with<T: Send + 'static>(&self, f: impl FnOnce(&mut Engine) -> T + Send + 'static) -> Option<T> {
        let (tx, rx) = std::sync::mpsc::channel();
        let job: Box<dyn FnOnce(&mut Engine) + Send> = Box::new(move |e| {
            let _ = tx.send(f(e));
        });
        self.tx.send(EngineMsg::With(job)).ok()?;
        rx.recv_timeout(std::time::Duration::from_secs(5)).ok()
    }

    /// Read text from the engine (blocks briefly on the engine thread).
    pub fn read(&self, screen: bool) -> Option<(Vec<String>, u16, u16)> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.tx.send(EngineMsg::Read { screen, reply: tx }).ok()?;
        rx.recv_timeout(std::time::Duration::from_secs(5)).ok()
    }

    /// Hang up the process group; SIGKILL after a grace period if `force`.
    pub fn kill(&self, force: bool) {
        unsafe {
            libc::kill(-self.pid, libc::SIGHUP);
            libc::kill(self.pid, libc::SIGHUP);
        }
        let pid = self.pid;
        let grace = if force { 500 } else { 3000 };
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(grace));
            unsafe {
                // Still there? (kill 0 probes; a reaped pid returns ESRCH.)
                if libc::kill(pid, 0) == 0 {
                    libc::kill(-pid, libc::SIGKILL);
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        });
    }
}

/// Keys the daemon itself injects (Enter after `session.input`, prompt answers).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Enter,
    Escape,
    Up,
    Down,
}

/// Legacy bytes, or the kitty CSI-u form once the app has enabled the kitty keyboard protocol.
/// Claude Code 2.1.288 enables it as soon as the terminal answers the query (libghostty does):
/// from then on a bare `\r` inserts a newline instead of submitting, and only `CSI 13 u` submits.
/// Unmodified arrows are the same `CSI A` / `CSI B` in both modes.
pub fn encode_key(k: Key, kitty_flags: u8) -> Vec<u8> {
    match (k, kitty_flags != 0) {
        (Key::Up, _) => b"\x1b[A".to_vec(),
        (Key::Down, _) => b"\x1b[B".to_vec(),
        (Key::Enter, false) => b"\r".to_vec(),
        (Key::Enter, true) => b"\x1b[13u".to_vec(),
        (Key::Escape, false) => b"\x1b".to_vec(),
        (Key::Escape, true) => b"\x1b[27u".to_vec(),
    }
}

pub struct Launch {
    pub sid: String,
    pub argv: Vec<String>,
    pub cwd: String,
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
}

/// Spawn the PTY and its threads. Callbacks into the daemon use a Weak so sessions never keep
/// a shut-down daemon alive.
/// Largest grid a client may ask for. A terminal engine allocates cols × rows cells (plus
/// scrollback), so an unclamped resize to 65535 × 65535 from any client would exhaust memory.
pub const MAX_COLS: u16 = 1000;
pub const MAX_ROWS: u16 = 500;

pub fn clamp_size(cols: u16, rows: u16) -> (u16, u16) {
    (cols.clamp(2, MAX_COLS), rows.clamp(2, MAX_ROWS))
}

/// A GUI size is applied once no newer one has come for `SETTLE`, and within `SETTLE_MAX` of
/// the first while a window is being dragged.
const SETTLE: Duration = Duration::from_millis(50);
const SETTLE_MAX: Duration = Duration::from_millis(200);

/// A GUI size waiting to settle (`EngineMsg::SettleResize`).
struct Settling {
    size: (u16, u16, u32, u32),
    /// When it's applied: `SETTLE` after the latest size, but no later than `by`.
    at: Instant,
    /// `SETTLE_MAX` after the first size of this run.
    by: Instant,
}

impl Settling {
    fn due(&self) -> bool {
        Instant::now() >= self.at
    }
}

fn apply_resize(eng: &mut Engine, fd: RawFd, (c, r, cw, ch): (u16, u16, u32, u32)) {
    let (c, r) = clamp_size(c, r);
    let (cw, ch) = (cw.min(1000), ch.min(1000));
    eng.resize(c, r, cw, ch);
    pty::resize(fd, c, r);
}

pub fn start(d: &Arc<Daemon>, mut l: Launch) -> std::io::Result<RtHandle> {
    (l.cols, l.rows) = clamp_size(l.cols, l.rows);
    let sp = pty::spawn(&l.argv, &l.cwd, &l.env, l.cols, l.rows)?;
    let pid = sp.child.id() as i32;
    // The reaper waits on the pid (not the Child), so adopted sessions after an upgrade work
    // the same way. Dropping a Child neither waits nor kills.
    drop(sp.child);
    let (m, cols, rows) = (sp.master, l.cols, l.rows);
    Ok(run(d, &l.sid, sp.master, pid, true, Box::new(move || Engine::new(cols, rows, Some(m))), String::new()))
}

/// Re-adopt a session whose PTY master fd and child survived a daemon upgrade (same PID
/// execv) or were handed to the watchdog fallback (then the child isn't ours: exit tracking
/// falls back to kqueue, without an exit code).
pub fn adopt(d: &Arc<Daemon>, sid: &str, master: RawFd, pid: i32, alive: bool, snap: &crate::engine::TermSnap) -> RtHandle {
    let snap = snap.clone();
    let title = snap.title.clone();
    run(d, sid, master, pid, alive, Box::new(move || Engine::restored(&snap, Some(master))), title)
}

/// Builds the engine on its own thread (libghostty's Terminal is !Send).
type MakeEngine = Box<dyn FnOnce() -> Engine + Send>;

fn run(d: &Arc<Daemon>, sid: &str, master: RawFd, pid: i32, reap: bool, eng: MakeEngine, title: String) -> RtHandle {
    let fd = Arc::new(PtyFd(master));
    let generation = d.next_gen.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = sync_channel::<EngineMsg>(64);
    let activity = Arc::new(AtomicI64::new(midna_proto::time::now_unix()));
    let kitty = Arc::new(AtomicU8::new(0));
    let h = RtHandle { tx: tx.clone(), fd: fd.clone(), pid, generation, activity: activity.clone(), kitty: kitty.clone() };
    let weak = Arc::downgrade(d);
    engine_thread(sid.to_string(), generation, eng, title, fd.clone(), rx, weak.clone(), kitty);
    let drained = reader_thread(fd, tx, activity);
    if reap {
        reaper_thread(sid.to_string(), generation, pid, weak, drained);
    }
    h
}

// ---------------------------------------------------------------- reader pause (upgrades)

static READS_PAUSED: AtomicBool = AtomicBool::new(false);
/// Readers between "poll said readable" and "bytes handed to the engine".
static READS_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// Stop every PTY reader from taking more bytes (they stay in the kernel buffer for the next
/// image). Returns once no reader holds bytes the engines haven't been sent yet.
pub fn pause_readers(timeout: std::time::Duration) -> bool {
    READS_PAUSED.store(true, Ordering::SeqCst);
    let t0 = std::time::Instant::now();
    while READS_IN_FLIGHT.load(Ordering::SeqCst) != 0 {
        if t0.elapsed() > timeout {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    true
}

pub fn resume_readers() {
    READS_PAUSED.store(false, Ordering::SeqCst);
}

/// The returned receiver disconnects once the reader has handed its last bytes to the engine.
fn reader_thread(fd: Arc<PtyFd>, tx: SyncSender<EngineMsg>, activity: Arc<AtomicI64>) -> Receiver<()> {
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let _ = std::thread::Builder::new().name("pty-reader".into()).spawn(move || {
        let _done = done_tx;
        let mut buf = vec![0u8; 65536];
        loop {
            if READS_PAUSED.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(5));
                continue;
            }
            // Poll (not a blocking read) so a pause takes effect within one interval.
            let mut pfd = libc::pollfd { fd: fd.0, events: libc::POLLIN, revents: 0 };
            let r = unsafe { libc::poll(&mut pfd, 1, 50) };
            if r == 0 || (r < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted) {
                continue;
            }
            READS_IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
            if READS_PAUSED.load(Ordering::SeqCst) {
                READS_IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
                continue;
            }
            let n = unsafe { libc::read(fd.0, buf.as_mut_ptr() as *mut _, buf.len()) };
            if n < 0 && matches!(std::io::Error::last_os_error().kind(), std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock) {
                READS_IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
                continue;
            }
            if n <= 0 {
                READS_IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
                break; // EOF / EIO: the child side is gone
            }
            activity.store(midna_proto::time::now_unix(), Ordering::Relaxed);
            let sent = tx.send(EngineMsg::Bytes(buf[..n as usize].to_vec(), now_ns())).is_ok();
            READS_IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
            if !sent {
                break;
            }
        }
    });
    done_rx
}

fn reaper_thread(sid: String, generation: u64, pid: i32, d: Weak<Daemon>, drained: Receiver<()>) {
    let _ = std::thread::Builder::new().name("reaper".into()).spawn(move || {
        let (code, signal) = wait_exit(pid);
        // Let the last output reach the engine first (the exit handler reads the screen). A
        // background process still holding the PTY open keeps the reader going: don't wait long.
        let _ = drained.recv_timeout(std::time::Duration::from_millis(500));
        if let Some(d) = d.upgrade() {
            crate::rpc::session::on_exit(&d, &sid, generation, code, signal);
        }
    });
}

/// Block until `pid` exits: `(exit code, signal)`. Our own children (including ones kept
/// across a same-PID execv upgrade) are reaped with waitpid. A process that isn't our child
/// (adopted by the watchdog fallback) is watched with kqueue `EVFILT_PROC NOTE_EXIT`; macOS
/// only reports exit statuses to the parent, so both values are None then.
pub fn wait_exit(pid: i32) -> (Option<i32>, Option<i32>) {
    loop {
        let mut st = 0;
        let r = unsafe { libc::waitpid(pid, &mut st, 0) };
        if r == pid {
            if libc::WIFEXITED(st) {
                return (Some(libc::WEXITSTATUS(st)), None);
            }
            if libc::WIFSIGNALED(st) {
                return (None, Some(libc::WTERMSIG(st)));
            }
            continue; // stopped/continued: keep waiting
        }
        let e = std::io::Error::last_os_error();
        if e.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        break; // ECHILD: not our child
    }
    kqueue_wait_exit(pid);
    (None, None)
}

fn kqueue_wait_exit(pid: i32) {
    unsafe {
        let kq = libc::kqueue();
        if kq < 0 {
            while libc::kill(pid, 0) == 0 {
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            return;
        }
        let mut ev: libc::kevent = std::mem::zeroed();
        ev.ident = pid as usize;
        ev.filter = libc::EVFILT_PROC;
        ev.flags = libc::EV_ADD | libc::EV_ONESHOT;
        ev.fflags = libc::NOTE_EXIT;
        if libc::kevent(kq, &ev, 1, std::ptr::null_mut(), 0, std::ptr::null()) == 0 {
            let mut out: libc::kevent = std::mem::zeroed();
            loop {
                let n = libc::kevent(kq, std::ptr::null(), 0, &mut out, 1, std::ptr::null());
                if n > 0 || (n < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted) {
                    break;
                }
            }
        } // registration fails with ESRCH when it's already gone
        libc::close(kq);
    }
}

struct Client {
    pending: Option<Frame>,
    credit: bool,
    out: SyncSender<Frame>,
}

#[allow(clippy::too_many_arguments)]
fn engine_thread(sid: String, generation: u64, make: MakeEngine, title: String, fd: Arc<PtyFd>, rx: Receiver<EngineMsg>, d: Weak<Daemon>, kitty: Arc<AtomicU8>) {
    let _ = std::thread::Builder::new().name(format!("engine-{sid}")).spawn(move || {
        let mut eng = make();
        kitty.store(eng.kitty_flags(), Ordering::Relaxed);
        let mut clients: HashMap<u64, Client> = HashMap::new();
        let mut title = title;
        let mut settling: Option<Settling> = None;
        // Returns false on Stop.
        let handle = |eng: &mut Engine, clients: &mut HashMap<u64, Client>, settling: &mut Option<Settling>, m: EngineMsg| -> bool {
            // Reads and snapshots see the size the GUI last asked for.
            if matches!(m, EngineMsg::Read { .. } | EngineMsg::Snapshot(_))
                && let Some(s) = settling.take()
            {
                apply_resize(eng, fd.0, s.size);
            }
            match m {
                EngineMsg::Bytes(b, t) => eng.feed(&b, t),
                EngineMsg::Resize(c, r, cw, ch) => {
                    *settling = None;
                    apply_resize(eng, fd.0, (c, r, cw, ch));
                }
                EngineMsg::SettleResize(c, r, cw, ch) => {
                    let now = Instant::now();
                    let by = settling.as_ref().map_or(now + SETTLE_MAX, |s| s.by);
                    *settling = Some(Settling { size: (c, r, cw, ch), by, at: (now + SETTLE).min(by) });
                }
                EngineMsg::Attach(id, out) => {
                    eng.force_full();
                    clients.insert(id, Client { pending: None, credit: true, out });
                }
                EngineMsg::Detach(id) => {
                    clients.remove(&id);
                }
                EngineMsg::Want(id) => {
                    if let Some(c) = clients.get_mut(&id) {
                        c.credit = true;
                    }
                }
                EngineMsg::Read { screen, reply } => {
                    let (c, r) = eng.size();
                    let lines = if screen { eng.screen_lines() } else { eng.plain_lines() };
                    let _ = reply.send((lines, c, r));
                }
                EngineMsg::Snapshot(reply) => {
                    let _ = reply.send(eng.snapshot());
                }
                EngineMsg::Client(m) => {
                    use midna_proto::frame::ClientMsg;
                    let out = match m {
                        ClientMsg::Key(k) => eng.key(&k),
                        ClientMsg::Scroll(sc) => eng.scroll(&sc),
                        ClientMsg::Mouse(mm) => eng.mouse(&mm),
                        ClientMsg::Focus(g) => eng.focus(g),
                        ClientMsg::Paste(t) => eng.paste(&t),
                        ClientMsg::Input(b) => {
                            eng.snap_to_bottom();
                            eng.clear_selection();
                            b
                        }
                        _ => vec![],
                    };
                    if !out.is_empty() {
                        pty::write_all(fd.0, &out);
                    }
                }
                EngineMsg::With(f) => f(eng),
                EngineMsg::Stop => return false,
            }
            true
        };
        'outer: loop {
            // While a GUI size settles, wake when it's due even if nothing else arrives.
            let m = match settling.as_ref().map(|s| s.at.saturating_duration_since(Instant::now())) {
                None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
                Some(wait) => rx.recv_timeout(wait),
            };
            match m {
                Ok(m) => {
                    if !handle(&mut eng, &mut clients, &mut settling, m) {
                        break;
                    }
                    // Drain what's queued (bounded so frames keep flowing under a flood).
                    for _ in 0..256 {
                        match rx.try_recv() {
                            Ok(m) => {
                                if !handle(&mut eng, &mut clients, &mut settling, m) {
                                    break 'outer;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            if let Some(s) = settling.take_if(|s| s.due()) {
                apply_resize(&mut eng, fd.0, s.size);
            }
            kitty.store(eng.kitty_flags(), Ordering::Relaxed);
            // Title (OSC 0/2) changes are reported to the daemon.
            let t = eng.title();
            if t != title {
                title = t;
                if let Some(d) = d.upgrade() {
                    let mut screen = || eng.screen_lines();
                    crate::rpc::session::on_title(&d, &sid, generation, &title, &mut screen);
                }
            }
            // Only build a frame when someone can take it; every client's pending gets the
            // dirty rows so clients without credit don't miss them.
            if clients.values().any(|c| c.credit) {
                if let Some(f) = eng.frame() {
                    for c in clients.values_mut() {
                        match c.pending.as_mut() {
                            Some(p) => p.merge(f.clone()),
                            None => c.pending = Some(f.clone()),
                        }
                    }
                }
                clients.retain(|_, c| {
                    if c.credit
                        && let Some(f) = c.pending.take() {
                            c.credit = false;
                            // Full = the writer still holds a frame (can't happen with credit);
                            // Disconnected = the stream connection is gone.
                            return !matches!(c.out.try_send(f), Err(TrySendError::Disconnected(_)));
                        }
                    true
                });
            }
        }
        // Dropping `clients` closes every stream writer; dropping `fd` may close the PTY.
    });
}
