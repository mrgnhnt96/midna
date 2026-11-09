//! A project's Summary (design C on the Project Summary canvas,
//! https://claude.ai/artifact/DPdaEwsdrQgP263HeWc76c): the terminals that closed in it lately
//! (`summary.list`), a list on the left and the one you pick on the right, with apps' panels
//! (`summary.annotate`) beside it.
//!
//! The project's … menu opens it (`keys.project_summary`). A project whose terminals have all
//! closed keeps its heading with a Summary row instead of leaving the sidebar; closing that row
//! (its ✕, or ⌘W on the Summary) lets the project go until another terminal closes in it.
use super::screen_kit::{clock, day_label, hm, now_unix, same_day, weekday_date};
use crate::actions::{Kind, overlay, overlay_context};
use crate::app::{MainWindow, Menu, Screen};
use crate::icons::Icon;
use crate::model::Glyph;
use crate::ui::close_anim;
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use midna_proto::{ActorKind, AgentKind, ClosedTerminal, LinkKind, SessionKind, StatusState, SummaryListResult, SummaryPanel};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// A project whose Summary was on screen this recently gets no Summary row when its last
/// terminal closes: it leaves the sidebar instead, as if the row was closed.
const SEEN_FOR: Duration = Duration::from_secs(30 * 60);
/// How long a close waits for its record (`summary.changed`) to dismiss it (`skip`).
const SKIP_FOR: Duration = Duration::from_secs(10);

#[derive(Default)]
pub struct SummaryPage {
    /// Every project's records, newest first.
    pub closed: Vec<ClosedTerminal>,
    pub keep_days: u32,
    /// The project on screen.
    pub project: Option<String>,
    /// The record on the right (its id); None = the newest.
    sel: Option<String>,
    /// Projects whose Summary row was closed, with the newest record then (`app-state.json`
    /// `summary_closed`). The row comes back once a newer one closes.
    pub dismissed: HashMap<String, String>,
    /// When each project's Summary last left the screen.
    seen: HashMap<String, Instant>,
    /// Projects whose last terminal just closed within `SEEN_FOR` of seeing their Summary: the
    /// record on its way is dismissed as it arrives (`apply`), and until then they keep no row.
    /// With when, and the newest record then.
    skip: HashMap<String, (Instant, Option<String>)>,
    /// Sections the user folded, and sections older than a day the user opened (those start
    /// folded). By `Section::key`.
    folded: HashSet<String>,
    opened: HashSet<String>,
    /// The list, for the heading that sticks to its top.
    scroll: ScrollHandle,
}

impl SummaryPage {
    pub fn new(dismissed: HashMap<String, String>) -> SummaryPage {
        SummaryPage { dismissed, ..Default::default() }
    }

    pub fn records<'a>(&'a self, pid: &'a str) -> impl Iterator<Item = &'a ClosedTerminal> + 'a {
        self.closed.iter().filter(move |c| c.project_id == pid)
    }

    fn newest(&self, pid: &str) -> Option<&str> {
        self.closed.iter().find(|c| c.project_id == pid).map(|c| c.closed_at.as_str())
    }

    /// A project with no terminals keeps its heading for its Summary row: it has records, and
    /// none closed since the row was closed.
    pub fn keeps(&self, pid: &str) -> bool {
        if self.skipping(pid) {
            return false;
        }
        let Some(newest) = self.newest(pid) else { return false };
        let seconds = |ts: &str| midna_proto::time::parse_rfc3339(ts).unwrap_or(0);
        self.dismissed.get(pid).is_none_or(|d| seconds(newest) > seconds(d))
    }

    fn skipping(&self, pid: &str) -> bool {
        self.skip.get(pid).is_some_and(|(at, _)| at.elapsed() < SKIP_FOR)
    }

    /// `pid`'s Summary just left the screen.
    fn saw(&mut self, pid: &str) {
        self.seen.insert(pid.to_string(), Instant::now());
    }

    /// `pid`'s last terminal is closing: whether it keeps a Summary row. Not when its Summary
    /// was on screen within `SEEN_FOR`: then the record on its way is skipped.
    fn last_closed(&mut self, pid: &str) -> bool {
        if self.seen.get(pid).is_none_or(|at| at.elapsed() >= SEEN_FOR) {
            return true;
        }
        let newest = self.newest(pid).map(str::to_string);
        self.skip.insert(pid.to_string(), (Instant::now(), newest));
        false
    }

    /// Skipped projects whose record turned up: dismiss it, as closing their row would.
    /// Whether any was (`dismissed` changed).
    fn dismiss_skipped(&mut self) -> bool {
        self.skip.retain(|_, (at, _)| at.elapsed() < SKIP_FOR);
        let arrived: Vec<(String, String)> = self
            .skip
            .iter()
            .filter_map(|(p, (_, before))| self.newest(p).filter(|n| before.as_deref() != Some(*n)).map(|n| (p.clone(), n.to_string())))
            .collect();
        for (pid, newest) in &arrived {
            self.skip.remove(pid);
            self.dismissed.insert(pid.clone(), newest.clone());
        }
        !arrived.is_empty()
    }
}

