//! Input for an [`Engine`]: keys, mouse, wheel, focus and paste, encoded with libghostty-vt's
//! own encoders against the terminal's current modes (DECCKM, keypad, modifyOtherKeys, kitty
//! flags, mouse tracking/format, focus reporting, bracketed paste). Also the viewport
//! (scrollback), the terminal-owned selection, links under the pointer, and find.
//!
//! Child module of `engine` so it can reach the engine's private fields.
use super::Engine;
use libghostty_vt::fmt::Format;
use libghostty_vt::key::{self, Action, Key, Mods, OptionAsAlt};
use libghostty_vt::mouse::{self, Button, EncoderSize};
use libghostty_vt::screen::Screen;
use libghostty_vt::selection::gesture::{AutoscrollTickEvent, DragEvent, Geometry, Gesture, PressEvent, ReleaseEvent};
use libghostty_vt::selection::{FormatOptions, Selection};
use libghostty_vt::terminal::{Mode, Point, PointCoordinate, PointSpace, ScrollViewport, Terminal};
use midna_proto::link_patterns::LinkPattern;
use midna_proto::frame::{KeyAction, KeyMsg, MOD_ALT, MOD_CTRL, MOD_SHIFT, MOD_SUPER, MouseAction, MouseMsg, ScrollKind, ScrollMsg};
use std::time::{Duration, Instant};

/// Encoders and gesture state, created once per engine.
pub(super) struct Input {
    key_enc: key::Encoder<'static>,
    key_ev: key::Event<'static>,
    mouse_enc: mouse::Encoder<'static>,
    mouse_ev: mouse::Event<'static>,
    gesture: Gesture<'static>,
    press: PressEvent<'static>,
    drag: DragEvent<'static>,
    release: ReleaseEvent<'static>,
    tick: AutoscrollTickEvent<'static>,
    epoch: Instant,
    /// Mouse buttons currently held (bit per button 1..3) as sent to the app.
    buttons: u8,
    /// The left button is driving a selection gesture.
    selecting: bool,
    find: Option<FindState>,
}

/// A selection on the alternate screen, kept on its text when the app repaints the screen
/// instead of scrolling it (see [`Engine::follow_selection`]).
pub(super) struct Follow {
    /// First and last selected cells `(x, y)`, top row first, and whether it's a rectangle.
    a: (u16, u16),
    b: (u16, u16),
    rect: bool,
    /// Each screen row's text when the selection was last placed; read at the next frame.
    screen: Option<Vec<String>>,
}

/// Where a selection's text went after the app redrew the screen.
#[derive(Debug, PartialEq)]
enum Moved {
    /// Down by this many rows (up when negative).
    By(i32),
    /// Off the screen.
    Gone,
}

/// Where the text of rows `top..=bottom` in `old` is in `now`: the nearest shift that puts
/// every row still on the screen on the same text. When the rows are nowhere, the screen's
/// own shift (the one most rows agree on) tells whether they went off it.
fn moved(old: &[String], now: &[String], top: usize, bottom: usize) -> Option<Moved> {
    let n = now.len() as i32;
    let at = |y: usize, d: i32| -> Option<&String> { (y as i32 + d >= 0 && (y as i32 + d) < n).then(|| &now[(y as i32 + d) as usize]) };
    for k in 1..n {
        for d in [-k, k] {
            let mut text = false;
            let fits = (top..=bottom.min(old.len().saturating_sub(1))).all(|y| match at(y, d) {
                Some(row) => {
                    text |= !old[y].is_empty();
                    *row == old[y]
                }
                None => true,
            });
            if fits && text {
                return Some(Moved::By(d));
            }
        }
    }
    // Rows of words, not rules and borders, which repeat all over a screen.
    let wordy = |row: &String| row.chars().any(char::is_alphanumeric);
    let agree = |d: i32| old.iter().enumerate().filter(|(y, row)| wordy(row) && at(*y, d) == Some(row)).count();
    let (d, votes) = (1 - n..n).filter(|&d| d != 0).map(|d| (d, agree(d))).max_by_key(|&(d, v)| (v, -d.abs()))?;
    let off = top as i32 + d >= n || (bottom as i32) + d < 0;
    (off && votes >= 2 && votes > agree(0)).then_some(Moved::Gone)
}

struct FindState {
    query: String,
    /// (screen row, column) of the current match.
    at: Option<(usize, usize)>,
}

impl Input {
    pub(super) fn new() -> Input {
        let mut press = PressEvent::new().expect("press event");
        let _ = press.set_repeat_interval(Duration::from_millis(450));
        Input {
            key_enc: key::Encoder::new().expect("key encoder"),
            key_ev: key::Event::new().expect("key event"),
            mouse_enc: mouse::Encoder::new().expect("mouse encoder"),
            mouse_ev: mouse::Event::new().expect("mouse event"),
            gesture: Gesture::new().expect("selection gesture"),
            press,
            drag: DragEvent::new().expect("drag event"),
            release: ReleaseEvent::new().expect("release event"),
            tick: AutoscrollTickEvent::new().expect("autoscroll event"),
            epoch: Instant::now(),
            buttons: 0,
            selecting: false,
            find: None,
        }
    }
}

/// A link under the pointer (`session.link_at`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Link {
    Url(String),
    /// A `<scheme>://…` URL an app on this Mac opens (`taskboard://…`), and that app's name.
    App { url: String, app: String },
    File { path: String, line: Option<u32>, column: Option<u32> },
    /// Another terminal, by id (see `tab_ref`).
    Session(String),
    /// A link pattern's match whose rule names a preview command (already filled from the
    /// captures): its URL, the app that opens it (None: http(s)), and the command.
    Preview { url: String, app: Option<String>, preview: Vec<String> },
}

/// Scheme of an explicit terminal link: `midna://session/<id>`.
pub const SESSION_LINK: &str = "midna://session/";

/// Result of a find step.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FindHit {
    pub total: usize,
    /// 1-based index of the current match (0 = none).
    pub index: usize,
}

fn mods_of(m: u8) -> Mods {
    let mut out = Mods::empty();
    if m & MOD_SHIFT != 0 {
        out |= Mods::SHIFT;
    }
    if m & MOD_ALT != 0 {
        out |= Mods::ALT;
    }
    if m & MOD_CTRL != 0 {
        out |= Mods::CTRL;
    }
    if m & MOD_SUPER != 0 {
        out |= Mods::SUPER;
    }
    out
}

/// US-layout shifted punctuation -> the unshifted key.
fn unshift(c: char) -> Option<char> {
    const PAIRS: &[(char, char)] = &[
        ('~', '`'), ('!', '1'), ('@', '2'), ('#', '3'), ('$', '4'), ('%', '5'), ('^', '6'), ('&', '7'), ('*', '8'), ('(', '9'),
        (')', '0'), ('_', '-'), ('+', '='), ('{', '['), ('}', ']'), ('|', '\\'), (':', ';'), ('"', '\''), ('<', ','), ('>', '.'),
        ('?', '/'),
    ];
    if c.is_ascii_uppercase() {
        return Some(c.to_ascii_lowercase());
    }
    PAIRS.iter().find(|(s, _)| *s == c).map(|(_, u)| *u)
}

fn shifted(c: char) -> char {
    const PAIRS: &str = "`~1!2@3#4$5%6^7&8*9(0)-_=+[{]}\\|;:'\",<.>/?";
    if c.is_ascii_lowercase() {
        return c.to_ascii_uppercase();
    }
    let v: Vec<char> = PAIRS.chars().collect();
    v.chunks(2).find(|p| p[0] == c).map(|p| p[1]).unwrap_or(c)
}

fn char_key(c: char) -> Key {
    let code = match c {
        'a'..='z' => Key::A as u32 + (c as u32 - 'a' as u32),
        '0'..='9' => Key::Digit0 as u32 + (c as u32 - '0' as u32),
        '`' => Key::Backquote as u32,
        '\\' => Key::Backslash as u32,
        '[' => Key::BracketLeft as u32,
        ']' => Key::BracketRight as u32,
        ',' => Key::Comma as u32,
        '=' => Key::Equal as u32,
        '-' => Key::Minus as u32,
        '.' => Key::Period as u32,
        '\'' => Key::Quote as u32,
        ';' => Key::Semicolon as u32,
        '/' => Key::Slash as u32,
        ' ' => Key::Space as u32,
        _ => Key::Unidentified as u32,
    };
    Key::try_from(code).unwrap_or(Key::Unidentified)
}

/// GPUI key name -> (libghostty key, unshifted codepoint, shift implied by the name).
pub fn key_from_name(name: &str) -> Option<(Key, Option<char>, bool)> {
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.clone().next()) {
        let (base, implied) = match unshift(c) {
            Some(u) => (u, true),
            None => (c, false),
        };
        return Some((char_key(base), Some(base), implied));
    }
    let k = match name {
        "enter" => Key::Enter,
        "tab" => Key::Tab,
        "space" => return Some((Key::Space, Some(' '), false)),
        "backspace" => Key::Backspace,
        "escape" => Key::Escape,
        "up" => Key::ArrowUp,
        "down" => Key::ArrowDown,
        "left" => Key::ArrowLeft,
        "right" => Key::ArrowRight,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "insert" => Key::Insert,
        "delete" => Key::Delete,
        f if f.starts_with('f') => {
            let n: u32 = f[1..].parse().ok()?;
            if !(1..=25).contains(&n) {
                return None;
            }
            Key::try_from(Key::F1 as u32 + n - 1).ok()?
        }
        _ => return None,
    };
    Some((k, None, false))
}

impl Engine {
    fn cell_px(&self) -> (u32, u32) {
        (self.cell_w.max(1), self.cell_h.max(1))
    }

    fn at_bottom(&self) -> bool {
        self.term.scrollbar().map(|s| s.offset + s.len >= s.total).unwrap_or(true)
    }

