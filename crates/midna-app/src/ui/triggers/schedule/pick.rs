//! The schedule editor's pickers: a date and time as a button that opens a month calendar
//! and hour and minute grids under its line, a time of day as the grids alone, and a number
//! as a digits-only field between − and + held to its range.
use super::*;

/// The most runs an end can count to.
pub(super) const MAX_RUNS: u64 = 1_000_000;

/// One of the editor's date or time values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Pick {
    /// Runs once at (date and time).
    At,
    /// Every day at (time).
    DailyAt,
    /// Between … and … (times).
    From,
    Until,
    /// Starts at (date and time).
    Starts,
    /// Ends on (date).
    EndsAt,
}

impl Pick {
    fn date(self) -> bool {
        matches!(self, Pick::At | Pick::Starts | Pick::EndsAt)
    }

    fn time(self) -> bool {
        self != Pick::EndsAt
    }
}

/// The picker that's open and the month its calendar shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Open {
    pub pick: Pick,
    pub year: i64,
    pub month: u32,
}

/// Days in a month.
pub(super) fn month_days(y: i64, m: u32) -> u32 {
    let (ny, nm) = shift_month(y, m, 1);
    (time::days_from_civil(ny, nm, 1) - time::days_from_civil(y, m, 1)) as u32
}

/// The month `delta` months from `y`-`m`.
pub(super) fn shift_month(y: i64, m: u32, delta: i32) -> (i64, u32) {
    let i = y * 12 + i64::from(m) - 1 + i64::from(delta);
    (i.div_euclid(12), (i.rem_euclid(12) + 1) as u32)
}

/// The month's first day, Monday = 0 (the calendar's first column, as the day buttons).
pub(super) fn first_column(y: i64, m: u32) -> u32 {
    // 1970-01-01 was a Thursday (3, Monday first)
    (time::days_from_civil(y, m, 1) + 3).rem_euclid(7) as u32
}

/// `t` moved to another local day, keeping its time of day (the day's last valid date when the
/// month is shorter).
pub(super) fn with_date(t: i64, y: i64, m: u32, d: u32) -> i64 {
    let (.., h, mi, _) = time::local_parts(t);
    time::local_unix(y, m, d.min(month_days(y, m)), h, mi)
}

/// `t` at another local time of day, on the same day.
pub(super) fn with_time(t: i64, h: u32, mi: u32) -> i64 {
    let (y, m, d, ..) = time::local_parts(t);
    time::local_unix(y, m, d, h, mi)
}

/// `Tue, Oct 6, 2026`.
pub(super) fn date_label(t: i64) -> String {
    const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let (y, m, d, _, _, wd) = time::local_parts(t);
    format!("{}, {} {d}, {y}", WEEKDAYS[wd as usize], MONTHS[m as usize - 1])
}

/// `October 2026`.
fn month_label(y: i64, m: u32) -> String {
    const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
    format!("{} {y}", MONTHS[m as usize - 1])
}

/// `HH:MM` from minutes past midnight (what `cron::parse_hm` reads back).
pub(super) fn hm(minutes: u32) -> String {
    format!("{:02}:{:02}", minutes / 60 % 24, minutes % 60)
}

impl ScheduleEditor {
    /// A date-and-time pick's value.
    fn moment(&self, pick: Pick) -> i64 {
        match pick {
            Pick::At => self.at,
            Pick::Starts => self.starts.unwrap_or_else(time::now_unix),
            _ => self.ends_at,
        }
    }

    fn set_moment(&mut self, pick: Pick, v: i64) {
        match pick {
            Pick::At => self.at = v,
            Pick::Starts => self.starts = Some(v),
            _ => self.ends_at = v,
        }
    }

    /// A pick's time of day, in minutes past midnight.
    fn minutes(&self, pick: Pick) -> u32 {
        match pick {
            Pick::DailyAt => self.daily_at,
            Pick::From => self.from,
            Pick::Until => self.until,
            _ => {
                let (.., h, mi, _) = time::local_parts(self.moment(pick));
                h * 60 + mi
            }
        }
    }

    fn set_minutes(&mut self, pick: Pick, v: u32) {
        match pick {
            Pick::DailyAt => self.daily_at = v,
            Pick::From => self.from = v,
            Pick::Until => self.until = v,
            _ => {
                let t = with_time(self.moment(pick), v / 60, v % 60);
                self.set_moment(pick, t);
            }
        }
    }

    /// Open a picker on its value's month, or close it if it's the one open.
    fn toggle(&mut self, pick: Pick) {
        if self.open.is_some_and(|o| o.pick == pick) {
            self.open = None;
            return;
        }
        let (year, month, ..) = time::local_parts(self.moment(pick));
        self.open = Some(Open { pick, year, month });
    }

    /// Hold a number field to its range: a bigger number becomes the most, 0 the least.
    pub(super) fn clamp_numbers(&self, cx: &mut App) {
        let max = if self.unit == Unit::Hours { 168 } else { 1440 };
        for (input, max) in [(&self.every, max), (&self.runs, MAX_RUNS)] {
            let text = input.text(cx);
            let Ok(n) = text.parse::<u64>() else { continue };
            let held = n.clamp(1, max);
            if held.to_string() != text {
                input.set_text(&held.to_string(), cx);
            }
        }
    }
}

