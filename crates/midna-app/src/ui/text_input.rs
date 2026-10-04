//! A shared single-line text field with IME (`EntityInputHandler`), a cursor, selection and
//! the usual macOS editing keys. Used by the ⌘K palette, the Rules test strip, the Triggers
//! secret field and the Settings Ask box (through `screen_kit::LineInput`).
//!
//! Typing arrives through GPUI's input handler (so dead keys, option characters and IME
//! composition work, with the candidate window anchored at the cursor). The field's own key
//! handler takes the editing keys (arrows, ⌥/⌘ word and line moves, ⇧ selection, ⌫/⌦, ⌘A/C/X/V);
//! everything else (↩, esc, ⇥, ↑/↓, ⌘-shortcuts) bubbles to the parent, which decides what
//! submit/cancel mean. Font, size and color are inherited from the parent element.
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use std::ops::Range;

/// Emitted on every content change.
pub struct FieldChanged;

pub struct TextField {
    pub focus: FocusHandle,
    content: String,
    /// Byte range into `content`.
    selected: Range<usize>,
    reversed: bool,
    marked: Option<Range<usize>>,
    pub placeholder: SharedString,
    /// Draw bullets instead of the text (and never copy it out).
    pub secret: bool,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    selecting: bool,
}

impl EventEmitter<FieldChanged> for TextField {}

impl TextField {
    pub fn new(cx: &mut Context<Self>, secret: bool, placeholder: impl Into<SharedString>) -> TextField {
        TextField {
            focus: cx.focus_handle(),
            content: String::new(),
            selected: 0..0,
            reversed: false,
            marked: None,
            placeholder: placeholder.into(),
            secret,
            last_layout: None,
            last_bounds: None,
            selecting: false,
        }
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    /// Replace the whole text (cursor at the end).
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if self.content == text {
            return;
        }
        self.wipe();
        self.content = text.to_string();
        self.selected = self.content.len()..self.content.len();
        self.marked = None;
        cx.emit(FieldChanged);
        cx.notify();
    }

    /// Select the whole text (typing replaces it).
    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        self.selected = 0..self.content.len();
        self.reversed = false;
        cx.notify();
    }

    /// Clear, overwriting the old buffer first so a secret doesn't linger in memory.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        if self.content.is_empty() {
            return;
        }
        self.wipe();
        self.selected = 0..0;
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
        cx.notify();
    }

    fn select_to(&mut self, off: usize, cx: &mut Context<Self>) {
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

    fn replace(&mut self, range: Range<usize>, text: &str, cx: &mut Context<Self>) {
        let text = text.replace(['\r', '\n'], " ");
        self.content.replace_range(range.clone(), &text);
        let end = range.start + text.len();
        self.selected = end..end;
        self.reversed = false;
        self.marked = None;
        cx.emit(FieldChanged);
        cx.notify();
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
        let target = |s: &Self, left: bool| -> usize {
            match (left, m.platform, m.alt) {
                (true, true, _) => 0,
                (false, true, _) => len,
                (true, _, true) => s.prev_word(c),
                (false, _, true) => s.next_word(c),
                (true, _, _) => s.prev_char(c),
                (false, _, _) => s.next_char(c),
            }
        };
        match ks.key.as_str() {
            "left" | "right" if !m.control => {
                let left = ks.key == "left";
                if m.shift {
                    let t = target(self, left);
                    self.select_to(t, cx);
                } else if !self.selected.is_empty() && !m.platform && !m.alt {
                    let edge = if left { self.selected.start } else { self.selected.end };
                    self.move_to(edge, cx);
                } else {
                    let t = target(self, left);
                    self.move_to(t, cx);
                }
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
                    0
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

    fn index_for_point(&self, p: Point<Pixels>) -> usize {
        let (Some(b), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref()) else {
            return self.content.len();
        };
        if self.content.is_empty() {
            return 0;
        }
        self.content_offset(line.closest_index_for_x(p.x - b.left()))
    }
}

/// Byte offset of the start of the word left of `off` (spaces skipped first).
pub fn word_left(s: &str, off: usize) -> usize {
    let before = &s[..off];
    let trimmed = before.trim_end();
    trimmed.rfind(|c: char| c.is_whitespace() || "/.-_:=,".contains(c)).map(|i| i + trimmed[i..].chars().next().map_or(1, char::len_utf8)).unwrap_or(0)
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
        cx.emit(FieldChanged);
        cx.notify();
    }

    fn bounds_for_range(&mut self, r: Range<usize>, b: Bounds<Pixels>, _w: &mut Window, _cx: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        let line = self.last_layout.as_ref()?;
        let range = self.range_from_utf16(&r);
        let x0 = line.x_for_index(self.display_offset(range.start));
        let x1 = line.x_for_index(self.display_offset(range.end));
        Some(Bounds::from_corners(point(b.left() + x0, b.top()), point(b.left() + x1, b.bottom())))
    }

    fn character_index_for_point(&mut self, p: Point<Pixels>, _w: &mut Window, _cx: &mut Context<Self>) -> Option<usize> {
        let b = self.last_bounds?;
        let line = self.last_layout.as_ref()?;
        let i = line.index_for_x(p.x - b.left())?;
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
            .cursor(CursorStyle::IBeam)
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|f, ev: &MouseDownEvent, window, cx| {
                    f.focus.focus(window, cx);
                    f.selecting = true;
                    let i = f.index_for_point(ev.position);
                    if ev.modifiers.shift {
                        f.select_to(i, cx);
                    } else if ev.click_count >= 2 {
                        f.selected = 0..f.content.len();
                        f.reversed = false;
                        cx.notify();
                    } else {
                        f.move_to(i, cx);
                    }
                }),
            )
            .on_mouse_up(MouseButton::Left, cx.listener(|f, _: &MouseUpEvent, _, _| f.selecting = false))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|f, _: &MouseUpEvent, _, _| f.selecting = false))
            .on_mouse_move(cx.listener(|f, ev: &MouseMoveEvent, _, cx| {
                if f.selecting {
                    let i = f.index_for_point(ev.position);
                    f.select_to(i, cx);
                }
            }))
            .child(TextLine { field: cx.entity() })
    }
}