    /// Back to the live screen (typing, pasting).
    pub fn snap_to_bottom(&mut self) {
        if !self.at_bottom() {
            self.term.scroll_viewport(ScrollViewport::Bottom);
            self.force_full = true;
        }
    }

    pub fn clear_selection(&mut self) {
        if self.has_selection {
            let _ = self.term.set_selection(None);
            self.has_selection = false;
            self.sel_col = None;
            self.sel_follow = None;
            self.meta_dirty = true;
        }
    }

    /// Encode a key for the PTY. Typing (press/repeat that produces bytes) snaps the viewport
    /// to the bottom and clears the selection, like other terminals.
    pub fn key(&mut self, k: &KeyMsg) -> Vec<u8> {
        let Some((key, base, implied_shift)) = key_from_name(&k.key) else { return vec![] };
        let mut m = k.mods;
        if implied_shift {
            m |= MOD_SHIFT;
        }
        let shift = m & MOD_SHIFT != 0;
        let meta = m & (MOD_CTRL | MOD_ALT) != 0;
        // The text the key types: the unmodified character for ctrl/alt combos (the encoder
        // derives those sequences from the key), otherwise what the layout produced.
        let text: Option<String> = match base {
            Some(b) => {
                let derived = if shift { shifted(b) } else { b };
                let client = k.text.chars().next().filter(|c| !c.is_control() && !meta && !('\u{f700}'..='\u{f8ff}').contains(c));
                Some(match client {
                    Some(_) => k.text.clone(),
                    None => derived.to_string(),
                })
            }
            None => None,
        };
        let consumed = match (base, text.as_deref()) {
            (Some(b), Some(t)) if shift && t != b.to_string() => Mods::SHIFT,
            _ => Mods::empty(),
        };
        let action = match k.action {
            KeyAction::Press => Action::Press,
            KeyAction::Release => Action::Release,
            KeyAction::Repeat => Action::Repeat,
        };
        let kitty = self.kitty_flags();
        // midna's long-standing shift-enter in legacy mode: ESC CR, which agents (Claude Code)
        // read as "newline without submit". The legacy encoder would send a bare CR.
        let out = if key == Key::Enter && m == MOD_SHIFT && kitty == 0 && action != Action::Release {
            b"\x1b\r".to_vec()
        } else {
            let Input { key_enc, key_ev, .. } = &mut self.input;
            key_enc.set_options_from_terminal(&self.term).set_macos_option_as_alt(OptionAsAlt::True);
            key_ev
                .set_action(action)
                .set_key(key)
                .set_mods(mods_of(m))
                .set_consumed_mods(consumed)
                .set_composing(false)
                .set_utf8(text)
                .set_unshifted_codepoint(base.unwrap_or('\0'));
            let mut out = Vec::new();
            let _ = key_enc.encode_to_vec(key_ev, &mut out);
            out
        };
        if !out.is_empty() && action != Action::Release {
            self.snap_to_bottom();
            self.clear_selection();
        }
        out
    }

    /// Paste: unsafe control bytes become spaces; bracketed when the app enabled 2004,
    /// otherwise newlines become CR.
    pub fn paste(&mut self, text: &str) -> Vec<u8> {
        let bracketed = self.term.mode(Mode::BRACKETED_PASTE).unwrap_or(false);
        let mut data = text.replace("\r\n", "\n").into_bytes();
        let mut out = vec![0u8; data.len() + 16];
        let n = match libghostty_vt::paste::encode(&mut data, bracketed, &mut out) {
            Ok(n) => n,
            Err(libghostty_vt::Error::OutOfSpace { required }) => {
                out.resize(required, 0);
                libghostty_vt::paste::encode(&mut data, bracketed, &mut out).unwrap_or(0)
            }
            Err(_) => 0,
        };
        out.truncate(n);
        if !out.is_empty() {
            self.snap_to_bottom();
            self.clear_selection();
        }
        out
    }

    /// CSI I / CSI O when the app asked for focus events (mode 1004).
    pub fn focus(&mut self, gained: bool) -> Vec<u8> {
        if !self.term.mode(Mode::FOCUS_EVENT).unwrap_or(false) {
            return vec![];
        }
        let mut buf = [0u8; 8];
        let ev = if gained { libghostty_vt::focus::Event::Gained } else { libghostty_vt::focus::Event::Lost };
        ev.encode(&mut buf).map(|n| buf[..n].to_vec()).unwrap_or_default()
    }

    fn mouse_tracking(&self) -> bool {
        self.term.is_mouse_tracking().unwrap_or(false)
    }

    fn encode_mouse(&mut self, action: mouse::Action, button: Option<Button>, mods: u8, x: f32, y: f32) -> Vec<u8> {
        let (cw, ch) = self.cell_px();
        let (cols, rows) = self.size();
        let Input { mouse_enc, mouse_ev, buttons, .. } = &mut self.input;
        mouse_enc
            .set_options_from_terminal(&self.term)
            .set_size(EncoderSize {
                screen_width: cols as u32 * cw,
                screen_height: rows as u32 * ch,
                cell_width: cw,
                cell_height: ch,
                padding_top: 0,
                padding_bottom: 0,
                padding_right: 0,
                padding_left: 0,
            })
            .set_any_button_pressed(*buttons != 0)
            .set_track_last_cell(true);
        mouse_ev
            .set_action(action)
            .set_button(button)
            .set_mods(mods_of(mods))
            .set_position(mouse::Position { x: x * cw as f32, y: y * ch as f32 });
        let mut out = Vec::new();
        let _ = mouse_enc.encode_to_vec(mouse_ev, &mut out);
        out
    }

    /// Mouse press/release/motion. Goes to the app when it has mouse reporting on (shift
    /// overrides, as in other terminals); otherwise the left button selects.
    pub fn mouse(&mut self, m: &MouseMsg) -> Vec<u8> {
        let button = match m.button {
            1 => Some(Button::Left),
            2 => Some(Button::Right),
            3 => Some(Button::Middle),
            _ => None,
        };
        if self.mouse_tracking() && m.mods & MOD_SHIFT == 0 && !self.input.selecting {
            let bit = 1u8 << m.button.min(7);
            let action = match m.action {
                MouseAction::Press => {
                    self.input.buttons |= bit;
                    mouse::Action::Press
                }
                MouseAction::Release => {
                    self.input.buttons &= !bit;
                    mouse::Action::Release
                }
                MouseAction::Motion => mouse::Action::Motion,
            };
            let x = m.x.max(0.0);
            let y = m.y.max(0.0);
            return self.encode_mouse(action, button, m.mods, x, y);
        }
        match (m.action, m.button) {
            (MouseAction::Press, 1) => self.select_press(m),
            (MouseAction::Motion, _) if self.input.selecting => self.select_drag(m),
            (MouseAction::Release, 1) if self.input.selecting => self.select_release(m),
            _ => {}
        }
        vec![]
    }

    fn viewport_ref_coord(&self, x: f32, y: f32) -> PointCoordinate {
        let (cols, rows) = self.size();
        PointCoordinate { x: (x.max(0.0) as u16).min(cols.saturating_sub(1)), y: (y.max(0.0) as u32).min(rows.saturating_sub(1) as u32) }
    }

    fn geometry(&self) -> Geometry {
        let (cw, ch) = self.cell_px();
        let (cols, rows) = self.size();
        Geometry { columns: cols as u32, cell_width: cw, padding_left: 0, screen_height: rows as u32 * ch }
    }

    /// Returns whether there's a selection now, the column it starts at (reading order), and
    /// on the alternate screen, what [`Self::follow_selection`] keeps on its text.
    fn install(term: &Terminal<'static, 'static>, sel: Option<Selection<'_>>) -> (bool, Option<u16>, Option<Follow>) {
        let _ = term.set_selection(sel.as_ref());
        let Some(s) = sel.as_ref() else { return (false, None, None) };
        let at = |r| term.point_from_grid_ref(&r, PointSpace::Screen).ok().flatten();
        let Some((a, b)) = at(s.start()).zip(at(s.end())) else { return (true, None, None) };
        let (a, b) = if (a.y, a.x) <= (b.y, b.x) { (a, b) } else { (b, a) };
        let rect = s.is_rectangle();
        let alt = matches!(term.active_screen(), Ok(Screen::Alternate));
        let follow = alt.then(|| Follow { a: (a.x, a.y as u16), b: (b.x, b.y as u16), rect, screen: None });
        (true, (!rect).then_some(a.x), follow)
    }

    /// Viewport row `y`'s text, trailing spaces trimmed.
    fn row_text(&self, y: u16) -> String {
        let (chars, _) = self.rows_text(y, y, None);
        chars.into_iter().collect::<String>().trim_end().to_string()
    }

    /// Full-screen apps (Claude Code's fullscreen transcript) redraw the alternate screen
    /// rather than scroll it, so the selection's cells stay put while their text moves. Called
    /// before each frame: once the selected rows read differently, find their text again and
    /// move the selection onto it, or drop it when the text went off the screen. Rows that
    /// match nowhere (half-way through a redraw) leave it alone until a later frame.
    pub(super) fn follow_selection(&mut self) {
        if self.input.selecting || !self.has_selection || self.sel_follow.is_none() {
            return;
        }
        if !matches!(self.term.active_screen(), Ok(Screen::Alternate)) {
            self.sel_follow = None;
            return;
        }
        let (cols, rows) = self.size();
        let Some(f) = self.sel_follow.as_ref() else { return };
        let (a, b, rect) = (f.a, f.b, f.rect);
        let bottom = b.1.min(rows.saturating_sub(1));
        let Some(old) = f.screen.as_ref() else {
            let screen = (0..rows).map(|y| self.row_text(y)).collect();
            if let Some(f) = self.sel_follow.as_mut() {
                f.screen = Some(screen);
            }
            return;
        };
        if old.len() != rows as usize {
            self.sel_follow = None;
            return;
        }
        if (a.1..=bottom).all(|y| self.row_text(y) == old[y as usize]) {
            return;
        }
        let now: Vec<String> = (0..rows).map(|y| self.row_text(y)).collect();
        match moved(old, &now, a.1 as usize, bottom as usize) {
            None => {}
            Some(Moved::Gone) => self.clear_selection(),
            Some(Moved::By(d)) => {
                let last = rows as i32 - 1;
                let (ay, by) = (a.1 as i32 + d, b.1 as i32 + d);
                let a = if ay < 0 { (if rect { a.0 } else { 0 }, 0) } else { (a.0, ay.min(last)) };
                let b = if by > last { (if rect { b.0 } else { cols.saturating_sub(1) }, last) } else { (b.0, by.max(0)) };
                let at = |(x, y): (u16, i32)| self.term.grid_ref(Point::Viewport(PointCoordinate { x, y: y as u32 }));
                let (Ok(ra), Ok(rb)) = (at(a), at(b)) else { return };
                let sel = Selection::new(ra, rb, rect);
                let follow;
                (self.has_selection, self.sel_col, follow) = Self::install(&self.term, Some(sel));
                self.sel_follow = follow.map(|f| Follow { screen: Some(now), ..f });
                self.meta_dirty = true;
            }
        }
    }

