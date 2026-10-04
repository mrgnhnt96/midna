//! Insights: graphs, not a report. Replaces the terminal pane (the sidebar stays).
//!
//! Range switcher (Today / Week / Month) → headline numbers with vs-previous deltas →
//! agent turns over time stacked by project → spend over time → working vs waiting on you
//! per terminal → approvals and triggers → the activity log with filters (project,
//! terminal, actor, kind, "while you were away"). Every number comes from midnad
//! (`insights.summary`, `insights.series`, `insights.activity`), i.e. from events only.
use crate::app::{MainWindow, Screen};
use crate::backend::Backend;
use crate::model::{Event, Project, Session, parse_list, parse_rfc3339};
use crate::theme::Theme;
use crate::ui::charts::{self, ChartData, ChartState, HRow, Series, Unit};
use gpui_kit::prelude::*;
use gpui_kit::*;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

// ------------------------------------------------------------------ data

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Range {
    Today,
    Week,
    Month,
}

impl Range {
    fn wire(self) -> &'static str {
        match self {
            Range::Today => "today",
            Range::Week => "week",
            Range::Month => "month",
        }
    }
    fn label(self) -> &'static str {
        match self {
            Range::Today => "Today",
            Range::Week => "Week",
            Range::Month => "Month",
        }
    }
    fn vs(self) -> &'static str {
        match self {
            Range::Today => "vs yesterday",
            Range::Week => "vs prior week",
            Range::Month => "vs prior 30d",
        }
    }
    fn per(self) -> &'static str {
        match self {
            Range::Today => "per hour, today",
            Range::Week => "per day, last 7 days",
            Range::Month => "per day, last 30 days",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct SBucket {
    start: String,
    total: f64,
    values: HashMap<String, f64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct SGroup {
    key: String,
    label: String,
    total: f64,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct SeriesData {
    unit: String,
    bucket: String,
    buckets: Vec<SBucket>,
    groups: Vec<SGroup>,
    total: f64,
    previous_total: f64,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Totals {
    turns: f64,
    messages: f64,
    spend_usd: f64,
    working_secs: f64,
    waiting_secs: f64,
    approvals: f64,
    triggers_fired: f64,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Row {
    key: String,
    label: String,
    totals: Totals,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Summary {
    totals: Totals,
    vs_previous: Totals,
    rows: Vec<Row>,
}

#[derive(Clone, Default)]
struct Data {
    range: Option<Range>,
    summary: Summary,
    turns: SeriesData,
    spend: SeriesData,
    approvals: SeriesData,
    triggers: SeriesData,
    terminals: Vec<Row>,
    activity: Vec<Event>,
    /// `session.opened` / `session.renamed` events: names and projects of closed terminals.
    opened: Vec<Event>,
    projects: Vec<Project>,
    sessions: Vec<Session>,
}

// ------------------------------------------------------------------ filters

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KindFilter {
    Turns,
    NeedsYou,
    Status,
    Terminals,
    Rules,
    Triggers,
    Settings,
}

const KINDS: &[(KindFilter, &str)] = &[
    (KindFilter::Turns, "Turns & messages"),
    (KindFilter::NeedsYou, "Needs you"),
    (KindFilter::Status, "Status"),
    (KindFilter::Terminals, "Terminals"),
    (KindFilter::Rules, "Rules"),
    (KindFilter::Triggers, "Triggers"),
    (KindFilter::Settings, "Settings"),
];

impl KindFilter {
    fn matches(self, kind: &str) -> bool {
        match self {
            KindFilter::Turns => kind.starts_with("agent."),
            KindFilter::NeedsYou => kind.starts_with("needs_you."),
            KindFilter::Status => kind == "session.status",
            KindFilter::Terminals => kind.starts_with("session.") && kind != "session.status",
            KindFilter::Rules => kind.starts_with("rule."),
            KindFilter::Triggers => kind.starts_with("trigger."),
            KindFilter::Settings => kind.starts_with("settings."),
        }
    }
}

const ACTORS: &[(&str, &str)] = &[("human", "You"), ("agent", "Agents"), ("trigger", "Triggers"), ("system", "midnad")];

#[derive(Clone, Default)]
struct Filters {
    project: Option<String>,
    terminal: Option<String>,
    actor: Option<String>,
    kind: Option<KindFilter>,
    away: bool,
}

impl Filters {
    fn any(&self) -> bool {
        self.project.is_some() || self.terminal.is_some() || self.actor.is_some() || self.kind.is_some() || self.away
    }
}

// ------------------------------------------------------------------ view

pub struct InsightsView {
    backend: Arc<dyn Backend>,
    main: WeakEntity<MainWindow>,
    range: Range,
    data: Data,
    loading: bool,
    error: Option<String>,
    filters: Filters,
    turns_chart: ChartState,
    spend_chart: ChartState,
    approvals_chart: ChartState,
    triggers_chart: ChartState,
    term_hover: Option<usize>,
    /// When the window last lost focus, and when it came back (for "while you were away").
    went_away: Option<i64>,
    came_back: Option<i64>,
    last_render: Instant,
    stale: bool,
    fetch_gen: u64,
    scroll: ScrollHandle,
    _subs: Vec<Subscription>,
}

fn turns_state(v: &mut InsightsView) -> &mut ChartState {
    &mut v.turns_chart
}
fn spend_state(v: &mut InsightsView) -> &mut ChartState {
    &mut v.spend_chart
}
fn approvals_state(v: &mut InsightsView) -> &mut ChartState {
    &mut v.approvals_chart
}
fn triggers_state(v: &mut InsightsView) -> &mut ChartState {
    &mut v.triggers_chart
}
fn term_hover(v: &mut InsightsView) -> &mut Option<usize> {
    &mut v.term_hover
}

impl MainWindow {
    /// The Insights view, created on first use.
    pub fn insights_view(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InsightsView> {
        if let Some(v) = &self.insights {
            return v.clone();
        }
        let backend = self.backend.clone();
        let main = cx.entity().downgrade();
        let v = cx.new(|cx| InsightsView::new(backend, main, window, cx));
        self.insights = Some(v.clone());
        v
    }
}

impl InsightsView {
    pub fn new(backend: Arc<dyn Backend>, main: WeakEntity<MainWindow>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let range = match crate::dev::var("MIDNA_INSIGHTS_RANGE").as_deref() {
            Ok("week") => Range::Week,
            Ok("month") => Range::Month,
            _ => Range::Today,
        };
        let sub = cx.observe_window_activation(window, |v, window, _| {
            if window.is_window_active() {
                if v.went_away.is_some() {
                    v.came_back = Some(now_unix());
                }
            } else {
                v.went_away = Some(now_unix());
                v.came_back = None;
            }
        });
        let mut v = InsightsView {
            backend,
            main,
            range,
            data: Data::default(),
            loading: true,
            error: None,
            filters: Filters::default(),
            turns_chart: ChartState::default(),
            spend_chart: ChartState::default(),
            approvals_chart: ChartState::default(),
            triggers_chart: ChartState::default(),
            term_hover: None,
            went_away: None,
            came_back: None,
            last_render: Instant::now(),
            stale: false,
            fetch_gen: 0,
            scroll: ScrollHandle::new(),
            _subs: vec![sub],
        };
        // dev (screenshots): preset hover on the turns chart / spend chart / a terminal row
        let env_i = |k: &str| crate::dev::var(k).ok().and_then(|x| x.parse::<usize>().ok());
        v.turns_chart.hover = env_i("MIDNA_INSIGHTS_HOVER");
        v.spend_chart.hover = env_i("MIDNA_INSIGHTS_HOVER_SPEND");
        v.term_hover = env_i("MIDNA_INSIGHTS_HOVER_ROW");
        if crate::dev::var("MIDNA_INSIGHTS_AWAY").is_ok() {
            v.filters.away = true;
        }
        v.fetch(cx);
        v
    }

    /// Called by the main window for every daemon event; refetches (coalesced) while visible.
    pub fn on_event(&mut self, e: &Event, cx: &mut Context<Self>) {
        let k = e.kind.as_str();
        let relevant = ["agent.", "session.", "needs_you.", "trigger.", "rule.", "settings.changed", "project."].iter().any(|p| k.starts_with(p));
        if !relevant || self.stale {
            return;
        }
        self.stale = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1500)).await;
            let _ = this.update(cx, |v, cx| {
                v.stale = false;
                // only while on screen (rendered recently); otherwise refetch on next open
                if v.last_render.elapsed() < Duration::from_secs(3) {
                    v.fetch(cx);
                } else {
                    v.data.range = None;
                }
            });
        })
        .detach();
    }

    fn set_range(&mut self, r: Range, cx: &mut Context<Self>) {
        if self.range != r {
            self.range = r;
            self.turns_chart.hover = None;
            self.spend_chart.hover = None;
            self.fetch(cx);
        }
    }

    fn fetch(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        self.fetch_gen += 1;
        let generation = self.fetch_gen;
        let backend = self.backend.clone();
        let range = self.range;
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    let r = range.wire();
                    let call = |m: &str, p: Value| backend.call(m, p);
                    let series = |metric: &str, by: Option<&str>| -> anyhow::Result<SeriesData> {
                        let mut p = json!({"range": r, "metric": metric});
                        if let Some(b) = by {
                            p["by"] = json!(b);
                        }
                        Ok(serde_json::from_value(call("insights.series", p)?)?)
                    };
                    let today = local_day_start(now_unix());
                    let since = match range {
                        Range::Today => today,
                        Range::Week => today - 6 * 86_400,
                        Range::Month => today - 29 * 86_400,
                    };
                    let summary: Summary = serde_json::from_value(call("insights.summary", json!({"range": r}))?)?;
                    let terms: Summary = serde_json::from_value(call("insights.summary", json!({"range": r, "by": "terminal"}))?)?;
                    anyhow::Ok(Data {
                        range: Some(range),
                        summary,
                        turns: series("turns", Some("project"))?,
                        spend: series("spend", None)?,
                        approvals: series("approvals", None)?,
                        triggers: series("triggers", None)?,
                        terminals: terms.rows,
                        opened: call("events.list", json!({"filter": {"kinds": ["session.opened", "session.renamed"]}, "limit": 5000})).map(|v| parse_list(&v)).unwrap_or_default(),
                        activity: call("insights.activity", json!({"since": crate::model::rfc3339_from_unix(since), "limit": 1000})).map(|v| parse_list(&v)).unwrap_or_default(),
                        projects: call("project.list", json!({})).map(|v| parse_list(&v)).unwrap_or_default(),
                        sessions: call("session.list", json!({})).map(|v| parse_list(&v)).unwrap_or_default(),
                    })
                })
                .await;
            let _ = this.update(cx, |v, cx| {
                if v.fetch_gen != generation {
                    return;
                }
                v.loading = false;
                match res {
                    Ok(d) => {
                        v.data = d;
                        if crate::dev::var("MIDNA_INSIGHTS_SCROLL").is_ok() {
                            // dev: screenshot the log (after a layout pass)
                            cx.spawn(async move |this, cx| {
                                cx.background_executor().timer(Duration::from_millis(400)).await;
                                let _ = this.update(cx, |v, cx| {
                                    v.scroll.scroll_to_item(LOG_INDEX);
                                    cx.notify();
                                });
                            })
                            .detach();
                        }
                        v.error = None;
                    }
                    Err(e) => v.error = Some(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(m) = self.main.upgrade() {
            m.update(cx, |m, cx| m.set_screen(Screen::Terminal, window, cx));
        }
    }

    fn open_terminal(&mut self, sid: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.data.sessions.iter().any(|s| s.id == sid) {
            return;
        }
        if let Some(m) = self.main.upgrade() {
            m.update(cx, |m, cx| m.select(sid, window, cx));
        }
    }

    /// Show the log filtered (and scroll to it).
    fn filter_log(&mut self, f: impl FnOnce(&mut Filters), cx: &mut Context<Self>) {
        f(&mut self.filters);
        self.scroll.scroll_to_item(LOG_INDEX);
        cx.notify();
    }

    // -------------------------------------------------------------- derived

    /// Stable color per project: its sidebar order picks the palette slot.
    fn project_color(&self, t: &Theme, key: &str) -> Hsla {
        match self.data.projects.iter().position(|p| p.id == key) {
            Some(i) => charts::cat_color(t, i),
            None => charts::other_color(t),
        }
    }

    fn project_name(&self, key: &str) -> String {
        self.data.projects.iter().find(|p| p.id == key).map(|p| p.name.clone()).unwrap_or_else(|| if key == "none" { "No project".into() } else { key.to_string() })
    }

    fn session_names(&self) -> HashMap<String, String> {
        let mut m: HashMap<String, String> = HashMap::new();
        for e in self.data.opened.iter().chain(self.data.activity.iter().rev()) {
            if let (Some(sid), Some(name)) = (&e.session_id, e.data.get("name").and_then(Value::as_str))
                && (e.kind == "session.opened" || e.kind == "session.renamed")
            {
                m.insert(sid.clone(), name.to_string());
            }
        }
        for s in &self.data.sessions {
            m.insert(s.id.clone(), s.name.clone());
        }
        for r in &self.data.terminals {
            m.entry(r.key.clone()).or_insert_with(|| r.label.clone());
        }
        m
    }

    fn turns_chart_data(&self, t: &Theme) -> ChartData {
        let s = &self.data.turns;
        // stack order: sidebar project order (stable colors), others last
        let mut keys: Vec<String> = s.groups.iter().map(|g| g.key.clone()).collect();
        keys.sort_by_key(|k| self.data.projects.iter().position(|p| &p.id == k).unwrap_or(usize::MAX));
        let series: Vec<Series> = keys
            .iter()
            .map(|k| {
                let mut color = self.project_color(t, k);
                if self.filters.project.as_ref().is_some_and(|p| p != k) {
                    color = charts::alpha(color, 0.28);
                }
                Series { key: k.clone(), label: self.project_name(k).into(), color }
            })
            .collect();
        let values = s.buckets.iter().map(|b| keys.iter().map(|k| b.values.get(k).copied().unwrap_or(0.)).collect()).collect();
        self.column_data(s, series, values)
    }

    fn single_series(&self, s: &SeriesData, label: &str, color: Hsla) -> ChartData {
        let series = vec![Series { key: "total".into(), label: label.to_string().into(), color }];
        let values = s.buckets.iter().map(|b| vec![b.total]).collect();
        self.column_data(s, series, values)
    }

    fn column_data(&self, s: &SeriesData, series: Vec<Series>, values: Vec<Vec<f64>>) -> ChartData {
        let now = now_unix();
        let starts: Vec<i64> = s.buckets.iter().map(|b| parse_rfc3339(&b.start).unwrap_or(0)).collect();
        let reached = starts.iter().filter(|t| **t <= now).count();
        let hourly = s.bucket == "hour";
        let n = starts.len();
        let x_labels = starts
            .iter()
            .enumerate()
            .filter_map(|(i, t)| {
                let tm = local_tm(*t);
                let show = if hourly {
                    tm.tm_hour % 3 == 0
                } else if n <= 7 {
                    true
                } else {
                    (n - 1 - i).is_multiple_of(5)
                };
                show.then(|| {
                    let text = if hourly {
                        hour_label(tm.tm_hour)
                    } else if n <= 7 {
                        format!("{} {}", WEEKDAYS[tm.tm_wday as usize % 7], tm.tm_mday)
                    } else {
                        format!("{} {}", MONTHS[tm.tm_mon as usize % 12], tm.tm_mday)
                    };
                    (i, SharedString::from(text))
                })
            })
            .collect();
        let titles = starts
            .iter()
            .map(|t| {
                let tm = local_tm(*t);
                let day = format!("{} {} {}", WEEKDAYS[tm.tm_wday as usize % 7], MONTHS[tm.tm_mon as usize % 12], tm.tm_mday);
                SharedString::from(if hourly { format!("{} – {}, {day}", hour_label(tm.tm_hour), hour_label((tm.tm_hour + 1) % 24)) } else { day })
            })
            .collect();
        ChartData { unit: Unit::from_str(&s.unit), series, values, x_labels, titles, reached }
    }

    fn away_since(&self) -> (i64, Option<i64>) {
        if let Some(a) = self.went_away {
            return (a, self.came_back);
        }
        // Not tracked yet this run: everything since your last action.
        let last_human = self.data.activity.iter().find(|e| e.actor.kind == "human").and_then(|e| parse_rfc3339(&e.at));
        (last_human.map(|t| t + 1).unwrap_or(0), None)
    }

    fn filtered_log(&self) -> Vec<&Event> {
        let f = &self.filters;
        let (away_from, away_to) = self.away_since();
        self.data
            .activity
            .iter()
            .filter(|e| f.project.as_ref().is_none_or(|p| e.project_id.as_ref() == Some(p) || (p == "none" && e.project_id.is_none())))
            .filter(|e| f.terminal.as_ref().is_none_or(|s| e.session_id.as_ref() == Some(s)))
            .filter(|e| f.actor.as_ref().is_none_or(|a| &e.actor.kind == a))
            .filter(|e| f.kind.is_none_or(|k| k.matches(&e.kind)))
            .filter(|e| {
                !f.away || {
                    let t = parse_rfc3339(&e.at).unwrap_or(0);
                    t >= away_from && away_to.is_none_or(|to| t <= to) && e.actor.kind != "human"
                }
            })
            .collect()
    }
}

/// Index of the activity log among the scroll body's children (for scroll_to_item).
const LOG_INDEX: usize = 4;

// ------------------------------------------------------------------ render

impl Render for InsightsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.last_render = Instant::now();
        if self.data.range.is_none() && !self.loading {
            self.fetch(cx);
        }
        let t = cx.global::<Theme>().clone();
        let header = self.header(&t, cx);
        let body: Vec<AnyElement> = if let Some(e) = self.error.clone().filter(|_| self.data.range.is_none()) {
            vec![message_card(&t, "Couldn't load insights", &e, "midna insights").into_any_element()]
        } else if self.data.range.is_none() {
            vec![div().p(px(8.)).text_color(t.dim).child("Loading insights…").into_any_element()]
        } else if self.is_empty() {
            // no numbers yet, but the log may still have terminal/settings activity
            let names = self.session_names();
            let mut v = vec![self.empty_state(&t, cx).into_any_element()];
            if !self.data.activity.is_empty() {
                v.push(self.log(&t, &names, cx).into_any_element());
            }
            v
        } else {
            self.dashboard(&t, cx)
        };
        div().flex_1().min_w_0().h_full().flex().flex_col().bg(t.bg).child(header).child(
            div().id("insights-scroll").flex_1().min_h_0().overflow_y_scroll().track_scroll(&self.scroll).flex().flex_col().gap(px(14.)).p(px(20.)).pb(px(28.)).children(body),
        )
    }
}

impl InsightsView {
    fn is_empty(&self) -> bool {
        let s = &self.data.summary.totals;
        s.turns == 0. && s.messages == 0. && s.spend_usd == 0. && s.working_secs == 0. && s.waiting_secs == 0. && s.approvals == 0. && s.triggers_fired == 0.
    }

    fn header(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let mut seg = div().flex().p(px(2.)).gap(px(2.)).rounded(px(8.)).border_1().border_color(t.line).bg(t.panel);
        for r in [Range::Today, Range::Week, Range::Month] {
            let on = r == self.range;
            seg = seg.child(
                div()
                    .id(SharedString::from(format!("range-{}", r.wire())))
                    .h(px(24.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .text_size(px(12.))
                    .cursor_pointer()
                    .when(on, |d| {
                        d.bg(t.raised).text_color(t.fg).font_weight(FontWeight::BOLD).shadow(vec![BoxShadow {
                            color: hsla(0., 0., 0., 0.18),
                            offset: point(px(0.), px(1.)),
                            blur_radius: px(2.),
                            spread_radius: px(0.),
                            inset: false,
                        }])
                    })
                    .when(!on, |d| d.text_color(t.dim).hover(|s| s.text_color(t.fg)))
                    .on_click(cx.listener(move |v, _, _, cx| v.set_range(r, cx)))
                    .child(r.label()),
            );
        }
        div()
            .h(px(44.))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(14.))
            .pl(px(18.))
            .pr(px(10.))
            .border_b_1()
            .border_color(t.line)
            .child(div().text_size(px(15.)).font_weight(FontWeight::BOLD).child("Insights"))
            .child(seg)
            .child(div().text_size(px(12.)).text_color(t.dim).child(format!("deltas {}", self.range.vs())))
            .when(self.loading && self.data.range.is_some(), |d| d.child(div().text_size(px(11.5)).text_color(t.dim).child("updating…")))
            .child(div().flex_1())
            .child(
                div()
                    .id("insights-back")
                    .px(px(10.))
                    .py(px(5.))
                    .rounded(px(7.))
                    .text_color(t.dim)
                    .cursor_pointer()
                    .hover(|st| st.bg(t.raised))
                    .on_click(cx.listener(|v, _, w, cx| v.back(w, cx)))
                    .child("Back to terminal  esc"),
            )
    }

    fn empty_state(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let others: Vec<Range> = [Range::Today, Range::Week, Range::Month].into_iter().filter(|r| *r != self.range).collect();
        let mut buttons = div().flex().gap(px(8.));
        for r in others {
            buttons = buttons.child(
                div()
                    .id(SharedString::from(format!("empty-{}", r.wire())))
                    .h(px(28.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .rounded(px(7.))
                    .border_1()
                    .border_color(t.line)
                    .cursor_pointer()
                    .hover(|s| s.bg(t.raised))
                    .on_click(cx.listener(move |v, _, _, cx| v.set_range(r, cx)))
                    .child(format!("Show {}", r.label().to_lowercase())),
            );
        }
        let when = match self.range {
            Range::Today => "today",
            Range::Week => "in the last 7 days",
            Range::Month => "in the last 30 days",
        };
        div().p(px(28.)).flex().justify_center().child(
            div()
                .max_w(px(560.))
                .mt(px(40.))
                .flex()
                .flex_col()
                .items_center()
                .gap(px(12.))
                .p(px(28.))
                .rounded(px(14.))
                .bg(t.panel)
                .border_1()
                .border_color(t.line)
                .child(empty_bars(t))
                .child(div().text_size(px(16.)).font_weight(FontWeight::BOLD).child(format!("No agent activity {when}")))
                .child(
                    div()
                        .text_color(t.dim)
                        .text_center()
                        .child("Charts fill in as agents work: turns, messages, spend, time spent working and waiting on you. Start an agent with ⇧⌘T, or ask one from ⌘K."),
                )
                .child(buttons)
                .child(div().mt(px(4.)).font_family(t.mono_font.clone()).text_size(px(11.5)).text_color(t.dim).child(format!("midna insights --range {}", self.range.wire()))),
        )
    }

    fn dashboard(&self, t: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let d = &self.data;
        let s = &d.summary;
        let vs = self.range.vs();
        // 0: headline tiles
        let tiles = div()
            .flex()
            .flex_wrap()
            .gap(px(10.))
            .child(tile(t, "Agent turns", Unit::Count.fmt(s.totals.turns), s.vs_previous.turns, Unit::Count, vs, Tone::MoreIsGood))
            .child(tile(t, "Messages sent", Unit::Count.fmt(s.totals.messages), s.vs_previous.messages, Unit::Count, vs, Tone::MoreIsGood))
            .child(tile(t, "Spend", Unit::Usd.fmt(s.totals.spend_usd), s.vs_previous.spend_usd, Unit::Usd, vs, Tone::MoreIsCost))
            .child(tile(t, "Agents working", Unit::Secs.fmt(s.totals.working_secs), s.vs_previous.working_secs, Unit::Secs, vs, Tone::MoreIsGood))
            .child(tile(t, "Waiting on you", Unit::Secs.fmt(s.totals.waiting_secs), s.vs_previous.waiting_secs, Unit::Secs, vs, Tone::MoreIsCost))
            .child(tile(t, "Approvals", Unit::Count.fmt(s.totals.approvals), s.vs_previous.approvals, Unit::Count, vs, Tone::Neutral))
            .child(tile(t, "Triggers fired", Unit::Count.fmt(s.totals.triggers_fired), s.vs_previous.triggers_fired, Unit::Count, vs, Tone::Neutral));

        // 1: turns by project
        let turns = self.turns_chart_data(t);
        let keys: Vec<String> = turns.series.iter().map(|s| s.key.clone()).collect();
        let pick_keys = keys.clone();
        let on_pick: charts::OnPick<InsightsView> = Rc::new(move |v, _bucket, series, _w, cx| {
            if let Some(k) = series.and_then(|s| pick_keys.get(s)).cloned() {
                v.filter_log(|f| f.project = if f.project.as_ref() == Some(&k) { None } else { Some(k) }, cx);
            }
        });
        let mut legend = div().flex().flex_wrap().gap(px(4.)).justify_end();
        for g in &turns.series {
            let key = g.key.clone();
            let total = d.turns.groups.iter().find(|x| x.key == g.key).map(|x| x.total).unwrap_or(0.);
            let active = self.filters.project.as_ref() == Some(&g.key);
            legend = legend.child(
                charts::legend_item(t, self.project_color(t, &g.key), g.label.clone(), Some(Unit::Count.fmt(total)), active)
                    .id(SharedString::from(format!("legend-{}", g.key)))
                    .cursor_pointer()
                    .hover(|s| s.bg(t.raised))
                    .on_click(cx.listener(move |v, _, _, cx| {
                        let k = key.clone();
                        v.filter_log(|f| f.project = if f.project.as_ref() == Some(&k) { None } else { Some(k) }, cx);
                    })),
            );
        }
        let turns_card = card(t)
            .child(card_head(t, "Agent turns by project", &format!("{} · click a bar to filter the log", self.range.per()), Some(legend.into_any_element())))
            .child(charts::stacked_bars("turns", &turns, 250., "No agent turns in this range", t, &self.turns_chart, turns_state, Some(on_pick), cx));

        // 2: spend + working/waiting
        let spend = self.single_series(&d.spend, "Spend", t.accent);
        let spend_card = card(t)
            .flex_1()
            .min_w(px(300.))
            .child(card_head(t, "Spend", &format!("{} · {} total", self.range.per(), Unit::Usd.fmt(d.spend.total)), None))
            .child(charts::area_line("spend", &spend, 250., "No spend recorded (Claude's status line reports cost)", t, &self.spend_chart, spend_state, cx));

        let names = self.session_names();
        let mut term_rows: Vec<&Row> = d.terminals.iter().filter(|r| r.totals.working_secs + r.totals.waiting_secs > 0.).collect();
        term_rows
            .sort_by(|a, b| (b.totals.working_secs + b.totals.waiting_secs).partial_cmp(&(a.totals.working_secs + a.totals.waiting_secs)).unwrap_or(std::cmp::Ordering::Equal));
        let extra = term_rows.len().saturating_sub(6);
        term_rows.truncate(6);
        let project_of: HashMap<String, String> = d.opened.iter().chain(d.activity.iter()).filter_map(|e| Some((e.session_id.clone()?, e.project_id.clone()?))).collect();
        let hrows: Vec<HRow> = term_rows
            .iter()
            .map(|r| {
                let name = names.get(&r.key).cloned().unwrap_or_else(|| r.label.clone());
                let proj = d.sessions.iter().find(|s| s.id == r.key).and_then(|s| s.project_id.clone()).or_else(|| project_of.get(&r.key).cloned());
                let sub = proj.map(|p| self.project_name(&p)).unwrap_or_default();
                HRow { key: r.key.clone(), label: name.into(), sub: sub.into(), values: vec![r.totals.working_secs, r.totals.waiting_secs] }
            })
            .collect();
        let ww_series =
            vec![Series { key: "working".into(), label: "Working".into(), color: t.work }, Series { key: "waiting".into(), label: "Waiting on you".into(), color: t.need }];
        let row_keys: Vec<String> = hrows.iter().map(|r| r.key.clone()).collect();
        let pick_row: crate::ui::charts::RowPick<InsightsView> = Rc::new(move |v, i, _w, cx| {
            if let Some(k) = row_keys.get(i).cloned() {
                v.filter_log(|f| f.terminal = if f.terminal.as_ref() == Some(&k) { None } else { Some(k) }, cx);
            }
        });
        let ww_legend = div().flex().gap(px(4.)).child(charts::legend_item(t, t.work, "Working", None, false)).child(charts::legend_item(t, t.need, "Waiting on you", None, false));
        let ww_card = card(t)
            .flex_1()
            .min_w(px(300.))
            .child(card_head(t, "Working vs waiting on you", "per agent terminal · click to filter the log", Some(ww_legend.into_any_element())))
            .child(if hrows.is_empty() {
                div().h(px(190.)).flex().items_center().justify_center().text_size(px(12.)).text_color(t.dim).child("No working or waiting time in this range")
            } else {
                charts::hbars("ww", &hrows, &ww_series, Unit::Secs, t, self.term_hover, term_hover, Some(pick_row), cx)
            })
            .when(extra > 0, |c| c.child(div().pl(px(8.)).text_size(px(11.5)).text_color(t.dim).child(format!("+ {extra} more terminals"))));

        // 3: approvals + triggers
        let appr = self.single_series(&d.approvals, "Approvals", t.ok);
        let trig = self.single_series(&d.triggers, "Triggers fired", t.work);
        let appr_card = card(t)
            .flex_1()
            .min_w(px(300.))
            .child(card_head(t, "Approvals", &format!("{} granted · {}", Unit::Count.fmt(d.approvals.total), self.range.per()), None))
            .child(charts::stacked_bars("approvals", &appr, 120., "No approvals in this range", t, &self.approvals_chart, approvals_state, None, cx));
        let trig_card = card(t)
            .flex_1()
            .min_w(px(300.))
            .child(card_head(t, "Triggers fired", &format!("{} fired · {}", Unit::Count.fmt(d.triggers.total), self.range.per()), None))
            .child(charts::stacked_bars("triggers", &trig, 120., "No triggers fired in this range", t, &self.triggers_chart, triggers_state, None, cx));

        // direct children of the scroll container, so scroll_to_item(LOG_INDEX) reaches the log
        vec![
            tiles.into_any_element(),
            turns_card.into_any_element(),
            div().flex().flex_wrap().gap(px(14.)).child(spend_card).child(ww_card).into_any_element(),
            div().flex().flex_wrap().gap(px(14.)).child(appr_card).child(trig_card).into_any_element(),
            self.log(t, &names, cx).into_any_element(),
        ]
    }

    fn log(&self, t: &Theme, names: &HashMap<String, String>, cx: &mut Context<Self>) -> Div {
        let f = &self.filters;
        let rows = self.filtered_log();
        let (away_from, _) = self.away_since();
        let away_count = self.data.activity.iter().filter(|e| parse_rfc3339(&e.at).unwrap_or(0) >= away_from && e.actor.kind != "human").count();

        // filters: one row of chips per dimension
        let mut fl = div().flex().flex_col().gap(px(6.));
        let mut top = div().flex().flex_wrap().items_center().gap(px(6.));
        top = top.child(toggle_chip(t, "f-away", &format!("While you were away · {away_count}"), f.away, true).on_click(cx.listener(|v, _, _, cx| {
            v.filters.away = !v.filters.away;
            cx.notify();
        })));
        if let Some(sid) = &f.terminal {
            let name = names.get(sid).cloned().unwrap_or_else(|| sid.clone());
            top = top.child(toggle_chip(t, "f-term", &format!("Terminal: {name}  ✕"), true, false).on_click(cx.listener(|v, _, _, cx| {
                v.filters.terminal = None;
                cx.notify();
            })));
        }
        if f.any() {
            top = top.child(div().flex_1()).child(
                div()
                    .id("f-clear")
                    .text_size(px(12.))
                    .text_color(t.accent)
                    .cursor_pointer()
                    .on_click(cx.listener(|v, _, _, cx| {
                        v.filters = Filters::default();
                        cx.notify();
                    }))
                    .child("Clear filters"),
            );
        }
        fl = fl.child(top);
        // projects
        let mut pr = div().flex().flex_wrap().items_center().gap(px(4.)).child(filter_label(t, "Project"));
        let mut seen: Vec<String> = self.data.projects.iter().map(|p| p.id.clone()).collect();
        for e in &self.data.activity {
            if let Some(p) = &e.project_id
                && !seen.contains(p)
            {
                seen.push(p.clone());
            }
        }
        let used: Vec<String> = seen.into_iter().filter(|p| self.data.activity.iter().any(|e| e.project_id.as_ref() == Some(p))).collect();
        pr = pr.child(toggle_chip(t, "fp-all", "All", f.project.is_none(), false).on_click(cx.listener(|v, _, _, cx| {
            v.filters.project = None;
            cx.notify();
        })));
        for p in used {
            let on = f.project.as_ref() == Some(&p);
            let key = p.clone();
            pr = pr.child(
                toggle_chip(t, &format!("fp-{p}"), &self.project_name(&p), on, false)
                    .child(div().size(px(8.)).rounded(px(2.)).bg(self.project_color(t, &p)))
                    .flex_row_reverse()
                    .on_click(cx.listener(move |v, _, _, cx| {
                        v.filters.project = if v.filters.project.as_ref() == Some(&key) { None } else { Some(key.clone()) };
                        cx.notify();
                    })),
            );
        }
        fl = fl.child(pr);
        // actors
        let mut ar = div().flex().flex_wrap().items_center().gap(px(4.)).child(filter_label(t, "Who"));
        ar = ar.child(toggle_chip(t, "fa-all", "Anyone", f.actor.is_none(), false).on_click(cx.listener(|v, _, _, cx| {
            v.filters.actor = None;
            cx.notify();
        })));
        for (k, label) in ACTORS {
            let on = f.actor.as_deref() == Some(*k);
            ar = ar.child(toggle_chip(t, &format!("fa-{k}"), label, on, false).on_click(cx.listener(move |v, _, _, cx| {
                v.filters.actor = if v.filters.actor.as_deref() == Some(*k) { None } else { Some(k.to_string()) };
                cx.notify();
            })));
        }
        fl = fl.child(ar);
        // kinds
        let mut kr = div().flex().flex_wrap().items_center().gap(px(4.)).child(filter_label(t, "What"));
        kr = kr.child(toggle_chip(t, "fk-all", "Everything", f.kind.is_none(), false).on_click(cx.listener(|v, _, _, cx| {
            v.filters.kind = None;
            cx.notify();
        })));
        for (k, label) in KINDS {
            let on = f.kind == Some(*k);
            let kk = *k;
            kr = kr.child(toggle_chip(t, &format!("fk-{label}"), label, on, false).on_click(cx.listener(move |v, _, _, cx| {
                v.filters.kind = if v.filters.kind == Some(kk) { None } else { Some(kk) };
                cx.notify();
            })));
        }
        fl = fl.child(kr);

        // rows, grouped by local day
        let mut list = div().flex().flex_col();
        let mut last_day = String::new();
        let shown = rows.len().min(300);
        for (i, e) in rows.iter().take(shown).enumerate() {
            let ts = parse_rfc3339(&e.at).unwrap_or(0);
            let tm = local_tm(ts);
            let day = format!("{} {} {}", WEEKDAYS[tm.tm_wday as usize % 7], MONTHS[tm.tm_mon as usize % 12], tm.tm_mday);
            if self.range != Range::Today && day != last_day {
                list =
                    list.child(div().pt(px(if i == 0 { 2. } else { 12. })).pb(px(4.)).text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(t.dim).child(day.to_uppercase()));
                last_day = day;
            }
            list = list.child(self.log_row(t, e, &tm, names, i, cx));
        }
        if rows.is_empty() {
            list = list.child(div().py(px(24.)).flex().justify_center().text_color(t.dim).child(if f.away {
                "Nothing happened while you were away."
            } else {
                "No activity matches these filters."
            }));
        } else if rows.len() > shown {
            list = list.child(
                div()
                    .pt(px(8.))
                    .text_size(px(11.5))
                    .text_color(t.dim)
                    .child(format!("Showing the newest {shown} of {}. Narrow the filters, or `midna events` for everything.", rows.len())),
            );
        }
        let capped = if self.data.activity.len() >= 1000 { " · newest 1000 in this range" } else { "" };
        let title = format!("{} event{}{capped}", rows.len(), if rows.len() == 1 { "" } else { "s" });
        card(t).child(card_head(t, "Activity", &title, None)).child(fl).child(div().h(px(1.)).bg(t.line).my(px(4.))).child(list)
    }

    fn log_row(&self, t: &Theme, e: &Event, tm: &libc::tm, names: &HashMap<String, String>, i: usize, cx: &mut Context<Self>) -> Stateful<Div> {
        let (text, color) = describe(e, t);
        let who = match e.actor.kind.as_str() {
            "human" => "You".to_string(),
            "agent" => e.actor.name.clone().map(|n| capitalize(&n)).unwrap_or_else(|| "Agent".into()),
            "trigger" => e.actor.name.clone().unwrap_or_else(|| "Trigger".into()),
            _ => "midnad".into(),
        };
        let term = e.session_id.as_ref().map(|s| names.get(s).cloned().unwrap_or_else(|| s.clone()));
        let proj = e.project_id.as_ref().map(|p| self.project_name(p));
        let pcolor = e.project_id.as_ref().map(|p| self.project_color(t, p));
        let live = e.session_id.as_ref().is_some_and(|s| self.data.sessions.iter().any(|x| &x.id == s));
        let sid = e.session_id.clone();
        div()
            .id(SharedString::from(format!("log-{i}-{}", e.seq)))
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(8.))
            .py(px(5.))
            .rounded(px(6.))
            .hover(|s| s.bg(charts::alpha(t.fg, 0.04)))
            .when(live, |d| d.cursor_pointer())
            .on_click(cx.listener(move |v, _, w, cx| {
                if let Some(s) = sid.clone() {
                    v.open_terminal(s, w, cx);
                }
            }))
            .child(div().w(px(42.)).flex_none().font_family(t.mono_font.clone()).text_size(px(11.5)).text_color(t.dim).child(format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)))
            .child(div().size(px(7.)).rounded_full().flex_none().bg(color))
            .child(div().w(px(76.)).flex_none().text_size(px(12.)).text_color(t.dim).whitespace_nowrap().overflow_hidden().text_ellipsis().child(who))
            .child(div().flex_1().min_w_0().text_size(px(12.5)).whitespace_nowrap().overflow_hidden().text_ellipsis().child(text))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(6.))
                    .max_w(px(260.))
                    .text_size(px(11.5))
                    .text_color(t.dim)
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .when_some(pcolor, |d, c| d.child(div().size(px(8.)).rounded(px(2.)).bg(c).flex_none()))
                    .child(match (proj, term) {
                        (Some(p), Some(s)) => format!("{p} › {s}"),
                        (Some(p), None) => p,
                        (None, Some(s)) => s,
                        (None, None) => String::new(),
                    }),
            )
    }
}

// ------------------------------------------------------------------ pieces

#[derive(Clone, Copy)]
enum Tone {
    MoreIsGood,
    MoreIsCost,
    Neutral,
}

fn tile(t: &Theme, label: &str, value: String, delta: f64, unit: Unit, _vs: &str, tone: Tone) -> Div {
    let (arrow, color) = if delta.abs() < 1e-9 {
        ("=", t.dim)
    } else if delta > 0. {
        (
            "▲",
            match tone {
                Tone::MoreIsGood => t.ok,
                Tone::MoreIsCost => t.need,
                Tone::Neutral => t.dim,
            },
        )
    } else {
        (
            "▼",
            match tone {
                Tone::MoreIsGood => t.need,
                Tone::MoreIsCost => t.ok,
                Tone::Neutral => t.dim,
            },
        )
    };
    let amount = if delta.abs() < 1e-9 { "same".to_string() } else { unit.fmt(delta.abs()) };
    div()
        .flex_1()
        .min_w(px(128.))
        .flex()
        .flex_col()
        .gap(px(2.))
        .px(px(14.))
        .py(px(11.))
        .rounded(px(10.))
        .bg(t.panel)
        .border_1()
        .border_color(t.line)
        .child(div().text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(t.dim).whitespace_nowrap().child(label.to_uppercase()))
        .child(div().text_size(px(24.)).line_height(px(30.)).font_weight(FontWeight::BOLD).whitespace_nowrap().child(value))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(4.))
                .text_size(px(11.5))
                .text_color(t.dim)
                .flex_wrap()
                .child(div().text_size(px(9.)).text_color(color).child(arrow))
                .child(div().text_color(t.fg).whitespace_nowrap().child(amount))
                .child(div().whitespace_nowrap().child(if delta.abs() < 1e-9 {
                    ""
                } else if delta > 0. {
                    "more"
                } else {
                    "less"
                })),
        )
}

