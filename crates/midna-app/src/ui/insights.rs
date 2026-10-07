//! Insights: graphs, not a report. Replaces the terminal pane (the sidebar stays).
//!
//! Range switcher (Today / Week / Month) → headline numbers with vs-previous deltas →
//! agent turns over time stacked by project → spend over time → working vs waiting on you
//! per terminal → approvals and triggers. Every number comes from midnad
//! (`insights.summary`, `insights.series`), i.e. from events only.
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
    /// `session.opened` / `session.renamed` events: names and projects of closed terminals.
    opened: Vec<Event>,
    projects: Vec<Project>,
    sessions: Vec<Session>,
}

// ------------------------------------------------------------------ view

pub struct InsightsView {
    backend: Arc<dyn Backend>,
    main: WeakEntity<MainWindow>,
    range: Range,
    data: Data,
    loading: bool,
    error: Option<String>,
    turns_chart: ChartState,
    spend_chart: ChartState,
    approvals_chart: ChartState,
    triggers_chart: ChartState,
    term_hover: Option<usize>,
    last_render: Instant,
    stale: bool,
    fetch_gen: u64,
    scroll: ScrollHandle,
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
    pub fn insights_view(&mut self, cx: &mut Context<Self>) -> Entity<InsightsView> {
        if let Some(v) = &self.insights {
            return v.clone();
        }
        let backend = self.backend.clone();
        let main = cx.entity().downgrade();
        let v = cx.new(|cx| InsightsView::new(backend, main, cx));
        self.insights = Some(v.clone());
        v
    }
}

impl InsightsView {
    pub fn new(backend: Arc<dyn Backend>, main: WeakEntity<MainWindow>, cx: &mut Context<Self>) -> Self {
        let range = match crate::dev::var("MIDNA_INSIGHTS_RANGE").as_deref() {
            Ok("week") => Range::Week,
            Ok("month") => Range::Month,
            _ => Range::Today,
        };
        let mut v = InsightsView {
            backend,
            main,
            range,
            data: Data::default(),
            loading: true,
            error: None,
            turns_chart: ChartState::default(),
            spend_chart: ChartState::default(),
            approvals_chart: ChartState::default(),
            triggers_chart: ChartState::default(),
            term_hover: None,
            last_render: Instant::now(),
            stale: false,
            fetch_gen: 0,
            scroll: ScrollHandle::new(),
        };
        // dev (screenshots): preset hover on the turns chart / spend chart / a terminal row
        let env_i = |k: &str| crate::dev::var(k).ok().and_then(|x| x.parse::<usize>().ok());
        v.turns_chart.hover = env_i("MIDNA_INSIGHTS_HOVER");
        v.spend_chart.hover = env_i("MIDNA_INSIGHTS_HOVER_SPEND");
        v.term_hover = env_i("MIDNA_INSIGHTS_HOVER_ROW");
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
        for e in self.data.opened.iter() {
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
                Series { key: k.clone(), label: self.project_name(k).into(), color: self.project_color(t, k) }
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
}

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
            vec![self.empty_state(&t, cx).into_any_element()]
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
        let mut legend = div().flex().flex_wrap().gap(px(4.)).justify_end();
        for g in &turns.series {
            let total = d.turns.groups.iter().find(|x| x.key == g.key).map(|x| x.total).unwrap_or(0.);
            legend = legend.child(charts::legend_item(t, self.project_color(t, &g.key), g.label.clone(), Some(Unit::Count.fmt(total)), false));
        }
        let turns_card = card(t)
            .child(card_head(t, "Agent turns by project", self.range.per(), Some(legend.into_any_element())))
            .child(charts::stacked_bars("turns", &turns, 250., "No agent turns in this range", t, &self.turns_chart, turns_state, None, cx));

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
        let project_of: HashMap<String, String> = d.opened.iter().filter_map(|e| Some((e.session_id.clone()?, e.project_id.clone()?))).collect();
        let hrows: Vec<HRow> = term_rows
            .iter()
            .map(|r| {
                let name = names.get(&r.key).cloned().unwrap_or_else(|| r.label.clone());
                let proj = d.sessions.iter().find(|s| s.id == r.key).and_then(|s| s.project_id.clone()).or_else(|| project_of.get(&r.key).cloned());
                let sub = proj.map(|p| self.project_name(&p)).unwrap_or_default();
                HRow { label: name.into(), sub: sub.into(), values: vec![r.totals.working_secs, r.totals.waiting_secs] }
            })
            .collect();
        let ww_series =
            vec![Series { key: "working".into(), label: "Working".into(), color: t.work }, Series { key: "waiting".into(), label: "Waiting on you".into(), color: t.need }];
        let ww_legend = div().flex().gap(px(4.)).child(charts::legend_item(t, t.work, "Working", None, false)).child(charts::legend_item(t, t.need, "Waiting on you", None, false));
        let ww_card = card(t)
            .flex_1()
            .min_w(px(300.))
            .child(card_head(t, "Working vs waiting on you", "per agent terminal", Some(ww_legend.into_any_element())))
            .child(if hrows.is_empty() {
                div().h(px(190.)).flex().items_center().justify_center().text_size(px(12.)).text_color(t.dim).child("No working or waiting time in this range")
            } else {
                charts::hbars("ww", &hrows, &ww_series, Unit::Secs, t, self.term_hover, term_hover, None, cx)
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

        vec![
            tiles.into_any_element(),
            turns_card.into_any_element(),
            div().flex().flex_wrap().gap(px(14.)).child(spend_card).child(ww_card).into_any_element(),
            div().flex().flex_wrap().gap(px(14.)).child(appr_card).child(trig_card).into_any_element(),
        ]
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

fn hour_label(h: i32) -> String {
    match h {
        0 => "12a".into(),
        12 => "12p".into(),
        h if h < 12 => format!("{h}a"),
        h => format!("{}p", h - 12),
    }
}

/// The Insights screen in place of the terminal pane (Escape returns to the terminal).
pub fn render(m: &mut MainWindow, _window: &mut Window, cx: &mut Context<MainWindow>) -> AnyElement {
    let view = m.insights_view(cx);
    div().id("screen-insights").key_context("MidnaOverlay").track_focus(&m.overlay_focus).flex().flex_1().min_w_0().h_full().child(view).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{SeriesData, hour_label};
    use serde_json::json;

    #[test]
    fn hour_labels() {
        assert_eq!(hour_label(0), "12a");
        assert_eq!(hour_label(15), "3p");
        assert_eq!(hour_label(9), "9a");
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
