//! Throwaway spike: can midnad re-exec itself (same PID) without its PTY children noticing?
//! Not product code.
use libghostty_vt::fmt::{Format, Formatter, FormatterOptions};
use libghostty_vt::render::RenderState;
use libghostty_vt::screen::Screen;
use libghostty_vt::terminal::{Mode, Options, Terminal};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::ffi::CString;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::{Duration, Instant};

const RING_CAP: usize = 4 << 20;
const STATE_VERSION: u32 = 1;

fn log(msg: &str) {
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
    eprintln!("[{}.{:03} pid={} {}] {}", ts.as_secs() % 1000, ts.subsec_millis(), std::process::id(), whoami(), msg);
}
fn whoami() -> String {
    std::env::args().next().map(|a| a.rsplit('/').next().unwrap_or("").to_string()).unwrap_or_default()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
}

fn set_cloexec(fd: RawFd, on: bool) {
    unsafe {
        let f = libc::fcntl(fd, libc::F_GETFD);
        let nf = if on { f | libc::FD_CLOEXEC } else { f & !libc::FD_CLOEXEC };
        libc::fcntl(fd, libc::F_SETFD, nf);
    }
}
fn set_nonblock(fd: RawFd, on: bool) {
    unsafe {
        let f = libc::fcntl(fd, libc::F_GETFL);
        let nf = if on { f | libc::O_NONBLOCK } else { f & !libc::O_NONBLOCK };
        libc::fcntl(fd, libc::F_SETFL, nf);
    }
}

// ---------------------------------------------------------------- snapshot / restore

#[derive(Serialize, Deserialize, Debug, Clone)]
struct TermSnap {
    cols: u16,
    rows: u16,
    alt_active: bool,
    primary_vt: String, // hex
    alt_vt: Option<String>,
    title: String,
    cursor_shape: String, // from RenderState, replayed as DECSCUSR
    cursor_blink: bool,
    primary_cursor: (u16, u16),
    alt_cursor: Option<(u16, u16)>,
}

fn vt_opts<'a, 'b>() -> FormatterOptions<'a, 'b> {
    FormatterOptions::new()
        .with_format(Format::Vt)
        .with_unwrap(false)
        .with_trim(false)
        .with_palette(true)
        .with_modes(true)
        .with_scrolling_region(true)
        .with_tabstops(true)
        .with_pwd(true)
        .with_keyboard(true)
        .with_cursor(true)
        .with_style(true)
        .with_hyperlink(true)
        .with_protection(true)
        .with_kitty_keyboard(true)
        .with_charsets(true)
}

fn fmt_vt(t: &Terminal) -> Vec<u8> {
    let mut f = Formatter::new(t, vt_opts()).unwrap();
    f.format_alloc(None).unwrap().to_vec()
}
fn fmt_plain(t: &Terminal) -> String {
    let mut f = Formatter::new(t, FormatterOptions::new().with_format(Format::Plain).with_trim(true)).unwrap();
    String::from_utf8_lossy(&f.format_alloc(None).unwrap()).to_string()
}

fn cursor_shape(t: &Terminal) -> (String, bool) {
    let mut rs = RenderState::new().unwrap();
    let snap = rs.update(t).unwrap();
    (format!("{:?}", snap.cursor_visual_style().unwrap()), snap.cursor_blinking().unwrap_or(false))
}

/// NOTE: destructive for alt-screen terminals (switches back to primary to export it).
fn snapshot(t: &mut Terminal) -> TermSnap {
    let alt = matches!(t.active_screen().unwrap(), Screen::Alternate);
    let title = t.title().unwrap_or("").to_string();
    let (shape, blink) = cursor_shape(t);
    let cur = |t: &Terminal| (t.cursor_x().unwrap(), t.cursor_y().unwrap());
    let alt_cursor = if alt { Some(cur(t)) } else { None };
    let alt_vt = if alt { Some(hex(&fmt_vt(t))) } else { None };
    if alt {
        t.vt_write(b"\x1b[?1049l");
    }
    let primary_cursor = cur(t);
    let primary_vt = hex(&fmt_vt(t));
    TermSnap { cols: t.cols().unwrap(), rows: t.rows().unwrap(), alt_active: alt, primary_vt, alt_vt, title, cursor_shape: shape, cursor_blink: blink, primary_cursor, alt_cursor }
}

