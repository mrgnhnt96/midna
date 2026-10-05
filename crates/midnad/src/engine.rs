//! libghostty-vt engine wrapper (from spikes/gpui-terminal). Owns a !Send Terminal, so it
//! must be created and used on one thread (see `term.rs`).
use libghostty_vt::fmt::{Format, Formatter, FormatterOptions};
use libghostty_vt::render::{CellIterator, CursorVisualStyle, Dirty, RenderState, RowIterator};
use libghostty_vt::screen::{CellWide, Screen};
use libghostty_vt::style::Underline;
use libghostty_vt::terminal::{Mode, Options, Terminal};
use midna_proto::frame::*;
use std::os::fd::RawFd;

#[path = "engine_input.rs"]
mod input;
pub use input::{FindHit, Link, detect_link, key_from_name};

pub fn now_ns() -> u64 {
    unsafe extern "C" {
        fn clock_gettime_nsec_np(id: libc::clockid_t) -> u64;
    }
    unsafe { clock_gettime_nsec_np(libc::CLOCK_UPTIME_RAW) }
}

pub struct Engine {
    term: Terminal<'static, 'static>,
    rs: RenderState<'static>,
    rows_it: RowIterator<'static>,
    cells_it: CellIterator<'static>,
    first_read_ns: u64,
    last_read_ns: u64,
    force_full: bool,
    pub bytes: u64,
    /// Where DA/DSR replies go (re-registered when the terminal is rebuilt from a snapshot).
    pty_fd: Option<RawFd>,
    /// Trailing bytes of an escape sequence (or UTF-8 char) the parser has only half seen.
    /// libghostty's VT export can't carry parser state, so an upgrade replays these.
    tail: Vec<u8>,
    /// Cell size in pixels from the last resize (mouse encoding, selection geometry).
    cell_w: u32,
    cell_h: u32,
    input: input::Input,
    /// The terminal has an installed selection.
    has_selection: bool,
    /// Selection or modes changed: send a frame even when no row is dirty.
    meta_dirty: bool,
    /// Cursor (position, style) in the last frame: an app that only moves the cursor (Claude
    /// Code's word jumps) dirties no row, but the client still has to see it move.
    sent_cursor: Option<(Option<(u16, u16)>, u8)>,
}

impl Engine {
    /// `pty_fd`: where DA/DSR responses are written (None = discard).
    pub fn new(cols: u16, rows: u16, pty_fd: Option<RawFd>) -> Engine {
        let mut term = new_terminal(cols, rows);
        connect_pty(&mut term, pty_fd);
        Engine {
            pty_fd,
            tail: Vec::new(),
            term,
            rs: RenderState::new().expect("render state"),
            rows_it: RowIterator::new().expect("row iterator"),
            cells_it: CellIterator::new().expect("cell iterator"),
            first_read_ns: 0,
            last_read_ns: 0,
            force_full: true,
            bytes: 0,
            cell_w: 8,
            cell_h: 16,
            input: input::Input::new(),
            has_selection: false,
            meta_dirty: false,
            sent_cursor: None,
        }
    }

    pub fn feed(&mut self, data: &[u8], read_ns: u64) {
        if self.first_read_ns == 0 {
            self.first_read_ns = read_ns;
        }
        self.last_read_ns = read_ns;
        self.bytes += data.len() as u64;
        self.term.vt_write(data);
        self.track_tail(data);
    }

    pub fn resize(&mut self, cols: u16, rows: u16, cw: u32, ch: u32) {
        let _ = self.term.resize(cols, rows, cw.max(1), ch.max(1));
        if cw > 0 && ch > 0 {
            (self.cell_w, self.cell_h) = (cw, ch);
        }
        self.force_full = true;
    }

    /// Next frame must contain every row (a new client attached).
    pub fn force_full(&mut self) {
        self.force_full = true;
    }

    /// Current kitty keyboard protocol flags (0 when the app hasn't enabled it).
    pub fn kitty_flags(&self) -> u8 {
        self.term.kitty_keyboard_flags().map(|f| f.bits()).unwrap_or(0)
    }

    pub fn size(&self) -> (u16, u16) {
        (self.term.cols().unwrap_or(80), self.term.rows().unwrap_or(24))
    }