/// ⌘W is closing `ids`, still in `m.sessions`: the projects they leave with no terminals, and
/// whether each keeps its heading for a Summary row (its record is on its way). One whose
/// Summary was on screen in the last 30 minutes doesn't: it goes, its record dismissed. The
/// project whose Summary is on screen counts as seen; its row is already there, so it's left
/// out (it goes when its Summary does, `moved`).
pub fn closing(m: &mut MainWindow, ids: &[String]) -> Vec<(String, bool)> {
    let emptied: Vec<String> = m
        .groups()
        .iter()
        .filter(|g| !g.sessions.is_empty() && g.sessions.iter().all(|s| ids.contains(&s.id)))
        .filter_map(|g| g.project.map(|p| p.id.clone()))
        .collect();
    let on_screen = shown(m).map(str::to_string);
    let mut out = vec![];
    for pid in emptied {
        if on_screen.as_deref() == Some(pid.as_str()) {
            m.summary.saw(&pid);
            m.summary.last_closed(&pid);
        } else {
            out.push((pid.clone(), m.summary.last_closed(&pid)));
        }
    }
    out
}

/// The project on screen, when the Summary is.
pub fn shown(m: &MainWindow) -> Option<&str> {
    m.summary.project.as_deref().filter(|_| m.screen == Screen::Summary)
}

/// Whether `pid`'s group shows a Summary row: its Summary is on screen, or it has no
/// terminals and keeps its heading for one.
pub fn row_wanted(m: &MainWindow, pid: &str, empty: bool) -> bool {
    shown(m) == Some(pid) || (empty && m.summary.keeps(pid))
}

pub fn fetch(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    m.rpc("summary.list", json!({}), cx, |m, v, _, cx| {
        let Ok(r) = serde_json::from_value::<SummaryListResult>(v) else { return };
        apply(m, r, cx);
    });
}

pub fn apply(m: &mut MainWindow, r: SummaryListResult, cx: &mut Context<MainWindow>) {
    m.summary.closed = r.closed;
    m.summary.keep_days = r.keep_days;
    if m.summary.dismiss_skipped() {
        crate::ui::statusbar::save_state(m);
    }
    if m.summary.sel.as_ref().is_some_and(|s| !m.summary.closed.iter().any(|c| &c.id == s)) {
        m.summary.sel = None;
    }
    cx.notify();
}

/// The sidebar row with the highlight: the Summary's while it's on screen, else the selected
/// terminal's.
pub fn lit_row(m: &MainWindow) -> Option<String> {
    match shown(m) {
        Some(p) => Some(close_anim::summary_key(p)),
        None => m.selected.clone(),
    }
}

/// The screen just moved to or from a Summary: `from` had the highlight before (`lit_row`),
/// and `was` is the project whose Summary was on screen. Its row glides the highlight, and folds
/// away when it leaves the sidebar (with its whole project, when that goes too).
pub fn moved(m: &mut MainWindow, from: Option<String>, was: Option<String>, cx: &mut Context<MainWindow>) {
    if let Some(p) = &was {
        m.summary.saw(p);
    }
    let gone = was.filter(|p| shown(m) != Some(p.as_str())).and_then(|p| leaves(m, p));
    let to = lit_row(m);
    close_anim::summary(m, from, to, gone, cx);
}

/// Whether `pid`'s Summary row is gone from the sidebar, and whether its project went with it.
fn leaves(m: &MainWindow, pid: String) -> Option<(String, bool)> {
    let groups = m.groups();
    match groups.iter().find(|g| g.project.is_some_and(|p| p.id == pid)) {
        None => Some((pid, true)),
        Some(g) if !row_wanted(m, &pid, g.sessions.is_empty()) => Some((pid, false)),
        Some(_) => None,
    }
}

/// Show `pid`'s Summary (from the project's … menu, its Summary row or ⌥⌘Y).
pub fn open(m: &mut MainWindow, pid: String, window: &mut Window, cx: &mut Context<MainWindow>) {
    if shown(m) == Some(pid.as_str()) {
        return;
    }
    let (from, was) = (lit_row(m), shown(m).map(str::to_string));
    m.summary.project = Some(pid);
    m.summary.sel = None;
    if m.screen != Screen::Summary {
        m.set_screen(Screen::Summary, window, cx);
    } else {
        moved(m, from, was, cx);
    }
    fetch(m, cx);
    cx.notify();
}

/// Close `pid`'s Summary row: a project with no terminals leaves the sidebar until another
/// terminal closes in it. Leaves its Summary when it's on screen.
pub fn close_row(m: &mut MainWindow, pid: &str, window: &mut Window, cx: &mut Context<MainWindow>) {
    if let Some(newest) = m.summary.newest(pid).map(str::to_string) {
        m.summary.dismissed.insert(pid.to_string(), newest);
        crate::ui::statusbar::save_state(m);
    }
    if shown(m) == Some(pid) {
        m.set_screen(Screen::Terminal, window, cx);
    } else if let Some(gone) = leaves(m, pid.to_string()) {
        close_anim::summary(m, None, None, Some(gone), cx);
    }
    cx.notify();
}

/// A time section of the list (design C on the Summary time dividers canvas,
/// https://claude.ai/artifact/3ZwxbveCg9DZgQdDZQJWCG): the last hour, 1–3, 3–6 and 6–24 hours
/// ago, then one per local day. Each takes only what the one before it left, so the labels name
/// ranges.
#[derive(Clone, Debug, PartialEq)]
struct Section {
    key: String,
    label: String,
    /// Older than a day: starts folded.
    old: bool,
}

fn section(closed: i64, now: i64) -> Section {
    let ago = (now - closed).max(0);
    let label = match ago {
        _ if ago < 3600 => "Last hour",
        _ if ago < 3 * 3600 => "1–3 hours ago",
        _ if ago < 6 * 3600 => "3–6 hours ago",
        _ if ago < 24 * 3600 => "6–24 hours ago",
        _ => {
            let day = weekday_date(closed);
            return Section { key: day.clone(), label: day, old: true };
        }
    };
    Section { key: label.into(), label: label.into(), old: false }
}

