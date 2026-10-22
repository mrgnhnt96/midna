//! A shared single-line text field with IME (`EntityInputHandler`), a cursor, selection and
//! the usual macOS editing keys. Used by the ⌘K palette, the Rules test strip, the Triggers
//! secret field and the Settings Ask box (through `screen_kit::LineInput`).
//!
//! Typing arrives through GPUI's input handler (so dead keys, option characters and IME
//! composition work, with the candidate window anchored at the cursor). The field's own key
//! handler takes the editing keys (arrows, ⌥/⌘ word and line moves, ⇧ selection, ⌫/⌦, ⌘A/C/X/V,
//! ⌘Z/⇧⌘Z undo and redo);
//! everything else (↩, esc, ⇥, ↑/↓ outside `wrap`, ⌘-shortcuts) bubbles to the parent, which decides what
//! submit/cancel mean. Font, size and color are inherited from the parent element.
//!
//! With `wrap` set (the image sheet's note field) the text wraps at the field's width and the
//! field grows a line at a time instead of scrolling sideways. The [`Newline`] action
//! (`keys.note_newline`, ⇧↩ by default) starts a new line there; ↩ stays the parent's. ↑/↓
//! (⇧ to select) move a row there, to the start or end past the first or last row; ⌘↑/↓ still
//! bubble. ⌘←/→ and ⌘⌫ stop at the drawn row's ends. Rows break after spaces, never before
//! punctuation, and [`wrap_text`] draws text with the same rows when the field isn't on it.
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use std::ops::Range;

actions!(
    midna,
    [
        /// Start a new line in a wrapping field (`keys.note_newline`).
        Newline,
    ]
);

/// Key context of a wrapping field, where [`Newline`] is bound.
pub const CTX_WRAP: &str = "TextFieldWrap";

/// Emitted on every content change.
pub struct FieldChanged;

pub struct TextField {
    pub focus: FocusHandle,
    content: String,
    /// Byte range into `content`.
    selected: Range<usize>,
    reversed: bool,
    /// The cursor sits at the end of a wrapped row rather than the start of the next one (the
    /// same offset): ⌘→, or a click past a row's end.
    eol: bool,
    marked: Option<Range<usize>>,
    pub placeholder: SharedString,
    /// Draw bullets instead of the text (and never copy it out).
    pub secret: bool,
    /// Wrap at the field's width (growing taller) instead of scrolling sideways, and keep
    /// newlines (⇧↩, pastes).
    pub wrap: bool,
    /// Take only the digits 0–9 from typing, pastes and IME commits.
    pub digits: bool,
    last_layout: Option<Shaped>,
    last_line_h: Pixels,
    last_bounds: Option<Bounds<Pixels>>,
    selecting: bool,
    /// Undo/redo (never kept for a secret field).
    history: History,
}

impl EventEmitter<FieldChanged> for TextField {}

impl TextField {
    pub fn new(cx: &mut Context<Self>, secret: bool, placeholder: impl Into<SharedString>) -> TextField {
        TextField {
            focus: cx.focus_handle(),
            content: String::new(),
            selected: 0..0,
            reversed: false,
            eol: false,
            marked: None,
            placeholder: placeholder.into(),
            secret,
            wrap: false,
            digits: false,
            last_layout: None,
            last_line_h: px(0.),
            last_bounds: None,
            selecting: false,
            history: History::default(),
        }
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    /// Replace the whole text (cursor at the end). Starts a fresh undo history: the caller is
    /// loading something new (the image sheet's field moving to another note).
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.history = History::default();
        if self.content == text {
            return;
        }
        self.wipe();
        self.content = text.to_string();
        self.selected = self.content.len()..self.content.len();
        self.eol = false;
        self.marked = None;
        cx.emit(FieldChanged);
        cx.notify();
    }