    pub fn title(&self) -> String {
        self.term.title().unwrap_or("").to_string()
    }

    /// Plain text of scrollback + screen, one String per row, trailing spaces trimmed.
    pub fn plain_lines(&self) -> Vec<String> {
        let opts = FormatterOptions::new().with_format(Format::Plain).with_trim(false).with_unwrap(false);
        let Ok(mut f) = Formatter::new(&self.term, opts) else { return vec![] };
        let Ok(bytes) = f.format_alloc(None) else { return vec![] };
        String::from_utf8_lossy(&bytes).split('\n').map(|l| l.trim_end().to_string()).collect()
    }

    /// Exactly the visible screen (last `rows` lines of the plain output).
    pub fn screen_lines(&self) -> Vec<String> {
        let rows = self.size().1 as usize;
        let mut all = self.plain_lines();
        // The formatter may drop trailing blank rows; pad so the screen is always `rows` tall.
        if all.len() < rows {
            all.resize(rows, String::new());
        }
        all.split_off(all.len() - rows)
    }

    /// What the viewport shows: its rows (scrolled back or not), the cursor's row when the live
    /// screen is shown, and whether the alternate screen is on.
    pub fn view(&self) -> (Vec<String>, Option<u16>, bool) {
        if matches!(self.term.active_screen(), Ok(Screen::Alternate)) {
            return (self.screen_lines(), Some(self.cursor().1), true);
        }
        let rows = self.size().1 as usize;
        let mut all = self.plain_lines();
        let Some(sb) = self.term.scrollbar().ok().filter(|s| s.offset + s.len < s.total) else {
            return (self.screen_lines(), Some(self.cursor().1), false);
        };
        let start = (sb.offset as usize).min(all.len());
        let mut v = all.split_off(start);
        v.truncate(rows);
        v.resize(rows, String::new());
        (v, None, false)
    }

    /// The visible screen with dim text blanked (`agent_work::undim`), `rows` lines: for readers
    /// that must tell typed text from dim placeholder text.
    pub fn screen_undimmed(&self) -> Vec<String> {
        let opts = FormatterOptions::new().with_format(Format::Vt).with_trim(false).with_unwrap(false).with_style(true);
        let Ok(mut f) = Formatter::new(&self.term, opts) else { return vec![] };
        let Ok(bytes) = f.format_alloc(None) else { return vec![] };
        let rows = self.size().1 as usize;
        let mut all = crate::agent_work::undim(&String::from_utf8_lossy(&bytes));
        if all.len() < rows {
            all.resize(rows, String::new());
        }
        all.split_off(all.len() - rows)
    }

    /// Build a frame of dirty rows (None when clean).
    pub fn frame(&mut self) -> Option<Frame> {
        let t0 = now_ns();
        let Engine { term, rs, rows_it, cells_it, .. } = self;
        let snap = rs.update(term).ok()?;
        let dirty = snap.dirty().ok()?;
        let full = self.force_full || dirty == Dirty::Full;
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
        let cursor_moved = self.sent_cursor != Some((cursor, cursor_style));
        if dirty == Dirty::Clean && !self.force_full && !self.meta_dirty && !cursor_moved {
            return None;
        }
        self.sent_cursor = Some((cursor, cursor_style));
        let colors = snap.colors().ok()?;
        let dfg = [colors.foreground.r, colors.foreground.g, colors.foreground.b];
        let dbg = [colors.background.r, colors.background.g, colors.background.b];
        let cols = snap.cols().ok()?;
        let nrows = snap.rows().ok()?;
        let mut changed = vec![];
        let mut it = rows_it.update(&snap).ok()?;
        let mut y: u16 = 0;
        let mut gbuf: Vec<char> = Vec::new();
        let mut selection = Vec::new();
        while let Some(row) = it.next() {
            if self.has_selection
                && let Ok(Some(s)) = row.selection()
            {
                selection.push((y, s.start_x, s.end_x));
            }
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

        let _ = snap.set_dirty(Dirty::Clean);
        let sb = self.term.scrollbar().ok();
        let ext = FrameExt {
            mouse_tracking: self.term.is_mouse_tracking().unwrap_or(false),
            alt_screen: matches!(self.term.active_screen(), Ok(Screen::Alternate)),
            kitty_flags: self.kitty_flags(),
            scroll_total: sb.map(|s| s.total).unwrap_or(0),
            scroll_offset: sb.map(|s| s.offset).unwrap_or(0),
            scroll_len: sb.map(|s| s.len).unwrap_or(0),
            selection,
        };
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
            marker_ns: 0,
            ext,
        };
        self.first_read_ns = 0;
        self.force_full = false;
        self.meta_dirty = false;
        Some(f)
    }
}