fn restore(s: &TermSnap) -> Terminal<'static, 'static> {
    let mut t = Terminal::new(Options { cols: s.cols, rows: s.rows, max_scrollback: 10_000 }).unwrap();
    let fix = std::env::var("NO_CURSOR_FIX").is_err();
    let cup = |(x, y): (u16, u16)| format!("\x1b[{};{}H", y + 1, x + 1);
    // WORKAROUND: libghostty-vt VT export (a) assumes cursor starts at home, and (b) emits CUP
    // *before* DECSTBM/tabstops, both of which move the cursor. Re-issue CUP at the end.
    if fix { t.vt_write(b"\x1b[H"); }
    t.vt_write(&unhex(&s.primary_vt));
    if fix { t.vt_write(cup(s.primary_cursor).as_bytes()); }
    if let Some(a) = &s.alt_vt {
        t.vt_write(b"\x1b[?1049h");
        if fix { t.vt_write(b"\x1b[H"); }
        t.vt_write(&unhex(a));
        if fix { t.vt_write(cup(s.alt_cursor.unwrap()).as_bytes()); }
    }
    if !s.title.is_empty() {
        t.vt_write(format!("\x1b]2;{}\x07", s.title).as_bytes());
    }
    let n = match (s.cursor_shape.as_str(), s.cursor_blink) {
        ("Block", true) => 1, ("Block", false) => 2, ("Underline", true) => 3, ("Underline", false) => 4,
        ("Bar", true) => 5, ("Bar", false) => 6, _ => 0,
    };
    if n != 0 {
        t.vt_write(format!("\x1b[{n} q").as_bytes());
    }
    t
}

fn describe(t: &Terminal) -> String {
    let modes: [(&str, Mode); 14] = [
        ("DECCKM", Mode::DECCKM), ("ORIGIN", Mode::ORIGIN), ("WRAP", Mode::WRAPAROUND), ("CURSOR_VISIBLE", Mode::CURSOR_VISIBLE),
        ("CURSOR_BLINK", Mode::CURSOR_BLINKING), ("KEYPAD", Mode::KEYPAD_KEYS), ("MOUSE1000", Mode::NORMAL_MOUSE),
        ("MOUSE1002", Mode::BUTTON_MOUSE), ("MOUSE1003", Mode::ANY_MOUSE), ("SGR_MOUSE", Mode::SGR_MOUSE),
        ("FOCUS", Mode::FOCUS_EVENT), ("ALT1049", Mode::ALT_SCREEN_SAVE), ("BRACKETED_PASTE", Mode::BRACKETED_PASTE),
        ("INSERT", Mode::INSERT),
    ];
    let mut out = String::new();
    let (shape, blink) = cursor_shape(t);
    out += &format!(
        "screen={:?} cursor=({},{}) shape={} blink={} kitty={:?} title={:?} pwd={:?} scrollback_rows={}\n",
        t.active_screen().unwrap(), t.cursor_x().unwrap(), t.cursor_y().unwrap(), shape, blink,
        t.kitty_keyboard_flags().unwrap(), t.title().unwrap_or(""), t.pwd().unwrap_or(""), t.scrollback_rows().unwrap_or(0)
    );
    for (n, m) in modes {
        out += &format!("{n}={} ", t.mode(m).unwrap() as u8);
    }
    out
}

// ---------------------------------------------------------------- fidelity test (no PTYs)