    /// Select the whole text (typing replaces it).
    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        self.selected = 0..self.content.len();
        self.reversed = false;
        self.eol = false;
        cx.notify();
    }

    /// Clear, overwriting the old buffer first so a secret doesn't linger in memory.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.history = History::default();
        if self.content.is_empty() {
            return;
        }
        self.wipe();
        self.selected = 0..0;
        self.eol = false;
        self.marked = None;
        cx.emit(FieldChanged);
        cx.notify();
    }

    fn wipe(&mut self) {
        let n = self.content.len();
        self.content.clear();
        self.content.extend(std::iter::repeat_n('\0', n));
        self.content.clear();
    }

    fn cursor(&self) -> usize {
        if self.reversed { self.selected.start } else { self.selected.end }
    }

    fn move_to(&mut self, off: usize, cx: &mut Context<Self>) {
        self.selected = off..off;
        self.reversed = false;
        self.eol = false;
        cx.notify();
    }

    fn select_to(&mut self, off: usize, cx: &mut Context<Self>) {
        self.eol = false;
        if self.reversed {
            self.selected.start = off;
        } else {
            self.selected.end = off;
        }
        if self.selected.end < self.selected.start {
            self.reversed = !self.reversed;
            self.selected = self.selected.end..self.selected.start;
        }
        cx.notify();
    }

    fn prev_char(&self, off: usize) -> usize {
        self.content[..off].char_indices().next_back().map(|(i, _)| i).unwrap_or(0)
    }

    fn next_char(&self, off: usize) -> usize {
        self.content[off..].chars().next().map(|c| off + c.len_utf8()).unwrap_or(self.content.len())
    }

    /// Start of the word before `off` (skipping spaces first), like ⌥←.
    fn prev_word(&self, off: usize) -> usize {
        word_left(&self.content, off)
    }

    fn next_word(&self, off: usize) -> usize {
        word_right(&self.content, off)
    }

    /// The offset one drawn row above (`up`) or below `off` at the same x, from the last paint:
    /// the start of the text above the first row, its end below the last. `None` when not wrapping.
    /// With it, whether that's the end of a wrapped row (see `eol`).
    fn vertical(&self, off: usize, up: bool) -> Option<(usize, bool)> {
        let line = self.last_layout.as_ref().filter(|_| self.wrap)?;
        let Shaped::Wrapped(rows) = line else { return None };
        let lh = self.last_line_h;
        let at = line.pos(self.display_offset(off), self.eol && off == self.cursor());
        let y = at.y + lh * 0.5 + if up { -lh } else { lh };
        Some(if y < px(0.) {
            (0, false)
        } else if y >= lh * rows.len() as f32 {
            (self.content.len(), false)
        } else {
            let (i, eol) = line.closest(point(at.x, y), lh);
            (self.content_offset(i), eol)
        })
    }

    /// The start and end of the drawn row the cursor is on, and whether the text wraps on past
    /// that end. The whole text in a field that doesn't wrap.
    fn row_edges(&self) -> (usize, usize, bool) {
        let len = self.content.len();
        let Some(Shaped::Wrapped(rows)) = self.last_layout.as_ref() else { return (0, len, false) };
        if rows.is_empty() || self.content.is_empty() {
            return (0, len, false);
        }
        let r = &rows[Shaped::row_of(rows, self.display_offset(self.cursor()), self.eol)];
        (self.content_offset(r.start), self.content_offset(r.start + r.line.len()), r.soft)
    }

    fn replace(&mut self, range: Range<usize>, text: &str, cx: &mut Context<Self>) {
        let text = if self.digits {
            text.chars().filter(char::is_ascii_digit).collect()
        } else if self.wrap {
            text.replace("\r\n", "\n").replace('\r', "\n")
        } else {
            text.replace(['\r', '\n'], " ")
        };
        // An IME commit replaces its marked text; the history was saved when composing began.
        if !self.secret && self.marked.is_none() {
            self.history.record(&self.content, &self.selected, &range, &text);
        }
        self.content.replace_range(range.clone(), &text);
        let end = range.start + text.len();
        self.selected = end..end;
        self.reversed = false;
        self.eol = false;
        self.marked = None;
        cx.emit(FieldChanged);
        cx.notify();
    }

    /// ⌘Z (`redo` false) or ⇧⌘Z. False when there's nothing to undo or redo.
    fn undo(&mut self, redo: bool, cx: &mut Context<Self>) -> bool {
        let now = Snap { text: self.content.clone(), selected: self.selected.clone() };
        let Some(s) = (if redo { self.history.redo(now) } else { self.history.undo(now) }) else { return false };
        self.content = s.text;
        self.selected = s.selected;
        self.reversed = false;
        self.eol = false;
        self.marked = None;
        cx.emit(FieldChanged);
        cx.notify();
        true
    }

    fn newline(&mut self, _: &Newline, _w: &mut Window, cx: &mut Context<Self>) {
        if self.marked.is_some() {
            return;
        }
        let r = self.selected.clone();
        self.replace(r, "\n", cx);
    }

    fn delete_to(&mut self, off: usize, cx: &mut Context<Self>) {
        if !self.selected.is_empty() {
            let r = self.selected.clone();
            self.replace(r, "", cx);
            return;
        }
        let c = self.cursor();
        let r = if off < c { off..c } else { c..off };
        if !r.is_empty() {
            self.replace(r, "", cx);
        }
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let m = &ks.modifiers;
        // While the IME composes, every key belongs to it.
        if self.marked.is_some() {
            return;
        }
        let c = self.cursor();
        let len = self.content.len();
        // ⌘←/→ go to the ends of the drawn row (the whole text unless wrapping).
        let (row_start, row_end, soft) = self.row_edges();
        let target = |s: &Self, left: bool| -> (usize, bool) {
            match (left, m.platform, m.alt) {
                (true, true, _) => (row_start, false),
                (false, true, _) => (row_end, soft),
                (true, _, true) => (s.prev_word(c), false),
                (false, _, true) => (s.next_word(c), false),
                (true, _, _) => (s.prev_char(c), false),
                (false, _, _) => (s.next_char(c), false),
            }
        };
        match ks.key.as_str() {
            "left" | "right" if !m.control => {
                let left = ks.key == "left";
                if m.shift {
                    let (t, eol) = target(self, left);
                    self.select_to(t, cx);
                    self.eol = eol;
                } else if !self.selected.is_empty() && !m.platform && !m.alt {
                    let edge = if left { self.selected.start } else { self.selected.end };
                    self.move_to(edge, cx);
                } else {
                    let (t, eol) = target(self, left);
                    self.move_to(t, cx);
                    self.eol = eol;
                }
            }
            "up" | "down" if self.wrap && !m.platform && !m.control && !m.alt => {
                let up = ks.key == "up";
                // Without ⇧ a selection collapses from its edge on that side.
                let from = if m.shift || self.selected.is_empty() {
                    c
                } else if up {
                    self.selected.start
                } else {
                    self.selected.end
                };
                let Some((t, eol)) = self.vertical(from, up) else { return };
                if m.shift { self.select_to(t, cx) } else { self.move_to(t, cx) }
                self.eol = eol;
            }
            "home" => self.move_to(0, cx),
            "end" => self.move_to(len, cx),
            "a" if m.control => self.move_to(0, cx),
            "e" if m.control => self.move_to(len, cx),
            "k" if m.control => {
                let r = c..len;
                if !r.is_empty() {
                    self.replace(r, "", cx);
                }
            }
            "backspace" => {
                let to = if m.platform {
                    row_start
                } else if m.alt {
                    self.prev_word(c)
                } else {
                    self.prev_char(c)
                };
                self.delete_to(to, cx);
            }
            "delete" => {
                let to = if m.alt { self.next_word(c) } else { self.next_char(c) };
                self.delete_to(to, cx);
            }
            "a" if m.platform => {
                self.selected = 0..len;
                self.reversed = false;
                self.eol = false;
                cx.notify();
            }
            "c" | "x" if m.platform => {
                if self.secret || self.selected.is_empty() {
                    return; // let the parent's copy (if any) run
                }
                cx.write_to_clipboard(ClipboardItem::new_string(self.content[self.selected.clone()].to_string()));
                if ks.key == "x" {
                    let r = self.selected.clone();
                    self.replace(r, "", cx);
                }
            }
            "z" if m.platform && !m.alt && !m.control => {
                if !self.undo(m.shift, cx) {
                    return;
                }
            }
            "v" if m.platform => {
                if let Some(t) = cx.read_from_clipboard().and_then(|i| i.text()) {
                    let r = self.selected.clone();
                    self.replace(r, t.trim_end_matches(['\r', '\n']), cx);
                }
            }
            _ => return, // not ours: typing goes to the IME handler, the rest to the parent
        }
        let _ = window;
        cx.stop_propagation();
    }

    // ------------------------------------------------------------------ utf16 / display

    fn to_utf16(&self, off: usize) -> usize {
        self.content[..off.min(self.content.len())].encode_utf16().count()
    }

    fn utf16_to_byte(&self, u: usize) -> usize {
        let mut n = 0;
        for (i, ch) in self.content.char_indices() {
            if n >= u {
                return i;
            }
            n += ch.len_utf16();
        }
        self.content.len()
    }

    fn range_to_utf16(&self, r: &Range<usize>) -> Range<usize> {
        self.to_utf16(r.start)..self.to_utf16(r.end)
    }

    fn range_from_utf16(&self, r: &Range<usize>) -> Range<usize> {
        self.utf16_to_byte(r.start)..self.utf16_to_byte(r.end)
    }

    /// What is drawn, and a map from content offsets to drawn offsets.
    fn display(&self) -> String {
        if self.secret { "•".repeat(self.content.chars().count()) } else { self.content.clone() }
    }

    fn display_offset(&self, off: usize) -> usize {
        if self.secret { self.content[..off.min(self.content.len())].chars().count() * '•'.len_utf8() } else { off }
    }

    fn content_offset(&self, disp: usize) -> usize {
        if !self.secret {
            return disp.min(self.content.len());
        }
        let n = disp / '•'.len_utf8();
        self.content.char_indices().nth(n).map(|(i, _)| i).unwrap_or(self.content.len())
    }

    /// The offset under `p`, and whether it's the end of a wrapped row (see `eol`).
    fn index_for_point(&self, p: Point<Pixels>) -> (usize, bool) {
        let (Some(b), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref()) else {
            return (self.content.len(), false);
        };
        if self.content.is_empty() {
            return (0, false);
        }
        let (i, eol) = line.closest(p - b.origin, self.last_line_h);
        (self.content_offset(i), eol)
    }
}

