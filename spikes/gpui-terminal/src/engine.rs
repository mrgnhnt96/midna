//! libghostty-vt engine wrapper + plain-Rust frame (dirty-row diff) that crosses threads/processes.
use libghostty_vt::render::{CellIterator, CursorVisualStyle, Dirty, RenderState, RowIterator};
use libghostty_vt::screen::CellWide;
use libghostty_vt::style::Underline;
use libghostty_vt::terminal::{Mode, Options, Terminal};
use std::os::fd::RawFd;

pub fn now_ns() -> u64 {
    unsafe extern "C" { fn clock_gettime_nsec_np(id: libc::clockid_t) -> u64; }
    unsafe { clock_gettime_nsec_np(libc::CLOCK_UPTIME_RAW) }
}

pub const F_BOLD: u8 = 1;
pub const F_ITALIC: u8 = 2;
pub const F_UNDER: u8 = 4;
pub const F_STRIKE: u8 = 8;
pub const F_WIDE: u8 = 16;
pub const F_SPACER: u8 = 32;
pub const F_FAINT: u8 = 64;

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Cell {
    pub ch: char,
    pub fg: [u8; 3],
    pub bg: [u8; 3],
    pub flags: u8,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RowData {
    pub cells: Vec<Cell>,
    /// multi-codepoint graphemes (combining marks, ZWJ emoji): (col, text)
    pub extras: Vec<(u16, String)>,
}

#[derive(Clone, Debug, Default)]
pub struct Frame {
    pub cols: u16,
    pub rows: u16,
    pub full: bool,
    pub changed: Vec<(u16, RowData)>,
    pub cursor: Option<(u16, u16)>,
    pub cursor_style: u8, // 0 block 1 bar 2 underline 3 hollow
    pub default_fg: [u8; 3],
    pub default_bg: [u8; 3],
    pub decckm: bool,
    pub bracketed_paste: bool,
    /// first PTY read (ns) folded into this frame (0 = none)
    pub first_read_ns: u64,
    pub last_read_ns: u64,
    pub built_ns: u64,
    pub build_us: u32,
    pub marker_ns: u64,
}

impl Frame {
    pub fn merge(&mut self, newer: Frame) {
        let first = if self.first_read_ns == 0 { newer.first_read_ns } else { self.first_read_ns };
        if newer.full || newer.cols != self.cols || newer.rows != self.rows {
            *self = Frame { first_read_ns: first, ..newer };
            return;
        }
        let changed = std::mem::take(&mut self.changed);
        let mut map: std::collections::BTreeMap<u16, RowData> = changed.into_iter().collect();
        let full = self.full;
        for (y, r) in newer.changed.iter().cloned() {
            map.insert(y, r);
        }
        *self = Frame { changed: map.into_iter().collect(), full, first_read_ns: first, ..newer };
    }

    pub fn encode(&self, out: &mut Vec<u8>) {
        let p = |out: &mut Vec<u8>, v: u64| out.extend_from_slice(&v.to_le_bytes());
        out.extend_from_slice(&self.cols.to_le_bytes());
        out.extend_from_slice(&self.rows.to_le_bytes());
        let (cx, cy, has) = self.cursor.map(|(x, y)| (x, y, 1u8)).unwrap_or((0, 0, 0));
        out.extend_from_slice(&cx.to_le_bytes());
        out.extend_from_slice(&cy.to_le_bytes());
        out.extend_from_slice(&[has, self.cursor_style, self.full as u8, self.decckm as u8, self.bracketed_paste as u8]);
        out.extend_from_slice(&self.default_fg);
        out.extend_from_slice(&self.default_bg);
        p(out, self.first_read_ns);
        p(out, self.last_read_ns);
        p(out, self.built_ns);
        p(out, self.marker_ns);
        out.extend_from_slice(&self.build_us.to_le_bytes());
        out.extend_from_slice(&(self.changed.len() as u16).to_le_bytes());
        for (y, r) in &self.changed {
            out.extend_from_slice(&y.to_le_bytes());
            out.extend_from_slice(&(r.cells.len() as u16).to_le_bytes());
            for c in &r.cells {
                out.extend_from_slice(&(c.ch as u32).to_le_bytes());
                out.extend_from_slice(&c.fg);
                out.extend_from_slice(&c.bg);
                out.push(c.flags);
            }
            out.extend_from_slice(&(r.extras.len() as u16).to_le_bytes());
            for (x, s) in &r.extras {
                out.extend_from_slice(&x.to_le_bytes());
                out.extend_from_slice(&(s.len() as u16).to_le_bytes());
                out.extend_from_slice(s.as_bytes());
            }
        }
    }

    pub fn decode(b: &[u8]) -> Frame {
        let mut i = 0usize;
        macro_rules! take {
            ($n:expr) => {{
                let s = &b[i..i + $n];
                i += $n;
                s
            }};
        }
        let u16_ = |s: &[u8]| u16::from_le_bytes([s[0], s[1]]);
        let u64_ = |s: &[u8]| u64::from_le_bytes(s.try_into().unwrap());
        let cols = u16_(take!(2));
        let rows = u16_(take!(2));
        let cx = u16_(take!(2));
        let cy = u16_(take!(2));
        let f = take!(5).to_vec();
        let default_fg: [u8; 3] = take!(3).try_into().unwrap();
        let default_bg: [u8; 3] = take!(3).try_into().unwrap();
        let first_read_ns = u64_(take!(8));
        let last_read_ns = u64_(take!(8));
        let built_ns = u64_(take!(8));
        let marker_ns = u64_(take!(8));
        let build_us = u32::from_le_bytes(take!(4).try_into().unwrap());
        let n = u16_(take!(2));
        let mut changed = Vec::with_capacity(n as usize);
        for _ in 0..n {
            let y = u16_(take!(2));
            let nc = u16_(take!(2)) as usize;
            let mut cells = Vec::with_capacity(nc);
            for _ in 0..nc {
                let s = take!(11);
                cells.push(Cell {
                    ch: char::from_u32(u32::from_le_bytes(s[0..4].try_into().unwrap())).unwrap_or(' '),
                    fg: [s[4], s[5], s[6]],
                    bg: [s[7], s[8], s[9]],
                    flags: s[10],
                });
            }
            let ne = u16_(take!(2));
            let mut extras = vec![];
            for _ in 0..ne {
                let x = u16_(take!(2));
                let l = u16_(take!(2)) as usize;
                extras.push((x, String::from_utf8_lossy(take!(l)).into_owned()));
            }
            changed.push((y, RowData { cells, extras }));
        }
        Frame {
            cols, rows, full: f[2] != 0, changed,
            cursor: (f[0] != 0).then_some((cx, cy)),
            cursor_style: f[1], default_fg, default_bg, decckm: f[3] != 0, bracketed_paste: f[4] != 0,
            first_read_ns, last_read_ns, built_ns, build_us, marker_ns,
        }
    }
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

    pub fn set_grapheme(&mut self, on: bool) {
        let _ = self.term.set_mode(Mode::GRAPHEME_CLUSTER, on);
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
        let cursor = if snap.cursor_visible().unwrap_or(true) {
            snap.cursor_viewport().ok().flatten().map(|c| (c.x, c.y))
        } else {
            None
        };
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
                            if st.bold { cell.flags |= F_BOLD; }
                            if st.italic { cell.flags |= F_ITALIC; }
                            if st.faint { cell.flags |= F_FAINT; }
                            if st.strikethrough { cell.flags |= F_STRIKE; }
                            if st.underline != Underline::None { cell.flags |= F_UNDER; }
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
        drop(it);
        let _ = snap.set_dirty(Dirty::Clean);
        let f = Frame {
            cols, rows: nrows, full, changed, cursor, cursor_style,
            default_fg: dfg, default_bg: dbg,
            decckm: self.term.mode(Mode::DECCKM).unwrap_or(false),
            bracketed_paste: self.term.mode(Mode::BRACKETED_PASTE).unwrap_or(false),
            first_read_ns: self.first_read_ns,
            last_read_ns: self.last_read_ns,
            built_ns: now_ns(),
            build_us: ((now_ns() - t0) / 1000) as u32,
            marker_ns: self.marker_ns,
        };
        self.first_read_ns = 0;
        self.force_full = false;
        Some(f)
    }
}

// ------------------------------------------------------------------ PTY

pub fn spawn_pty(cmd: &str, cols: u16, rows: u16) -> (RawFd, i32) {
    use std::ffi::CString;
    let sh = CString::new("/bin/sh").unwrap();
    let a0 = CString::new("sh").unwrap();
    let a1 = CString::new("-c").unwrap();
    let a2 = CString::new(format!("exec {cmd}")).unwrap();
    let argv = [a0.as_ptr(), a1.as_ptr(), a2.as_ptr(), std::ptr::null()];
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
            let set = |k: &str, v: &str| {
                let k = CString::new(k).unwrap();
                let v = CString::new(v).unwrap();
                libc::setenv(k.as_ptr(), v.as_ptr(), 1);
            };
            set("TERM", "xterm-256color");
            set("COLORTERM", "truecolor");
            set("TERM_PROGRAM", "midna-spike");
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

/// CPU time (user+sys, ns) and resident bytes of any pid we own.
#[allow(deprecated)]
pub fn proc_usage(pid: i32) -> (u64, u64) {
    unsafe {
        let mut ti: libc::proc_taskinfo = std::mem::zeroed();
        let sz = std::mem::size_of::<libc::proc_taskinfo>() as i32;
        let r = libc::proc_pidinfo(pid, libc::PROC_PIDTASKINFO, 0, &mut ti as *mut _ as *mut _, sz);
        if r != sz {
            return (0, 0);
        }
        // pti_total_* are in mach absolute time units (== ns on Apple silicon? use timebase)
        let mut tb = libc::mach_timebase_info { numer: 0, denom: 0 };
        libc::mach_timebase_info(&mut tb);
        let t = (ti.pti_total_user + ti.pti_total_system) * tb.numer as u64 / tb.denom as u64;
        (t, ti.pti_resident_size)
    }
}