fn fidelity() {
    let mut t = Terminal::new(Options { cols: 40, rows: 8, max_scrollback: 1000 }).unwrap();
    for i in 0..20 {
        t.vt_write(format!("primary line {i} \x1b[1;31mred\x1b[0m\r\n").as_bytes());
    }
    t.vt_write(b"\x1b]2;my title\x07\x1b]7;file://host/tmp/x\x07");
    t.vt_write(b"\x1b[?2004h"); // bracketed paste
    t.vt_write(b"\x1b[?1h"); // DECCKM
    t.vt_write(b"\x1b[3;3Hprompt$ "); // primary cursor
    t.vt_write(b"\x1b[?1049h"); // alt screen (saves cursor)
    t.vt_write(b"\x1b[?1002h\x1b[?1006h\x1b[?1004h"); // mouse + focus
    t.vt_write(b"\x1b[>5u"); // kitty keyboard push flags=5
    t.vt_write(b"\x1b[2;6r"); // scroll region
    t.vt_write(b"\x1b[5 q"); // blinking bar
    t.vt_write(b"\x1b[?25l"); // hide cursor
    t.vt_write(b"\x1b[1;1H\x1b[7m ALT HEADER \x1b[0m\x1b[4;10Halt body\x1b[5;12H");
    let before = describe(&t);
    let before_alt = fmt_plain(&t);
    let snap = snapshot(&mut t);
    let before_primary = fmt_plain(&t);
    println!("primary_vt bytes={} alt_vt bytes={}", snap.primary_vt.len() / 2, snap.alt_vt.as_ref().map_or(0, |a| a.len() / 2));
    if std::env::var("SHOW_RAW").is_ok() { println!("--- raw alt VT export ---\n{:?}", String::from_utf8_lossy(&unhex(snap.alt_vt.as_ref().unwrap())));
    let pv = String::from_utf8_lossy(&unhex(&snap.primary_vt)).to_string();
    println!("--- raw primary VT export (tail) ---\n{:?}", &pv[pv.len().saturating_sub(300)..]); }
    let mut r = restore(&snap);
    let after = describe(&r);
    let after_alt = fmt_plain(&r);
    println!("BEFORE: {before}\nAFTER:  {after}");
    println!("alt plain equal: {}", before_alt == after_alt);
    if before_alt != after_alt {
        println!("before_alt={before_alt:?}\nafter_alt={after_alt:?}");
    }
    // Scroll region check: write lines at row 6 in both, see whether region scrolled.
    // leave alt on restored and compare primary
    r.vt_write(b"\x1b[?1049l");
    let after_primary = fmt_plain(&r);
    println!("primary plain equal: {} (before {} chars, after {} chars)", before_primary == after_primary, before_primary.len(), after_primary.len());
    if before_primary != after_primary {
        println!("before_primary={before_primary:?}\nafter_primary={after_primary:?}");
    }
    println!("primary after leaving alt: {}", describe(&r));
    // scroll-region probe on a fresh pair
    let mut a = Terminal::new(Options { cols: 20, rows: 6, max_scrollback: 100 }).unwrap();
    a.vt_write(b"1\r\n2\r\n3\r\n4\r\n5\r\n6\x1b[2;4r\x1b[4;1H");
    let s2 = snapshot(&mut a);
    let mut b = restore(&s2);
    a.vt_write(b"\nX");
    b.vt_write(b"\nX");
    println!("scroll-region probe equal: {}\n a={:?}\n b={:?}", fmt_plain(&a) == fmt_plain(&b), fmt_plain(&a), fmt_plain(&b));
    // partial escape sequence at snapshot time
    let mut p = Terminal::new(Options { cols: 20, rows: 4, max_scrollback: 100 }).unwrap();
    p.vt_write(b"hello \x1b[3");
    let s3 = snapshot(&mut p);
    let mut q = restore(&s3);
    p.vt_write(b"1mRED");
    q.vt_write(b"1mRED");
    println!("partial-escape probe: orig={:?} restored={:?}", fmt_plain(&p), fmt_plain(&q));
}

// ---------------------------------------------------------------- daemon

struct Session {
    id: u32,
    fd: RawFd,
    pid: i32,
    term: Terminal<'static, 'static>,
    seq: u64,
    ring: VecDeque<(u64, Vec<u8>)>,
    ring_bytes: usize,
    exited: Option<i32>,
}

#[derive(Serialize, Deserialize)]
struct SessionState {
    id: u32,
    fd: RawFd,
    pid: i32,
    seq: u64,
    kernel_pending_bytes: i32,
    term: TermSnap,
    ring: Vec<(u64, String)>,
}

#[derive(Serialize, Deserialize)]
struct DaemonState {
    version: u32,
    from_binary: String,
    listener_fd: RawFd,
    sock_path: String,
    next_id: u32,
    sessions: Vec<SessionState>,
}