/// The shaped text from the last paint: one line, or rows wrapped at the field's width.
enum Shaped {
    Line(ShapedLine),
    Wrapped(Vec<Row>),
}

/// One drawn row of wrapped text: where it starts in the text and on screen.
pub struct Row {
    start: usize,
    y: Pixels,
    line: ShapedLine,
    /// The paragraph wraps on to the next row (no newline here), so the row's end offset is
    /// also the next row's start.
    soft: bool,
}

impl Shaped {
    /// The row display offset `i` is drawn on: the later one at a wrap unless `eol`.
    fn row_of(rows: &[Row], i: usize, eol: bool) -> usize {
        let k = rows.iter().rposition(|r| r.start <= i).unwrap_or(0);
        if eol && k > 0 && rows[k].start == i && rows[k - 1].soft { k - 1 } else { k }
    }

    /// Top-left of the cursor slot before display offset `i`, relative to the text's origin.
    fn pos(&self, i: usize, eol: bool) -> Point<Pixels> {
        match self {
            Shaped::Line(l) => point(l.x_for_index(i), px(0.)),
            Shaped::Wrapped(rows) if rows.is_empty() => point(px(0.), px(0.)),
            Shaped::Wrapped(rows) => {
                let r = &rows[Self::row_of(rows, i, eol)];
                point(r.line.x_for_index(i - r.start), r.y)
            }
        }
    }

