//! Small building blocks shared by the Rules (Rules-B) and Triggers (Triggers-A) screens:
//! effect pills, chips, buttons, the switch, a one-line text input, and local-time labels.
use crate::model::parse_rfc3339;
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;

// ------------------------------------------------------------------ colors

/// `--err-soft` / `--ok-soft` from the Rules-B helmet (not in the shared theme).
pub fn err_soft(t: &Theme) -> Hsla {
    let mut c = t.err;
    c.a = if t.mode == crate::theme::ThemeMode::Dark { 0.12 } else { 0.07 };
    c
}

pub fn ok_soft(t: &Theme) -> Hsla {
    let mut c = t.ok;
    c.a = if t.mode == crate::theme::ThemeMode::Dark { 0.12 } else { 0.08 };
    c
}

pub fn effect_color(t: &Theme, effect: &str) -> (Hsla, Hsla) {
    match effect {
        "deny" => (t.err, err_soft(t)),
        "ask" => (t.need, t.need_soft),
        "allow" => (t.ok, ok_soft(t)),
        _ => (t.dim, t.raised),
    }
}

// ------------------------------------------------------------------ elements

/// The uppercase effect pill (min 46px, 20px tall).
pub fn pill(t: &Theme, effect: &str) -> Div {
    let (fg, bg) = effect_color(t, effect);
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .min_w(px(46.))
        .h(px(20.))
        .px(px(7.))
        .rounded(px(5.))
        .border_1()
        .border_color(fg)
        .bg(bg)
        .text_color(fg)
        .text_size(px(11.))
        .font_weight(FontWeight::BOLD)
        .child(effect.to_uppercase())
}

/// `.chip`: 20px outlined label.
pub fn chip(t: &Theme, text: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .h(px(20.))
        .px(px(7.))
        .rounded(px(6.))
        .border_1()
        .border_color(t.line)
        .text_size(px(11.5))
        .text_color(t.dim)
        .whitespace_nowrap()
        .child(text.into())
}

/// `.cap`: 11px bold uppercase dim label.
pub fn cap(t: &Theme, text: &str) -> Div {
    div().text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(t.dim).whitespace_nowrap().child(text.to_uppercase())
}

/// `.btn`: 28px outlined button.
pub fn btn(t: &Theme, id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    let raised = t.raised;
    btn_base(t, id, label).hover(move |s| s.bg(raised))
}

fn btn_base(t: &Theme, id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.))
        .h(px(28.))
        .px(px(10.))
        .rounded(px(7.))
        .border_1()
        .border_color(t.line)
        .text_size(px(12.))
        .whitespace_nowrap()
        .cursor_pointer()
        .child(label.into())
}

/// `.btn.btn-p`: accent filled.
pub fn btn_primary(t: &Theme, id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    btn_base(t, id, label).bg(t.accent).border_color(t.accent).text_color(t.accent_fg).font_weight(FontWeight::BOLD).hover(|s| s.opacity(0.92))
}

/// Solid red "Remove" (human-only destructive action).
pub fn btn_danger(t: &Theme, id: impl Into<ElementId>, label: impl Into<SharedString>, h: f32) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .h(px(h))
        .px(px(if h < 26. { 8. } else { 12. }))
        .rounded(px(6.))
        .bg(t.err)
        .text_color(white())
        .font_weight(FontWeight::BOLD)
        .text_size(px(12.))
        .cursor_pointer()
        .hover(|s| s.opacity(0.9))
        .child(label.into())
}

/// `.sw` switch, 30×18.
pub fn switch(t: &Theme, id: impl Into<ElementId>, on: bool) -> Stateful<Div> {
    div()
        .id(id)
        .relative()
        .flex_none()
        .w(px(30.))
        .h(px(18.))
        .rounded(px(9.))
        .bg(if on { t.ok } else { t.line })
        .cursor_pointer()
        .child(div().absolute().top(px(2.)).left(px(if on { 14. } else { 2. })).size(px(14.)).rounded_full().bg(if on { t.panel } else { t.dim }))
}

pub fn dot(c: Hsla, size: f32) -> Div {
    div().size(px(size)).flex_none().rounded_full().bg(c)
}

pub fn mono(t: &Theme, text: impl Into<SharedString>, size: f32) -> Div {
    div().font_family(t.mono_font.clone()).text_size(px(size)).child(text.into())
}

/// Accent ring used to highlight a card or row.
pub fn highlight_ring(t: &Theme) -> Vec<BoxShadow> {
    vec![BoxShadow { color: t.accent_soft, offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(3.), inset: false }]
}

// ------------------------------------------------------------------ one-line input

/// What a key did to a [`LineInput`].
#[derive(Debug, PartialEq, Eq)]
pub enum KeyOutcome {
    Submit,
    Cancel,
    Ignored,
}

/// A single-line field for the screens: a handle on the shared IME-capable
/// [`TextField`](super::text_input::TextField). The field edits itself (cursor, selection,
/// IME, paste); the parent's key handler on the box only sees ↩ / esc / ⇥ and the rest, which
/// [`LineInput::on_key`] maps to [`KeyOutcome`]. With `secret` the text is drawn as bullets.
pub struct LineInput {
    pub field: Entity<super::text_input::TextField>,
    pub focus: FocusHandle,
}

impl LineInput {
    pub fn new(cx: &mut App, secret: bool, placeholder: impl Into<SharedString>) -> LineInput {
        let field = cx.new(|cx| super::text_input::TextField::new(cx, secret, placeholder));
        let focus = field.read(cx).focus.clone();
        LineInput { field, focus }
    }