struct Client {
    stream: UnixStream,
    buf: Vec<u8>,
    attached: Option<u32>,
}

struct Daemon {
    listener: UnixListener,
    sock_path: String,
    sessions: Vec<Session>,
    clients: Vec<Client>,
    next_id: u32,
}

fn spawn_pty(cmd: &str, cols: u16, rows: u16) -> (RawFd, i32) {
    let sh = CString::new("/bin/sh").unwrap();
    let a0 = CString::new("sh").unwrap();
    let a1 = CString::new("-c").unwrap();
    let a2 = CString::new(format!("exec {cmd}")).unwrap();
    let argv = [a0.as_ptr(), a1.as_ptr(), a2.as_ptr(), std::ptr::null()];
    unsafe {
        let mut m = 0;
        let mut s = 0;
        let mut ws = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
        assert_eq!(libc::openpty(&mut m, &mut s, std::ptr::null_mut(), std::ptr::null_mut(), &mut ws), 0);
        let pid = libc::fork();
        if pid == 0 {
            libc::setsid();
            libc::ioctl(s, libc::TIOCSCTTY as _, 0);
            libc::dup2(s, 0);
            libc::dup2(s, 1);
            libc::dup2(s, 2);
            if s > 2 {
                libc::close(s);
            }
            libc::close(m);
            // reset signal dispositions we changed in the daemon
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
            libc::execv(sh.as_ptr(), argv.as_ptr());
            libc::_exit(127);
        }
        libc::close(s);
        set_cloexec(m, true);
        set_nonblock(m, true);
        (m, pid)
    }
}

fn send_frame(c: &mut Client, header: String, body: &[u8]) -> bool {
    c.stream.write_all(header.as_bytes()).is_ok() && c.stream.write_all(body).is_ok()
}

impl Daemon {
    fn push_output(&mut self, idx: usize, data: Vec<u8>) {
        let s = &mut self.sessions[idx];
        s.term.vt_write(&data);
        s.seq += 1;
        let seq = s.seq;
        let id = s.id;
        s.ring_bytes += data.len();
        s.ring.push_back((seq, data.clone()));
        while s.ring_bytes > RING_CAP {
            let (_, d) = s.ring.pop_front().unwrap();
            s.ring_bytes -= d.len();
        }
        self.clients.retain_mut(|c| {
            if c.attached == Some(id) {
                send_frame(c, format!("O {id} {seq} {}\n", data.len()), &data)
            } else {
                true
            }
        });
    }

    fn handle_cmd(&mut self, ci: usize, line: &str) -> Option<String> {
        let mut parts = line.splitn(2, ' ');
        let cmd = parts.next().unwrap_or("");
        let rest = parts.next().unwrap_or("");
        let reply = |s: String| Some(s);
        match cmd {
            "SPAWN" => {
                let (fd, pid) = spawn_pty(rest, 100, 30);
                let id = self.next_id;
                self.next_id += 1;
                let term = Terminal::new(Options { cols: 100, rows: 30, max_scrollback: 10_000 }).unwrap();
                self.sessions.push(Session { id, fd, pid, term, seq: 0, ring: VecDeque::new(), ring_bytes: 0, exited: None });
                log(&format!("spawned session {id} pid={pid} fd={fd}: {rest}"));
                reply(format!("id={id} pid={pid}"))
            }
            "LIST" => reply(
                self.sessions
                    .iter()
                    .map(|s| format!("id={} pid={} fd={} seq={} exited={:?}\n", s.id, s.pid, s.fd, s.seq, s.exited))
                    .collect::<String>()
                    + &format!("daemon pid={} binary={}\n", std::process::id(), whoami()),
            ),
            "DUMP" => {
                let id: u32 = rest.trim().parse().unwrap_or(0);
                let s = self.sessions.iter().find(|s| s.id == id)?;
                reply(format!("{}\n----\n{}", describe(&s.term), fmt_plain(&s.term)))
            }
            "SEND" => {
                let mut p = rest.splitn(2, ' ');
                let id: u32 = p.next()?.parse().ok()?;
                let data = unhex(p.next()?.trim());
                let s = self.sessions.iter().find(|s| s.id == id)?;
                let n = unsafe { libc::write(s.fd, data.as_ptr() as *const _, data.len()) };
                reply(format!("wrote {n}"))
            }
            "ATTACH" => {
                let mut p = rest.split(' ');
                let id: u32 = p.next()?.parse().ok()?;
                let since: u64 = p.next().unwrap_or("0").trim().parse().unwrap_or(0);
                let s = self.sessions.iter().find(|s| s.id == id)?;
                let frames: Vec<(u64, Vec<u8>)> = s.ring.iter().filter(|(q, _)| *q > since).cloned().collect();
                let c = &mut self.clients[ci];
                c.attached = Some(id);
                log(&format!("client attached to {id} since={since}, replaying {} chunks", frames.len()));
                for (q, d) in frames {
                    send_frame(c, format!("O {id} {q} {}\n", d.len()), &d);
                }
                None
            }
            "UPGRADE" => {
                let res = self.upgrade(rest.trim());
                reply(res)
            }
            _ => reply(format!("unknown command {cmd}")),
        }
    }