    /// The row under `y` (the first or last one when above or below the text).
    fn row_at(rows: &[Row], y: Pixels, line_h: Pixels) -> Option<&Row> {
        rows.iter().find(|r| y < r.y + line_h).or(rows.last())
    }

    /// The offset nearest `p`, and whether it's the end of a wrapped row.
    fn closest(&self, p: Point<Pixels>, line_h: Pixels) -> (usize, bool) {
        match self {
            Shaped::Line(l) => (l.closest_index_for_x(p.x), false),
            Shaped::Wrapped(rows) => Self::row_at(rows, p.y, line_h).map_or((0, false), |r| {
                let j = r.line.closest_index_for_x(p.x);
                (r.start + j, r.soft && j == r.line.len())
            }),
        }
    }

    fn index(&self, p: Point<Pixels>, line_h: Pixels) -> Option<usize> {
        match self {
            Shaped::Line(l) => l.index_for_x(p.x),
            Shaped::Wrapped(rows) => {
                let r = Self::row_at(rows, p.y, line_h)?;
                r.line.index_for_x(p.x).map(|j| r.start + j)
            }
        }
    }

    fn paint(&self, origin: Point<Pixels>, line_h: Pixels, window: &mut Window, cx: &mut App) {
        match self {
            Shaped::Line(l) => {
                let _ = l.paint(origin, line_h, TextAlign::Left, None, window, cx);
            }
            Shaped::Wrapped(rows) => paint_rows(rows, origin, line_h, window, cx),
        }
    }
}

fn paint_rows(rows: &[Row], origin: Point<Pixels>, line_h: Pixels, window: &mut Window, cx: &mut App) {
    for r in rows {
        let _ = r.line.paint(origin + point(px(0.), r.y), line_h, TextAlign::Left, None, window, cx);
    }
}

/// Where a paragraph's rows end, as byte offsets, when it wraps at `width` and `x` gives the
/// pen position before each offset. Rows break after a run of spaces, so punctuation stays with
/// its word (GPUI's own wrapper breaks before `?` and `/`); a word wider than a row breaks
/// between characters. Spaces at a row's end hang past the edge.
fn row_ends(text: &str, width: Pixels, x: impl Fn(usize) -> Pixels) -> Vec<usize> {
    let ws = |c: char| c.is_whitespace();
    let breaks: Vec<usize> = text.char_indices().filter(|&(i, c)| i > 0 && !ws(c) && text[..i].ends_with(ws)).map(|(i, _)| i).collect();
    let mut ends = Vec::new();
    let mut s = 0;
    loop {
        let x0 = x(s);
        let fits = |e: usize| x(text[..e].trim_end_matches(ws).len().max(s)) - x0 <= width;
        if fits(text.len()) {
            ends.push(text.len());
            return ends;
        }
        let e = breaks.iter().copied().filter(|&b| b > s).take_while(|&b| fits(b)).last().unwrap_or_else(|| {
            // No space to break at: as many characters as fit, at least one.
            let mut chars = text[s..].char_indices().map(|(i, c)| s + i + c.len_utf8());
            let first = chars.next().unwrap_or(text.len());
            chars.take_while(|&e| x(e) - x0 <= width).last().unwrap_or(first)
        });
        ends.push(e);
        s = e;
    }
}

/// The runs covering `r` of the text `runs` cover.
fn slice_runs(runs: &[TextRun], r: Range<usize>) -> Vec<TextRun> {
    let mut out = Vec::new();
    let mut at = 0;
    for run in runs {
        let (a, b) = (at.max(r.start), (at + run.len).min(r.end));
        if a < b {
            out.push(TextRun { len: b - a, ..run.clone() });
        }
        at += run.len;
    }
    if out.is_empty()
        && let Some(run) = runs.first()
    {
        out.push(TextRun { len: 0, ..run.clone() });
    }
    out
}

/// `text` split at its newlines, each paragraph wrapped at `width` (see [`row_ends`]), one row
/// below the last.
fn wrapped(window: &Window, text: &str, size: Pixels, runs: &[TextRun], width: Pixels, line_h: Pixels) -> Vec<Row> {
    let (mut start, mut y) = (0, px(0.));
    let mut out = Vec::new();
    for para in text.split('\n') {
        let end = start + para.len();
        let line = window.text_system().shape_line(SharedString::from(para.to_string()), size, &slice_runs(runs, start..end), None);
        let ends = row_ends(para, width, |i| line.x_for_index(i));
        let mut cur = line.cursor();
        let mut s = 0;
        for (k, &e) in ends.iter().enumerate() {
            out.push(Row { start: start + s, y, line: cur.take_until(e), soft: k + 1 < ends.len() });
            y += line_h;
            s = e;
        }
        start = end + 1;
    }
    out
}