fn card(t: &Theme) -> Div {
    div().flex().flex_col().gap(px(10.)).p(px(16.)).rounded(px(12.)).bg(t.panel).border_1().border_color(t.line)
}

fn card_head(t: &Theme, title: &str, sub: &str, right: Option<AnyElement>) -> Div {
    div()
        .flex()
        .items_start()
        .gap(px(12.))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_none()
                .child(div().text_size(px(14.)).font_weight(FontWeight::BOLD).child(title.to_string()))
                .child(div().text_size(px(12.)).text_color(t.dim).child(sub.to_string())),
        )
        .child(div().flex_1())
        .children(right.map(|r| div().flex_shrink(1.).min_w_0().child(r)))
}

fn filter_label(t: &Theme, s: &str) -> Div {
    div().w(px(56.)).flex_none().text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(t.dim).child(s.to_uppercase())
}

fn toggle_chip(t: &Theme, id: &str, label: &str, on: bool, need: bool) -> Stateful<Div> {
    let (fg, bg, border) = match (on, need) {
        (true, true) => (t.need, t.need_soft, t.need),
        (true, false) => (t.fg, t.accent_soft, t.accent),
        (false, _) => (t.dim, gpui_kit::transparent_black(), t.line),
    };
    div()
        .id(SharedString::from(id.to_string()))
        .flex()
        .items_center()
        .gap(px(6.))
        .h(px(24.))
        .px(px(9.))
        .rounded(px(7.))
        .border_1()
        .border_color(border)
        .bg(bg)
        .text_size(px(12.))
        .text_color(fg)
        .when(on, |d| d.font_weight(FontWeight::BOLD))
        .cursor_pointer()
        .hover(|s| s.text_color(t.fg))
        .child(label.to_string())
}