    fn select_press(&mut self, m: &MouseMsg) {
        let (cw, ch) = self.cell_px();
        let coord = self.viewport_ref_coord(m.x, m.y);
        let t = self.input.epoch.elapsed();
        let Engine { term, input, .. } = self;
        let Ok(r) = term.grid_ref(Point::Viewport(coord)) else { return };
        let _ = input.press.set_position((m.x * cw as f32) as f64, (m.y * ch as f32) as f64);
        let _ = input.press.set_repeat_distance(cw.max(ch) as f64);
        let _ = input.press.set_time(t);
        let sel = input.press.apply(&mut input.gesture, term, r).ok().flatten();
        (self.has_selection, self.sel_col, self.sel_follow) = Self::install(term, sel);
        self.input.selecting = true;
        self.meta_dirty = true;
    }

    fn select_drag(&mut self, m: &MouseMsg) {
        let (cw, ch) = self.cell_px();
        let geom = self.geometry();
        let coord = self.viewport_ref_coord(m.x, m.y);
        let (_, rows) = self.size();
        // Dragging past the top/bottom edge scrolls the viewport one row per event; the
        // client repeats the last motion while the pointer stays outside.
        let edge = if m.y < 0.0 { -1 } else if m.y >= rows as f32 { 1 } else { 0 };
        if edge != 0 && !matches!(self.term.active_screen(), Ok(Screen::Alternate)) {
            self.term.scroll_viewport(ScrollViewport::Delta(edge));
            self.force_full = true;
            let Engine { term, input, .. } = self;
            let vp = PointCoordinate { x: coord.x, y: if edge < 0 { 0 } else { rows.saturating_sub(1) as u32 } };
            let _ = input.tick.set_position((m.x * cw as f32) as f64, (m.y * ch as f32) as f64);
            let _ = input.tick.set_rectangle(m.mods & MOD_ALT != 0);
            if let Ok(Some(sel)) = input.tick.apply(&mut input.gesture, term, vp, geom) {
                (self.has_selection, self.sel_col, self.sel_follow) = Self::install(term, Some(sel));
                self.meta_dirty = true;
                return;
            }
        }
        let Engine { term, input, .. } = self;
        let Ok(r) = term.grid_ref(Point::Viewport(coord)) else { return };
        let _ = input.drag.set_position((m.x * cw as f32) as f64, (m.y * ch as f32) as f64);
        let _ = input.drag.set_rectangle(m.mods & MOD_ALT != 0);
        if let Ok(Some(sel)) = input.drag.apply(&mut input.gesture, term, r, geom) {
            (self.has_selection, self.sel_col, self.sel_follow) = Self::install(term, Some(sel));
            self.meta_dirty = true;
        }
    }

    fn select_release(&mut self, m: &MouseMsg) {
        let coord = self.viewport_ref_coord(m.x, m.y);
        let Engine { term, input, .. } = self;
        let r = term.grid_ref(Point::Viewport(coord)).ok();
        let _ = input.release.apply(&mut input.gesture, term, r);
        input.selecting = false;
        // A plain click (no drag, single click) leaves an empty selection behind: drop it.
        if let Some(t) = self.selection_text()
            && t.is_empty()
        {
            self.clear_selection();
        }
    }

    /// Wheel / scroll keys. Returns bytes for the app (wheel reports or arrow keys).
    pub fn scroll(&mut self, s: &ScrollMsg) -> Vec<u8> {
        let rows = self.size().1 as isize;
        let delta = match s.kind {
            ScrollKind::Top => {
                self.term.scroll_viewport(ScrollViewport::Top);
                self.force_full = true;
                return vec![];
            }
            ScrollKind::Bottom => {
                self.term.scroll_viewport(ScrollViewport::Bottom);
                self.force_full = true;
                return vec![];
            }
            ScrollKind::Pages => s.amount as isize * (rows - 1).max(1),
            ScrollKind::Lines => s.amount as isize,
            ScrollKind::Wheel => {
                let n = s.amount.unsigned_abs().min(20) as usize;
                if n == 0 {
                    return vec![];
                }
                if self.mouse_tracking() && s.mods & MOD_SHIFT == 0 {
                    let b = if s.amount < 0 { Button::Four } else { Button::Five };
                    let mut out = Vec::new();
                    for _ in 0..n {
                        out.extend(self.encode_mouse(mouse::Action::Press, Some(b), s.mods, s.x.max(0.0), s.y.max(0.0)));
                    }
                    return out;
                }
                if matches!(self.term.active_screen(), Ok(Screen::Alternate)) {
                    // Alternate scroll: full-screen apps without mouse reporting (less, man)
                    // get arrow keys.
                    let k = KeyMsg { key: if s.amount < 0 { "up" } else { "down" }.into(), ..Default::default() };
                    let one = self.key(&k);
                    return one.repeat(n);
                }
                s.amount as isize
            }
        };
        if delta != 0 {
            self.term.scroll_viewport(ScrollViewport::Delta(delta));
            self.force_full = true;
        }
        vec![]
    }