/// The double-clicked word at byte offset `off`: a run of word characters, of spaces, or the
/// one other character there (the one before it at the end of a line).
pub fn word_at(s: &str, off: usize) -> Range<usize> {
    let class = |c: char| if c.is_alphanumeric() || c == '_' { 0 } else if c == ' ' || c == '\t' { 1 } else { 2 };
    let at = s[off..].chars().next().filter(|&c| c != '\n').map(|c| (off, c)).or_else(|| s[..off].char_indices().next_back().filter(|&(_, c)| c != '\n'));
    let Some((i, c)) = at else { return off..off };
    let k = class(c);
    if k == 2 {
        return i..i + c.len_utf8();
    }
    let start = s[..i].char_indices().rev().take_while(|&(_, c)| class(c) == k).last().map_or(i, |(j, _)| j);
    let end = s[i..].char_indices().find(|&(_, c)| class(c) != k).map_or(s.len(), |(j, _)| i + j);
    start..end
}

/// Byte offset of the start of the word left of `off` (spaces skipped first). Right after
/// punctuation (`foo.`, `--`), that run of punctuation is the word, so it always moves.
pub fn word_left(s: &str, off: usize) -> usize {
    let sep = |c: char| "/.-_:=,".contains(c);
    let trimmed = s[..off].trim_end();
    let word = trimmed.trim_end_matches(sep);
    if word.len() < trimmed.len() {
        return word.len();
    }
    trimmed.rfind(|c: char| c.is_whitespace() || sep(c)).map(|i| i + trimmed[i..].chars().next().map_or(1, char::len_utf8)).unwrap_or(0)
}

/// Byte offset of the end of the word right of `off`.
pub fn word_right(s: &str, off: usize) -> usize {
    let after = &s[off..];
    let skip = after.len() - after.trim_start().len();
    let rest = &after[skip..];
    let word =
        rest.find(|c: char| c.is_whitespace() || "/.-_:=,".contains(c)).map(|i| if i == 0 { rest.chars().next().map_or(0, char::len_utf8) } else { i }).unwrap_or(rest.len());
    off + skip + word
}

/// The text and selection before an edit.
#[derive(Clone, Debug, PartialEq)]
struct Snap {
    text: String,
    selected: Range<usize>,
}

/// Undo and redo stacks. A run of typing (or of ⌫/⌦) at one spot is a single step, like
/// macOS text fields; moving the cursor, or switching between typing and deleting, starts a
/// new one.
#[derive(Default)]
struct History {
    undo: Vec<Snap>,
    redo: Vec<Snap>,
    /// The run being extended: whether it inserts, and where its next edit would land.
    run: Option<(bool, usize)>,
}

impl History {
    const MAX: usize = 200;

    /// Called before `range` of `text` is replaced with `new`.
    fn record(&mut self, text: &str, selected: &Range<usize>, range: &Range<usize>, new: &str) {
        let insert = range.is_empty() && !new.is_empty();
        let delete = !range.is_empty() && new.is_empty() && selected.is_empty();
        let continues = match self.run {
            Some((true, at)) => insert && range.start == at,
            Some((false, at)) => delete && (range.end == at || range.start == at),
            None => false,
        };
        if !continues {
            self.undo.push(Snap { text: text.to_string(), selected: selected.clone() });
            if self.undo.len() > Self::MAX {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.run = if insert {
            Some((true, range.start + new.len()))
        } else if delete {
            Some((false, range.start))
        } else {
            None
        };
    }

    fn undo(&mut self, now: Snap) -> Option<Snap> {
        let s = self.undo.pop()?;
        self.redo.push(now);
        self.run = None;
        Some(s)
    }

    fn redo(&mut self, now: Snap) -> Option<Snap> {
        let s = self.redo.pop()?;
        self.undo.push(now);
        self.run = None;
        Some(s)
    }
}

impl EntityInputHandler for TextField {
    fn text_for_range(&mut self, r: Range<usize>, actual: &mut Option<Range<usize>>, _w: &mut Window, _cx: &mut Context<Self>) -> Option<String> {
        if self.secret {
            return None;
        }
        let range = self.range_from_utf16(&r);
        actual.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(&mut self, _ignore: bool, _w: &mut Window, _cx: &mut Context<Self>) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: self.range_to_utf16(&self.selected), reversed: self.reversed })
    }

    fn marked_text_range(&self, _w: &mut Window, _cx: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|r| self.range_to_utf16(r))
    }

    fn unmark_text(&mut self, _w: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        cx.notify();
    }

    fn replace_text_in_range(&mut self, r: Option<Range<usize>>, text: &str, _w: &mut Window, cx: &mut Context<Self>) {
        let range = r.as_ref().map(|r| self.range_from_utf16(r)).or(self.marked.clone()).unwrap_or(self.selected.clone());
        self.replace(range, text, cx);
    }

    fn replace_and_mark_text_in_range(&mut self, r: Option<Range<usize>>, text: &str, sel: Option<Range<usize>>, _w: &mut Window, cx: &mut Context<Self>) {
        let range = r.as_ref().map(|r| self.range_from_utf16(r)).or(self.marked.clone()).unwrap_or(self.selected.clone());
        if self.digits {
            // No composing in a number field: whatever digits come in go straight in.
            self.marked = None;
            return self.replace(range, text, cx);
        }
        if !self.secret && self.marked.is_none() {
            self.history.record(&self.content, &self.selected, &range, "");
        }
        self.content.replace_range(range.clone(), text);
        self.marked = (!text.is_empty()).then(|| range.start..range.start + text.len());
        self.selected = sel
            .as_ref()
            .map(|s| {
                // `sel` is relative to the marked text, in UTF-16.
                let sub = &text.encode_utf16().collect::<Vec<_>>();
                let to8 = |u: usize| String::from_utf16_lossy(&sub[..u.min(sub.len())]).len();
                range.start + to8(s.start)..range.start + to8(s.end)
            })
            .unwrap_or(range.start + text.len()..range.start + text.len());
        self.eol = false;
        cx.emit(FieldChanged);
        cx.notify();
    }

    fn bounds_for_range(&mut self, r: Range<usize>, b: Bounds<Pixels>, _w: &mut Window, _cx: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        let line = self.last_layout.as_ref()?;
        let range = self.range_from_utf16(&r);
        let h = self.last_line_h;
        let p0 = line.pos(self.display_offset(range.start), false);
        let p1 = line.pos(self.display_offset(range.end), false);
        let x1 = if p1.y == p0.y { p1.x } else { p0.x };
        Some(Bounds::from_corners(point(b.left() + p0.x, b.top() + p0.y), point(b.left() + x1, b.top() + p0.y + h)))
    }

    fn character_index_for_point(&mut self, p: Point<Pixels>, _w: &mut Window, _cx: &mut Context<Self>) -> Option<usize> {
        let b = self.last_bounds?;
        let line = self.last_layout.as_ref()?;
        let i = line.index(p - b.origin, self.last_line_h)?;
        Some(self.to_utf16(self.content_offset(i)))
    }
}

