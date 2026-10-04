//! In-process libghostty-vt engine + PTY for the fake backend (ported from
//! spikes/gpui-terminal/src/engine.rs). Not used when talking to midnad.
use crate::frame::*;
use libghostty_vt::render::{CellIterator, CursorVisualStyle, Dirty, RenderState, RowIterator};
use libghostty_vt::screen::CellWide;
use libghostty_vt::style::Underline;
use libghostty_vt::terminal::{Mode, Options, Terminal};
use std::os::fd::RawFd;

pub fn now_ns() -> u64 {
    unsafe extern "C" {
        fn clock_gettime_nsec_np(id: libc::clockid_t) -> u64;
    }
    unsafe { clock_gettime_nsec_np(libc::CLOCK_UPTIME_RAW) }
}

const MARKER: &[u8] = b"__MIDNA_DONE__";

/// Owns a !Send libghostty Terminal. Must be created and used on one thread.
pub struct Engine {
    term: Terminal<'static, 'static>,
    rs: RenderState<'static>,
    rows_it: RowIterator<'static>,
    cells_it: CellIterator<'static>,
    first_read_ns: u64,
    last_read_ns: u64,
    marker_ns: u64,
    tail: Vec<u8>,
    force_full: bool,
    pub bytes: u64,
}

impl Engine {
    /// `pty_fd`: where DA/DSR responses are written (None = discard; non-authoritative engine).
    pub fn new(cols: u16, rows: u16, pty_fd: Option<RawFd>) -> Engine {
        let mut term = Terminal::new(Options { cols, rows, max_scrollback: 10_000 }).unwrap();
        let _ = term.set_mode(Mode::GRAPHEME_CLUSTER, true);
        if let Some(fd) = pty_fd {
            term.on_pty_write(move |_t, data: &[u8]| unsafe {
                libc::write(fd, data.as_ptr() as *const _, data.len());
            })
            .unwrap();
        }
        Engine {
            term,
            rs: RenderState::new().unwrap(),
            rows_it: RowIterator::new().unwrap(),
            cells_it: CellIterator::new().unwrap(),
            first_read_ns: 0,
            last_read_ns: 0,
            marker_ns: 0,
            tail: vec![],
            force_full: true,
            bytes: 0,
        }
    }

    pub fn feed(&mut self, data: &[u8], read_ns: u64) {
        if self.first_read_ns == 0 {
            self.first_read_ns = read_ns;
        }
        self.last_read_ns = read_ns;
        self.bytes += data.len() as u64;
        // marker detection across chunk boundaries
        let mut hay = std::mem::take(&mut self.tail);
        hay.extend_from_slice(&data[..data.len().min(64)]);
        let found_head = hay.windows(MARKER.len()).any(|w| w == MARKER);
        let found = found_head || data[data.len().saturating_sub(4096)..].windows(MARKER.len()).any(|w| w == MARKER);
        if found {
            self.marker_ns = now_ns();
        }
        self.tail = data[data.len().saturating_sub(MARKER.len())..].to_vec();
        self.term.vt_write(data);
    }

    pub fn resize(&mut self, cols: u16, rows: u16, cw: u32, ch: u32) {
        let _ = self.term.resize(cols, rows, cw, ch);
        self.force_full = true;
    }

