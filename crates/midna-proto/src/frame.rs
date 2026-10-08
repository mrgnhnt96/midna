//! Dirty-rows terminal frame (the "B-snap" format proven in spikes/gpui-terminal). The daemon's
//! engine builds frames; the stream connection carries `u32 LE len` + `Frame::encode()` payloads.

pub const F_BOLD: u8 = 1;
pub const F_ITALIC: u8 = 2;
pub const F_UNDER: u8 = 4;
pub const F_STRIKE: u8 = 8;
pub const F_WIDE: u8 = 16;
pub const F_SPACER: u8 = 32;
pub const F_FAINT: u8 = 64;
/// Typed at a shell prompt (between OSC 133 B and C).
pub const F_INPUT: u8 = 128;

/// Client -> daemon stream tags.
pub const TAG_WANT: u8 = 0x01;
pub const TAG_RESIZE: u8 = 0x02;
/// Raw bytes for the PTY (text, already-encoded sequences).
pub const TAG_INPUT: u8 = 0x03;
/// A structured key event ([`KeyMsg`]); the daemon encodes it for the app's keyboard mode
/// (legacy / modifyOtherKeys / kitty flags) with libghostty's key encoder.
pub const TAG_KEY: u8 = 0x04;
/// Scrolling ([`ScrollMsg`]): the viewport, or wheel events for apps with mouse reporting.
pub const TAG_SCROLL: u8 = 0x05;
/// A mouse button / motion event ([`MouseMsg`]): mouse reporting or selection.
pub const TAG_MOUSE: u8 = 0x06;
/// Focus gained (1) / lost (0); reported to apps that enabled mode 1004.
pub const TAG_FOCUS: u8 = 0x07;
/// Pasted text (u32 len + UTF-8); bracketed when the app enabled mode 2004.
pub const TAG_PASTE: u8 = 0x08;

/// Modifier bits used by [`KeyMsg`], [`MouseMsg`] and [`ScrollMsg`].
pub const MOD_SHIFT: u8 = 1;
pub const MOD_ALT: u8 = 2;
pub const MOD_CTRL: u8 = 4;
pub const MOD_SUPER: u8 = 8;

/// Trailer marker for the extended frame state (see [`Frame::encode`]).
const EXT_MAGIC: u8 = 0xE1;

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
    /// Multi-codepoint graphemes (combining marks, ZWJ emoji): (col, text).
    pub extras: Vec<(u16, String)>,
    /// The line goes on in the next row (soft-wrapped at the right edge).
    pub wrapped: bool,
    /// A shell prompt starts on the row (OSC 133 A).
    pub prompt: bool,
}

impl RowData {
    /// The row as plain text (graphemes expanded, spacer cells skipped).
    pub fn text(&self) -> String {
        let mut s = String::with_capacity(self.cells.len());
        for (x, c) in self.cells.iter().enumerate() {
            if c.flags & F_SPACER != 0 {
                continue;
            }
            match self.extras.iter().find(|(ex, _)| *ex as usize == x) {
                Some((_, g)) => s.push_str(g),
                None => s.push(if c.ch == '\0' { ' ' } else { c.ch }),
            }
        }
        s
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Frame {
    pub cols: u16,
    pub rows: u16,
    /// Every row is present; the client should drop its previous grid.
    pub full: bool,
    pub changed: Vec<(u16, RowData)>,
    pub cursor: Option<(u16, u16)>,
    pub cursor_style: u8, // 0 block 1 bar 2 underline 3 hollow
    pub default_fg: [u8; 3],
    pub default_bg: [u8; 3],
    pub decckm: bool,
    pub bracketed_paste: bool,
    /// First PTY read (ns, CLOCK_UPTIME_RAW) folded into this frame (0 = none). Latency metrics.
    pub first_read_ns: u64,
    pub last_read_ns: u64,
    pub built_ns: u64,
    pub build_us: u32,
    pub marker_ns: u64,
    /// Extended state, sent as a trailer after the rows (older decoders ignore it; frames
    /// from older daemons decode with these at their defaults).
    pub ext: FrameExt,
}

/// Terminal state beyond the cells: modes the client needs, the scroll position and the
/// selection. Always describes the whole viewport (a newer frame replaces it).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FrameExt {
    /// The app enabled mouse reporting (1000/1002/1003): mouse events go to it, not selection.
    pub mouse_tracking: bool,
    /// The alternate screen is active (full-screen TUI).
    pub alt_screen: bool,
    /// Kitty keyboard protocol flags the app pushed (0 = legacy keys).
    pub kitty_flags: u8,
    /// Rows of scrollback + screen, the viewport's first row, and the viewport height.
    pub scroll_total: u64,
    pub scroll_offset: u64,
    pub scroll_len: u64,
    /// Selected cells per viewport row: (row, first col, last col), inclusive.
    pub selection: Vec<(u16, u16, u16)>,
}

impl FrameExt {
    /// The viewport shows the live screen (not scrolled back).
    pub fn at_bottom(&self) -> bool {
        self.scroll_offset + self.scroll_len >= self.scroll_total
    }
}

#[derive(Debug)]
pub struct DecodeError;

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("truncated or malformed frame")
    }
}
impl std::error::Error for DecodeError {}