impl Focusable for TextField {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TextField {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("text-field")
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .track_focus(&self.focus)
            .when(self.wrap, |d| d.key_context(CTX_WRAP).on_action(cx.listener(Self::newline)))
            .cursor(CursorStyle::IBeam)
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|f, ev: &MouseDownEvent, window, cx| {
                    f.focus.focus(window, cx);
                    f.selecting = true;
                    let (i, eol) = f.index_for_point(ev.position);
                    if ev.modifiers.shift {
                        f.select_to(i, cx);
                        f.eol = eol;
                    } else if ev.click_count == 2 && !f.secret {
                        f.selected = word_at(&f.content, i);
                        f.reversed = false;
                        f.eol = false;
                        cx.notify();
                    } else if ev.click_count >= 2 {
                        f.selected = 0..f.content.len();
                        f.reversed = false;
                        f.eol = false;
                        cx.notify();
                    } else {
                        f.move_to(i, cx);
                        f.eol = eol;
                    }
                }),
            )
            .on_mouse_up(MouseButton::Left, cx.listener(|f, _: &MouseUpEvent, _, _| f.selecting = false))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|f, _: &MouseUpEvent, _, _| f.selecting = false))
            .on_mouse_move(cx.listener(|f, ev: &MouseMoveEvent, _, cx| {
                if f.selecting {
                    let (i, eol) = f.index_for_point(ev.position);
                    f.select_to(i, cx);
                    f.eol = eol;
                }
            }))
            .child(TextLine { field: cx.entity() })
    }
}

/// The painted text: text (or placeholder), IME underline, selection and cursor.
struct TextLine {
    field: Entity<TextField>,
}

struct LinePrepaint {
    line: Option<Shaped>,
    cursor: Option<PaintQuad>,
    selection: Vec<PaintQuad>,
    scroll: Pixels,
    line_h: Pixels,
}

impl IntoElement for TextLine {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl TextLine {
    /// What to shape: the shown text (or the placeholder) and its runs, with the IME's marked
    /// text underlined.
    fn text_and_runs(&self, window: &Window, cx: &App) -> (String, Vec<TextRun>) {
        let theme = cx.global::<Theme>();
        let f = self.field.read(cx);
        let style = window.text_style();
        let shown = f.display();
        let (text, color) = if shown.is_empty() { (f.placeholder.to_string(), theme.dim) } else { (shown, style.color) };
        let run = TextRun { len: text.len(), font: style.font(), color, background_color: None, underline: None, strikethrough: None };
        let runs = match f.marked.as_ref().filter(|_| !f.content.is_empty()) {
            Some(m) => {
                let (a, b) = (f.display_offset(m.start), f.display_offset(m.end));
                let ul = Some(UnderlineStyle { color: Some(color), thickness: px(1.), wavy: false });
                vec![TextRun { len: a, ..run.clone() }, TextRun { len: b - a, underline: ul, ..run.clone() }, TextRun { len: text.len() - b, ..run }]
                    .into_iter()
                    .filter(|r| r.len > 0)
                    .collect()
            }
            None => vec![run],
        };
        (text, runs)
    }
}

impl Element for TextLine {
    type RequestLayoutState = ();
    type PrepaintState = LinePrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _id: Option<&GlobalElementId>, _i: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let lh = line_h(window);
        if !self.field.read(cx).wrap {
            style.size.height = lh.into();
            return (window.request_layout(style, [], cx), ());
        }
        // As tall as the text's rows at the width the layout settles on.
        let (text, runs) = self.text_and_runs(window, cx);
        let size = window.text_style().font_size.to_pixels(window.rem_size());
        let id = window.request_measured_layout(style, move |known, avail, window, _cx| {
            let width = known.width.unwrap_or(match avail.width {
                AvailableSpace::Definite(w) => w,
                _ => px(f32::MAX),
            });
            let rows: usize = wrapped(window, &text, size, &runs, width, lh).len();
            gpui_kit::size(width, lh * rows.max(1) as f32)
        });
        (id, ())
    }