fn closed_secs(c: &ClosedTerminal) -> i64 {
    midna_proto::time::parse_rfc3339(&c.closed_at).unwrap_or(0)
}

/// The records (newest first) in their sections, in order; empty sections don't appear.
fn sections<'a>(records: &[&'a ClosedTerminal], now: i64) -> Vec<(Section, Vec<&'a ClosedTerminal>)> {
    let mut out: Vec<(Section, Vec<&ClosedTerminal>)> = vec![];
    for c in records {
        let s = section(closed_secs(c), now);
        match out.last_mut() {
            Some((last, rows)) if last.key == s.key => rows.push(c),
            _ => out.push((s, vec![c])),
        }
    }
    out
}

/// A row's time: the section already says the day, so only the clock, plus "Yesterday" when
/// 6–24 hours ago crosses midnight.
fn row_time(closed: i64, now: i64) -> String {
    if now - closed < 24 * 3600 && !same_day(closed, now) { format!("Yesterday {}", hm(closed)) } else { hm(closed) }
}

impl SummaryPage {
    fn is_folded(&self, s: &Section) -> bool {
        self.folded.contains(&s.key) || (s.old && !self.opened.contains(&s.key))
    }

    fn set_folded(&mut self, s: &Section, fold: bool) {
        if fold {
            self.folded.insert(s.key.clone());
            self.opened.remove(&s.key);
        } else {
            self.folded.remove(&s.key);
            self.opened.insert(s.key.clone());
        }
    }
}

/// ← folds the open record's section, → unfolds it. The record stays in view either way.
fn fold_current(m: &mut MainWindow, fold: bool, cx: &mut Context<MainWindow>) {
    let Some(c) = current(m) else { return };
    let s = section(closed_secs(&c), now_unix());
    if m.summary.is_folded(&s) != fold {
        m.summary.set_folded(&s, fold);
        cx.notify();
    }
}

fn list(m: &MainWindow) -> Vec<&ClosedTerminal> {
    m.summary.project.as_deref().map(|p| m.summary.records(p).collect()).unwrap_or_default()
}

fn current(m: &MainWindow) -> Option<ClosedTerminal> {
    let list = list(m);
    m.summary.sel.as_ref().and_then(|s| list.iter().find(|c| &c.id == s)).or(list.first()).map(|c| (*c).clone())
}

fn step(m: &mut MainWindow, by: isize, cx: &mut Context<MainWindow>) {
    let ids: Vec<String> = list(m).iter().map(|c| c.id.clone()).collect();
    if ids.is_empty() {
        return;
    }
    let at = m.summary.sel.as_ref().and_then(|s| ids.iter().position(|i| i == s)).unwrap_or(0) as isize;
    m.summary.sel = Some(ids[(at + by).clamp(0, ids.len() as isize - 1) as usize].clone());
    cx.notify();
}

fn resume(m: &mut MainWindow, id: String, cx: &mut Context<MainWindow>) {
    m.menu = Menu::None;
    m.rpc("summary.resume", json!({ "id": id }), cx, |m, v, window, cx| {
        m.summary.closed.retain(|c| Some(&c.id) != v["session"]["id"].as_str().map(str::to_string).as_ref());
        m.show_opened(v, window, cx);
        fetch(m, cx);
    });
    cx.notify();
}

fn remove(m: &mut MainWindow, id: String, cx: &mut Context<MainWindow>) {
    m.menu = Menu::None;
    // Optimistic; `summary.changed` refetches. The next one down takes its place.
    let ids: Vec<String> = list(m).iter().map(|c| c.id.clone()).collect();
    let at = ids.iter().position(|i| *i == id).unwrap_or(0);
    m.summary.sel = ids.iter().filter(|i| **i != id).nth(at.min(ids.len().saturating_sub(2))).cloned();
    m.summary.closed.retain(|c| c.id != id);
    m.rpc("summary.remove", json!({ "id": id }), cx, |_, _, _, _| {});
    cx.notify();
}

fn on_key(m: &mut MainWindow, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<MainWindow>) {
    match ev.keystroke.key.as_str() {
        "down" | "j" => step(m, 1, cx),
        "up" | "k" => step(m, -1, cx),
        "left" | "h" => fold_current(m, true, cx),
        "right" | "l" => fold_current(m, false, cx),
        _ => return,
    }
    cx.stop_propagation();
}

/// The status a record ended in, as its dot's color.
fn state_color(t: &Theme, c: &ClosedTerminal) -> Hsla {
    match c.state {
        StatusState::Done => t.ok,
        StatusState::Failed => t.err,
        StatusState::NeedsYou => t.need,
        StatusState::Working => t.work,
        _ => t.dim,
    }
}

fn ring(color: Hsla, size: f32) -> Div {
    super::border_w(div().size(px(size)).flex_none().rounded_full(), 2.).border_color(color)
}

/// Who closed it, as the detail's "Closed by" says it.
fn closed_by(m: &MainWindow, c: &ClosedTerminal) -> String {
    if c.how == "exited" {
        return match c.exit_code {
            Some(code) => format!("Its process exited ({code})"),
            None => "Its process exited".into(),
        };
    }
    match (&c.by.kind, c.by.session.as_deref()) {
        (ActorKind::Human, _) => "You".into(),
        (ActorKind::Agent, Some(s)) if s == c.id => "Its agent".into(),
        (ActorKind::Agent, Some(s)) if m.sessions.iter().any(|x| x.id == s) => super::notifications::source(m, Some(s), None),
        (ActorKind::Agent, _) => "An agent".into(),
        _ => "midna".into(),
    }
}

/// "14:02" today, else "26 Oct".
fn when(ts: &str) -> String {
    let day = day_label(ts);
    if day.starts_with("today") { clock(ts).chars().take(5).collect() } else { day }
}