    /// Build a frame of dirty rows (None when clean).
    pub fn frame(&mut self) -> Option<Frame> {
        let t0 = now_ns();
        let Engine { term, rs, rows_it, cells_it, .. } = self;
        let snap = rs.update(term).ok()?;
        let dirty = snap.dirty().ok()?;
        let full = self.force_full || dirty == Dirty::Full;
        if dirty == Dirty::Clean && !self.force_full {
            return None;
        }
        let colors = snap.colors().ok()?;
        let dfg = [colors.foreground.r, colors.foreground.g, colors.foreground.b];
        let dbg = [colors.background.r, colors.background.g, colors.background.b];
        let cols = snap.cols().ok()?;
        let nrows = snap.rows().ok()?;
        let cursor = if snap.cursor_visible().unwrap_or(true) { snap.cursor_viewport().ok().flatten().map(|c| (c.x, c.y)) } else { None };
        let cursor_style = match snap.cursor_visual_style() {
            Ok(CursorVisualStyle::Bar) => 1,
            Ok(CursorVisualStyle::Underline) => 2,
            Ok(CursorVisualStyle::BlockHollow) => 3,
            _ => 0,
        };
        let mut changed = vec![];
        let mut it = rows_it.update(&snap).ok()?;
        let mut y: u16 = 0;
        let mut gbuf: Vec<char> = Vec::new();
        while let Some(row) = it.next() {
            if full || row.dirty().unwrap_or(true) {
                let mut rd = RowData { cells: Vec::with_capacity(cols as usize), extras: vec![] };
                let mut ci = cells_it.update(row).ok()?;
                let mut x: u16 = 0;
                while let Some(c) = ci.next() {
                    let mut cell = Cell { ch: ' ', fg: dfg, bg: dbg, flags: 0 };
                    let n = c.graphemes_len().unwrap_or(0);
                    if n == 1 {
                        let mut one = [' '];
                        let _ = c.graphemes_buf(&mut one);
                        cell.ch = one[0];
                    } else if n > 1 {
                        gbuf.resize(n, '\0');
                        let _ = c.graphemes_buf(&mut gbuf);
                        cell.ch = gbuf[0];
                        rd.extras.push((x, gbuf.iter().collect()));
                    }
                    if let Ok(raw) = c.raw_cell() {
                        match raw.wide() {
                            Ok(CellWide::Wide) => cell.flags |= F_WIDE,
                            Ok(CellWide::SpacerTail) | Ok(CellWide::SpacerHead) => cell.flags |= F_SPACER,
                            _ => {}
                        }
                    }
                    if c.has_styling().unwrap_or(false) {
                        let st = c.style().ok();
                        let mut fg = c.fg_color().ok().flatten().map(|c| [c.r, c.g, c.b]).unwrap_or(dfg);
                        let mut bg = c.bg_color().ok().flatten().map(|c| [c.r, c.g, c.b]).unwrap_or(dbg);
                        if let Some(st) = st {
                            if st.inverse {
                                std::mem::swap(&mut fg, &mut bg);
                            }
                            if st.invisible {
                                fg = bg;
                            }
                            if st.bold {
                                cell.flags |= F_BOLD;
                            }
                            if st.italic {
                                cell.flags |= F_ITALIC;
                            }
                            if st.faint {
                                cell.flags |= F_FAINT;
                            }
                            if st.strikethrough {
                                cell.flags |= F_STRIKE;
                            }
                            if st.underline != Underline::None {
                                cell.flags |= F_UNDER;
                            }
                        }
                        cell.fg = fg;
                        cell.bg = bg;
                    }
                    rd.cells.push(cell);
                    x += 1;
                }
                changed.push((y, rd));
                let _ = row.set_dirty(false);
            }
            y += 1;
        }
        let _ = it; // end the row iteration before touching the snapshot again
        let _ = snap.set_dirty(Dirty::Clean);
        let f = Frame {
            cols,
            rows: nrows,
            full,
            changed,
            cursor,
            cursor_style,
            default_fg: dfg,
            default_bg: dbg,
            decckm: self.term.mode(Mode::DECCKM).unwrap_or(false),
            bracketed_paste: self.term.mode(Mode::BRACKETED_PASTE).unwrap_or(false),
            first_read_ns: self.first_read_ns,
            last_read_ns: self.last_read_ns,
            built_ns: now_ns(),
            build_us: ((now_ns() - t0) / 1000) as u32,
            marker_ns: self.marker_ns,
            ext: {
                let sb = self.term.scrollbar().ok();
                FrameExt {
                    alt_screen: matches!(self.term.active_screen(), Ok(libghostty_vt::screen::Screen::Alternate)),
                    scroll_total: sb.map(|s| s.total).unwrap_or(0),
                    scroll_offset: sb.map(|s| s.offset).unwrap_or(0),
                    scroll_len: sb.map(|s| s.len).unwrap_or(0),
                    ..Default::default()
                }
            },
        };
        self.first_read_ns = 0;
        self.force_full = false;
        Some(f)
    }
}

// ------------------------------------------------------------------ PTY

/// Run `script` with `/bin/sh -c` on a fresh PTY. Returns (master fd, child pid).
pub fn spawn_pty(script: &str, cols: u16, rows: u16, env: &[(&str, &str)]) -> (RawFd, i32) {
    use std::ffi::CString;
    let sh = CString::new("/bin/sh").unwrap();
    let a0 = CString::new("sh").unwrap();
    let a1 = CString::new("-c").unwrap();
    let a2 = CString::new(script).unwrap();
    let argv = [a0.as_ptr(), a1.as_ptr(), a2.as_ptr(), std::ptr::null()];
    let mut vars: Vec<(CString, CString)> = vec![
        (CString::new("TERM").unwrap(), CString::new("xterm-256color").unwrap()),
        (CString::new("COLORTERM").unwrap(), CString::new("truecolor").unwrap()),
        (CString::new("TERM_PROGRAM").unwrap(), CString::new("midna").unwrap()),
    ];
    for (k, v) in env {
        vars.push((CString::new(*k).unwrap(), CString::new(*v).unwrap()));
    }
    unsafe {
        let (mut m, mut s) = (0, 0);
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
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
            for (k, v) in &vars {
                libc::setenv(k.as_ptr(), v.as_ptr(), 1);
            }
            libc::execv(sh.as_ptr(), argv.as_ptr());
            libc::_exit(127);
        }
        libc::close(s);
        let f = libc::fcntl(m, libc::F_GETFD);
        libc::fcntl(m, libc::F_SETFD, f | libc::FD_CLOEXEC);
        (m, pid)
    }
}