    /// Returns only on failure (on success we execv and never return).
    fn upgrade(&mut self, new_bin: &str) -> String {
        let t0 = Instant::now();
        log(&format!("UPGRADE requested -> {new_bin}; pausing PTY reads"));
        // Reads are paused implicitly: we're single-threaded and don't return to poll().
        let mut sessions = vec![];
        for s in &mut self.sessions {
            let mut pending: i32 = 0;
            unsafe { libc::ioctl(s.fd, libc::FIONREAD, &mut pending) };
            let term = snapshot(&mut s.term);
            sessions.push(SessionState {
                id: s.id, fd: s.fd, pid: s.pid, seq: s.seq, kernel_pending_bytes: pending, term,
                ring: s.ring.iter().map(|(q, d)| (*q, hex(d))).collect(),
            });
        }
        let state = DaemonState {
            version: STATE_VERSION, from_binary: whoami(), listener_fd: self.listener.as_raw_fd(),
            sock_path: self.sock_path.clone(), next_id: self.next_id, sessions,
        };
        let path = format!("{}.state.json", self.sock_path);
        std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
        let snap_ms = t0.elapsed().as_millis();
        for s in &state.sessions {
            log(&format!("  session {} pid={} seq={} kernel_pending_bytes={} alt={}", s.id, s.pid, s.seq, s.kernel_pending_bytes, s.term.alt_active));
        }
        // Preflight: v2 must be able to parse/restore the state.
        let pf = std::process::Command::new(new_bin).arg("--preflight").arg(&path).output();
        let ok = matches!(&pf, Ok(o) if o.status.success());
        if !ok {
            let why = match &pf {
                Ok(o) => format!("status={} stderr={}", o.status, String::from_utf8_lossy(&o.stderr).trim()),
                Err(e) => format!("spawn error {e}"),
            };
            log(&format!("PREFLIGHT FAILED ({why}); aborting upgrade, v1 resumes"));
            // snapshot() was destructive for alt-screen terminals: rebuild from the snapshot.
            for (s, st) in self.sessions.iter_mut().zip(&state.sessions) {
                if st.term.alt_active {
                    s.term = restore(&st.term);
                }
            }
            let _ = std::fs::remove_file(&path);
            return format!("upgrade aborted: preflight failed: {why}");
        }
        log(&format!("preflight ok (snapshot {snap_ms}ms, total {}ms); execv", t0.elapsed().as_millis()));
        for s in &self.sessions {
            set_cloexec(s.fd, false);
        }
        set_cloexec(self.listener.as_raw_fd(), false);
        // client sockets keep CLOEXEC -> they close at exec; clients reconnect.
        let bin = CString::new(new_bin).unwrap();
        let a1 = CString::new("--resume").unwrap();
        let a2 = CString::new(path.clone()).unwrap();
        let argv = [bin.as_ptr(), a1.as_ptr(), a2.as_ptr(), std::ptr::null()];
        unsafe { libc::execv(bin.as_ptr(), argv.as_ptr()) };
        let err = std::io::Error::last_os_error();
        for s in &self.sessions {
            set_cloexec(s.fd, true);
        }
        set_cloexec(self.listener.as_raw_fd(), true);
        format!("execv failed: {err}")
    }