/// "6m", "2h 5m", "less than a minute".
fn ran(c: &ClosedTerminal) -> String {
    let secs = |ts: &str| midna_proto::time::parse_rfc3339(ts).unwrap_or(0);
    let d = (secs(&c.closed_at) - secs(&c.opened_at)).max(0);
    match d {
        0..=59 => "less than a minute".into(),
        60..=3599 => format!("{}m", d / 60),
        _ if d % 3600 < 60 => format!("{}h", d / 3600),
        _ => format!("{}h {}m", d / 3600, d % 3600 / 60),
    }
}

/// The list row's second line: the first app's `line` in its color, else how it ended.
fn line(m: &MainWindow, t: &Theme, c: &ClosedTerminal, now: i64) -> (String, Hsla) {
    let at = row_time(closed_secs(c), now);
    if let Some(p) = c.line() {
        let color = p.color.as_deref().map_or(t.dim, |x| t.status_color(x));
        return (format!("{} · {at}", p.line.clone().unwrap_or_default()), color);
    }
    let (text, color) = match c.state {
        StatusState::Failed => (c.reason.clone().unwrap_or_else(|| "failed".into()), t.err),
        _ if c.how == "exited" => (c.exit_code.map_or("exited".into(), |e| format!("exited {e}")), t.dim),
        _ => (format!("closed by {}", closed_by(m, c).to_lowercase()), t.dim),
    };
    (format!("{text} · {at}"), color)
}

fn glyph(c: &ClosedTerminal) -> Glyph {
    match (c.kind, c.agent) {
        (_, Some(AgentKind::Claude)) => Glyph::Claude,
        (_, Some(AgentKind::Codex)) => Glyph::Codex,
        (SessionKind::Monitor, _) => Glyph::Monitor,
        _ => Glyph::Shell,
    }
}

fn caps(t: &Theme, text: &str) -> Div {
    div().text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(t.dim).mb(px(6.)).child(text.to_uppercase())
}

fn card(t: &Theme) -> Div {
    div().flex().flex_col().rounded(px(10.)).border_1().border_color(t.line).bg(t.panel).overflow_hidden()
}

/// A section's heading: a small uppercase label at the rows' leading edge (never a row title's
/// look, design lesson L1), the count with failures in words, and the fold chevron last.
fn section_head(t: &Theme, sec: &Section, recs: &[&ClosedTerminal], folded: bool, rule: bool, cx: &mut Context<MainWindow>) -> Stateful<Div> {
    let failed = recs.iter().filter(|c| c.state == StatusState::Failed).count();
    let s = sec.clone();
    div()
        .id(SharedString::from(format!("sum-sec-{}", sec.key)))
        .flex()
        .flex_none()
        .items_center()
        .gap(px(8.))
        .min_h(px(30.))
        .px(px(14.))
        .py(px(4.))
        .bg(t.bg)
        .when(rule, |d| d.border_t_1().border_color(t.line))
        .cursor_pointer()
        .hover(|d| d.bg(t.raised.opacity(0.4)))
        .on_click(cx.listener(move |m, _, _, cx| {
            let fold = !m.summary.is_folded(&s);
            m.summary.set_folded(&s, fold);
            cx.notify();
        }))
        .child(div().flex_1().min_w_0().truncate().text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(t.dim).child(sec.label.to_uppercase()))
        .child(div().flex_none().text_size(px(12.)).text_color(t.dim).child(recs.len().to_string()))
        .when(failed > 0, |d| d.child(div().flex_none().text_size(px(12.)).text_color(t.err).child(format!("· {failed} failed"))))
        .child(Icon::Chevron.el(12., t.dim).when(folded, |s| s.with_transformation(Transformation::rotate(radians(-std::f32::consts::FRAC_PI_2)))))
}

fn row(m: &MainWindow, t: &Theme, c: &ClosedTerminal, on: bool, now: i64, cx: &mut Context<MainWindow>) -> Stateful<Div> {
    let (text, color) = line(m, t, c, now);
    let id = c.id.clone();
    div()
        .id(SharedString::from(format!("sum-row-{}", c.id)))
        .relative()
        .flex()
        .flex_col()
        .gap(px(2.))
        .px(px(14.))
        .py(px(8.))
        .cursor_pointer()
        .when(on, |d| d.bg(t.raised).child(div().absolute().left_0().top_0().bottom_0().w(px(2.)).bg(t.accent)))
        .when(!on, |d| d.hover(|s| s.bg(t.raised.opacity(0.5))))
        .on_click(cx.listener(move |m, _, _, cx| {
            m.summary.sel = Some(id.clone());
            m.menu = Menu::None;
            cx.notify();
        }))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(9.))
                .child(div().w(px(12.)).flex_none().flex().items_center().child(ring(state_color(t, c), 10.)))
                .child(div().flex_1().min_w_0().font_weight(FontWeight::MEDIUM).truncate().child(c.name.clone()))
                .child(Icon::from_glyph(glyph(c)).el(14., t.dim)),
        )
        .child(div().pl(px(21.)).text_size(px(12.)).text_color(color).truncate().child(text))
}