impl TriggersView {
    /// A picker's button: its value, and a caret that's up while its panel shows.
    pub(super) fn pick_button(&self, t: &Theme, e: &ScheduleEditor, pick: Pick, cx: &mut Context<Self>) -> Stateful<Div> {
        let open = e.open.is_some_and(|o| o.pick == pick);
        let label = match pick {
            Pick::EndsAt => date_label(e.ends_at),
            Pick::DailyAt | Pick::From | Pick::Until => {
                let m = e.minutes(pick);
                cron::clock(m / 60, m % 60)
            }
            _ => {
                let v = e.moment(pick);
                let (.., h, mi, _) = time::local_parts(v);
                format!("{} · {}", date_label(v), cron::clock(h, mi))
            }
        };
        kit::btn(t, SharedString::from(format!("pick-{pick:?}")), label)
            .when(open, |d| d.border_color(t.accent))
            .child(div().text_color(t.dim).child(if open { "▴" } else { "▾" }))
            .on_click(cx.listener(move |v, _, _, cx| {
                if let Some(e) = v.editor.as_mut() {
                    e.toggle(pick);
                }
                cx.notify();
            }))
    }

    /// The open picker's panel, when it's one of `only`: a calendar and/or the hour and minute grids.
    pub(super) fn pick_panel(&self, t: &Theme, e: &ScheduleEditor, only: &[Pick], cx: &mut Context<Self>) -> Option<Div> {
        let open = e.open.filter(|o| only.contains(&o.pick))?;
        let pick = open.pick;
        let mut panel = div().ml(px(108.)).flex().flex_wrap().gap(px(18.)).p(px(12.)).rounded(px(10.)).border_1().border_color(t.line).bg(t.panel).w_auto().max_w(px(560.));
        if pick.date() {
            panel = panel.child(self.calendar(t, e, open, cx));
        }
        if pick.time() {
            panel = panel.child(self.clock_grids(t, e, pick, cx));
        }
        Some(panel)
    }