impl Frame {
    /// Fold a newer frame into this one (used while the client has no credit).
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
        let e = &self.ext;
        out.push(EXT_MAGIC);
        out.push(e.mouse_tracking as u8 | (e.alt_screen as u8) << 1);
        out.push(e.kitty_flags);
        p(out, e.scroll_total);
        p(out, e.scroll_offset);
        p(out, e.scroll_len);
        out.extend_from_slice(&(e.selection.len() as u16).to_le_bytes());
        for (y, a, b) in &e.selection {
            for v in [y, a, b] {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        // The soft-wrapped and prompt rows among `changed`, as (row, 1 = wrapped | 2 = prompt),
        // after the ext block, where older apps stop reading.
        let marked: Vec<(u16, u8)> = self.changed.iter().map(|(y, r)| (*y, r.wrapped as u8 | (r.prompt as u8) << 1)).filter(|&(_, b)| b != 0).collect();
        out.extend_from_slice(&(marked.len() as u16).to_le_bytes());
        for (y, bits) in marked {
            out.extend_from_slice(&y.to_le_bytes());
            out.push(bits);
        }
    }

    pub fn decode(b: &[u8]) -> Result<Frame, DecodeError> {
        let mut r = Reader { b, i: 0 };
        let cols = r.u16()?;
        let rows = r.u16()?;
        let cx = r.u16()?;
        let cy = r.u16()?;
        let f = r.take(5)?.to_vec();
        let default_fg: [u8; 3] = r.take(3)?.try_into().unwrap();
        let default_bg: [u8; 3] = r.take(3)?.try_into().unwrap();
        let first_read_ns = r.u64()?;
        let last_read_ns = r.u64()?;
        let built_ns = r.u64()?;
        let marker_ns = r.u64()?;
        let build_us = u32::from_le_bytes(r.take(4)?.try_into().unwrap());
        let n = r.u16()?;
        let mut changed = Vec::with_capacity(n as usize);
        for _ in 0..n {
            let y = r.u16()?;
            let nc = r.u16()? as usize;
            let mut cells = Vec::with_capacity(nc);
            for _ in 0..nc {
                let s = r.take(11)?;
                cells.push(Cell {
                    ch: char::from_u32(u32::from_le_bytes(s[0..4].try_into().unwrap())).unwrap_or(' '),
                    fg: [s[4], s[5], s[6]],
                    bg: [s[7], s[8], s[9]],
                    flags: s[10],
                });
            }
            let ne = r.u16()?;
            let mut extras = vec![];
            for _ in 0..ne {
                let x = r.u16()?;
                let l = r.u16()? as usize;
                extras.push((x, String::from_utf8_lossy(r.take(l)?).into_owned()));
            }
            changed.push((y, RowData { cells, extras, ..Default::default() }));
        }
        let mut ext = FrameExt::default();
        if r.i < b.len() && b[r.i] == EXT_MAGIC {
            r.i += 1;
            let fl = r.take(2)?;
            ext.mouse_tracking = fl[0] & 1 != 0;
            ext.alt_screen = fl[0] & 2 != 0;
            ext.kitty_flags = fl[1];
            ext.scroll_total = r.u64()?;
            ext.scroll_offset = r.u64()?;
            ext.scroll_len = r.u64()?;
            let n = r.u16()?;
            for _ in 0..n {
                ext.selection.push((r.u16()?, r.u16()?, r.u16()?));
            }
            // An older daemon's frame has no row marks.
            if r.i < b.len() {
                for _ in 0..r.u16()? {
                    let (y, bits) = (r.u16()?, r.take(1)?[0]);
                    if let Some((_, row)) = changed.iter_mut().find(|(ry, _)| *ry == y) {
                        row.wrapped = bits & 1 != 0;
                        row.prompt = bits & 2 != 0;
                    }
                }
            }
        }
        Ok(Frame {
            cols,
            rows,
            full: f[2] != 0,
            changed,
            cursor: (f[0] != 0).then_some((cx, cy)),
            cursor_style: f[1],
            default_fg,
            default_bg,
            decckm: f[3] != 0,
            bracketed_paste: f[4] != 0,
            first_read_ns,
            last_read_ns,
            built_ns,
            build_us,
            marker_ns,
            ext,
        })
    }
}

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let s = self.b.get(self.i..self.i + n).ok_or(DecodeError)?;
        self.i += n;
        Ok(s)
    }
    fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, DecodeError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
}