    /// The selection as plain text (soft-wrapped lines joined, trailing spaces trimmed).
    pub fn selection_text(&self) -> Option<String> {
        if !self.has_selection {
            return None;
        }
        let opts = FormatOptions::new().with_emit_format(Format::Plain).with_unwrap(true).with_trim(true);
        let bytes = self.term.format_selection_alloc(None, opts).ok().flatten()?;
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// The selection as copied from an agent's TUI: as [`Self::selection_text`], with the rows
    /// Claude Code broke at the right edge joined again (see [`rejoin_wrapped`]).
    pub fn agent_selection_text(&self) -> Option<String> {
        let text = self.selection_text()?;
        Some(match self.sel_col {
            Some(col) => rejoin_wrapped(&text, col as usize, self.size().0 as usize),
            None => text,
        })
    }

    /// Drop the scrollback (primary screen only). Returns false on the alternate screen or
    /// while a half-received escape sequence is pending (injecting bytes would corrupt it).
    pub fn clear_scrollback(&mut self) -> bool {
        let alt = matches!(self.term.active_screen(), Ok(Screen::Alternate));
        if alt || !self.tail.is_empty() {
            return false;
        }
        self.term.vt_write(b"\x1b[3J");
        self.clear_selection();
        self.force_full = true;
        true
    }

    /// Select everything (scrollback + screen).
    pub fn select_all(&mut self) {
        let sel = self.term.select_all().ok().flatten();
        (self.has_selection, self.sel_col, self.sel_follow) = Self::install(&self.term, sel);
        self.meta_dirty = true;
    }

    /// The viewport rows `(top, bottom)` of the soft-wrapped line through row `y`.
    fn line_rows(&self, y: u16) -> (u16, u16) {
        let rows = self.size().1;
        let wrapped = |yy: u16| -> bool {
            self.term
                .grid_ref(Point::Viewport(PointCoordinate { x: 0, y: yy as u32 }))
                .ok()
                .and_then(|r| r.row().ok())
                .and_then(|r| r.is_wrapped().ok())
                .unwrap_or(false)
        };
        let mut top = y;
        while top > 0 && y - top < 8 && wrapped(top - 1) {
            top -= 1;
        }
        let mut bottom = y;
        while bottom + 1 < rows && bottom - y < 8 && wrapped(bottom) {
            bottom += 1;
        }
        (top, bottom)
    }

    /// Text of viewport rows `top..=bottom`, one char per cell (wide chars once), and the
    /// char index of cell `at` when it's among them.
    fn rows_text(&self, top: u16, bottom: u16, at: Option<(u16, u16)>) -> (Vec<char>, Option<usize>) {
        let cols = self.size().0;
        let mut chars = Vec::new();
        let mut hit = None;
        let mut buf = ['\0'; 16];
        for yy in top..=bottom {
            for xx in 0..cols {
                let Ok(r) = self.term.grid_ref(Point::Viewport(PointCoordinate { x: xx, y: yy as u32 })) else { continue };
                if at == Some((xx, yy)) {
                    hit = Some(chars.len());
                }
                let n = r.graphemes(&mut buf).unwrap_or(0);
                if n == 0 {
                    // Spacer tail of a wide char, or an empty cell (a space).
                    let spacer = r.cell().ok().and_then(|c| c.wide().ok()).is_some_and(|w| !matches!(w, libghostty_vt::screen::CellWide::Narrow | libghostty_vt::screen::CellWide::Wide));
                    if !spacer {
                        chars.push(' ');
                    } else if hit == Some(chars.len()) && !chars.is_empty() {
                        hit = Some(chars.len() - 1);
                    }
                } else {
                    chars.extend_from_slice(&buf[..n.min(1)]);
                }
            }
        }
        (chars, hit)
    }

    /// The logical lines around viewport row `y` (up to `HARD_WRAP_ROWS` each way), the
    /// index of the one holding (x, y) and the char index of (x, y) in it.
    fn nearby_lines(&self, x: u16, y: u16) -> Option<(Vec<Vec<char>>, usize, usize)> {
        let rows = self.size().1;
        let (top, bottom) = self.line_rows(y);
        let (line, hit) = self.rows_text(top, bottom, Some((x, y)));
        let mut lines = vec![line];
        let mut at = 0;
        let mut t = top;
        while t > 0 && at < HARD_WRAP_ROWS {
            let (a, b) = self.line_rows(t - 1);
            lines.insert(0, self.rows_text(a, b, None).0);
            at += 1;
            t = a;
        }
        let mut b = bottom;
        while b + 1 < rows && lines.len() - at - 1 < HARD_WRAP_ROWS {
            let (a, bb) = self.line_rows(b + 1);
            lines.push(self.rows_text(a, bb, None).0);
            b = bb;
        }
        Some((lines, at, hit?))
    }

    /// The OSC 8 hyperlink at a viewport cell, else a detected http(s) URL, a reference to
    /// another terminal (`tabs`, see `tab_ref`), an app link (`schemes.rs`) or an existing file
    /// path (`path:line[:col]`, relative paths resolved against `cwd`, then the shell's OSC 7
    /// directory), or text one of `patterns` (`terminal.link_patterns`) turns into a link.
    pub fn link_at(&self, x: u16, y: u16, cwd: &str, tabs: &[(String, String)], patterns: &[LinkPattern]) -> Option<Link> {
        if let Ok(r) = self.term.grid_ref(Point::Viewport(PointCoordinate { x, y: y as u32 })) {
            let mut buf = vec![0u8; 2048];
            if let Ok(n) = r.hyperlink_uri(&mut buf)
                && n > 0
            {
                let url = String::from_utf8_lossy(&buf[..n]).into_owned();
                if url.starts_with(SESSION_LINK) {
                    return tab_ref(&url, tabs).map(Link::Session);
                }
                let app = url.split_once("://").and_then(|(scheme, _)| crate::schemes::app_for(scheme));
                return Some(match app {
                    Some(app) => Link::App { url, app },
                    None => Link::Url(url),
                });
            }
        }
        let (lines, at, i) = self.nearby_lines(x, y)?;
        let pwd = osc7_path(self.term.pwd().unwrap_or(""));
        let dirs = [pwd.as_deref().unwrap_or(""), cwd];
        // A URL an app broke onto the next line itself (Claude Code does, indenting the rest).
        if let Some((joined, j)) = join_hard_wrapped(&lines, at, i)
            && let Some(link @ (Link::Url(_) | Link::App { .. } | Link::File { .. })) = detect_link(&joined, j, &dirs, &crate::schemes::app_for, tabs, patterns)
        {
            return Some(link);
        }
        detect_link(&lines[at], i, &dirs, &crate::schemes::app_for, tabs, patterns)
    }

    /// Find `query` (case-insensitive unless it has capitals) in scrollback + screen, moving
    /// to the next (or previous) match: the viewport scrolls to it and it becomes the
    /// selection. An empty query clears the search.
    pub fn find(&mut self, query: &str, backwards: bool) -> FindHit {
        if query.is_empty() {
            self.input.find = None;
            self.clear_selection();
            return FindHit::default();
        }
        let case = query.chars().any(|c| c.is_uppercase());
        let norm = |s: &str| if case { s.to_string() } else { s.to_lowercase() };
        let q: Vec<char> = norm(query).chars().collect();
        let lines = self.plain_lines();
        let mut hits: Vec<(usize, usize, usize)> = Vec::new(); // (row, col, width)
        for (row, l) in lines.iter().enumerate() {
            let lc: Vec<char> = norm(l).chars().collect();
            let orig: Vec<char> = l.chars().collect();
            if lc.len() != orig.len() || lc.len() < q.len() {
                continue;
            }
            let mut i = 0;
            while i + q.len() <= lc.len() {
                if lc[i..i + q.len()] == q[..] {
                    let col: usize = orig[..i].iter().map(|&c| libghostty_vt::unicode::codepoint_width(c) as usize).sum();
                    let w: usize = orig[i..i + q.len()].iter().map(|&c| (libghostty_vt::unicode::codepoint_width(c) as usize).max(1)).sum();
                    hits.push((row, col, w));
                    i += q.len();
                } else {
                    i += 1;
                }
            }
        }
        let prev = self.input.find.as_ref().filter(|f| f.query == query).and_then(|f| f.at);
        if hits.is_empty() {
            self.input.find = Some(FindState { query: query.into(), at: None });
            self.clear_selection();
            return FindHit { total: 0, index: 0 };
        }
        let idx = match prev {
            None => {
                // Start from the bottom of the viewport: the newest match at or above it.
                hits.len() - 1
            }
            Some(at) => {
                let cur = hits.iter().position(|h| (h.0, h.1) == at);
                match (cur, backwards) {
                    (Some(c), true) => (c + hits.len() - 1) % hits.len(),
                    (Some(c), false) => (c + 1) % hits.len(),
                    (None, _) => hits.iter().rposition(|h| (h.0, h.1) <= at).unwrap_or(hits.len() - 1),
                }
            }
        };
        let (row, col, w) = hits[idx];
        self.input.find = Some(FindState { query: query.into(), at: Some((row, col)) });
        let rows = self.size().1 as usize;
        self.term.scroll_viewport(ScrollViewport::Row(row.saturating_sub(rows / 2)));
        self.force_full = true;
        let cols = self.size().0 as usize;
        let a = PointCoordinate { x: col.min(cols - 1) as u16, y: row as u32 };
        let b = PointCoordinate { x: (col + w - 1).min(cols - 1) as u16, y: row as u32 };
        if let (Ok(ra), Ok(rb)) = (self.term.grid_ref(Point::Screen(a)), self.term.grid_ref(Point::Screen(b))) {
            let sel = Selection::new(ra, rb, false);
            let _ = self.term.set_selection(Some(&sel));
            self.has_selection = true;
            self.sel_col = Some(a.x);
            self.sel_follow = None;
            self.meta_dirty = true;
        }
        FindHit { total: hits.len(), index: idx + 1 }
    }
}

/// How many lines above and below a click `join_hard_wrapped` may join.
const HARD_WRAP_ROWS: usize = 4;

/// `lines[at]` with the word at char `i` joined to its pieces on the lines around it, when an
/// app broke a long word with a newline itself (Claude Code does, indenting what follows):
/// a line whose word runs to the right edge goes on with the first word of the next line.
/// Returns the joined text and the click's index in it, or None when there's nothing to join.
fn join_hard_wrapped(lines: &[Vec<char>], at: usize, i: usize) -> Option<(Vec<char>, usize)> {
    let ws = |c: &char| c.is_whitespace() || *c == '\0';
    // The line's last word ends at the right edge (at most one cell short of it).
    let to_edge = |l: &[char]| l.len() >= 2 && l.iter().rev().take_while(|c| ws(c)).count() <= 1;
    let trimmed = |l: &[char]| l.iter().position(|c| !ws(c)).map_or(&l[l.len()..], |p| &l[p..]).to_vec();
    let word_start = |l: &[char], i: usize| (0..=i).rev().find(|&k| k == 0 || ws(&l[k - 1])).unwrap_or(0);
    let mut line = lines.get(at)?.clone();
    let mut hit = i;
    if hit >= line.len() || ws(&line[hit]) {
        return None;
    }
    let mut joined = false;
    // Up: while the clicked word starts its line and the line above ends at the edge.
    let mut k = at;
    while k > 0 && line[..word_start(&line, hit)].iter().all(ws) && to_edge(&lines[k - 1]) {
        let lead = line.iter().take_while(|c| ws(c)).count();
        let mut prev = lines[k - 1].clone();
        while prev.last().is_some_and(ws) {
            prev.pop();
        }
        hit = hit - lead + prev.len();
        prev.extend_from_slice(&line[lead..]);
        line = prev;
        joined = true;
        k -= 1;
    }
    // Down: while the clicked word ends its line at the edge.
    let mut k = at;
    while k + 1 < lines.len() && to_edge(&line) && line[hit..].iter().skip_while(|c| !ws(c)).all(ws) {
        let next = trimmed(&lines[k + 1]);
        if next.is_empty() {
            break;
        }
        while line.last().is_some_and(ws) {
            line.pop();
        }
        line.extend_from_slice(&next);
        joined = true;
        k += 1;
    }
    joined.then_some((line, hit))
}

/// `file://host/path` (OSC 7) -> `/path`.
fn osc7_path(pwd: &str) -> Option<String> {
    if pwd.is_empty() {
        return None;
    }
    let p = pwd.strip_prefix("file://").map(|rest| rest.find('/').map(|i| &rest[i..]).unwrap_or("")).unwrap_or(pwd);
    let p = percent_decode(p);
    (!p.is_empty()).then_some(p)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Link detection on a logical line: `i` is the clicked char. `app_for` names the app a URL
/// scheme opens (`schemes::app_for`); `tabs` are the terminals a word can name (`tab_ref`);
/// `patterns` are the `terminal.link_patterns` rules that apply in this terminal.
pub fn detect_link(chars: &[char], i: usize, dirs: &[&str], app_for: &dyn Fn(&str) -> Option<String>, tabs: &[(String, String)], patterns: &[LinkPattern]) -> Option<Link> {
    if i >= chars.len() || chars[i].is_whitespace() {
        return None;
    }
    let mut a = i;
    while a > 0 && !chars[a - 1].is_whitespace() {
        a -= 1;
    }
    let mut b = i + 1;
    while b < chars.len() && !chars[b].is_whitespace() {
        b += 1;
    }
    let word: String = chars[a..b].iter().collect();
    let click = chars[a..i].iter().map(|c| c.len_utf8()).sum::<usize>();
    // URLs: the http(s) span containing the click.
    for (start, _) in word.match_indices("http") {
        let rest = &word[start..];
        if !(rest.starts_with("http://") || rest.starts_with("https://")) {
            continue;
        }
        let url = trim_url(rest);
        if click >= start && click < start + url.len().max(1) && url.len() > 8 {
            return Some(Link::Url(url.to_string()));
        }
    }
    // Another terminal: its id, an agent's conversation id, or `midna://session/<id>`.
    let t = word.trim_matches(|c: char| matches!(c, '(' | ')' | '[' | ']' | '<' | '>' | '"' | '\'' | '`' | ',' | ';'));
    let t = t.trim_end_matches(['.', ':']);
    if let Some(id) = tab_ref(t, tabs) {
        return Some(Link::Session(id));
    }
    // App links: `<scheme>://…` when an app opens the scheme.
    for (start, url, app) in crate::schemes::app_links(&word, trim_url, app_for) {
        if click >= start && click < start + url.len() {
            return Some(Link::App { url: url.to_string(), app });
        }
    }
    // Link patterns: the first rule whose match is under the pointer and whose URL opens.
    if !patterns.is_empty() {
        let line: String = chars.iter().collect();
        let at = chars[..i].iter().map(|c| c.len_utf8()).sum::<usize>();
        for p in patterns {
            let Some((_, url)) = p.link_at(&line, at) else { continue };
            let scheme = url.split_once("://").map_or("", |(s, _)| s).to_ascii_lowercase();
            let app = if matches!(scheme.as_str(), "http" | "https") {
                None
            } else if let Some(app) = app_for(&scheme) {
                Some(app)
            } else {
                continue;
            };
            return Some(match (p.preview_at(&line, at), app) {
                (Some(preview), app) => Link::Preview { url, app, preview },
                (None, Some(app)) => Link::App { url, app },
                (None, None) => Link::Url(url),
            });
        }
    }
    // File paths, optionally with :line[:col].
    if t.is_empty() || t.contains("://") {
        return None;
    }
    let mut parts = t.splitn(3, ':');
    let path = parts.next()?;
    let line = parts.next().and_then(|s| s.parse::<u32>().ok());
    let column = parts.next().and_then(|s| s.trim_end_matches(|c: char| !c.is_ascii_digit()).parse::<u32>().ok());
    if path.is_empty() || !(path.contains('/') || path.contains('.')) {
        return None;
    }
    let expanded = match path.strip_prefix("~/") {
        Some(rest) => std::env::var("HOME").map(|h| format!("{h}/{rest}")).unwrap_or_else(|_| path.to_string()),
        None => path.to_string(),
    };
    let candidates: Vec<std::path::PathBuf> = if expanded.starts_with('/') {
        vec![expanded.clone().into()]
    } else {
        dirs.iter().filter(|d| !d.is_empty()).map(|d| std::path::Path::new(d).join(&expanded)).collect()
    };
    let found = candidates.into_iter().find(|p| p.exists())?;
    Some(Link::File { path: found.to_string_lossy().into_owned(), line, column })
}

/// The terminal a word names: `tabs` maps each key (a terminal id, an agent's conversation id)
/// to its terminal. The word, optionally `midna://session/<key>`, must be 8+ hex digits (dashes
/// allowed, as in a UUID) equal to a key or the start of one, and name one terminal only. A bare
/// word also needs a digit, so hex-looking words (`deadbeef`, `accepted`) stay text.
pub fn tab_ref(word: &str, tabs: &[(String, String)]) -> Option<String> {
    let explicit = word.strip_prefix(SESSION_LINK);
    let w = explicit.unwrap_or(word).trim_end_matches('/').to_ascii_lowercase();
    if w.len() < 8 || !w.chars().all(|c| c.is_ascii_hexdigit() || c == '-') || (explicit.is_none() && !w.chars().any(|c| c.is_ascii_digit())) {
        return None;
    }
    let mut hits = tabs.iter().filter(|(k, _)| k.to_ascii_lowercase().starts_with(&w)).map(|(_, id)| id);
    let id = hits.next()?;
    hits.all(|o| o == id).then(|| id.clone())
}

fn trim_url(s: &str) -> &str {
    let mut end = s.len();
    loop {
        let t = &s[..end];
        let Some(last) = t.chars().last() else { break };
        let strip = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' | '>' | ']' | '}' => true,
            ')' => t.matches('(').count() < t.matches(')').count(),
            _ => false,
        };
        if !strip {
            break;
        }
        end -= last.len_utf8();
    }
    &s[..end]
}