// ---------------------------------------------------------------- upgrade snapshots
//
// libghostty-vt's VT formatter (0.2.2) is what carries a terminal across `daemon.upgrade`. Its
// known export bugs (docs/design/design-notes.html, spikes/daemon-reexec) and our workarounds:
//   * it assumes the cursor starts at home and emits CUP before DECSTBM/tabstops (which move
//     the cursor): we write `CSI H` first and re-issue CUP from our own reading, last;
//   * OSC 7 carries a stray NUL: NULs are stripped from the export;
//   * the title and cursor shape aren't exported: we save them ourselves and replay
//     OSC 2 / DECSCUSR;
//   * parser state isn't exported, so a half-received escape is lost: `tail` replays it.
// Not carried: DECSC saved cursor, origin-mode-relative CUP, the parser's in-progress OSC
// beyond 64 KiB.

/// Everything needed to rebuild a terminal in a new daemon image.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct TermSnap {
    pub cols: u16,
    pub rows: u16,
    pub alt_active: bool,
    /// Primary screen + scrollback as VT (written to its own file in the handoff).
    #[serde(skip)]
    pub primary_vt: Vec<u8>,
    #[serde(skip)]
    pub alt_vt: Option<Vec<u8>>,
    pub title: String,
    /// DECSCUSR parameter (1..6), 0 = leave the default.
    pub cursor_shape: u8,
    pub primary_cursor: (u16, u16),
    pub alt_cursor: Option<(u16, u16)>,
    /// Primary-screen scrollback rows: the export drops trailing blank rows, so a restore
    /// scrolls until it has as many (otherwise the cursor row lands on the last line).
    #[serde(default)]
    pub scrollback_rows: usize,
    /// Half-parsed trailing bytes, replayed last.
    #[serde(default)]
    pub pending: Vec<u8>,
}

const TAIL_CAP: usize = 64 * 1024;

fn new_terminal(cols: u16, rows: u16) -> Terminal<'static, 'static> {
    let mut term = Terminal::new(Options { cols, rows, max_scrollback: 10_000 }).expect("libghostty terminal");
    // Grapheme clustering (mode 2027) on every terminal.
    let _ = term.set_mode(Mode::GRAPHEME_CLUSTER, true);
    term
}

fn connect_pty(term: &mut Terminal<'static, 'static>, pty_fd: Option<RawFd>) {
    if let Some(fd) = pty_fd {
        let _ = term.on_pty_write(move |_t, data: &[u8]| unsafe {
            libc::write(fd, data.as_ptr() as *const _, data.len());
        });
    }
}

fn vt_export(t: &Terminal) -> Vec<u8> {
    let opts = FormatterOptions::new()
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
        .with_charsets(true);
    let Ok(mut f) = Formatter::new(t, opts) else { return vec![] };
    let mut v = f.format_alloc(None).map(|b| b.to_vec()).unwrap_or_default();
    v.retain(|&b| b != 0); // stray NUL in the OSC 7 export
    v
}

fn cup((x, y): (u16, u16)) -> String {
    format!("\x1b[{};{}H", y + 1, x + 1)
}

/// Replay a snapshot into `t` (a fresh terminal of the snapshot's size). Order matters: the
/// screens, then title and cursor shape, then the cursor position, then the half-parsed tail.
fn replay(t: &mut Terminal<'static, 'static>, s: &TermSnap) {
    t.vt_write(b"\x1b[H");
    write_primary(t, s);
    t.vt_write(cup(s.primary_cursor).as_bytes());
    if let Some(alt) = &s.alt_vt {
        t.vt_write(b"\x1b[?1049h\x1b[H");
        t.vt_write(alt);
    }
    let title: String = s.title.chars().filter(|c| !c.is_control()).collect();
    if !title.is_empty() {
        t.vt_write(format!("\x1b]2;{title}\x07").as_bytes());
    }
    if (1..=6).contains(&s.cursor_shape) {
        t.vt_write(format!("\x1b[{} q", s.cursor_shape).as_bytes());
    }
    let cursor = if s.alt_vt.is_some() { s.alt_cursor.unwrap_or(s.primary_cursor) } else { s.primary_cursor };
    t.vt_write(cup(cursor).as_bytes());
    if !s.pending.is_empty() {
        t.vt_write(&s.pending);
    }
}