/// Encode the client -> daemon resize message (tag 0x02).
pub fn encode_resize(cols: u16, rows: u16, cw: u32, ch: u32) -> Vec<u8> {
    let mut p = vec![TAG_RESIZE];
    p.extend_from_slice(&cols.to_le_bytes());
    p.extend_from_slice(&rows.to_le_bytes());
    p.extend_from_slice(&cw.to_le_bytes());
    p.extend_from_slice(&ch.to_le_bytes());
    p
}

/// Encode the client -> daemon input message (tag 0x03).
pub fn encode_input(bytes: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(5 + bytes.len());
    p.push(TAG_INPUT);
    p.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    p.extend_from_slice(bytes);
    p
}

/// Key event action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum KeyAction {
    #[default]
    Press,
    Release,
    Repeat,
}

/// A key as the GUI saw it. `key` uses GPUI's names: a single character for character keys
/// (unshifted, e.g. "a", "1", "["; shifted punctuation may arrive as "!"), or a name:
/// enter tab space backspace escape up down left right home end pageup pagedown insert
/// delete f1..f25. `text` is what the key would type on the current layout (may be empty).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct KeyMsg {
    pub action: KeyAction,
    pub mods: u8,
    pub key: String,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollKind {
    /// Mouse wheel / trackpad, `amount` rows (negative = up, toward history). Goes to the app
    /// as wheel events when it has mouse reporting, as arrow keys on the alternate screen,
    /// otherwise scrolls the viewport.
    Wheel = 0,
    /// Scroll the viewport by `amount` rows (negative = up).
    Lines = 1,
    /// Scroll the viewport by `amount` pages.
    Pages = 2,
    Top = 3,
    Bottom = 4,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollMsg {
    pub kind: ScrollKind,
    pub amount: i32,
    /// Pointer position in cell units from the grid's top-left (wheel reports need it).
    pub x: f32,
    pub y: f32,
    pub mods: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    Press = 0,
    Release = 1,
    Motion = 2,
}

/// A mouse event. `button`: 0 none, 1 left, 2 right, 3 middle. Position in cell units from
/// the grid's top-left (fractions matter for selection; may be outside the grid while
/// dragging). With mouse reporting on (and no shift) it goes to the app; otherwise the left
/// button drives selection: click, double-click word, triple-click line, alt-drag rectangle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MouseMsg {
    pub action: MouseAction,
    pub button: u8,
    pub mods: u8,
    pub x: f32,
    pub y: f32,
}

/// Everything a client can send on a frame stream after `stream.attach`.
#[derive(Clone, Debug, PartialEq)]
pub enum ClientMsg {
    Want,
    Resize { cols: u16, rows: u16, cell_w: u32, cell_h: u32 },
    Input(Vec<u8>),
    Key(KeyMsg),
    Scroll(ScrollMsg),
    Mouse(MouseMsg),
    Focus(bool),
    Paste(String),
}

