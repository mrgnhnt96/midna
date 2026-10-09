//! Settings ▸ Agents ▸ Keep the Mac awake: the hours as a range on a 24-hour track (drag either
//! end, or the bar to move both; 15-minute steps; an end before the start runs past midnight)
//! and the days as seven buttons. They're the plain `keep_awake.start` / `end` / `days`
//! settings; the hours save together through `keep_awake.set` when the drag ends.
use super::*;
use midna_proto::keep_awake::{DAYS, clock, hhmm, parse_time};
use midna_proto::time;

const DAY: u32 = 1440;
const STEP: u32 = 15;
const HANDLE: f32 = 18.;
const LABEL_W: f32 = 64.;

/// What a drag on the hours track moves.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Grab {
    Start,
    End,
    Bar,
}

/// The drag payload (handles and the bar).
pub(super) struct HoursDrag(Grab);

struct Ghost;

impl Render for Ghost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// The range while it's being dragged: shown instead of the settings until the drop saves it.
#[derive(Clone, Copy)]
pub(super) struct HoursLive {
    pub start: u32,
    pub end: u32,
    /// The range and the minute under the cursor when the drag began (the bar moves by the
    /// difference).
    from: (u32, u32, u32),
}

/// `9 hours`, `8 hours 15 min`, `all day`: how long a start–end window is.
pub(super) fn span_words(start: u32, end: u32) -> String {
    let m = (end + DAY - start) % DAY;
    match (m / 60, m % 60) {
        (0, 0) => "all day".into(),
        (0, mi) => format!("{mi} min"),
        (1, 0) => "1 hour".into(),
        (h, 0) => format!("{h} hours"),
        (h, mi) => format!("{h} h {mi} min"),
    }
}

/// The minute a cursor x is at on a track, snapped to 15 minutes (0..=1440).
fn minute_at(x: Pixels, track: Bounds<Pixels>) -> u32 {
    let w = f32::from(track.size.width).max(1.);
    let frac = (f32::from(x - track.left()) / w).clamp(0., 1.);
    ((frac * (DAY / STEP) as f32).round() as u32) * STEP
}

/// A drag's new range from where it began and the minute now under the cursor.
fn dragged(grab: Grab, from: (u32, u32, u32), m: u32) -> (u32, u32) {
    let (start, end, at) = from;
    match grab {
        Grab::Start => (m % DAY, end),
        Grab::End => (start, m % DAY),
        Grab::Bar => {
            let d = m as i64 - at as i64;
            let shift = |v: u32| (v as i64 + d).rem_euclid(DAY as i64) as u32;
            (shift(start), shift(end))
        }
    }
}

/// Where each end sits on the track: an end at midnight after a later start is drawn at the
/// right edge (9 AM–12 AM), not the left.
fn positions(start: u32, end: u32) -> (u32, u32) {
    (start, if end == 0 && start != 0 { DAY } else { end })
}

/// The filled stretches of the track: one, or two when the window runs past midnight.
fn fills(start: u32, end: u32) -> Vec<(u32, u32)> {
    let (s, e) = positions(start, end);
    match s.cmp(&e) {
        std::cmp::Ordering::Less => vec![(s, e)],
        std::cmp::Ordering::Equal => vec![(0, DAY)],
        std::cmp::Ordering::Greater => vec![(s, DAY), (0, e)],
    }
}

fn frac(m: u32) -> f32 {
    m as f32 / DAY as f32
}

impl SettingsWindow {
    /// The saved hours, or the dragged ones mid-drag.
    pub(super) fn hours(&self) -> (u32, u32) {
        if let Some(l) = self.hours_live {
            return (l.start, l.end);
        }
        let read = |k: &str, d: u32| self.value(k).as_str().and_then(|s| parse_time(s).ok()).unwrap_or(d);
        (read("keep_awake.start", 9 * 60), read("keep_awake.end", 18 * 60))
    }

    fn drag_hours(&mut self, grab: Grab, m: u32, cx: &mut Context<Self>) {
        let (start, end) = self.hours();
        let from = self.hours_live.map(|l| l.from).unwrap_or((start, end, m));
        let (start, end) = dragged(grab, from, m);
        self.hours_live = Some(HoursLive { start, end, from });
        cx.notify();
    }