/// The export is `palette, modes, rows (trailing blank rows dropped), SGR 0, CUP, DECSTBM,
/// tabstops...`. Write the rows, scroll until the scrollback matches the original (with the
/// full-screen region still in effect), then the trailing state.
fn write_primary(t: &mut Terminal<'static, 'static>, s: &TermSnap) {
    let v = &s.primary_vt;
    let Some(end) = content_end(v) else {
        t.vt_write(v);
        return;
    };
    t.vt_write(&v[..end]);
    let have = t.scrollback_rows().unwrap_or(0);
    if s.scrollback_rows > have {
        let rows = t.rows().unwrap_or(s.rows) as usize;
        let y = t.cursor_y().unwrap_or(0) as usize;
        let n = rows.saturating_sub(1 + y) + (s.scrollback_rows - have);
        t.vt_write(&b"\r\n".repeat(n));
    }
    t.vt_write(&v[end..]);
}

/// Where the export's trailing state block starts: the last `SGR 0` followed only by
/// escape sequences.
fn content_end(v: &[u8]) -> Option<usize> {
    let pat = b"\x1b[0m";
    let i = v.windows(pat.len()).rposition(|w| w == pat)?;
    let mut j = i;
    while j < v.len() {
        if v[j] != 0x1b || j + 1 >= v.len() {
            return None;
        }
        match v[j + 1] {
            b'[' => {
                j += 2;
                while j < v.len() && !(0x40..=0x7e).contains(&v[j]) {
                    j += 1;
                }
                j += 1;
            }
            b']' => {
                j += 2;
                while j < v.len() && v[j] != 0x07 && !(v[j] == 0x1b && v.get(j + 1) == Some(&b'\\')) {
                    j += 1;
                }
                j += if v.get(j) == Some(&0x07) { 1 } else { 2 };
            }
            _ => j += 2,
        }
    }
    Some(i)
}

impl Engine {
    /// Capture the terminal for a handoff. Exporting the primary screen needs a switch out of
    /// the alternate screen; the alternate screen is then replayed in place, so the live
    /// engine ends up exactly as a restored one would.
    pub fn snapshot(&mut self) -> TermSnap {
        let t = &mut self.term;
        let alt = matches!(t.active_screen(), Ok(Screen::Alternate));
        let cursor_shape = {
            let shape = self.rs.update(t).ok().map(|s| (s.cursor_visual_style().ok(), s.cursor_blinking().unwrap_or(false)));
            match shape {
                Some((Some(CursorVisualStyle::Bar), b)) => if b { 5 } else { 6 },
                Some((Some(CursorVisualStyle::Underline), b)) => if b { 3 } else { 4 },
                Some((Some(_), b)) => if b { 1 } else { 2 },
                _ => 0,
            }
        };
        let cur = |t: &Terminal| (t.cursor_x().unwrap_or(0), t.cursor_y().unwrap_or(0));
        let title = t.title().unwrap_or("").to_string();
        let (alt_vt, alt_cursor) = if alt { (Some(vt_export(t)), Some(cur(t))) } else { (None, None) };
        if alt {
            t.vt_write(b"\x1b[?1049l");
        }
        let primary_cursor = cur(t);
        let primary_vt = vt_export(t);
        let scrollback_rows = t.scrollback_rows().unwrap_or(0);
        let (cols, rows) = (t.cols().unwrap_or(80), t.rows().unwrap_or(24));
        let snap = TermSnap { cols, rows, alt_active: alt, primary_vt, alt_vt, title, cursor_shape, primary_cursor, alt_cursor, scrollback_rows, pending: self.tail.clone() };
        if alt {
            // Back into the alternate screen the same way a restore gets there.
            t.vt_write(b"\x1b[?1049h\x1b[H");
            if let Some(a) = &snap.alt_vt {
                t.vt_write(a);
            }
            t.vt_write(cup(snap.alt_cursor.unwrap_or_default()).as_bytes());
            if !snap.pending.is_empty() {
                t.vt_write(&snap.pending);
            }
        }
        self.force_full = true;
        snap
    }