impl ClientMsg {
    pub fn encode(&self) -> Vec<u8> {
        let mut p = Vec::new();
        let str8 = |p: &mut Vec<u8>, s: &str| {
            let b = &s.as_bytes()[..s.len().min(255)];
            p.push(b.len() as u8);
            p.extend_from_slice(b);
        };
        match self {
            ClientMsg::Want => p.push(TAG_WANT),
            ClientMsg::Resize { cols, rows, cell_w, cell_h } => p = encode_resize(*cols, *rows, *cell_w, *cell_h),
            ClientMsg::Input(b) => p = encode_input(b),
            ClientMsg::Key(k) => {
                p.push(TAG_KEY);
                p.push(k.action as u8);
                p.push(k.mods);
                str8(&mut p, &k.key);
                str8(&mut p, &k.text);
            }
            ClientMsg::Scroll(s) => {
                p.push(TAG_SCROLL);
                p.push(s.kind as u8);
                p.extend_from_slice(&s.amount.to_le_bytes());
                p.extend_from_slice(&s.x.to_le_bytes());
                p.extend_from_slice(&s.y.to_le_bytes());
                p.push(s.mods);
            }
            ClientMsg::Mouse(m) => {
                p.push(TAG_MOUSE);
                p.push(m.action as u8);
                p.push(m.button);
                p.push(m.mods);
                p.extend_from_slice(&m.x.to_le_bytes());
                p.extend_from_slice(&m.y.to_le_bytes());
            }
            ClientMsg::Focus(g) => p.extend_from_slice(&[TAG_FOCUS, *g as u8]),
            ClientMsg::Paste(t) => {
                p.push(TAG_PASTE);
                p.extend_from_slice(&(t.len() as u32).to_le_bytes());
                p.extend_from_slice(t.as_bytes());
            }
        }
        p
    }