/// The painted line: text (or placeholder), IME underline, selection and cursor.
struct TextLine {
    field: Entity<TextField>,
}

struct LinePrepaint {
    line: Option<ShapedLine>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
    scroll: Pixels,
}

impl IntoElement for TextLine {
    type Element = Self;
    fn into_element(self) -> Self {
        self
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
        style.size.height = line_h(window).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(&mut self, _id: Option<&GlobalElementId>, _i: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _r: &mut (), window: &mut Window, cx: &mut App) -> LinePrepaint {
        let theme = cx.global::<Theme>().clone();
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
        let size = style.font_size.to_pixels(window.rem_size());
        let line = window.text_system().shape_line(SharedString::from(text), size, &runs, None);
        let empty = f.content.is_empty();
        let cx_of = |off: usize| {
            if empty { px(0.) } else { line.x_for_index(f.display_offset(off)) }
        };
        let cur = cx_of(f.cursor());
        // Keep the cursor visible in a narrow field: scroll the line left.
        let width = bounds.size.width - px(2.);
        let scroll = if cur > width { cur - width } else { px(0.) };
        let h = bounds.size.height;
        let (cursor, selection) = if f.selected.is_empty() {
            (Some(fill(Bounds::new(point(bounds.left() + cur - scroll, bounds.top() + h * 0.12), size_of(px(1.5), h * 0.76)), theme.accent)), None)
        } else {
            let (a, b) = (cx_of(f.selected.start), cx_of(f.selected.end));
            (None, Some(fill(Bounds::from_corners(point(bounds.left() + a - scroll, bounds.top()), point(bounds.left() + b - scroll, bounds.bottom())), theme.accent.opacity(0.3))))
        };
        LinePrepaint { line: Some(line), cursor, selection, scroll }
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
            if let Some(s) = pp.selection.take() {
                window.paint_quad(s);
            }
            let _ = line.paint(point(bounds.left() - pp.scroll, bounds.top()), bounds.size.height, TextAlign::Left, None, window, cx);
            if focus.is_focused(window)
                && let Some(c) = pp.cursor.take()
            {
                window.paint_quad(c);
            }
        });
        let shifted = Bounds::new(point(bounds.left() - pp.scroll, bounds.top()), bounds.size);
        self.field.update(cx, |f, _| {
            f.last_layout = Some(line);
            f.last_bounds = Some(shifted);
        });
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
    use super::{word_left, word_right};

    #[test]
    fn word_moves() {
        let s = "git push --force origin";
        assert_eq!(word_left(s, s.len()), 17);
        assert_eq!(word_left(s, 17), 11);
        assert_eq!(word_left(s, 4), 0);
        assert_eq!(word_right(s, 0), 3);
        assert_eq!(word_right(s, 3), 8);
        assert_eq!(word_left("héllo wörld", "héllo wörld".len()), "héllo ".len());
    }
}