    /// An engine rebuilt from a snapshot (the new daemon image after an upgrade).
    pub fn restored(s: &TermSnap, pty_fd: Option<RawFd>) -> Engine {
        let mut e = Engine::new(s.cols.max(2), s.rows.max(2), None);
        replay(&mut e.term, s);
        // Only now route DA/DSR replies to the PTY, so replaying never answers anything.
        connect_pty(&mut e.term, pty_fd);
        e.pty_fd = pty_fd;
        e.tail = s.pending.clone();
        e.force_full = true;
        e
    }

    /// Visible-screen and scrollback text (tests, selftest).
    pub fn cursor(&self) -> (u16, u16) {
        (self.term.cursor_x().unwrap_or(0), self.term.cursor_y().unwrap_or(0))
    }

    fn track_tail(&mut self, data: &[u8]) {
        if !self.tail.is_empty() && !data.contains(&0x1b) {
            self.tail.extend_from_slice(data);
            let cut = incomplete_suffix(&self.tail);
            self.tail.drain(..cut);
        } else {
            let cut = incomplete_suffix(data);
            self.tail.clear();
            self.tail.extend_from_slice(&data[cut..]);
        }
        if self.tail.len() > TAIL_CAP {
            self.tail.clear();
        }
    }
}

/// Start of a trailing escape sequence or UTF-8 character that isn't finished yet
/// (`buf.len()` when everything is complete). A heuristic over the common sequence shapes.
pub fn incomplete_suffix(buf: &[u8]) -> usize {
    let n = buf.len();
    if let Some(i) = buf.iter().rposition(|&b| b == 0x1b)
        && esc_incomplete(&buf[i..])
    {
        return i;
    }
    for k in 1..=3.min(n) {
        let b = buf[n - k];
        if b & 0xC0 == 0x80 {
            continue;
        }
        if b >= 0xC0 {
            let need = if b >= 0xF0 { 4 } else if b >= 0xE0 { 3 } else { 2 };
            if need > k {
                return n - k;
            }
        }
        break;
    }
    n
}