fn message_card(t: &Theme, title: &str, detail: &str, cli: &str) -> Div {
    div().p(px(28.)).child(
        card(t)
            .max_w(px(560.))
            .child(div().text_size(px(15.)).font_weight(FontWeight::BOLD).child(title.to_string()))
            .child(div().text_color(t.dim).child(detail.to_string()))
            .child(div().font_family(t.mono_font.clone()).text_size(px(12.)).text_color(t.dim).child(cli.to_string())),
    )
}

/// A small decorative bar glyph for the empty state, drawn with quads.
fn empty_bars(t: &Theme) -> impl IntoElement {
    let c1 = charts::alpha(t.accent, 0.55);
    let c2 = charts::alpha(t.dim, 0.35);
    canvas(
        |_, _, _| {},
        move |b, _, window, _| {
            let hs = [0.35f32, 0.6, 0.45, 0.85, 0.55, 0.3];
            let w = 10.;
            let gap = 6.;
            let total = hs.len() as f32 * (w + gap) - gap;
            let x0 = f32::from(b.origin.x) + (f32::from(b.size.width) - total) / 2.;
            let bottom = f32::from(b.origin.y + b.size.height);
            for (i, h) in hs.iter().enumerate() {
                let hh = h * f32::from(b.size.height);
                let c = if i == 3 { c1 } else { c2 };
                window.paint_quad(fill(Bounds::new(point(px(x0 + i as f32 * (w + gap)), px(bottom - hh)), size(px(w), px(hh))), c).corner_radii(Corners {
                    top_left: px(3.),
                    top_right: px(3.),
                    bottom_left: px(0.),
                    bottom_right: px(0.),
                }));
            }
            window.paint_quad(fill(Bounds::new(point(px(x0 - 8.), px(bottom)), size(px(total + 16.), px(1.))), c2));
        },
    )
    .w(px(140.))
    .h(px(56.))
}