/// Claude Code wraps its text itself, at words, `cols` cells wide: it ends the row with a line
/// break and drops the space it broke at, then indents the rest under the text it continues.
/// Copied as-is, a long command comes out in pieces. This joins `text` (one line per screen
/// row, its first row starting at `first_col`) where a row ends in such a wrap: when the next
/// row's first word wouldn't have fit after it. A word longer than a whole row (a path, a URL)
/// is cut at the edge instead, so a full row joins the next with no space when the two halves
/// of the word that meets there are longer than a row. Rows of box drawing (tables, rules) and
/// rows starting a list item or a new message are never joined.
pub fn rejoin_wrapped(text: &str, first_col: usize, cols: usize) -> String {
    let width = |s: &str| s.chars().map(|c| libghostty_vt::unicode::codepoint_width(c) as usize).sum::<usize>();
    let boxy = |s: &str| s.chars().any(|c| ('\u{2500}'..='\u{259F}').contains(&c));
    let starts_item = |s: &str| {
        let digits = s.chars().take_while(char::is_ascii_digit).count();
        ["● ", "⏺ ", "⎿ ", "- ", "* ", "• ", "❯ ", "> "].iter().any(|m| s.starts_with(m)) || (digits > 0 && s[digits..].starts_with(". "))
    };
    let mut lines = text.split('\n');
    let Some(first) = lines.next() else { return String::new() };
    let mut out = first.to_string();
    let (mut prev, mut end) = (first, first_col + width(first));
    for line in lines {
        let body = line.trim_start_matches(' ');
        // A row of soft-wrapped rows (joined already) ends on its last row.
        let row_end = if end > cols && cols > 0 { (end - 1) % cols + 1 } else { end };
        let word = width(body.split(' ').next().unwrap_or(""));
        let last = width(prev.rsplit(' ').next().unwrap_or(""));
        let joins = cols > 0 && !body.is_empty() && !prev.trim().is_empty() && !boxy(prev) && !boxy(body) && !starts_item(body);
        let sep = match () {
            _ if !joins => None,
            _ if row_end >= cols => Some(if last + word > cols { "" } else { " " }),
            _ if row_end + 1 + word > cols => Some(" "),
            _ => None,
        };
        match sep {
            Some(sep) => {
                out.push_str(sep);
                out.push_str(body);
            }
            None => {
                out.push('\n');
                out.push_str(line);
            }
        }
        (prev, end) = (line, width(line));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use midna_proto::frame::KeyMsg;

    fn k(e: &mut Engine, key: &str, mods: u8, text: &str) -> String {
        let out = e.key(&KeyMsg { action: KeyAction::Press, mods, key: key.into(), text: text.into() });
        String::from_utf8_lossy(&out).into_owned()
    }

    fn eng(setup: &[u8]) -> Engine {
        let mut e = Engine::new(80, 24, None);
        e.feed(setup, 1);
        e
    }

    #[test]
    fn legacy_keys() {
        let mut e = eng(b"");
        assert_eq!(k(&mut e, "a", 0, "a"), "a");
        assert_eq!(k(&mut e, "a", MOD_SHIFT, "A"), "A");
        assert_eq!(k(&mut e, "!", 0, "!"), "!");
        assert_eq!(k(&mut e, "c", MOD_CTRL, ""), "\x03");
        assert_eq!(k(&mut e, "b", MOD_ALT, "∫"), "\x1bb");
        assert_eq!(k(&mut e, "enter", 0, "\n"), "\r");
        assert_eq!(k(&mut e, "enter", MOD_SHIFT, "\n"), "\x1b\r");
        assert_eq!(k(&mut e, "tab", 0, "\t"), "\t");
        assert_eq!(k(&mut e, "tab", MOD_SHIFT, ""), "\x1b[Z");
        assert_eq!(k(&mut e, "backspace", 0, ""), "\x7f");
        assert_eq!(k(&mut e, "escape", 0, ""), "\x1b");
        assert_eq!(k(&mut e, "up", 0, ""), "\x1b[A");
        assert_eq!(k(&mut e, "up", MOD_SHIFT, ""), "\x1b[1;2A");
        assert_eq!(k(&mut e, "left", MOD_CTRL, ""), "\x1b[1;5D");
        assert_eq!(k(&mut e, "right", MOD_ALT, ""), "\x1b[1;3C");
        assert_eq!(k(&mut e, "home", 0, ""), "\x1b[H");
        assert_eq!(k(&mut e, "end", 0, ""), "\x1b[F");
        assert_eq!(k(&mut e, "pageup", 0, ""), "\x1b[5~");
        assert_eq!(k(&mut e, "pagedown", MOD_CTRL, ""), "\x1b[6;5~");
        assert_eq!(k(&mut e, "delete", 0, ""), "\x1b[3~");
        assert_eq!(k(&mut e, "f1", 0, ""), "\x1bOP");
        assert_eq!(k(&mut e, "f5", 0, ""), "\x1b[15~");
        assert_eq!(k(&mut e, "f12", MOD_SHIFT, ""), "\x1b[24;2~");
        assert_eq!(k(&mut e, "space", MOD_CTRL, " "), "\0");
        assert_eq!(k(&mut e, "space", 0, " "), " ");
        // Another layout's character on a base key types that character.
        assert_eq!(k(&mut e, "2", 0, "é"), "é");
        assert_eq!(k(&mut e, "e", 0, "é"), "é");
    }

    #[test]
    fn application_cursor_keys() {
        let mut e = eng(b"\x1b[?1h");
        assert_eq!(k(&mut e, "up", 0, ""), "\x1bOA");
        assert_eq!(k(&mut e, "up", MOD_CTRL, ""), "\x1b[1;5A");
    }

    #[test]
    fn kitty_disambiguate() {
        // CSI > 1 u: disambiguate escape codes (kitty spec examples).
        let mut e = eng(b"\x1b[>1u");
        assert_eq!(e.kitty_flags(), 1);
        assert_eq!(k(&mut e, "a", 0, "a"), "a");
        assert_eq!(k(&mut e, "a", MOD_SHIFT, "A"), "A");
        assert_eq!(k(&mut e, "a", MOD_CTRL, ""), "\x1b[97;5u");
        assert_eq!(k(&mut e, "a", MOD_ALT, "å"), "\x1b[97;3u");
        assert_eq!(k(&mut e, "a", MOD_CTRL | MOD_SHIFT, ""), "\x1b[97;6u");
        assert_eq!(k(&mut e, "escape", 0, ""), "\x1b[27u");
        assert_eq!(k(&mut e, "enter", 0, "\n"), "\r");
        assert_eq!(k(&mut e, "enter", MOD_SHIFT, "\n"), "\x1b[13;2u");
        assert_eq!(k(&mut e, "tab", MOD_SHIFT, ""), "\x1b[9;2u");
        assert_eq!(k(&mut e, "backspace", MOD_CTRL, ""), "\x1b[127;5u");
        assert_eq!(k(&mut e, "up", 0, ""), "\x1b[A");
        assert_eq!(k(&mut e, "f5", 0, ""), "\x1b[15~");
    }

    #[test]
    fn kitty_report_all_and_events() {
        // CSI > 1|8 u: every key as an escape code.
        let mut e = eng(b"\x1b[>9u");
        assert_eq!(k(&mut e, "a", 0, "a"), "\x1b[97u");
        assert_eq!(k(&mut e, "a", MOD_SHIFT, "A"), "\x1b[97;2u");
        assert_eq!(k(&mut e, "enter", 0, "\n"), "\x1b[13u");
        assert_eq!(k(&mut e, "1", 0, "1"), "\x1b[49u");
        // CSI > 1|2 u: releases are reported with event type 3.
        let mut e = eng(b"\x1b[>3u");
        let out = e.key(&KeyMsg { action: KeyAction::Release, mods: MOD_CTRL, key: "a".into(), text: String::new() });
        assert_eq!(String::from_utf8_lossy(&out), "\x1b[97;5:3u");
        let out = e.key(&KeyMsg { action: KeyAction::Repeat, mods: MOD_CTRL, key: "a".into(), text: String::new() });
        assert_eq!(String::from_utf8_lossy(&out), "\x1b[97;5:2u");
        // Popping the flags returns to legacy keys.
        e.feed(b"\x1b[<u", 1);
        assert_eq!(k(&mut e, "a", MOD_CTRL, ""), "\x01");
    }

    #[test]
    fn modify_other_keys() {
        let mut e = eng(b"\x1b[>4;2m");
        assert_eq!(k(&mut e, "a", MOD_CTRL | MOD_SHIFT, ""), "\x1b[27;6;65~");
    }

    #[test]
    fn paste_and_focus() {
        let mut e = eng(b"");
        assert_eq!(e.paste("a\nb\x1bc"), b"a\rb c");
        assert!(e.focus(true).is_empty());
        e.feed(b"\x1b[?2004h\x1b[?1004h", 1);
        assert_eq!(e.paste("a\nb"), b"\x1b[200~a\nb\x1b[201~");
        assert_eq!(e.focus(true), b"\x1b[I");
        assert_eq!(e.focus(false), b"\x1b[O");
    }

    #[test]
    fn frames_mark_the_prompt_and_the_input() {
        // OSC 133: A starts the prompt, B the input; C (the command runs) its output.
        let mut e = eng(b"out\r\n\x1b]133;A\x07$ \x1b]133;B\x07hello\r\nworld");
        let f = e.frame().unwrap();
        let row = |y: u16| f.changed.iter().find(|(r, _)| *r == y).map(|(_, r)| r.clone()).unwrap();
        let input = |y: u16| row(y).cells.iter().map(|c| if c.flags & midna_proto::frame::F_INPUT != 0 { 'i' } else { '.' }).take(8).collect::<String>();
        assert!(!row(0).prompt && row(1).prompt && !row(2).prompt);
        assert_eq!(input(0), "........");
        assert_eq!(input(1), "..iiiii.");
        assert_eq!(input(2), "iiiii...");
        e.feed(b"x\x1b]133;C\x07\r\nran", 1);
        let f = e.frame().unwrap();
        let out = f.changed.iter().find(|(r, _)| *r == 3).map(|(_, r)| r.cells[0].flags & midna_proto::frame::F_INPUT).unwrap();
        assert_eq!(out, 0, "output isn't input");
        let mut w = Engine::new(10, 4, None);
        w.feed(b"0123456789abc", 1);
        let f = w.frame().unwrap();
        assert!(f.changed[0].1.wrapped && !f.changed[1].1.wrapped);
    }

    #[test]
    fn modes_an_agent_left_on_are_reset() {
        let mut e = eng(b"\x1b[?1002h\x1b[?1006h\x1b[?1004h\x1b[>1u");
        assert!(e.mouse_tracking());
        e.reset_app_modes();
        assert!(!e.mouse_tracking(), "clicks select and move the shell's cursor again");
        assert!(e.focus(true).is_empty());
        assert_eq!(e.kitty_flags(), 0);
    }

    #[test]
    fn mouse_reporting_sgr() {
        let mut e = eng(b"\x1b[?1000h\x1b[?1006h");
        let m = |action, button, x, y| MouseMsg { action, button, mods: 0, x, y };
        assert_eq!(e.mouse(&m(MouseAction::Press, 1, 4.5, 2.2)), b"\x1b[<0;5;3M");
        assert_eq!(e.mouse(&m(MouseAction::Release, 1, 4.5, 2.2)), b"\x1b[<0;5;3m");
        let s = ScrollMsg { kind: ScrollKind::Wheel, amount: -2, x: 0.0, y: 0.0, mods: 0 };
        assert_eq!(e.scroll(&s), b"\x1b[<64;1;1M\x1b[<64;1;1M");
        // No selection while the app owns the mouse.
        assert!(e.selection_text().is_none());
    }

    #[test]
    fn scrollback_viewport_and_snap_back() {
        let mut e = Engine::new(40, 5, None);
        for i in 0..30 {
            e.feed(format!("line {i}\r\n").as_bytes(), 1);
        }
        let f = e.frame().unwrap();
        assert!(f.ext.at_bottom());
        e.scroll(&ScrollMsg { kind: ScrollKind::Wheel, amount: -10, x: 0.0, y: 0.0, mods: 0 });
        let f = e.frame().unwrap();
        assert!(!f.ext.at_bottom());
        assert_eq!(f.changed[0].1.text().trim_end(), "line 16");
        // More output keeps the viewport where it is.
        e.feed(b"more\r\n", 1);
        let f = e.frame();
        assert!(f.is_none_or(|f| !f.ext.at_bottom()));
        // Typing snaps back.
        assert_eq!(k(&mut e, "x", 0, "x"), "x");
        let f = e.frame().unwrap();
        assert!(f.ext.at_bottom());
        e.scroll(&ScrollMsg { kind: ScrollKind::Top, amount: 0, x: 0.0, y: 0.0, mods: 0 });
        let f = e.frame().unwrap();
        assert_eq!(f.ext.scroll_offset, 0);
        assert_eq!(f.changed[0].1.text().trim_end(), "line 0");
    }

    #[test]
    fn alt_screen_wheel_sends_arrows() {
        let mut e = eng(b"\x1b[?1049h");
        let out = e.scroll(&ScrollMsg { kind: ScrollKind::Wheel, amount: 3, x: 0.0, y: 0.0, mods: 0 });
        assert_eq!(out, b"\x1b[B\x1b[B\x1b[B");
    }

    #[test]
    fn rejoin_claudes_wrapped_rows() {
        // 30 columns. Claude dropped the space at each wrap and indented the rest by 2.
        let cols = 30;
        let text = "● for s in d6d9537d 7da865c9\n  bf1c2198; do midna send $s\n  -- \"$MSG\"; done";
        assert_eq!(rejoin_wrapped(text, 0, cols), "● for s in d6d9537d 7da865c9 bf1c2198; do midna send $s -- \"$MSG\"; done");
        // A selection starting mid-row ends that row at its column, not at its length.
        assert_eq!(rejoin_wrapped("7da865c9\n  bf1c2198; do", 20, cols), "7da865c9 bf1c2198; do");
        assert_eq!(rejoin_wrapped("7da865c9\n  bf1c2198; do", 0, cols), "7da865c9\n  bf1c2198; do");
        // Short lines are lines: the next word would have fit.
        let code = "fn main() {\n    println!(\"hi\");\n}";
        assert_eq!(rejoin_wrapped(code, 0, cols), code);
        // A full row: a word longer than a row is cut mid-word; a word that ends at the edge isn't.
        let url = "  see https://example.com/abcd\n  efgh/ijkl now";
        assert_eq!(rejoin_wrapped(url, 0, cols), "  see https://example.com/abcdefgh/ijkl now");
        assert_eq!(rejoin_wrapped("  the quick brown fox jumps ov\n  the dog", 0, cols), "  the quick brown fox jumps ov the dog");
        // Wide characters take two cells.
        assert_eq!(rejoin_wrapped("  世界世界世界世界世界世界 abc\n  defgh", 0, cols), "  世界世界世界世界世界世界 abc defgh");
        // Blank rows, list items, new messages and tables stay apart.
        for keep in [
            "  a sentence that runs right up\n\n  next",
            "  1. a step that runs right up to\n  2. the next step",
            "  - a point that runs right up to\n  - the next point",
            "  a reply that runs right up to t\n● the next message",
            "┌────────────────────────────┐\n│ cell                        │",
        ] {
            assert_eq!(rejoin_wrapped(keep, 0, cols), keep);
        }
    }

    #[test]
    fn agent_selection_joins_claudes_wraps() {
        let mut e = Engine::new(30, 6, None);
        e.feed("● for s in d6d9537d 7da865c9\r\n  bf1c2198; do midna send $s\r\n\r\nafter".as_bytes(), 1);
        let m = |action, x, y| MouseMsg { action, button: 1, mods: 0, x, y };
        // Drag from "for" to the end of the second row.
        e.mouse(&m(MouseAction::Press, 2.1, 0.5));
        e.mouse(&m(MouseAction::Motion, 29.9, 1.5));
        e.mouse(&m(MouseAction::Release, 29.9, 1.5));
        assert_eq!(e.selection_text().as_deref(), Some("for s in d6d9537d 7da865c9\n  bf1c2198; do midna send $s"));
        assert_eq!(e.agent_selection_text().as_deref(), Some("for s in d6d9537d 7da865c9 bf1c2198; do midna send $s"));
        assert_eq!(e.sel_col, Some(2));
        // Dragged backwards, the selection still starts where its text does.
        e.mouse(&m(MouseAction::Press, 29.9, 1.5));
        e.mouse(&m(MouseAction::Motion, 2.1, 0.5));
        e.mouse(&m(MouseAction::Release, 2.1, 0.5));
        assert_eq!(e.sel_col, Some(2));
        assert_eq!(e.agent_selection_text().as_deref(), Some("for s in d6d9537d 7da865c9 bf1c2198; do midna send $s"));
        // A selection starting at column 21 ends its row at 29, where the next word wouldn't fit.
        e.feed(b"\x1b[2J\x1b[Haaaaaaaaaaaaaaaaaaaa 7da865c9\r\n  bf1c2198; do", 2);
        e.mouse(&m(MouseAction::Press, 21.1, 0.5));
        e.mouse(&m(MouseAction::Motion, 13.9, 1.5));
        e.mouse(&m(MouseAction::Release, 13.9, 1.5));
        assert_eq!(e.sel_col, Some(21));
        assert_eq!(e.agent_selection_text().as_deref(), Some("7da865c9 bf1c2198; do"));
        // A rectangle (alt-drag) is copied as it is.
        let alt = |action, x, y| MouseMsg { action, button: 1, mods: MOD_ALT, x, y };
        e.mouse(&alt(MouseAction::Press, 0.1, 0.5));
        e.mouse(&alt(MouseAction::Motion, 29.9, 1.5));
        e.mouse(&alt(MouseAction::Release, 29.9, 1.5));
        assert_eq!(e.sel_col, None);
        assert_eq!(e.agent_selection_text(), e.selection_text());
    }

    #[test]
    fn selection_drag_word_line_and_copy() {
        let mut e = Engine::new(40, 6, None);
        e.feed("hello wide 世界 world\r\nsecond line here".as_bytes(), 1);
        let m = |action, x, y, mods| MouseMsg { action, button: 1, mods, x, y };
        // Drag from "wide" to the end of 世界.
        e.mouse(&m(MouseAction::Press, 6.1, 0.5, 0));
        e.mouse(&m(MouseAction::Motion, 13.9, 0.5, 0));
        e.mouse(&m(MouseAction::Release, 13.9, 0.5, 0));
        assert_eq!(e.selection_text().as_deref(), Some("wide 世界"));
        let f = e.frame().unwrap();
        assert_eq!(f.ext.selection, vec![(0, 6, 13)]); // ends on the wide head; the client widens it
        // A plain click clears it.
        e.mouse(&m(MouseAction::Press, 2.0, 1.5, 0));
        e.mouse(&m(MouseAction::Release, 2.0, 1.5, 0));
        assert_eq!(e.selection_text(), None);
        // Double click: a word (presses ~immediately after each other).
        e.mouse(&m(MouseAction::Press, 8.0, 1.5, 0));
        e.mouse(&m(MouseAction::Release, 8.0, 1.5, 0));
        e.mouse(&m(MouseAction::Press, 8.0, 1.5, 0));
        e.mouse(&m(MouseAction::Release, 8.0, 1.5, 0));
        assert_eq!(e.selection_text().as_deref(), Some("line"));
        // Triple click: the line.
        e.mouse(&m(MouseAction::Press, 8.0, 1.5, 0));
        e.mouse(&m(MouseAction::Release, 8.0, 1.5, 0));
        assert_eq!(e.selection_text().as_deref(), Some("second line here"));
        // New output keeps the selection on its text.
        std::thread::sleep(Duration::from_millis(500));
        e.feed(b"\r\nthird\r\nfourth\r\nfifth\r\nsixth\r\nseventh", 1);
        assert_eq!(e.selection_text().as_deref(), Some("second line here"));
        // Typing clears it.
        k(&mut e, "z", 0, "z");
        assert_eq!(e.selection_text(), None);
    }

    /// The alternate screen drawn the way Claude Code's fullscreen mode draws it: every row
    /// repainted in place, transcript lines `from..` on top and a fixed footer below.
    fn repaint(e: &mut Engine, from: usize, rows: usize) {
        let mut out = String::from("\x1b[H");
        for y in 0..rows - 1 {
            out += &format!("\x1b[{};1H\x1b[2Kline {}", y + 1, from + y);
        }
        out += &format!("\x1b[{rows};1H\x1b[2K> prompt");
        e.feed(out.as_bytes(), 1);
    }

    #[test]
    fn selection_follows_text_an_app_repaints() {
        let mut e = Engine::new(30, 8, None);
        e.feed(b"\x1b[?1049h", 1);
        repaint(&mut e, 0, 8);
        let m = |action, x, y| MouseMsg { action, button: 1, mods: 0, x, y };
        // Drag over "line 3" to "line 4".
        e.mouse(&m(MouseAction::Press, 0.1, 3.5));
        e.mouse(&m(MouseAction::Motion, 5.9, 4.5));
        e.mouse(&m(MouseAction::Release, 5.9, 4.5));
        assert_eq!(e.selection_text().as_deref(), Some("line 3\nline 4"));
        e.frame();
        // Two more lines of output: the transcript moves up two rows.
        repaint(&mut e, 2, 8);
        let f = e.frame().unwrap();
        assert_eq!(e.selection_text().as_deref(), Some("line 3\nline 4"));
        assert_eq!(f.ext.selection, vec![(1, 0, 29), (2, 0, 5)]);
        // Half off the top: what's left stays selected.
        repaint(&mut e, 4, 8);
        e.frame();
        assert_eq!(e.selection_text().as_deref(), Some("line 4"));
        // All the way off: no selection on whatever took its place.
        repaint(&mut e, 9, 8);
        e.frame();
        assert_eq!(e.selection_text(), None);
    }

    #[test]
    fn selection_stays_while_the_screen_is_half_redrawn() {
        let mut e = Engine::new(30, 8, None);
        e.feed(b"\x1b[?1049h", 1);
        repaint(&mut e, 0, 8);
        let m = |action, x, y| MouseMsg { action, button: 1, mods: 0, x, y };
        e.mouse(&m(MouseAction::Press, 0.1, 5.5));
        e.mouse(&m(MouseAction::Motion, 5.9, 5.5));
        e.mouse(&m(MouseAction::Release, 5.9, 5.5));
        e.frame();
        // Something else entirely (a dialog) draws over it: left where it was.
        e.feed(b"\x1b[6;1H\x1b[2KAllow this command?", 1);
        e.frame();
        assert_eq!(e.selection_text().as_deref(), Some("Allow"));
        // The transcript comes back, moved up one: the selection finds it.
        repaint(&mut e, 1, 8);
        e.frame();
        assert_eq!(e.selection_text().as_deref(), Some("line 5"));
    }

    #[test]
    fn moved_text() {
        let s = |v: &[&str]| v.iter().map(|r| r.to_string()).collect::<Vec<_>>();
        let old = s(&["a", "b", "c", "d", "", "> x"]);
        assert_eq!(moved(&old, &s(&["c", "d", "e", "f", "", "> x"]), 2, 3), Some(Moved::By(-2)));
        assert_eq!(moved(&old, &s(&["", "a", "b", "c", "d", "> x"]), 1, 2), Some(Moved::By(1)));
        // Blank rows alone prove nothing.
        assert_eq!(moved(&old, &s(&["q", "r", "s", "t", "u", "> x"]), 4, 4), None);
        // Text that's gone with the rest of the screen sliding up past it.
        let old = s(&["a", "b", "c", "d", "e", "f", "g", "> x"]);
        assert_eq!(moved(&old, &s(&["f", "g", "h", "i", "j", "k", "l", "> x"]), 0, 1), Some(Moved::Gone));
    }

    #[test]
    fn rectangle_selection_with_alt() {
        let mut e = Engine::new(20, 4, None);
        e.feed(b"abcdef\r\nghijkl\r\nmnopqr", 1);
        let m = |action, x, y| MouseMsg { action, button: 1, mods: MOD_ALT, x, y };
        e.mouse(&m(MouseAction::Press, 1.2, 0.5));
        e.mouse(&m(MouseAction::Motion, 3.8, 2.5));
        e.mouse(&m(MouseAction::Release, 3.8, 2.5));
        assert_eq!(e.selection_text().as_deref(), Some("bcd\nhij\nnop"));
    }

    #[test]
    fn wrapped_line_copies_as_one() {
        let mut e = Engine::new(10, 4, None);
        e.feed(b"0123456789abcdef", 1);
        e.select_all();
        assert_eq!(e.selection_text().as_deref(), Some("0123456789abcdef"));
    }

    #[test]
    fn links_hard_wrapped_by_the_app() {
        // Claude Code breaks a long URL with a newline and indents the rest.
        let mut e = Engine::new(30, 6, None);
        e.feed(b"1. Open https://a.dev/login?x1\r\n   2&y=true\r\n2. Check https://b.dev/q ok\r\n   and more", 1);
        let full = Some(Link::Url("https://a.dev/login?x12&y=true".into()));
        assert_eq!(e.link_at(12, 0, "/", &[], &[]), full, "from the first piece");
        assert_eq!(e.link_at(5, 1, "/", &[], &[]), full, "from the continuation");
        // A URL that stops short of the edge keeps to its line.
        assert_eq!(e.link_at(12, 2, "/", &[], &[]), Some(Link::Url("https://b.dev/q".into())));
        // Three rows, clicked in the middle.
        let mut e = Engine::new(20, 4, None);
        e.feed(b"https://c.dev/aaaaaa\r\n  bbbbbbbbbbbbbbbbbb\r\n  cc done", 1);
        let full = Some(Link::Url("https://c.dev/aaaaaabbbbbbbbbbbbbbbbbbcc".into()));
        assert_eq!(e.link_at(5, 1, "/", &[], &[]), full);
        assert_eq!(e.link_at(3, 2, "/", &[], &[]), full);
        assert_eq!(e.link_at(6, 2, "/", &[], &[]), None, "the word after the URL isn't part of it");
    }

    #[test]
    fn links() {
        let mut e = Engine::new(60, 4, None);
        e.feed(b"see \x1b]8;;https://example.com/x\x1b\\here\x1b]8;;\x1b\\ or https://midna.dev/a_(b). ok", 1);
        assert_eq!(e.link_at(5, 0, "/", &[], &[]), Some(Link::Url("https://example.com/x".into())));
        assert_eq!(e.link_at(20, 0, "/", &[], &[]), Some(Link::Url("https://midna.dev/a_(b)".into())));
        assert_eq!(e.link_at(1, 0, "/", &[], &[]), None);
        let dir = std::env::temp_dir().join(format!("midna-link-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {}").unwrap();
        let mut e = Engine::new(40, 4, None);
        e.feed(b"error at src/main.rs:12:5: oops", 1);
        let got = e.link_at(12, 0, dir.to_str().unwrap(), &[], &[]);
        assert_eq!(got, Some(Link::File { path: dir.join("src/main.rs").to_string_lossy().into(), line: Some(12), column: Some(5) }));
        assert_eq!(e.link_at(2, 0, dir.to_str().unwrap(), &[], &[]), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn no_apps(_: &str) -> Option<String> {
        None
    }

    fn taskboard(scheme: &str) -> Option<String> {
        (scheme == "taskboard").then(|| "Taskboard".to_string())
    }

    #[test]
    fn detect_app_links() {
        let at = |s: &str, i: usize| detect_link(&s.chars().collect::<Vec<_>>(), i, &[], &taskboard, &[], &[]);
        let app = |u: &str| Some(Link::App { url: u.into(), app: "Taskboard".into() });
        assert_eq!(at("open taskboard://#/?task=T6 now", 8), app("taskboard://#/?task=T6"));
        // Trailing punctuation and brackets the URL didn't open stay out.
        assert_eq!(at("(see taskboard://#/?task=T6).", 10), app("taskboard://#/?task=T6"));
        assert_eq!(at("taskboard://#/?task=T6, then", 0), app("taskboard://#/?task=T6"));
        // A scheme glued to a word is a different scheme, which no app opens.
        assert_eq!(at("seetaskboard://#/?task=T6", 5), None);
        // Unregistered schemes stay plain text.
        assert_eq!(at("got foo://bar/baz", 6), None);
        assert_eq!(detect_link(&"taskboard://#/?task=T6".chars().collect::<Vec<_>>(), 3, &[], &no_apps, &[], &[]), None);
        // Nothing after the `://`.
        assert_eq!(at("taskboard://", 3), None);
    }

    #[test]
    fn detect_pattern_links() {
        let rules: Vec<String> = [r"\b(T\d+)\b = taskboard://task/$1 agents", r"\b(G\d+)\b = taskboard://goal/$1 agents", r"#(\d+) = https://github.com/o/r/issues/$1"].iter().map(|s| s.to_string()).collect();
        let pats = midna_proto::link_patterns::parse_all(&rules);
        let at = |s: &str, i: usize, apps: &dyn Fn(&str) -> Option<String>| detect_link(&s.chars().collect::<Vec<_>>(), i, &[], apps, &[], &pats);
        let app = |u: &str| Some(Link::App { url: u.into(), app: "Taskboard".into() });
        assert_eq!(at("Took T42.", 6, &taskboard), app("taskboard://task/T42"));
        assert_eq!(at("(G3)", 1, &taskboard), app("taskboard://goal/G3"));
        assert_eq!(at("fixes #7", 7, &taskboard), Some(Link::Url("https://github.com/o/r/issues/7".into())));
        // A real URL wins over a ref inside it, and refs in paths stay text.
        assert_eq!(at("taskboard://task/T42", 18, &taskboard), app("taskboard://task/T42"));
        assert_eq!(at("https://x.dev/T42", 15, &taskboard), Some(Link::Url("https://x.dev/T42".into())));
        assert_eq!(at("src/T42.rs", 5, &taskboard), None);
        // No app opens the scheme: plain text.
        assert_eq!(at("Took T42.", 6, &no_apps), None);
        // A rule with a preview command carries it, filled from the captures.
        let rules: Vec<String> = [r#"\b(RRD-\d+)\b = https://x.atlassian.net/browse/$1 preview:"jira-card $1""#, r"\b(T\d+)\b = taskboard://t/$1 preview:tb-card"].iter().map(|s| s.to_string()).collect();
        let pats = midna_proto::link_patterns::parse_all(&rules);
        let at = |s: &str, i: usize, apps: &dyn Fn(&str) -> Option<String>| detect_link(&s.chars().collect::<Vec<_>>(), i, &[], apps, &[], &pats);
        let jira = Link::Preview { url: "https://x.atlassian.net/browse/RRD-7".into(), app: None, preview: vec!["jira-card".into(), "RRD-7".into()] };
        assert_eq!(at("on RRD-7 now", 4, &no_apps), Some(jira));
        let task = Link::Preview { url: "taskboard://t/T4".into(), app: Some("Taskboard".into()), preview: vec!["tb-card".into()] };
        assert_eq!(at("T4", 0, &taskboard), Some(task));
        assert_eq!(at("T4", 0, &no_apps), None);
    }

    #[test]
    fn detect_url_in_wrapped_text() {
        let chars: Vec<char> = "(https://a.b/c?d=1).".chars().collect();
        assert_eq!(detect_link(&chars, 5, &[], &no_apps, &[], &[]), Some(Link::Url("https://a.b/c?d=1".into())));
    }

    #[test]
    fn tab_links() {
        let tabs: Vec<(String, String)> = [("019da528", "019da528"), ("e0af734f", "e0af734f"), ("3f2a9c01-77d4-4b1e-9a0b-5c6d7e8f9a0b", "e0af734f")]
            .iter()
            .map(|(k, id)| (k.to_string(), id.to_string()))
            .collect();
        let sess = |s: &str| Some(s.to_string());
        assert_eq!(tab_ref("019da528", &tabs), sess("019da528"));
        assert_eq!(tab_ref("019DA528", &tabs), sess("019da528"));
        assert_eq!(tab_ref("midna://session/e0af734f", &tabs), sess("e0af734f"));
        // A conversation id, whole or its first 8+, names its terminal.
        assert_eq!(tab_ref("3f2a9c01", &tabs), sess("e0af734f"));
        assert_eq!(tab_ref("3f2a9c01-77d4-4b1e-9a0b-5c6d7e8f9a0b", &tabs), sess("e0af734f"));
        // Too short, not hex, or no live terminal: plain text.
        assert_eq!(tab_ref("019da5", &tabs), None);
        assert_eq!(tab_ref("deadbeef", &tabs), None);
        assert_eq!(tab_ref("12345678", &tabs), None);
        let ambiguous = vec![("aaaa1111x".into(), "s1".into()), ("aaaa1111y".into(), "s2".into())];
        assert_eq!(tab_ref("aaaa1111", &ambiguous), None);

        let mut e = Engine::new(80, 4, None);
        e.feed(b"Your \"Taskboard\" tab (019da528) is busy; see \x1b]8;;midna://session/e0af734f\x1b\\this tab\x1b]8;;\x1b\\ ok", 1);
        assert_eq!(e.link_at(25, 0, "/", &tabs, &[]), Some(Link::Session("019da528".into())));
        assert_eq!(e.link_at(47, 0, "/", &tabs, &[]), Some(Link::Session("e0af734f".into())));
        assert_eq!(e.link_at(25, 0, "/", &[], &[]), None);
    }

    #[test]
    fn find_moves_viewport_and_selects() {
        let mut e = Engine::new(40, 5, None);
        for i in 0..40 {
            e.feed(format!("row {i}{}\r\n", if i % 10 == 3 { " needle" } else { "" }).as_bytes(), 1);
        }
        let h = e.find("needle", true);
        assert_eq!((h.total, h.index), (4, 4));
        assert_eq!(e.selection_text().as_deref(), Some("needle"));
        let h = e.find("needle", true);
        assert_eq!(h.index, 3);
        let f = e.frame().unwrap();
        assert!(!f.ext.at_bottom());
        assert_eq!(f.ext.selection.len(), 1);
        let row = f.ext.selection[0].0 as usize;
        assert!(f.changed[row].1.text().starts_with("row 23 needle"));
        let h = e.find("NEEDLE", false);
        assert_eq!(h.total, 0);
        assert_eq!(e.find("", false), FindHit::default());
    }

    #[test]
    fn key_names() {
        assert_eq!(key_from_name("A"), Some((Key::A, Some('a'), true)));
        assert_eq!(key_from_name("?"), Some((Key::Slash, Some('/'), true)));
        assert_eq!(key_from_name("f13").map(|k| k.0), Some(Key::F13));
        assert_eq!(key_from_name("nope"), None);
    }
}