fn esc_incomplete(s: &[u8]) -> bool {
    let Some(&kind) = s.get(1) else { return true };
    match kind {
        b'[' => {
            for &b in &s[2..] {
                match b {
                    0x40..=0x7e | 0x18 | 0x1a => return false,
                    _ => {}
                }
            }
            true
        }
        // OSC ends with BEL or ST (ESC \); an ST would itself be the last ESC.
        b']' => !s[2..].contains(&0x07),
        b'P' | b'_' | b'^' | b'X' => true,
        0x20..=0x2f => s.len() < 3,
        _ => false,
    }
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;

    fn plain(e: &Engine) -> String {
        e.plain_lines().join("\n").trim_end().to_string()
    }

    #[test]
    fn cursor_only_moves_send_a_frame() {
        let mut e = Engine::new(40, 5, None);
        e.feed(b"> hello world", 0);
        assert_eq!(e.frame().unwrap().cursor, Some((13, 0)));
        assert!(e.frame().is_none(), "nothing changed");
        // ESC b in a line editor answers with a bare cursor move: no row changes.
        e.feed(b"\x1b[6D", 0);
        assert_eq!(e.frame().expect("cursor moved").cursor, Some((7, 0)));
        assert!(e.frame().is_none());
    }

    #[test]
    fn snapshot_full_screen_with_scrollback_then_more_output() {
        for n in [20, 29, 30, 31, 87, 200] {
            let mut e = Engine::new(100, 30, None);
            for i in 0..n {
                e.feed(format!("C {i}\r\n").as_bytes(), 1);
            }
            let snap = e.snapshot();
            let mut r = Engine::restored(&snap, None);
            assert_eq!(r.cursor(), e.cursor(), "n={n}");
            for i in n..n + 5 {
                e.feed(format!("C {i}\r\n").as_bytes(), 1);
                r.feed(format!("C {i}\r\n").as_bytes(), 1);
            }
            assert_eq!(plain(&r), plain(&e), "n={n}");
        }
    }

    #[test]
    fn snapshot_cleared_screen_and_scroll_region() {
        let cases: [&[u8]; 3] = [
            b"\x1b[H\x1b[2Jtop\r\n",
            b"\x1b[3;20r\x1b[10;1Hregion\r\n",
            b"\x1b[H\x1b[2J\x1b[5;1H\x1b[1;44mstyled\x1b[0m\r\n",
        ];
        for (k, tail) in cases.iter().enumerate() {
            let mut e = Engine::new(100, 30, None);
            for i in 0..45 {
                e.feed(format!("L {i}\r\n").as_bytes(), 1);
            }
            e.feed(tail, 1);
            let snap = e.snapshot();
            let mut r = Engine::restored(&snap, None);
            assert_eq!(r.cursor(), e.cursor(), "case {k}");
            assert_eq!(plain(&r), plain(&e), "case {k} right after restore");
            for i in 0..40 {
                e.feed(format!("M {i}\r\n").as_bytes(), 1);
                r.feed(format!("M {i}\r\n").as_bytes(), 1);
            }
            assert_eq!(plain(&r), plain(&e), "case {k}");
        }
    }

    #[test]
    fn incomplete_suffix_shapes() {
        assert_eq!(incomplete_suffix(b"abc"), 3);
        assert_eq!(incomplete_suffix(b"ab\x1b"), 2);
        assert_eq!(incomplete_suffix(b"ab\x1b[3"), 2);
        assert_eq!(incomplete_suffix(b"ab\x1b[31m"), 7);
        assert_eq!(incomplete_suffix(b"a\x1b]2;title"), 1);
        assert_eq!(incomplete_suffix(b"a\x1b]2;title\x07"), 11);
        assert_eq!(incomplete_suffix(b"a\x1b]2;t\x1b\\"), 8);
        assert_eq!(incomplete_suffix(b"a\x1b("), 1);
        assert_eq!(incomplete_suffix(b"a\x1b(B"), 4);
        assert_eq!(incomplete_suffix("a\u{2500}".as_bytes()), 4);
        assert_eq!(incomplete_suffix(&"a\u{2500}".as_bytes()[..3]), 1);
    }

    #[test]
    fn snapshot_roundtrip_keeps_screens_title_cursor_and_partial_escape() {
        let mut e = Engine::new(40, 8, None);
        for i in 0..20 {
            e.feed(format!("primary line {i} \x1b[1;31mred\x1b[0m\r\n").as_bytes(), 1);
        }
        e.feed(b"\x1b]2;my title\x07\x1b]7;file://host/tmp/x\x07\x1b[?2004h\x1b[3;3Hprompt$ ", 1);
        e.feed(b"\x1b[?1049h\x1b[2;6r\x1b[5 q\x1b[1;1H ALT HEADER \x1b[4;10Halt body\x1b[5;12H", 1);
        e.feed(b"\x1b[3", 1); // half an SGR
        let before = plain(&e);
        let before_cursor = e.cursor();
        let snap = e.snapshot();
        assert!(snap.alt_active);
        assert_eq!(snap.title, "my title");
        assert_eq!(snap.cursor_shape, 5);
        assert_eq!(snap.pending, b"\x1b[3");
        assert!(!snap.primary_vt.contains(&0));
        // The live engine is unchanged by the snapshot.
        assert_eq!(plain(&e), before);
        assert_eq!(e.cursor(), before_cursor);
        let mut r = Engine::restored(&snap, None);
        assert_eq!(plain(&r), before);
        assert_eq!(r.cursor(), before_cursor);
        assert_eq!(r.title(), "my title");
        // The half-received SGR completes in both.
        e.feed(b"1mX", 1);
        r.feed(b"1mX", 1);
        assert_eq!(plain(&r), plain(&e));
        assert!(plain(&r).contains("X"));
        assert!(!plain(&r).contains("1mX"));
        // Leaving the alternate screen gives back the same primary screen.
        e.feed(b"\x1b[?1049l", 1);
        r.feed(b"\x1b[?1049l", 1);
        assert_eq!(plain(&r), plain(&e));
        assert_eq!(r.cursor(), e.cursor());
    }
}