/// One-line description and dot color for an activity event.
fn describe(e: &Event, t: &Theme) -> (String, Hsla) {
    let d = &e.data;
    let s = |p: &str| d.pointer(p).and_then(Value::as_str).unwrap_or("").to_string();
    match e.kind.as_str() {
        "session.opened" => (format!("Opened {}", nonempty(s("/name"), "a terminal")), t.accent),
        "session.closed" => ("Closed the terminal".into(), t.dim),
        "session.exited" => (format!("Exited{}", d.get("exit_code").and_then(Value::as_i64).map(|c| format!(" with code {c}")).unwrap_or_default()), t.dim),
        "session.status" => {
            let st = s("/state").replace('_', " ");
            let reason = s("/reason");
            let c = match st.as_str() {
                "working" => t.work,
                "needs you" => t.need,
                "done" => t.ok,
                "failed" => t.err,
                _ => t.dim,
            };
            (if reason.is_empty() { format!("Now {st}") } else { format!("Now {st} · {reason}") }, c)
        }
        "agent.prompt_submitted" => {
            let p = s("/prompt");
            (if p.is_empty() || p == "…" { "Message sent to the agent".into() } else { format!("Message: “{}”", p.lines().next().unwrap_or("")) }, t.accent)
        }
        "agent.turn_started" => ("Turn started".into(), t.work),
        "agent.turn_ended" => ("Turn finished".into(), t.ok),
        "needs_you.raised" => (format!("Needs you: {}", nonempty(s("/title"), "attention")), t.need),
        "needs_you.resolved" => {
            let k = s("/resolution/kind");
            let text = match k.as_str() {
                "approve" => "Approved",
                "deny" => "Denied",
                "dismiss" => "Dismissed",
                "done" => "Marked done",
                "restart" => "Restarted",
                "timeout" => "Approval timed out",
                _ => "Resolved",
            };
            (text.to_string(), if k == "deny" { t.err } else { t.ok })
        }
        "rule.added" => (format!("Rule added: {} {}", s("/effect").to_uppercase(), s("/matcher/pattern")), t.accent),
        "rule.removed" => ("Rule removed".into(), t.dim),
        "rule.fired" => ("Rule matched".into(), t.dim),
        "rule.expired" => ("Rule expired".into(), t.dim),
        "rule.removal_requested" => ("Asked to remove a rule".into(), t.need),
        "trigger.fired" => (format!("Trigger fired{}", Some(s("/event")).filter(|x| !x.is_empty()).map(|x| format!(": {x}")).unwrap_or_default()), t.work),
        "settings.changed" => {
            let v = d.get("value").map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())).unwrap_or_default();
            (format!("Setting {} → {v}", s("/key")), t.dim)
        }
        k => (k.to_string(), t.dim),
    }
}