    fn run(&mut self) -> ! {
        loop {
            // reap
            loop {
                let mut st = 0;
                let pid = unsafe { libc::waitpid(-1, &mut st, libc::WNOHANG) };
                if pid <= 0 {
                    break;
                }
                for s in &mut self.sessions {
                    if s.pid == pid {
                        s.exited = Some(st);
                        log(&format!("session {} pid {pid} exited status={st}", s.id));
                    }
                }
            }
            let mut pfds = vec![libc::pollfd { fd: self.listener.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
            for s in &self.sessions {
                pfds.push(libc::pollfd { fd: s.fd, events: libc::POLLIN, revents: 0 });
            }
            for c in &self.clients {
                pfds.push(libc::pollfd { fd: c.stream.as_raw_fd(), events: libc::POLLIN, revents: 0 });
            }
            unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as _, 200) };
            if pfds[0].revents & libc::POLLIN != 0 {
                while let Ok((st, _)) = self.listener.accept() {
                    st.set_nonblocking(false).ok();
                    set_cloexec(st.as_raw_fd(), true);
                    self.clients.push(Client { stream: st, buf: vec![], attached: None });
                }
            }
            let ns = self.sessions.len();
            for i in 0..ns {
                let re = pfds[1 + i].revents;
                if re & (libc::POLLIN | libc::POLLHUP) != 0 {
                    let mut buf = vec![0u8; 65536];
                    let n = unsafe { libc::read(self.sessions[i].fd, buf.as_mut_ptr() as *mut _, buf.len()) };
                    if n > 0 {
                        buf.truncate(n as usize);
                        self.push_output(i, buf);
                    }
                }
            }
            // clients
            let mut ready = vec![];
            for (j, c) in self.clients.iter().enumerate() {
                let pf = pfds.iter().find(|p| p.fd == c.stream.as_raw_fd());
                if let Some(p) = pf {
                    if p.revents & (libc::POLLIN | libc::POLLHUP) != 0 {
                        ready.push(j);
                    }
                }
            }
            let mut dead = vec![];
            for j in ready {
                let mut b = [0u8; 4096];
                set_nonblock(self.clients[j].stream.as_raw_fd(), true);
                let r = self.clients[j].stream.read(&mut b);
                set_nonblock(self.clients[j].stream.as_raw_fd(), false);
                match r {
                    Ok(0) | Err(_) => dead.push(j),
                    Ok(n) => {
                        self.clients[j].buf.extend_from_slice(&b[..n]);
                        while let Some(pos) = self.clients[j].buf.iter().position(|&x| x == b'\n') {
                            let line: Vec<u8> = self.clients[j].buf.drain(..=pos).collect();
                            let line = String::from_utf8_lossy(&line[..line.len() - 1]).to_string();
                            if let Some(r) = self.handle_cmd(j, &line) {
                                let c = &mut self.clients[j];
                                send_frame(c, format!("M {}\n", r.len()), r.as_bytes());
                            }
                        }
                    }
                }
            }
            for j in dead.into_iter().rev() {
                self.clients.remove(j);
            }
        }
    }
}

fn load_state(path: &str) -> DaemonState {
    let st: DaemonState = serde_json::from_slice(&std::fs::read(path).expect("read state")).expect("parse state");
    assert_eq!(st.version, STATE_VERSION, "state version mismatch");
    st
}