pub fn render(m: &mut MainWindow, t: &Theme, _window: &mut Window, cx: &mut Context<MainWindow>) -> AnyElement {
    let m = &*m;
    let pid = m.summary.project.clone().unwrap_or_default();
    let project = m.projects.iter().find(|p| p.id == pid).map(|p| p.name.clone()).unwrap_or_default();
    let records = list(m);
    let cur = current(m);
    let days = match m.summary.keep_days {
        0 | 1 => "today".to_string(),
        n => format!("last {n} days"),
    };

    let header = div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(10.))
        .h(px(52.))
        .px(px(16.))
        .bg(t.panel)
        .border_b_1()
        .border_color(t.line)
        .child(div().text_size(px(15.)).font_weight(FontWeight::BOLD).child("Summary"))
        .child(div().text_color(t.dim).truncate().child(project))
        .child(div().flex_1())
        .child(div().flex_none().text_size(px(12.)).text_color(t.dim).child(days.clone()));

    let now = now_unix();
    let groups = sections(&records, now);
    let mut rows = div().id("sum-list").flex().flex_col().flex_1().min_h_0().overflow_y_scroll().track_scroll(&m.summary.scroll).pb(px(6.));
    for (ix, (sec, recs)) in groups.iter().enumerate() {
        let folded = m.summary.is_folded(sec);
        let mut group = div().flex().flex_col().child(section_head(t, sec, recs, folded, ix > 0, cx));
        for c in recs {
            let on = cur.as_ref().is_some_and(|x| x.id == c.id);
            // A folded section still shows the open record, so the selection never disappears.
            if !folded || on {
                group = group.child(row(m, t, c, on, now, cx));
            }
        }
        rows = rows.child(group);
    }
    // The heading of the section scrolled under the list's top edge sticks there.
    let scroll = &m.summary.scroll;
    let top = scroll.top_item();
    let stuck = scroll.bounds_for_item(top).filter(|b| b.top() < scroll.bounds().top() - px(1.)).and_then(|_| groups.get(top));
    let pane = div()
        .relative()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .child(rows)
        .children(stuck.map(|(sec, recs)| div().absolute().top_0().left_0().right_0().border_b_1().border_color(t.line).child(section_head(t, sec, recs, m.summary.is_folded(sec), false, cx))));

    let left = div().flex().flex_col().flex_none().w(px(330.)).h_full().border_r_1().border_color(t.line).child(header).child(pane);

    let right: AnyElement = match cur {
        Some(c) => detail(m, t, &c, cx).into_any_element(),
        None => div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(6.))
            .text_color(t.dim)
            .child(Icon::Summary.el(22., t.dim))
            .child(format!("No terminals closed here in the {days}."))
            .into_any_element(),
    };

    div()
        .id("summary")
        .key_context(overlay_context(Kind::Screen, overlay::SUMMARY))
        .track_focus(&m.overlay_focus)
        .on_key_down(cx.listener(on_key))
        .on_action(cx.listener(|m, _: &crate::actions::Dismiss, w, cx| {
            if m.menu == Menu::Summary {
                m.menu = Menu::None;
                cx.notify();
            } else {
                m.set_screen(Screen::Terminal, w, cx);
            }
        }))
        // ⌘↩ and ⌘⌫ (the approve and deny keys) act on the record on screen.
        .on_action(cx.listener(|m, _: &crate::actions::ApproveOnce, _, cx| {
            if let Some(c) = current(m) {
                resume(m, c.id, cx);
            }
        }))
        .on_action(cx.listener(|m, _: &crate::actions::Deny, _, cx| {
            if let Some(c) = current(m) {
                remove(m, c.id, cx);
            }
        }))
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .bg(t.bg)
        .child(left)
        .child(right)
        .into_any_element()
}