    fn calendar(&self, t: &Theme, e: &ScheduleEditor, open: Open, cx: &mut Context<Self>) -> Div {
        let Open { pick, year, month } = open;
        let (sy, sm, sd, ..) = time::local_parts(e.moment(pick));
        let (ty, tm, td, ..) = time::local_parts(time::now_unix());
        let today = time::days_from_civil(ty, tm, td);
        let nav = |id: &'static str, glyph: &'static str, delta: i32, cx: &mut Context<Self>| {
            div()
                .id(id)
                .size(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .text_color(t.dim)
                .cursor_pointer()
                .hover(|s| s.bg(t.raised).text_color(t.fg))
                .on_click(cx.listener(move |v, _, _, cx| {
                    if let Some(o) = v.editor.as_mut().and_then(|e| e.open.as_mut()) {
                        (o.year, o.month) = shift_month(o.year, o.month, delta);
                    }
                    cx.notify();
                }))
                .child(glyph)
        };
        let head = div()
            .flex()
            .items_center()
            .justify_between()
            .child(nav("cal-prev", "‹", -1, cx))
            .child(div().font_weight(FontWeight::BOLD).child(month_label(year, month)))
            .child(nav("cal-next", "›", 1, cx));
        let mut grid = div().flex().flex_wrap().w(px(7. * 30.));
        for l in DAY_LETTERS {
            grid = grid.child(div().w(px(30.)).h(px(22.)).flex().items_center().justify_center().text_size(px(11.)).text_color(t.dim).child(l));
        }
        for _ in 0..first_column(year, month) {
            grid = grid.child(div().w(px(30.)).h(px(28.)));
        }
        for d in 1..=month_days(year, month) {
            let on = (year, month, d) == (sy, sm, sd);
            let day = time::days_from_civil(year, month, d);
            let past = day < today;
            grid = grid.child(
                div()
                    .id(SharedString::from(format!("cal-{d}")))
                    .w(px(30.))
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .text_size(px(12.))
                    .when(day == today && !on, |d| d.border_1().border_color(t.line))
                    .when(on, |d| d.bg(t.accent).text_color(t.accent_fg).font_weight(FontWeight::BOLD))
                    .when(past && !on, |d| d.text_color(t.dim).opacity(0.5))
                    .when(!past && !on, |d| d.cursor_pointer().hover(|s| s.bg(t.raised)))
                    .when(!past, |el| {
                        el.on_click(cx.listener(move |v, _, _, cx| {
                            if let Some(e) = v.editor.as_mut() {
                                let at = with_date(e.moment(pick), year, month, d);
                                e.set_moment(pick, at);
                                // a date alone is picked once it's clicked
                                if !pick.time() {
                                    e.open = None;
                                }
                            }
                            cx.notify();
                        }))
                    })
                    .child(d.to_string()),
            );
        }
        div().flex().flex_col().gap(px(6.)).child(head).child(grid)
    }

    fn clock_grids(&self, t: &Theme, e: &ScheduleEditor, pick: Pick, cx: &mut Context<Self>) -> Div {
        let now = e.minutes(pick);
        let (h, mi) = (now / 60, now % 60);
        let cell = |id: String, label: String, on: bool, cx: &mut Context<Self>, to: u32, close: bool| {
            div()
                .id(SharedString::from(id))
                .w(px(34.))
                .h(px(26.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .text_size(px(12.))
                .cursor_pointer()
                .when(on, |d| d.bg(t.accent).text_color(t.accent_fg).font_weight(FontWeight::BOLD))
                .when(!on, |d| d.hover(|s| s.bg(t.raised)))
                .on_click(cx.listener(move |v, _, _, cx| {
                    if let Some(e) = v.editor.as_mut() {
                        e.set_minutes(pick, to);
                        if close {
                            e.open = None;
                        }
                    }
                    cx.notify();
                }))
                .child(label)
        };
        let half = |name: &'static str, pm: bool, cx: &mut Context<Self>| {
            let mut row = div().flex().items_center().child(div().w(px(30.)).text_size(px(11.)).text_color(t.dim).child(name));
            for i in 0..12 {
                let hour = i + if pm { 12 } else { 0 };
                let label = if i == 0 { "12".to_string() } else { i.to_string() };
                row = row.child(cell(format!("hour-{hour}"), label, hour == h, cx, hour * 60 + mi, false));
            }
            row
        };
        let mut minutes = div().flex().flex_wrap().w(px(6. * 34.)).ml(px(30.));
        for m in (0..60).step_by(5) {
            minutes = minutes.child(cell(format!("min-{m}"), format!(":{m:02}"), m == mi, cx, h * 60 + m, true));
        }
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(kit::cap(t, "Hour"))
            .child(half("AM", false, cx))
            .child(half("PM", true, cx))
            .child(div().mt(px(4.)).child(kit::cap(t, "Minute")))
            .child(minutes)
    }

    /// A number field between − and +, held to 1–`max`.
    pub(super) fn stepper(&self, t: &Theme, input: &LineInput, id: &'static str, max: u64, window: &Window, cx: &mut Context<Self>) -> Div {
        let n = input.text(cx).parse::<u64>().unwrap_or(0);
        let step = |sid: String, glyph: &'static str, to: u64, live: bool, field: Entity<crate::ui::text_input::TextField>, cx: &mut Context<Self>| {
            div()
                .id(SharedString::from(sid))
                .size(px(26.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .border_1()
                .border_color(t.line)
                .text_color(if live { t.fg } else { t.dim })
                .when(live, |d| d.cursor_pointer().hover(|s| s.bg(t.raised)))
                .when(live, |d| d.on_click(cx.listener(move |_, _, _, cx| field.update(cx, |f, cx| f.set_text(&to.to_string(), cx)))))
                .child(glyph)
        };
        div()
            .flex()
            .items_center()
            .gap(px(4.))
            .child(step(format!("{id}-down"), "−", n.saturating_sub(1).max(1), n > 1, input.field.clone(), cx))
            .child(self.sched_input(t, input, id, Some(56.), window, cx))
            .child(step(format!("{id}-up"), "+", (n + 1).min(max), n < max, input.field.clone(), cx))
    }
}

#[cfg(test)]
mod tests {
    use super::{date_label, first_column, hm, month_days, shift_month, with_date, with_time};
    use midna_proto::{cron, time};

    #[test]
    fn walks_the_calendar() {
        assert_eq!([month_days(2026, 2), month_days(2028, 2), month_days(2026, 10), month_days(2026, 12)], [28, 29, 31, 31]);
        assert_eq!(shift_month(2026, 12, 1), (2027, 1));
        assert_eq!(shift_month(2026, 1, -1), (2025, 12));
        assert_eq!(shift_month(2026, 10, -22), (2024, 12));
        // Oct 1 2026 is a Thursday; Jun 1 2026 a Monday
        assert_eq!((first_column(2026, 10), first_column(2026, 6)), (3, 0));
    }

    #[test]
    fn moves_dates_and_times_apart() {
        let t = time::local_unix(2026, 10, 6, 14, 30);
        assert_eq!(with_date(t, 2026, 11, 2), time::local_unix(2026, 11, 2, 14, 30));
        // Jan 31 to February: its last day
        assert_eq!(with_date(time::local_unix(2026, 1, 31, 9, 0), 2026, 2, 31), time::local_unix(2026, 2, 28, 9, 0));
        assert_eq!(with_time(t, 9, 5), time::local_unix(2026, 10, 6, 9, 5));
        assert_eq!(date_label(t), "Tue, Oct 6, 2026");
        assert_eq!((hm(9 * 60), hm(13 * 60 + 5)), ("09:00".to_string(), "13:05".to_string()));
        assert_eq!(cron::parse_hm(&hm(17 * 60 + 45)), Ok(17 * 60 + 45));
    }
}