fn main() {
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("fidelity") => fidelity(),
        Some("serve") => {
            let sock = args[2].clone();
            let _ = std::fs::remove_file(&sock);
            let listener = UnixListener::bind(&sock).unwrap();
            listener.set_nonblocking(true).unwrap();
            set_cloexec(listener.as_raw_fd(), true);
            log(&format!("serving on {sock}"));
            Daemon { listener, sock_path: sock, sessions: vec![], clients: vec![], next_id: 1 }.run();
        }
        Some("--preflight") => {
            let st = load_state(&args[2]);
            for s in &st.sessions {
                let t = restore(&s.term);
                let _ = describe(&t);
            }
            log(&format!("preflight OK: {} sessions from {}", st.sessions.len(), st.from_binary));
        }
        Some("--resume") => {
            let t0 = Instant::now();
            let st = load_state(&args[2]);
            let listener = unsafe { UnixListener::from_raw_fd(st.listener_fd) };
            set_cloexec(st.listener_fd, true);
            let mut sessions = vec![];
            for s in st.sessions {
                let fdflags = unsafe { libc::fcntl(s.fd, libc::F_GETFD) };
                let alive = unsafe { libc::kill(s.pid, 0) } == 0;
                let pgrp = unsafe { libc::tcgetpgrp(s.fd) };
                log(&format!("adopt session {} fd={} (F_GETFD={fdflags}) pid={} alive={alive} tty_fg_pgrp={pgrp} seq={}", s.id, s.fd, s.pid, s.seq));
                set_cloexec(s.fd, true);
                set_nonblock(s.fd, true);
                let ring: VecDeque<(u64, Vec<u8>)> = s.ring.iter().map(|(q, h)| (*q, unhex(h))).collect();
                let ring_bytes = ring.iter().map(|(_, d)| d.len()).sum();
                sessions.push(Session { id: s.id, fd: s.fd, pid: s.pid, term: restore(&s.term), seq: s.seq, ring, ring_bytes, exited: None });
            }
            let _ = std::fs::remove_file(&args[2]);
            log(&format!("resumed {} sessions from {} in {}ms", sessions.len(), st.from_binary, t0.elapsed().as_millis()));
            Daemon { listener, sock_path: st.sock_path, sessions, clients: vec![], next_id: st.next_id }.run();
        }
        Some("client") => client(&args[2], &args[3..]),
        _ => eprintln!("usage: midnad fidelity | serve SOCK | --preflight F | --resume F | client SOCK (cmd LINE | attach ID)"),
    }
}

// ---------------------------------------------------------------- client

fn read_frame(r: &mut std::io::BufReader<UnixStream>) -> Option<(String, Vec<u8>)> {
    use std::io::BufRead;
    let mut h = String::new();
    if r.read_line(&mut h).ok()? == 0 {
        return None;
    }
    let h = h.trim_end().to_string();
    let len: usize = h.rsplit(' ').next()?.parse().ok()?;
    let mut body = vec![0; len];
    r.read_exact(&mut body).ok()?;
    Some((h, body))
}

fn client(sock: &str, args: &[String]) {
    match args[0].as_str() {
        "cmd" => {
            let mut s = UnixStream::connect(sock).unwrap();
            s.write_all(format!("{}\n", args[1]).as_bytes()).unwrap();
            let mut r = std::io::BufReader::new(s);
            if let Some((_, b)) = read_frame(&mut r) {
                print!("{}", String::from_utf8_lossy(&b));
                println!();
            }
        }
        "attach" => {
            // Writes raw session bytes to stdout; reconnects forever with since_seq.
            let id = &args[1];
            let mut last = 0u64;
            let mut out = std::io::stdout();
            loop {
                let s = match UnixStream::connect(sock) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("[client] connect failed: {e}; retry");
                        std::thread::sleep(Duration::from_millis(20));
                        continue;
                    }
                };
                let t = Instant::now();
                let mut w = s.try_clone().unwrap();
                w.write_all(format!("ATTACH {id} {last}\n").as_bytes()).unwrap();
                eprintln!("[client] attached since={last}");
                let mut r = std::io::BufReader::new(s);
                let mut first = true;
                while let Some((h, b)) = read_frame(&mut r) {
                    let seq: u64 = h.split(' ').nth(2).unwrap().parse().unwrap();
                    if first {
                        eprintln!("[client] first frame seq={seq} after {}ms", t.elapsed().as_millis());
                        first = false;
                    }
                    if seq != last + 1 {
                        eprintln!("[client] SEQ GAP: last={last} got={seq}");
                    }
                    last = seq;
                    out.write_all(&b).unwrap();
                    out.flush().unwrap();
                }
                eprintln!("[client] disconnected at seq={last}; reconnecting");
                drop(w);
            }
        }
        _ => {}
    }
}