/// The right side: everything about one closed terminal.
fn detail(m: &MainWindow, t: &Theme, c: &ClosedTerminal, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let menu_open = m.menu == Menu::Summary;
    let resume_label = if c.conversation_id.is_some() { "Resume conversation" } else { "Open again" };
    let (resume_keys, remove_keys) = (m.key_label("keys.approve"), m.key_label("keys.deny"));
    let (r_id, x_id, b_id) = (c.id.clone(), c.id.clone(), c.id.clone());
    let reopen_tip = format!("{} in a new tab ({resume_keys})", if c.conversation_id.is_some() { "Resume its conversation" } else { "Open it again" });

    let menu = deferred(
        anchored().anchor(Anchor::TopRight).snap_to_window_with_margin(px(8.)).child(
            super::sidebar::menu_box(t)
                .occlude()
                .mt(px(34.))
                .child(super::sidebar::menu_item(t, "sum-resume", resume_label, "", cx.listener(move |m, _, _, cx| resume(m, r_id.clone(), cx))).child(super::header::key_chip(t, resume_keys.into())))
                .child(div().h(px(1.)).mx(px(6.)).my(px(4.)).bg(t.line))
                .child(super::sidebar::menu_item(t, "sum-remove", "Remove from summary", "", cx.listener(move |m, _, _, cx| remove(m, x_id.clone(), cx))).child(super::header::key_chip(t, remove_keys.into()))),
        ),
    )
    .with_priority(1);

    let more = div()
        .id("sum-more")
        .relative()
        .size(px(28.))
        .rounded(px(6.))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|s| s.bg(t.raised))
        .when(menu_open, |d| d.bg(t.raised))
        .tooltip(super::header::tip("Actions"))
        .on_click(cx.listener(|m, _, _, cx| {
            cx.stop_propagation();
            m.menu = if m.menu == Menu::Summary { Menu::None } else { Menu::Summary };
            cx.notify();
        }))
        .child(Icon::Dots.el(14., t.dim))
        // Anchored at the button's right edge, so the menu hangs under it.
        .when(menu_open, |d| d.child(div().absolute().top_0().right_0().child(menu)));

    let header = div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(10.))
        .h(px(52.))
        .px(px(20.))
        .bg(t.panel)
        .border_b_1()
        .border_color(t.line)
        .child(ring(state_color(t, c), 12.))
        .child(div().min_w_0().text_size(px(15.)).font_weight(FontWeight::BOLD).truncate().child(c.name.clone()))
        .child(Icon::from_glyph(glyph(c)).el(14., t.dim))
        .child(div().flex_1())
        .child(
            super::screen_kit::btn(t, "sum-reopen", "Reopen in new tab")
                .tooltip(super::header::tip(reopen_tip))
                .on_click(cx.listener(move |m, _, _, cx| resume(m, b_id.clone(), cx))),
        )
        .child(more);

    let mut facts: Vec<(&str, String, bool)> = vec![("Closed", when_long(&c.closed_at), false), ("Closed by", closed_by(m, c), false)];
    if c.state == StatusState::Failed {
        facts.push(("Ended", c.reason.clone().unwrap_or_else(|| "failed".into()), false));
    }
    let mut run = vec![ran(c)];
    if c.turns > 0 {
        run.push(if c.turns == 1 { "1 prompt".into() } else { format!("{} prompts", c.turns) });
    }
    if let Some(cost) = c.cost_usd {
        run.push(format!("${cost:.2}"));
    }
    facts.push(("Ran", run.join(" · "), false));
    if let Some(w) = &c.worktree {
        facts.push(("Worktree", w.clone(), true));
    }
    if let Some(b) = &c.branch {
        facts.push(("Branch", b.clone(), true));
    }
    let mut grid = div().flex().flex_col().gap(px(6.));
    for (label, value, mono) in facts {
        grid = grid.child(
            div()
                .flex()
                .gap(px(12.))
                .child(div().flex_none().w(px(110.)).text_color(t.dim).child(label))
                .child(div().min_w_0().when(mono, |d| d.font_family(t.mono_font.clone()).text_size(px(12.))).child(value)),
        );
    }
    // The agent's own session id (`claude --resume <id>`), with a copy button.
    if let Some(conv) = c.conversation_id.clone() {
        let label = match c.agent {
            Some(AgentKind::Claude) => "Claude session",
            Some(AgentKind::Codex) => "Codex session",
            None => "Session",
        };
        let copy = conv.clone();
        grid = grid.child(
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .child(div().flex_none().w(px(110.)).text_color(t.dim).child(label))
                .child(div().min_w_0().truncate().font_family(t.mono_font.clone()).text_size(px(12.)).child(conv))
                .child(
                    div()
                        .id("sum-copy-session")
                        .flex_none()
                        .size(px(22.))
                        .rounded(px(5.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(|s| s.bg(t.raised))
                        .tooltip(super::header::tip("Copy session id"))
                        .on_click(cx.listener(move |m, _, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                            m.toast("Copied the session id", cx);
                        }))
                        .child(Icon::Copy.el(13., t.dim)),
                ),
        );
    }

    let prompts = (!c.prompts.is_empty()).then(|| {
        let mut col = div().flex().flex_col().gap(px(4.));
        for p in &c.prompts {
            col = col.child(
                div()
                    .flex()
                    .gap(px(10.))
                    .px(px(10.))
                    .py(px(6.))
                    .rounded(px(6.))
                    .bg(t.panel)
                    .child(div().flex_none().w(px(52.)).text_size(px(12.)).text_color(t.dim).child(when(&p.at)))
                    .child(div().flex_1().min_w_0().child(p.text.trim().to_string())),
            );
        }
        let more = (c.turns as usize > c.prompts.len()).then(|| div().text_size(px(12.)).text_color(t.dim).child(format!("and {} more", c.turns as usize - c.prompts.len())));
        div().flex().flex_col().child(caps(t, "Your prompts")).child(col).children(more)
    });

    let main = div().flex_1().min_w_0().flex().flex_col().gap(px(18.)).child(grid).children(prompts);

    let mut side = div().flex_none().w(px(300.)).flex().flex_col().gap(px(12.));
    for (owner, p) in &c.apps {
        side = side.child(panel(t, owner, p));
    }
    if !c.links.is_empty() {
        let mut links = card(t).p(px(12.)).gap(px(6.)).child(caps(t, "Links").mb(px(0.)));
        for (i, l) in c.links.iter().enumerate() {
            let (target, file) = (l.target.clone(), l.kind == LinkKind::File);
            links = links.child(
                div()
                    .id(SharedString::from(format!("sum-link-{i}")))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .cursor_pointer()
                    .text_color(t.accent)
                    .hover(|s| s.text_color(t.fg))
                    .tooltip(super::header::tip(l.target.clone()))
                    .on_click(cx.listener(move |m, _, _, cx| open_link(m, &target, file, cx)))
                    .child(link_icon(l.kind).el(13., t.dim))
                    .child(div().min_w_0().truncate().child(l.title.clone())),
            );
        }
        side = side.child(links);
    }

    div()
        .id("sum-detail")
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .child(header)
        .child(div().id("sum-detail-body").flex_1().min_h_0().overflow_y_scroll().flex().items_start().gap(px(18.)).px(px(20.)).py(px(18.)).child(main).child(side))
}

/// "today 14:02" or "26 Oct 14:02".
fn when_long(ts: &str) -> String {
    let day = day_label(ts);
    if day.starts_with("today") { day } else { format!("{day} {}", clock(ts).chars().take(5).collect::<String>()) }
}

fn link_icon(k: LinkKind) -> Icon {
    match k {
        LinkKind::Pr => Icon::Pr,
        LinkKind::Artifact => Icon::Artifact,
        LinkKind::Web => Icon::Globe,
        LinkKind::File => Icon::File,
    }
}