    /// The drop: save both ends at once (the rows keep showing them until the reload).
    fn drop_hours(&mut self, cx: &mut Context<Self>) {
        let Some(l) = self.hours_live.take() else { return };
        let (start, end) = (hhmm(l.start), hhmm(l.end));
        if self.value("keep_awake.start") == json!(start) && self.value("keep_awake.end") == json!(end) {
            return cx.notify();
        }
        for (k, v) in [("keep_awake.start", &start), ("keep_awake.end", &end)] {
            self.mine.push((k.to_string(), Instant::now()));
            if let Some(e) = self.entries.iter_mut().find(|e| e.key == k) {
                e.value = json!(v);
            }
        }
        let words = |m: u32| clock(m).to_lowercase().replace(' ', "");
        let cmd = format!("midna keep-awake hours {} {}", words(l.start), words(l.end));
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { backend.call("keep_awake.set", json!({ "start": start, "end": end })) }).await;
            let _ = this.update(cx, |s, cx| {
                if let Err(e) = r {
                    s.last = Some(Last { cmd, ok: false, result: s.call_error(&e), who: "you, from this window".into(), at: Instant::now() });
                    s.load(cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// The hours track: rail, filled window, now, the two handles with their times, and the
    /// hours of the day under it.
    pub(super) fn hours_control(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let (start, end) = self.hours();
        let (s_pos, e_pos) = positions(start, end);
        let (_, _, _, h, mi, _) = time::local_parts(time::now_unix());
        let now = h * 60 + mi;
        let dragging = self.hours_live.is_some();
        let rail_top = 22.;
        let mut track = div()
            .id("ka-hours-track")
            .relative()
            .w_full()
            .h(px(58.))
            .on_drag_move(cx.listener(|s, ev: &DragMoveEvent<HoursDrag>, _, cx| {
                let grab = ev.drag(cx).0;
                s.drag_hours(grab, minute_at(ev.event.position.x, ev.bounds), cx);
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|s, _, _, cx| s.drop_hours(cx)))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|s, _, _, cx| s.drop_hours(cx)))
            .child(div().absolute().top(px(rail_top)).left_0().right_0().h(px(8.)).rounded(px(4.)).bg(t.raised).border_1().border_color(t.line));
        for (i, (a, b)) in fills(start, end).into_iter().enumerate() {
            track = track.child(
                div()
                    .id(SharedString::from(format!("ka-hours-bar-{i}")))
                    .absolute()
                    .top(px(rail_top))
                    .left(relative(frac(a)))
                    .w(relative(frac(b - a)))
                    .h(px(8.))
                    .rounded(px(4.))
                    .bg(t.accent)
                    .cursor(if dragging { CursorStyle::ClosedHand } else { CursorStyle::OpenHand })
                    .on_drag(HoursDrag(Grab::Bar), |_, _, _, cx| cx.new(|_| Ghost)),
            );
        }
        track = track
            .child(div().absolute().top(px(rail_top - 6.)).left(relative(frac(now))).ml(px(-1.)).w(px(2.)).h(px(20.)).bg(t.need))
            .child(div().absolute().top(px(-2.)).left(relative(frac(now))).ml(px(-20.)).w(px(40.)).text_center().text_size(px(11.)).text_color(t.need).child("now"));
        for (grab, pos, m) in [(Grab::Start, s_pos, start), (Grab::End, e_pos, end)] {
            let id = if grab == Grab::Start { "ka-hours-start" } else { "ka-hours-end" };
            track = track
                .child(
                    div()
                        .id(id)
                        .absolute()
                        .top(px(rail_top + 4. - HANDLE / 2.))
                        .left(relative(frac(pos)))
                        .ml(px(-HANDLE / 2.))
                        .size(px(HANDLE))
                        .rounded_full()
                        .bg(gpui_kit::white())
                        .border_1()
                        .border_color(t.line)
                        .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.35), offset: point(px(0.), px(1.)), blur_radius: px(4.), spread_radius: px(0.), inset: false }])
                        .cursor(CursorStyle::ResizeLeftRight)
                        .on_drag(HoursDrag(grab), |_, _, _, cx| cx.new(|_| Ghost)),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(rail_top + 14.))
                        .left(relative(frac(pos)))
                        .ml(px(-LABEL_W / 2.))
                        .w(px(LABEL_W))
                        .text_center()
                        .text_size(px(11.5))
                        .font_weight(FontWeight::BOLD)
                        .text_color(t.fg)
                        .child(clock(m)),
                );
        }
        let ticks = ["12 AM", "6 AM", "12 PM", "6 PM", "12 AM"];
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(track)
            .child(div().flex().justify_between().text_size(px(11.)).text_color(t.dim).children(ticks.map(|l| div().child(l))))
            .into_any_element()
    }

    /// Seven day buttons; the last day on can't be turned off (keep-awake has its own switch).
    pub(super) fn days_control(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let on: Vec<String> = self.value("keep_awake.days").as_array().map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
        let mut row = div().flex().flex_wrap().justify_end().gap(px(6.));
        for d in DAYS {
            let lit = on.iter().any(|x| x == d);
            let last = lit && on.len() == 1;
            let next: Vec<String> = DAYS.iter().filter(|x| (**x == d) != on.iter().any(|o| o == *x)).map(|x| x.to_string()).collect();
            row = row.child(
                div()
                    .id(SharedString::from(format!("ka-day-{d}")))
                    .w(px(40.))
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(7.))
                    .text_size(px(12.))
                    .map(|b| {
                        if lit {
                            b.bg(t.accent).text_color(t.accent_fg).font_weight(FontWeight::BOLD)
                        } else {
                            b.border_1().border_color(t.line).bg(t.raised).text_color(t.dim).hover(|s| s.text_color(t.fg).border_color(t.dim))
                        }
                    })
                    .map(|b| if last { b.tooltip(crate::ui::header::tip("Keep at least one day; turn keep-awake off instead")) } else { b.cursor_pointer() })
                    .on_click(cx.listener(move |s, _, _, cx| {
                        if !last {
                            s.set("keep_awake.days", json!(next), cx);
                        }
                    }))
                    .child(format!("{}{}", d[..1].to_uppercase(), &d[1..])),
            );
        }
        row.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's glob re-export would shadow `#[test]`.
    use super::{DAY, Grab, dragged, fills, span_words};

    #[test]
    fn drags_and_draws_ranges() {
        // ends move alone; the bar moves both and wraps around midnight
        assert_eq!(dragged(Grab::Start, (540, 1080, 540), 480), (480, 1080));
        assert_eq!(dragged(Grab::End, (540, 1080, 1080), DAY), (540, 0));
        assert_eq!(dragged(Grab::Bar, (540, 1080, 600), 1200), (1140, 240));
        // midnight as an end is the right edge; past midnight is two stretches; equal is all day
        assert_eq!(fills(540, 0), vec![(540, DAY)]);
        assert_eq!(fills(1320, 120), vec![(1320, DAY), (0, 120)]);
        assert_eq!(fills(600, 600), vec![(0, DAY)]);
        assert_eq!(span_words(540, 1080), "9 hours");
        assert_eq!(span_words(1320, 135), "4 h 15 min");
        assert_eq!(span_words(0, 0), "all day");
    }
}