fn nonempty(s: String, fallback: &str) -> String {
    if s.is_empty() { fallback.to_string() } else { s }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

// ------------------------------------------------------------------ local time

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn local_tm(secs: i64) -> libc::tm {
    let t: libc::time_t = secs as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    tm
}

fn local_day_start(secs: i64) -> i64 {
    let tm = local_tm(secs);
    secs - (tm.tm_hour as i64 * 3600 + tm.tm_min as i64 * 60 + tm.tm_sec as i64)
}

fn hour_label(h: i32) -> String {
    match h {
        0 => "12a".into(),
        12 => "12p".into(),
        h if h < 12 => format!("{h}a"),
        h => format!("{}p", h - 12),
    }
}

/// The Insights screen in place of the terminal pane (Escape returns to the terminal).
pub fn render(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) -> AnyElement {
    let view = m.insights_view(window, cx);
    div().id("screen-insights").key_context("MidnaOverlay").track_focus(&m.overlay_focus).flex().flex_1().min_w_0().h_full().child(view).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{KindFilter, SeriesData, hour_label};
    use serde_json::json;

    #[test]
    fn hour_labels() {
        assert_eq!(hour_label(0), "12a");
        assert_eq!(hour_label(15), "3p");
        assert_eq!(hour_label(9), "9a");
    }

    #[test]
    fn kind_filters() {
        assert!(KindFilter::Turns.matches("agent.turn_started"));
        assert!(KindFilter::Terminals.matches("session.opened"));
        assert!(!KindFilter::Terminals.matches("session.status"));
        assert!(KindFilter::Status.matches("session.status"));
    }

    #[test]
    fn reads_series_wire_shape() {
        let v = json!({"range": "week", "metric": "turns", "bucket": "day", "unit": "count", "from": "x", "to": "y",
            "buckets": [{"start": "2026-10-03T06:00:00Z", "end": "x", "total": 3.0, "values": {"p_a": 3.0}}],
            "groups": [{"key": "p_a", "label": "alpha", "total": 3.0}], "total": 3.0, "previous_total": 1.0});
        let s: SeriesData = serde_json::from_value(v).unwrap();
        assert_eq!(s.buckets[0].values["p_a"], 3.0);
        assert_eq!(s.groups[0].label, "alpha");
        assert_eq!(s.previous_total, 1.0);
    }
}