    /// A field that takes only the digits 0–9.
    pub fn digits(cx: &mut App, placeholder: impl Into<SharedString>) -> LineInput {
        let input = LineInput::new(cx, false, placeholder);
        input.field.update(cx, |f, _| f.digits = true);
        input
    }

    pub fn text(&self, cx: &App) -> String {
        self.field.read(cx).text().to_string()
    }

    pub fn is_empty(&self, cx: &App) -> bool {
        self.field.read(cx).text().is_empty()
    }

    pub fn set_text(&self, text: &str, cx: &mut App) {
        self.field.update(cx, |f, cx| f.set_text(text, cx));
    }

    pub fn clear(&self, cx: &mut App) {
        self.field.update(cx, |f, cx| f.clear(cx));
    }

    /// Keys the field left for the parent: ↩ submits, esc cancels.
    pub fn on_key(&mut self, ev: &KeyDownEvent, _cx: &mut App) -> KeyOutcome {
        let ks = &ev.keystroke;
        if ks.modifiers.platform || ks.modifiers.control {
            return KeyOutcome::Ignored;
        }
        match ks.key.as_str() {
            "enter" => KeyOutcome::Submit,
            "escape" => KeyOutcome::Cancel,
            _ => KeyOutcome::Ignored,
        }
    }

    /// The field's box. The caller attaches `on_key_down` (and may restyle the box).
    pub fn render(&self, t: &Theme, id: impl Into<ElementId>, window: &Window) -> Stateful<Div> {
        let focused = self.focus.is_focused(window);
        div()
            .id(id)
            .flex()
            .items_center()
            .h(px(32.))
            .px(px(10.))
            .rounded(px(7.))
            .border_1()
            .border_color(if focused { t.accent } else { t.line })
            .bg(t.raised)
            .font_family(t.mono_font.clone())
            .text_size(px(12.))
            .text_color(t.fg)
            .overflow_hidden()
            .cursor_text()
            .child(self.field.clone())
    }
}

// ------------------------------------------------------------------ time

pub fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Local broken-down time for a unix timestamp.
fn local_tm(secs: i64) -> libc::tm {
    let t: libc::time_t = secs as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    tm
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// "14:17:02" (local).
pub fn clock(ts: &str) -> String {
    let Some(s) = parse_rfc3339(ts) else {
        return String::new();
    };
    let tm = local_tm(s);
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

/// "14:17:02" when today, else "2 Sep" (local).
pub fn clock_or_day(ts: &str) -> String {
    let Some(s) = parse_rfc3339(ts) else {
        return String::new();
    };
    let (a, b) = (local_tm(s), local_tm(now_unix()));
    if a.tm_year == b.tm_year && a.tm_yday == b.tm_yday { clock(ts) } else { format!("{} {}", a.tm_mday, MONTHS[a.tm_mon.clamp(0, 11) as usize]) }
}

/// "today 11:20" or "2 Sep" (local), as on the Rules-B cards.
pub fn day_label(ts: &str) -> String {
    let Some(s) = parse_rfc3339(ts) else {
        return String::new();
    };
    let (a, b) = (local_tm(s), local_tm(now_unix()));
    if a.tm_year == b.tm_year && a.tm_yday == b.tm_yday {
        format!("today {:02}:{:02}", a.tm_hour, a.tm_min)
    } else {
        format!("{} {}", a.tm_mday, MONTHS[a.tm_mon.clamp(0, 11) as usize])
    }
}

/// "just now", "12m ago", "3h ago", "6d ago"; `never` when absent.
pub fn ago(ts: Option<&str>) -> String {
    let Some(s) = ts.and_then(parse_rfc3339) else {
        return "never".into();
    };
    let d = (now_unix() - s).max(0);
    match d {
        0..=44 => "just now".into(),
        45..=3599 => format!("{}m ago", (d / 60).max(1)),
        3600..=86399 => format!("{}h ago", d / 3600),
        _ => format!("{}d ago", d / 86400),
    }
}

/// "4 min left", "38 min left", "2 h left"; "expired" when past.
pub fn left(secs: i64) -> String {
    match secs {
        i64::MIN..=0 => "expired".into(),
        1..=59 => format!("{secs}s left"),
        60..=5399 => format!("{} min left", (secs + 59) / 60),
        5400..=172_799 => format!("{} h left", (secs + 1799) / 3600),
        _ => format!("{} days left", secs / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::{ago, left, now_unix};

    #[test]
    fn countdown_labels() {
        assert_eq!(left(-5), "expired");
        assert_eq!(left(30), "30s left");
        assert_eq!(left(4 * 60), "4 min left");
        assert_eq!(left(38 * 60 - 10), "38 min left");
        assert_eq!(left(3 * 3600), "3 h left");
    }

    #[test]
    fn ago_labels() {
        assert_eq!(ago(None), "never");
        let ts = crate::model::rfc3339_from_unix(now_unix() - 12 * 60);
        assert_eq!(ago(Some(&ts)), "12m ago");
    }
}

/// "Back to terminal  esc" at the right end of a screen's header (Rules, Triggers, Insights).
pub fn back_btn(t: &Theme, id: &'static str, main: WeakEntity<crate::app::MainWindow>) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .px(px(10.))
        .py(px(5.))
        .rounded(px(7.))
        .text_color(t.dim)
        .whitespace_nowrap()
        .cursor_pointer()
        .hover(|st| st.bg(t.raised))
        .on_click(move |_, w, cx| {
            let _ = main.update(cx, |m, cx| m.set_screen(crate::app::Screen::Terminal, w, cx));
        })
        .child("Back to terminal  esc")
}