pub fn pty_resize(fd: RawFd, cols: u16, rows: u16) {
    let ws = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
    unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &ws) };
}

pub fn write_all(fd: RawFd, mut b: &[u8]) {
    while !b.is_empty() {
        let n = unsafe { libc::write(fd, b.as_ptr() as *const _, b.len()) };
        if n <= 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock {
                std::thread::sleep(std::time::Duration::from_micros(200));
                continue;
            }
            return;
        }
        b = &b[n as usize..];
    }
}

impl Engine {
    /// The fake's stand-in for midnad's input handling: legacy key encoding, paste and the
    /// scrollback viewport (no mouse reporting, selection or kitty keys). Returns PTY bytes.
    pub fn client(&mut self, m: &ClientMsg) -> Vec<u8> {
        use libghostty_vt::terminal::ScrollViewport;
        match m {
            ClientMsg::Key(k) if k.action != KeyAction::Release => {
                let decckm = self.term.mode(Mode::DECCKM).unwrap_or(false);
                let out = legacy_key(k, decckm);
                if !out.is_empty() {
                    self.term.scroll_viewport(ScrollViewport::Bottom);
                    self.force_full = true;
                }
                out
            }
            ClientMsg::Paste(t) => {
                let t = t.replace("\r\n", "\r").replace('\n', "\r");
                if self.term.mode(Mode::BRACKETED_PASTE).unwrap_or(false) { format!("\x1b[200~{}\x1b[201~", t.replace("\x1b[201~", "")).into_bytes() } else { t.into_bytes() }
            }
            ClientMsg::Scroll(sc) => {
                let rows = self.term.rows().unwrap_or(24) as isize;
                let v = match sc.kind {
                    ScrollKind::Top => ScrollViewport::Top,
                    ScrollKind::Bottom => ScrollViewport::Bottom,
                    ScrollKind::Pages => ScrollViewport::Delta(sc.amount as isize * (rows - 1)),
                    _ => ScrollViewport::Delta(sc.amount as isize),
                };
                self.term.scroll_viewport(v);
                self.force_full = true;
                vec![]
            }
            _ => vec![],
        }
    }

    /// Make the next frame a full one (a new client attached).
    pub fn force_full(&mut self) {
        self.force_full = true;
    }
}

/// Legacy (xterm) key bytes for the fake backend; midnad uses libghostty's encoder instead.
fn legacy_key(k: &KeyMsg, decckm: bool) -> Vec<u8> {
    let (shift, alt, ctrl) = (k.mods & MOD_SHIFT != 0, k.mods & MOD_ALT != 0, k.mods & MOD_CTRL != 0);
    let modp = 1 + shift as u8 + 2 * alt as u8 + 4 * ctrl as u8;
    let csi = |fin: char| -> Vec<u8> {
        if modp > 1 {
            format!("\x1b[1;{modp}{fin}").into_bytes()
        } else if decckm && "ABCD".contains(fin) {
            format!("\x1bO{fin}").into_bytes()
        } else {
            format!("\x1b[{fin}").into_bytes()
        }
    };
    let tilde = |n: u8| {
        if modp > 1 { format!("\x1b[{n};{modp}~").into_bytes() } else { format!("\x1b[{n}~").into_bytes() }
    };
    let mut v = match k.key.as_str() {
        "enter" if shift => return b"\x1b\r".to_vec(),
        "enter" => b"\r".to_vec(),
        "backspace" => vec![if ctrl { 0x08 } else { 0x7f }],
        "tab" if shift => return b"\x1b[Z".to_vec(),
        "tab" => b"\t".to_vec(),
        "escape" => vec![0x1b],
        "space" if ctrl => vec![0],
        "space" => b" ".to_vec(),
        "up" => return csi('A'),
        "down" => return csi('B'),
        "right" => return csi('C'),
        "left" => return csi('D'),
        "home" => return csi('H'),
        "end" => return csi('F'),
        "pageup" => return tilde(5),
        "pagedown" => return tilde(6),
        "delete" => return tilde(3),
        "insert" => return tilde(2),
        "f1" => return b"\x1bOP".to_vec(),
        "f2" => return b"\x1bOQ".to_vec(),
        "f3" => return b"\x1bOR".to_vec(),
        "f4" => return b"\x1bOS".to_vec(),
        key if key.chars().count() == 1 => {
            let c = key.chars().next().unwrap();
            if ctrl {
                match c {
                    'a'..='z' => vec![c as u8 & 0x1f],
                    '[' => vec![0x1b],
                    '\\' => vec![0x1c],
                    ']' => vec![0x1d],
                    _ => return vec![],
                }
            } else if shift && c.is_ascii_lowercase() {
                c.to_ascii_uppercase().to_string().into_bytes()
            } else if !k.text.is_empty() && !alt {
                k.text.clone().into_bytes()
            } else {
                key.as_bytes().to_vec()
            }
        }
        _ => return vec![],
    };
    if alt {
        v.insert(0, 0x1b);
    }
    v
}