    /// Read one message. `Ok(None)` = clean end of stream; an unknown tag is an error.
    pub fn read(r: &mut impl std::io::Read) -> std::io::Result<Option<ClientMsg>> {
        use std::io::{Error, ErrorKind};
        let mut tag = [0u8; 1];
        match r.read_exact(&mut tag) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e),
        }
        let mut take = |n: usize| -> std::io::Result<Vec<u8>> {
            let mut v = vec![0u8; n];
            r.read_exact(&mut v)?;
            Ok(v)
        };
        let bad = |what: &str| Error::new(ErrorKind::InvalidData, what.to_string());
        let f32_at = |b: &[u8], i: usize| f32::from_le_bytes(b[i..i + 4].try_into().unwrap());
        Ok(Some(match tag[0] {
            TAG_WANT => ClientMsg::Want,
            TAG_RESIZE => {
                let b = take(12)?;
                ClientMsg::Resize {
                    cols: u16::from_le_bytes([b[0], b[1]]),
                    rows: u16::from_le_bytes([b[2], b[3]]),
                    cell_w: u32::from_le_bytes(b[4..8].try_into().unwrap()),
                    cell_h: u32::from_le_bytes(b[8..12].try_into().unwrap()),
                }
            }
            TAG_INPUT | TAG_PASTE => {
                let l = take(4)?;
                let n = u32::from_le_bytes(l.try_into().unwrap()) as usize;
                if n > 64 << 20 {
                    return Err(bad("input too large"));
                }
                let data = take(n)?;
                if tag[0] == TAG_INPUT {
                    ClientMsg::Input(data)
                } else {
                    ClientMsg::Paste(String::from_utf8_lossy(&data).into_owned())
                }
            }
            TAG_KEY => {
                let h = take(3)?;
                let key = String::from_utf8_lossy(&take(h[2] as usize)?).into_owned();
                let tl = take(1)?[0] as usize;
                let text = String::from_utf8_lossy(&take(tl)?).into_owned();
                let action = match h[0] {
                    1 => KeyAction::Release,
                    2 => KeyAction::Repeat,
                    _ => KeyAction::Press,
                };
                ClientMsg::Key(KeyMsg { action, mods: h[1], key, text })
            }
            TAG_SCROLL => {
                let b = take(14)?;
                let kind = match b[0] {
                    0 => ScrollKind::Wheel,
                    1 => ScrollKind::Lines,
                    2 => ScrollKind::Pages,
                    3 => ScrollKind::Top,
                    4 => ScrollKind::Bottom,
                    _ => return Err(bad("unknown scroll kind")),
                };
                let amount = i32::from_le_bytes(b[1..5].try_into().unwrap());
                ClientMsg::Scroll(ScrollMsg { kind, amount, x: f32_at(&b, 5), y: f32_at(&b, 9), mods: b[13] })
            }
            TAG_MOUSE => {
                let b = take(11)?;
                let action = match b[0] {
                    0 => MouseAction::Press,
                    1 => MouseAction::Release,
                    2 => MouseAction::Motion,
                    _ => return Err(bad("unknown mouse action")),
                };
                ClientMsg::Mouse(MouseMsg { action, button: b[1], mods: b[2], x: f32_at(&b, 3), y: f32_at(&b, 7) })
            }
            TAG_FOCUS => ClientMsg::Focus(take(1)?[0] != 0),
            _ => return Err(bad("unknown stream tag")),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let row = RowData {
            cells: vec![Cell { ch: 'a', fg: [1, 2, 3], bg: [4, 5, 6], flags: F_BOLD }, Cell { ch: 'é', ..Default::default() }],
            extras: vec![(1, "e\u{301}".into())],
            wrapped: true,
            prompt: true,
        };
        let ext = FrameExt { mouse_tracking: true, kitty_flags: 5, scroll_total: 900, scroll_offset: 10, scroll_len: 30, selection: vec![(3, 1, 7)], ..Default::default() };
        let f = Frame { cols: 2, rows: 1, full: true, changed: vec![(0, row)], cursor: Some((1, 0)), decckm: true, ext, ..Default::default() };
        let mut b = vec![];
        f.encode(&mut b);
        assert_eq!(Frame::decode(&b).unwrap(), f);
        assert!(Frame::decode(&b[..b.len() - 1]).is_err());
    }

    #[test]
    fn frames_without_trailer_still_decode() {
        let f = Frame { cols: 1, rows: 1, full: true, changed: vec![(0, RowData { cells: vec![Cell::default()], extras: vec![], ..Default::default() })], ..Default::default() };
        let mut b = vec![];
        f.encode(&mut b);
        // An older daemon's frame ends right after the rows.
        let plain_len = b.len() - (1 + 2 + 24 + 2 + 2);
        let old = Frame::decode(&b[..plain_len]).unwrap();
        assert_eq!(old.ext, FrameExt::default());
        assert_eq!(old.changed, f.changed);
    }

    #[test]
    fn client_messages_roundtrip() {
        let msgs = vec![
            ClientMsg::Want,
            ClientMsg::Resize { cols: 80, rows: 24, cell_w: 8, cell_h: 16 },
            ClientMsg::Input(b"ls\r".to_vec()),
            ClientMsg::Key(KeyMsg { action: KeyAction::Repeat, mods: MOD_CTRL | MOD_SHIFT, key: "pageup".into(), text: String::new() }),
            ClientMsg::Key(KeyMsg { action: KeyAction::Press, mods: 0, key: "a".into(), text: "a".into() }),
            ClientMsg::Scroll(ScrollMsg { kind: ScrollKind::Wheel, amount: -3, x: 1.5, y: 2.25, mods: MOD_SHIFT }),
            ClientMsg::Mouse(MouseMsg { action: MouseAction::Motion, button: 1, mods: MOD_ALT, x: -1.0, y: 40.5 }),
            ClientMsg::Focus(true),
            ClientMsg::Paste("héllo\nworld".into()),
        ];
        let mut wire = Vec::new();
        for m in &msgs {
            wire.extend(m.encode());
        }
        let mut r = std::io::Cursor::new(wire);
        for m in &msgs {
            assert_eq!(ClientMsg::read(&mut r).unwrap().as_ref(), Some(m));
        }
        assert!(ClientMsg::read(&mut r).unwrap().is_none());
        assert!(ClientMsg::read(&mut std::io::Cursor::new(vec![0x7f])).is_err());
    }
}