    fn prepaint(&mut self, _id: Option<&GlobalElementId>, _i: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _r: &mut (), window: &mut Window, cx: &mut App) -> LinePrepaint {
        let theme = cx.global::<Theme>().clone();
        let (text, runs) = self.text_and_runs(window, cx);
        let f = self.field.read(cx);
        let size = window.text_style().font_size.to_pixels(window.rem_size());
        let wrap = f.wrap;
        let line_h = if wrap { line_h(window) } else { bounds.size.height };
        let line = if wrap {
            Shaped::Wrapped(wrapped(window, &text, size, &runs, bounds.size.width, line_h))
        } else {
            Shaped::Line(window.text_system().shape_line(SharedString::from(text), size, &runs, None))
        };
        let empty = f.content.is_empty();
        // `eol` places the cursor end, not the anchor.
        let pos_of = |off: usize| {
            if empty { point(px(0.), px(0.)) } else { line.pos(f.display_offset(off), f.eol && off == f.cursor()) }
        };
        let cur = pos_of(f.cursor());
        // Keep the cursor visible in a narrow single-line field: scroll the line left.
        let width = bounds.size.width - px(2.);
        let scroll = if !wrap && cur.x > width { cur.x - width } else { px(0.) };
        let at = |p: Point<Pixels>| point(bounds.left() + p.x - scroll, bounds.top() + p.y);
        let (cursor, selection) = if f.selected.is_empty() {
            (Some(fill(Bounds::new(at(cur) + point(px(0.), line_h * 0.12), size_of(px(1.5), line_h * 0.76)), theme.accent)), vec![])
        } else {
            let (a, b) = (pos_of(f.selected.start), pos_of(f.selected.end));
            let tint = theme.accent.opacity(0.3);
            let row = |y: Pixels, x0: Pixels, x1: Pixels| fill(Bounds::from_corners(at(point(x0, y)), at(point(x1, y + line_h))), tint);
            let quads = if a.y == b.y {
                vec![row(a.y, a.x, b.x)]
            } else {
                // First row to the edge, whole rows between, the last row up to the end.
                let mut q = vec![row(a.y, a.x, bounds.size.width)];
                let mut y = a.y + line_h;
                while y < b.y {
                    q.push(row(y, px(0.), bounds.size.width));
                    y += line_h;
                }
                q.push(row(b.y, px(0.), b.x));
                q
            };
            (None, quads)
        };
        LinePrepaint { line: Some(line), cursor, selection, scroll, line_h }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _i: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _r: &mut (),
        pp: &mut LinePrepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.field.read(cx).focus.clone();
        window.handle_input(&focus, ElementInputHandler::new(bounds, self.field.clone()), cx);
        let Some(line) = pp.line.take() else { return };
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for s in pp.selection.drain(..) {
                window.paint_quad(s);
            }
            line.paint(point(bounds.left() - pp.scroll, bounds.top()), pp.line_h, window, cx);
            if focus.is_focused(window)
                && let Some(c) = pp.cursor.take()
            {
                window.paint_quad(c);
            }
        });
        let shifted = Bounds::new(point(bounds.left() - pp.scroll, bounds.top()), bounds.size);
        let line_h = pp.line_h;
        self.field.update(cx, |f, _| {
            f.last_layout = Some(line);
            f.last_bounds = Some(shifted);
            f.last_line_h = line_h;
        });
    }
}

/// Text wrapped exactly as a wrapping [`TextField`] wraps it, with the same row height: the
/// image sheet shows a note this way when its field isn't on it, so the words don't move as the
/// field comes and goes. Font, size and color are inherited.
pub fn wrap_text(text: impl Into<SharedString>) -> WrapText {
    WrapText { text: text.into() }
}

pub struct WrapText {
    text: SharedString,
}

impl WrapText {
    fn run(&self, window: &Window) -> (Pixels, Vec<TextRun>) {
        let style = window.text_style();
        let run = TextRun { len: self.text.len(), font: style.font(), color: style.color, background_color: None, underline: None, strikethrough: None };
        (style.font_size.to_pixels(window.rem_size()), vec![run])
    }
}

impl IntoElement for WrapText {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for WrapText {
    type RequestLayoutState = ();
    type PrepaintState = Vec<Row>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _id: Option<&GlobalElementId>, _i: Option<&InspectorElementId>, window: &mut Window, _cx: &mut App) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let lh = line_h(window);
        let (size, runs) = self.run(window);
        let text = self.text.clone();
        let id = window.request_measured_layout(style, move |known, avail, window, _cx| {
            let width = known.width.unwrap_or(match avail.width {
                AvailableSpace::Definite(w) => w,
                _ => px(f32::MAX),
            });
            gpui_kit::size(width, lh * wrapped(window, &text, size, &runs, width, lh).len().max(1) as f32)
        });
        (id, ())
    }

    fn prepaint(&mut self, _id: Option<&GlobalElementId>, _i: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _r: &mut (), window: &mut Window, _cx: &mut App) -> Vec<Row> {
        let (size, runs) = self.run(window);
        wrapped(window, &self.text, size, &runs, bounds.size.width, line_h(window))
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _i: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _r: &mut (),
        rows: &mut Vec<Row>,
        window: &mut Window,
        cx: &mut App,
    ) {
        paint_rows(rows, bounds.origin, line_h(window), window, cx);
    }
}