/// A link from a closed terminal: URLs open in their app; a file opens in the IDE rules pick,
/// as a click in a terminal would (its terminal is gone, so any open one stands in for it).
fn open_link(m: &mut MainWindow, target: &str, file: bool, cx: &mut Context<MainWindow>) {
    if crate::dev::var_os("MIDNA_DEBUG_TERM").is_some() {
        eprintln!("midna-app debug-term: link {target}"); // dev runs never launch apps
    } else if file {
        match m.sessions.first().map(|s| s.id.clone()) {
            Some(sid) => crate::terminal::open_file(m.backend.clone(), sid, target.to_string(), None),
            None => cx.open_url(&format!("file://{target}")),
        }
    } else {
        cx.open_url(target);
    }
}

/// An app's panel: its name, the title (a link when it has a URL), a colored badge with a
/// subtitle, and its buttons.
fn panel(t: &Theme, owner: &str, p: &SummaryPanel) -> impl IntoElement + use<> {
    let color = p.color.as_deref().map_or(t.accent, |c| t.status_color(c));
    let app = p.app.clone().unwrap_or_else(|| owner.to_string());
    let initial: String = app.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().collect()).unwrap_or_default();
    let link = |id: String, text: String, url: Option<String>, fg: Hsla, bold: bool| {
        let d = div().id(SharedString::from(id)).min_w_0().text_color(fg).when(bold, |d| d.font_weight(FontWeight::BOLD)).child(text);
        match url {
            Some(u) => d.cursor_pointer().hover(|s| s.text_color(t.accent)).on_click(move |_, _, cx| cx.open_url(&u)),
            None => d,
        }
    };
    let mut buttons = div().flex().flex_wrap().gap(px(6.));
    for (i, b) in p.buttons.iter().enumerate() {
        let url = b.open.clone();
        buttons = buttons.child(super::screen_kit::btn(t, SharedString::from(format!("sum-app-{owner}-{i}")), b.label.clone()).on_click(move |_, _, cx| cx.open_url(&url)));
    }
    card(t)
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(12.))
                .py(px(10.))
                .border_b_1()
                .border_color(t.line)
                .child(
                    div()
                        .size(px(18.))
                        .flex_none()
                        .rounded(px(4.))
                        .bg(t.accent)
                        .text_color(t.accent_fg)
                        .text_size(px(10.))
                        .font_weight(FontWeight::BOLD)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(initial),
                )
                .child(div().flex_1().min_w_0().font_weight(FontWeight::BOLD).truncate().child(app)),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .px(px(12.))
                .py(px(10.))
                .child(link(format!("sum-app-{owner}-title"), p.title.clone(), p.url.clone(), t.fg, true))
                .when(p.badge.is_some() || p.subtitle.is_some(), |d| {
                    d.child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(8.))
                            .children(p.badge.clone().map(|b| div().px(px(8.)).rounded(px(10.)).bg(color.opacity(0.16)).text_color(color).text_size(px(12.)).font_weight(FontWeight::BOLD).child(b)))
                            .children(p.subtitle.clone().map(|s| link(format!("sum-app-{owner}-sub"), s, p.subtitle_url.clone(), t.accent, false).text_size(px(12.5)))),
                    )
                })
                .when(!p.buttons.is_empty(), |d| d.child(buttons)),
        )
}