/// Tall enough for descenders whatever line height the parent set.
fn line_h(window: &Window) -> Pixels {
    let fs = window.text_style().font_size.to_pixels(window.rem_size());
    window.line_height().max(fs * 1.4)
}

fn size_of(w: Pixels, h: Pixels) -> Size<Pixels> {
    size(w, h)
}

#[cfg(test)]
mod tests {
    use super::{History, Snap, row_ends, word_at, word_left, word_right};
    use gpui_kit::px;

    /// Rows of `text` in a monospace font of 1px per byte.
    fn rows(text: &str, width: usize) -> Vec<&str> {
        let mut s = 0;
        row_ends(text, px(width as f32), |i| px(i as f32))
            .into_iter()
            .map(|e| {
                let r = &text[s..e];
                s = e;
                r
            })
            .collect()
    }

    #[test]
    fn rows_break_after_spaces_and_keep_punctuation() {
        assert_eq!(rows("is this right?", 13), ["is this ", "right?"]);
        assert_eq!(rows("see a/b/c", 7), ["see ", "a/b/c"]);
        // Spaces hang past the edge rather than starting a row.
        assert_eq!(rows("abc   def", 3), ["abc   ", "def"]);
        assert_eq!(rows("fits", 4), ["fits"]);
        assert_eq!(rows("", 4), [""]);
        // A word wider than a row breaks between characters.
        assert_eq!(rows("abcdefg hi", 3), ["abc", "def", "g ", "hi"]);
    }

    #[test]
    fn word_moves() {
        let s = "git push --force origin";
        assert_eq!(word_left(s, s.len()), 17);
        assert_eq!(word_left(s, 17), 11);
        assert_eq!(word_left(s, 4), 0);
        assert_eq!(word_right(s, 0), 3);
        assert_eq!(word_right(s, 3), 8);
        assert_eq!(word_left("héllo wörld", "héllo wörld".len()), "héllo ".len());
        // Right after punctuation, ⌥⌫ takes the punctuation instead of nothing.
        assert_eq!(word_left(s, 11), 9);
        assert_eq!(word_left("see foo.", 8), 7);
        assert_eq!(word_left("see foo. ", 9), 7);
        assert_eq!(word_left("a/b/", 4), 3);
        assert_eq!(word_left("...", 3), 0);
    }

    #[test]
    fn double_click_word() {
        let s = "These shortcuts don't work.";
        let w = |off| &s[word_at(s, off)];
        assert_eq!(w(8), "shortcuts");
        assert_eq!(w(6), "shortcuts");
        assert_eq!(w(0), "These");
        assert_eq!(w(5), " ");
        assert_eq!(w(19), "'");
        assert_eq!(w(s.len()), ".");
        assert_eq!(w(s.len() - 1), ".");
        assert_eq!(word_at("a\nbc", 1), 0..1);
        assert_eq!(word_at("", 0), 0..0);
        assert_eq!(&"héllo wörld"[word_at("héllo wörld", 8)], "wörld");
    }

    /// Applies an edit the way `TextField::replace` does, recording it first.
    fn edit(h: &mut History, text: &mut String, sel: &mut std::ops::Range<usize>, range: std::ops::Range<usize>, new: &str) {
        h.record(text, sel, &range, new);
        text.replace_range(range.clone(), new);
        let end = range.start + new.len();
        *sel = end..end;
    }

    #[test]
    fn typing_and_deleting_are_one_step_each() {
        let (mut h, mut t, mut sel) = (History::default(), String::new(), 0..0);
        for (i, c) in "hello".chars().enumerate() {
            edit(&mut h, &mut t, &mut sel, i..i, &c.to_string());
        }
        edit(&mut h, &mut t, &mut sel, 4..5, "");
        edit(&mut h, &mut t, &mut sel, 3..4, "");
        assert_eq!(t, "hel");
        let s = h.undo(Snap { text: t.clone(), selected: sel.clone() }).unwrap();
        assert_eq!(s.text, "hello");
        let s = h.undo(Snap { text: s.text, selected: s.selected }).unwrap();
        assert_eq!(s.text, "");
        assert!(h.undo(s.clone()).is_none());
        let s = h.redo(s).unwrap();
        assert_eq!(s.text, "hello");
        let s = h.redo(s).unwrap();
        assert_eq!(s.text, "hel");
        assert!(h.redo(s).is_none());
    }

    #[test]
    fn moving_the_cursor_starts_a_new_step_and_edits_drop_redo() {
        let (mut h, mut t, mut sel) = (History::default(), String::new(), 0..0);
        edit(&mut h, &mut t, &mut sel, 0..0, "ab");
        sel = 0..0;
        edit(&mut h, &mut t, &mut sel, 0..0, "x");
        assert_eq!(t, "xab");
        let s = h.undo(Snap { text: t.clone(), selected: sel.clone() }).unwrap();
        assert_eq!(s.text, "ab");
        let (mut t, mut sel) = (s.text, s.selected);
        edit(&mut h, &mut t, &mut sel, 2..2, "c");
        assert!(h.redo(Snap { text: t, selected: sel }).is_none());
    }
}