/// The Summary row in a project's group: the list icon, "Summary", and when the last terminal
/// closed where a terminal's icon sits (its ✕ on hover). A click shows it; its ✕ closes the
/// row (`close_row`).
pub fn sidebar_row(m: &MainWindow, t: &Theme, pid: &str, compact: bool, cx: &mut Context<MainWindow>) -> AnyElement {
    let on = shown(m) == Some(pid);
    // The highlight on its way to or from a terminal's row (`ui::close_anim::summary`); folding
    // away, the fold draws it.
    let hl = m.close_anim.highlight(&close_anim::summary_key(pid), on);
    let own = on && hl == close_anim::Highlight::Own;
    let moving = match hl {
        close_anim::Highlight::At { top, h } if m.close_anim.summary_leaving(pid).is_none() => Some((top, h)),
        _ => None,
    };
    let last = m.summary.newest(pid).map(|ts| super::screen_kit::ago(Some(ts)));
    let (open_id, close_id) = (pid.to_string(), pid.to_string());
    let group = SharedString::from(format!("sum-side-{pid}"));
    div()
        .id(SharedString::from(format!("summary-row-{pid}")))
        .relative()
        .flex()
        .items_center()
        .gap(px(9.))
        .w_full()
        .px(px(14.))
        .py(px(if compact { 5. } else { 8. }))
        .cursor_pointer()
        .group(group.clone())
        .when_some(moving, |d, (top, h)| d.child(super::sidebar::highlight(t).top(px(top)).h(px(h))))
        .when(own, |d| d.bg(t.raised).child(div().absolute().left_0().top_0().bottom_0().w(px(2.)).bg(t.accent)))
        .when(!on, |d| d.hover(|s| s.bg(t.raised.opacity(0.5))))
        .tooltip(super::header::tip_keys("This project's closed terminals", "keys.project_summary"))
        .on_click(cx.listener(move |m, _, w, cx| open(m, open_id.clone(), w, cx)))
        .child(div().w(px(12.)).flex_none().flex().items_center().child(Icon::Summary.el(12., if on { t.accent } else { t.dim })))
        .child(div().flex_1().min_w_0().font_weight(FontWeight::MEDIUM).truncate().child("Summary"))
        .child(
            div()
                .relative()
                .flex_none()
                .min_w(px(16.))
                .h(px(16.))
                .flex()
                .items_center()
                .justify_end()
                .children(last.map(|a| div().text_size(px(11.5)).text_color(t.dim).whitespace_nowrap().group_hover(group.clone(), |s| s.opacity(0.)).child(a)))
                .child(
                    div()
                        .id(SharedString::from(format!("summary-x-{pid}")))
                        .absolute()
                        .top_0()
                        .right_0()
                        .size(px(16.))
                        .rounded(px(4.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .opacity(0.)
                        .group_hover(group, |s| s.opacity(1.))
                        .hover(|s| s.bg(t.line))
                        .tooltip(super::header::tip("Close the Summary"))
                        .on_click(cx.listener(move |m, _, w, cx| {
                            cx.stop_propagation();
                            close_row(m, &close_id, w, cx);
                        }))
                        .child(Icon::Cross.el(10., t.dim)),
                ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{SummaryPage, row_time, section, sections};
    use crate::ui::screen_kit::{hm, weekday_date};
    use midna_proto::{Actor, ClosedTerminal, SessionKind, StatusState};

    fn rec(project: &str, closed_at: &str) -> ClosedTerminal {
        ClosedTerminal {
            id: format!("s-{closed_at}"),
            project_id: project.into(),
            name: "t".into(),
            kind: SessionKind::Shell,
            agent: None,
            cwd: "/tmp".into(),
            conversation_id: None,
            agent_args: vec![],
            worktree: None,
            branch: None,
            opened_at: closed_at.into(),
            closed_at: closed_at.into(),
            how: "close".into(),
            by: Actor::human(),
            state: StatusState::Idle,
            exit_code: None,
            reason: None,
            prompts: vec![],
            turns: 0,
            cost_usd: None,
            links: vec![],
            apps: Default::default(),
        }
    }

    #[test]
    fn a_closed_row_comes_back_when_a_newer_terminal_closes() {
        let mut page = SummaryPage { closed: vec![rec("p1", "2026-10-27T12:00:00Z")], ..Default::default() };
        assert!(page.keeps("p1"));
        assert!(!page.keeps("p2"), "nothing closed there");
        page.dismissed.insert("p1".into(), "2026-10-27T12:00:00Z".into());
        assert!(!page.keeps("p1"));
        page.closed.insert(0, rec("p1", "2026-10-27T13:00:00Z"));
        assert!(page.keeps("p1"));
    }

    #[test]
    fn closing_the_last_terminal_keeps_a_row_unless_the_summary_was_just_seen() {
        let mut page = SummaryPage { closed: vec![rec("p1", "2026-10-27T12:00:00Z")], ..Default::default() };
        assert!(page.last_closed("p1"), "never seen");
        assert!(!page.dismiss_skipped());
        page.saw("p1");
        assert!(!page.last_closed("p1"), "seen just now");
        assert!(!page.keeps("p1"), "no row while its record is on its way");
        assert!(!page.dismiss_skipped(), "its record hasn't turned up yet");
        page.closed.insert(0, rec("p1", "2026-10-27T13:00:00Z"));
        assert!(page.dismiss_skipped());
        assert_eq!(page.dismissed.get("p1").map(String::as_str), Some("2026-10-27T13:00:00Z"));
        assert!(!page.keeps("p1"), "dismissed");
        page.closed.insert(0, rec("p1", "2026-10-27T14:00:00Z"));
        assert!(page.keeps("p1"), "a later close brings the row back");
    }

    #[test]
    fn a_project_never_closed_before_skips_its_first_record_too() {
        let mut page = SummaryPage::default();
        page.saw("p1");
        assert!(!page.last_closed("p1"));
        page.closed.push(rec("p1", "2026-10-27T12:00:00Z"));
        assert!(page.dismiss_skipped());
        assert!(!page.keeps("p1"));
    }

    #[test]
    fn sections_name_ranges_that_do_not_overlap() {
        let now = 1_800_000_000;
        let label = |ago: i64| section(now - ago, now).label;
        assert_eq!(label(0), "Last hour");
        assert_eq!(label(3599), "Last hour");
        assert_eq!(label(3600), "1–3 hours ago");
        assert_eq!(label(3 * 3600), "3–6 hours ago");
        assert_eq!(label(6 * 3600), "6–24 hours ago");
        assert_eq!(label(24 * 3600 - 1), "6–24 hours ago");
        let day = section(now - 30 * 3600, now);
        assert_eq!(day.label, weekday_date(now - 30 * 3600));
        assert!(day.old && !section(now - 3600, now).old);
    }

    #[test]
    fn records_group_in_order_and_empty_sections_are_skipped() {
        let now = 1_800_000_000;
        let at = |ago: i64| midna_proto::time::format_unix(now - ago);
        let recs = [rec("p", &at(60)), rec("p", &at(120)), rec("p", &at(7 * 3600)), rec("p", &at(50 * 3600))];
        let refs: Vec<&_> = recs.iter().collect();
        let groups = sections(&refs, now);
        let got: Vec<(&str, usize)> = groups.iter().map(|(s, r)| (s.label.as_str(), r.len())).collect();
        assert_eq!(got[..2], [("Last hour", 2), ("6–24 hours ago", 1)]);
        assert_eq!(got.len(), 3);
    }

    #[test]
    fn a_row_says_yesterday_only_when_its_range_crosses_midnight() {
        let now = 1_800_000_000;
        assert_eq!(row_time(now - 600, now), hm(now - 600));
        // 30 hours back is always another day, but its section already names the day.
        assert_eq!(row_time(now - 30 * 3600, now), hm(now - 30 * 3600));
        let late = now - 20 * 3600;
        let want = if crate::ui::screen_kit::same_day(late, now) { hm(late) } else { format!("Yesterday {}", hm(late)) };
        assert_eq!(row_time(late, now), want);
    }
}
