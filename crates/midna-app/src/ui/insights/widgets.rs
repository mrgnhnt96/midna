//! Insights widgets: the catalog (ids in `insights.layouts`), the chart colors
//! (`insights.colors.*`), and one renderer per widget. Every widget is a fixed-height card;
//! wide ones span two grid columns. Design: docs/DECISIONS.md "Insights widgets".
use super::{InsightsView, Range, Row, card_head, hour_label, local_tm, tile, Tone};
use crate::model::parse_rfc3339;
use crate::theme::Theme;
use crate::ui::charts::{self, HRow, Series, Unit};
use gpui_kit::prelude::*;
use gpui_kit::*;
use midna_proto::InsightsDetail;
use serde_json::Value;
use std::collections::HashMap;

/// Height of every widget card.
pub const WIDGET_H: f32 = 268.;
/// Narrowest grid column.
pub const COL_MIN: f32 = 270.;
pub const GAP: f32 = 14.;

pub struct Spec {
    pub id: &'static str,
    pub title: &'static str,
    pub desc: &'static str,
    pub group: &'static str,
}

/// A widget's footprint on the grid: one cell, two across, or two across and two down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    Small,
    Wide,
    Large,
}

impl Size {
    pub fn parse(s: &str) -> Size {
        match s {
            "small" => Size::Small,
            "large" => Size::Large,
            _ => Size::Wide,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Size::Small => "small",
            Size::Wide => "wide",
            Size::Large => "large",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Size::Small => "Small",
            Size::Wide => "Wide",
            Size::Large => "Large",
        }
    }
    /// Columns and rows it takes in a grid `cols` wide.
    pub fn cells(self, cols: u16) -> (u16, u16) {
        match self {
            Size::Small => (1, 1),
            Size::Wide => (2.min(cols), 1),
            Size::Large => (2.min(cols), 2),
        }
    }
}

/// The sizes widget `id` comes in (`midna_proto::settings::INSIGHTS_WIDGETS`).
pub fn sizes(id: &str) -> Vec<Size> {
    midna_proto::settings::INSIGHTS_WIDGETS.iter().find(|w| w.0 == id).map(|w| w.2.iter().map(|s| Size::parse(s)).collect()).unwrap_or_else(|| vec![Size::Wide])
}

pub fn usual(id: &str) -> Size {
    midna_proto::settings::INSIGHTS_WIDGETS.iter().find(|w| w.0 == id).map(|w| Size::parse(w.1)).unwrap_or(Size::Wide)
}

pub const GROUPS: [&str; 4] = ["Overview", "Leverage", "Friction", "Patterns"];

pub const SPECS: &[Spec] = &[
    Spec { id: "headline", title: "Headline numbers", desc: "Turns, messages, spend, time, approvals", group: "Overview" },
    Spec { id: "turns", title: "Agent turns by project", desc: "Turns over time, stacked by project", group: "Overview" },
    Spec { id: "spend", title: "Spend", desc: "Dollars over time", group: "Overview" },
    Spec { id: "working_waiting", title: "Working vs waiting on you", desc: "Per agent terminal", group: "Overview" },
    Spec { id: "approvals", title: "Approvals", desc: "Approvals granted over time", group: "Overview" },
    Spec { id: "triggers", title: "Triggers fired", desc: "Triggers over time", group: "Overview" },
    Spec { id: "parallelism", title: "Agents working at once", desc: "How many ran in parallel", group: "Leverage" },
    Spec { id: "autonomy", title: "Autonomy", desc: "Turn lengths and the longest run", group: "Leverage" },
    Spec { id: "latency", title: "How fast you answer", desc: "Every needs-you wait", group: "Friction" },
    Spec { id: "agent_time", title: "Where agent time went", desc: "Working, blocked on you, idle", group: "Friction" },
    Spec { id: "approved", title: "You approved most", desc: "Turn repeat approvals into rules", group: "Friction" },
    Spec { id: "corrections", title: "Course corrections", desc: "Denials and stopped turns per day", group: "Friction" },
    Spec { id: "heatmap", title: "When work happens", desc: "Last 7 days by hour, you vs agents", group: "Patterns" },
    Spec { id: "projects", title: "Project mix", desc: "Agent hours by project", group: "Patterns" },
    Spec { id: "models", title: "Model mix", desc: "Spend by model", group: "Patterns" },
    Spec { id: "bests", title: "Personal bests", desc: "Records across all your history", group: "Patterns" },
];

pub fn spec(id: &str) -> Option<&'static Spec> {
    SPECS.iter().find(|s| s.id == id)
}

// ------------------------------------------------------------------ colors

/// The colors that mean the same thing in every widget.
#[derive(Clone, Copy)]
pub struct Palette {
    pub agents: Hsla,
    pub you: Hsla,
    pub waiting: Hsla,
}

/// `insights.colors.*`: a theme color name or `#RRGGBB`.
pub fn palette(t: &Theme, settings: &HashMap<String, Value>) -> Palette {
    let get = |k: &str, d: &str| resolve(t, settings.get(k).and_then(Value::as_str).unwrap_or(d)).unwrap_or_else(|| resolve(t, d).unwrap_or(t.accent));
    Palette { agents: get("insights.colors.agents", "accent"), you: get("insights.colors.you", "fg"), waiting: get("insights.colors.waiting", "need") }
}

fn resolve(t: &Theme, v: &str) -> Option<Hsla> {
    Some(match v.trim() {
        "accent" => t.accent,
        "work" => t.work,
        "need" => t.need,
        "ok" => t.ok,
        "err" => t.err,
        "fg" => t.fg,
        "dim" => t.dim,
        h if h.len() == 7 && h.starts_with('#') => rgb(u32::from_str_radix(&h[1..], 16).ok()?).into(),
        _ => return None,
    })
}

// ------------------------------------------------------------------ pieces

fn empty(t: &Theme, text: &str) -> Div {
    div().flex_1().flex().items_center().justify_center().text_size(px(12.)).text_color(t.dim).text_center().child(text.to_string())
}

fn stat(t: &Theme, value: String, label: &str) -> Div {
    div()
        .flex()
        .flex_col()
        .child(div().text_size(px(20.)).line_height(px(24.)).font_weight(FontWeight::BOLD).whitespace_nowrap().child(value))
        .child(div().text_size(px(11.5)).text_color(t.dim).child(label.to_string()))
}

fn side_stats(t: &Theme, items: Vec<Div>) -> Div {
    div().w(px(160.)).flex_none().flex().flex_col().gap(px(12.)).pl(px(16.)).border_l_1().border_color(t.line).children(items)
}

fn swatch(c: Hsla) -> Div {
    div().size(px(10.)).flex_none().rounded(px(2.)).bg(c)
}

fn legend(t: &Theme, items: &[(Hsla, String)]) -> Div {
    div().flex().flex_wrap().gap(px(12.)).text_size(px(11.)).text_color(t.dim).children(
        items.iter().map(|(c, l)| div().flex().items_center().gap(px(5.)).child(swatch(*c)).child(l.clone())).collect::<Vec<_>>(),
    )
}

/// A horizontal track with a fill `frac` of its width.
fn track(t: &Theme, frac: f32, color: Hsla, h: f32) -> Div {
    div().h(px(h)).w_full().rounded(px(2.)).bg(t.raised).child(div().h_full().w(relative(frac.clamp(0., 1.))).rounded(px(2.)).bg(color))
}

fn dur(secs: i64) -> String {
    charts::duration(secs as f64)
}

fn short_day(ts: &str) -> String {
    let tm = local_tm(parse_rfc3339(ts).unwrap_or(0));
    super::WEEKDAYS[tm.tm_wday as usize % 7].chars().next().map(String::from).unwrap_or_default()
}

fn when(ts: &str) -> String {
    let tm = local_tm(parse_rfc3339(ts).unwrap_or(0));
    format!("{} {} {}", super::WEEKDAYS[tm.tm_wday as usize % 7], super::MONTHS[tm.tm_mon as usize % 12], tm.tm_mday)
}

// ------------------------------------------------------------------ render

impl InsightsView {
    /// The card body for widget `id` (header included).
    pub(super) fn widget(&self, id: &str, size: Size, t: &Theme, p: &Palette, cx: &mut Context<Self>) -> Div {
        let Some(spec) = spec(id) else { return div() };
        let per = self.range.per();
        let d = &self.data.detail;
        let large = size == Size::Large;
        let chart_h = if large { 440. } else { 170. };
        match id {
            "headline" => self.headline(t),
            "turns" => self.turns_widget(t, chart_h, cx),
            "spend" => {
                let s = self.single_series(&self.data.spend, "Spend", p.agents);
                head(t, spec.title, &format!("{} · {}", Unit::Usd.fmt(self.data.spend.total), per), None)
                    .child(charts::area_line("spend", &s, chart_h, "No spend recorded (Claude's status line reports cost)", t, &self.spend_chart, super::spend_state, cx))
            }
            "working_waiting" => self.working_waiting(t, p, cx),
            "approvals" => {
                let s = self.single_series(&self.data.approvals, "Approvals", p.you);
                head(t, spec.title, &format!("{} granted · {per}", Unit::Count.fmt(self.data.approvals.total)), None)
                    .child(charts::stacked_bars("approvals", &s, chart_h, "No approvals in this range", t, &self.approvals_chart, super::approvals_state, None, cx))
            }
            "triggers" => {
                let s = self.single_series(&self.data.triggers, "Triggers fired", p.agents);
                head(t, spec.title, &format!("{} fired · {per}", Unit::Count.fmt(self.data.triggers.total)), None)
                    .child(charts::stacked_bars("triggers", &s, chart_h, "No triggers fired in this range", t, &self.triggers_chart, super::triggers_state, None, cx))
            }
            "parallelism" => match size {
                Size::Small => parallelism_small(t, p, d),
                Size::Wide => parallelism(t, p, d, self.range, 150.),
                Size::Large => parallelism(t, p, d, self.range, 300.).child(self.lanes(t, p)),
            },
            "autonomy" => autonomy(t, p, d),
            "latency" => match size {
                Size::Small => latency_small(t, p, d),
                Size::Wide => latency(t, p, d),
                Size::Large => latency(t, p, d).child(latency_more(t, p, d)),
            },
            "agent_time" => self.agent_time(t, p, d, size),
            "approved" => self.approved(t, d, size, cx),
            "corrections" => corrections(t, p, d),
            "heatmap" => match size {
                Size::Small => heatmap_small(t, p, d),
                Size::Wide => heatmap(t, p, d, 18., false),
                Size::Large => heatmap(t, p, d, 40., true),
            },
            "projects" => self.projects_widget(t, d),
            "models" => models(t, p, d),
            "bests" => bests(t, d),
            _ => div(),
        }
    }

    /// Large "agents at once": one lane per terminal, shaded by how much it worked in each bucket.
    fn lanes(&self, t: &Theme, p: &Palette) -> Div {
        let s = &self.data.working_terms;
        let names = self.session_names();
        let mut out = div().flex().flex_col().gap(px(6.)).pt(px(6.)).child(div().text_size(px(10.5)).font_weight(FontWeight::BOLD).text_color(t.dim).child("WHICH TERMINALS"));
        if s.groups.is_empty() {
            return out.child(div().text_size(px(12.)).text_color(t.dim).child("No terminal worked in this range"));
        }
        let size = if s.bucket == "hour" { 3600. } else { 86_400. };
        for g in s.groups.iter().take(8) {
            let mut lane = div().flex_1().min_w_0().h(px(14.)).flex().rounded(px(3.)).overflow_hidden().bg(t.raised);
            for b in &s.buckets {
                let v = b.values.get(&g.key).copied().unwrap_or(0.);
                let a = (v / size).min(1.) as f32;
                lane = lane.child(div().flex_1().h_full().when(a > 0., |c| c.bg(charts::alpha(p.agents, 0.2 + 0.8 * a))));
            }
            let name = names.get(&g.key).cloned().unwrap_or_else(|| g.label.clone());
            out = out.child(div().flex().items_center().gap(px(10.)).child(div().w(px(130.)).flex_none().truncate().text_size(px(12.)).child(name)).child(lane));
        }
        out
    }

    fn headline(&self, t: &Theme) -> Div {
        let s = &self.data.summary;
        let vs = self.range.vs();
        div().grid().grid_cols(3).gap(px(10.)).children([
            tile(t, "Agent turns", Unit::Count.fmt(s.totals.turns), s.vs_previous.turns, Unit::Count, vs, Tone::MoreIsGood),
            tile(t, "Messages sent", Unit::Count.fmt(s.totals.messages), s.vs_previous.messages, Unit::Count, vs, Tone::MoreIsGood),
            tile(t, "Spend", Unit::Usd.fmt(s.totals.spend_usd), s.vs_previous.spend_usd, Unit::Usd, vs, Tone::MoreIsCost),
            tile(t, "Agents working", Unit::Secs.fmt(s.totals.working_secs), s.vs_previous.working_secs, Unit::Secs, vs, Tone::MoreIsGood),
            tile(t, "Waiting on you", Unit::Secs.fmt(s.totals.waiting_secs), s.vs_previous.waiting_secs, Unit::Secs, vs, Tone::MoreIsCost),
            tile(t, "Approvals", Unit::Count.fmt(s.totals.approvals), s.vs_previous.approvals, Unit::Count, vs, Tone::Neutral),
        ])
    }

    fn turns_widget(&self, t: &Theme, chart_h: f32, cx: &mut Context<Self>) -> Div {
        let d = &self.data;
        let turns = self.turns_chart_data(t);
        let mut lg = div().flex().flex_wrap().gap(px(4.)).justify_end();
        for g in turns.series.iter().take(4) {
            let total = d.turns.groups.iter().find(|x| x.key == g.key).map(|x| x.total).unwrap_or(0.);
            lg = lg.child(charts::legend_item(t, self.project_color(t, &g.key), g.label.clone(), Some(Unit::Count.fmt(total)), false));
        }
        head(t, "Agent turns by project", self.range.per(), Some(lg.into_any_element()))
            .child(charts::stacked_bars("turns", &turns, chart_h, "No agent turns in this range", t, &self.turns_chart, super::turns_state, None, cx))
    }

    fn working_waiting(&self, t: &Theme, p: &Palette, cx: &mut Context<Self>) -> Div {
        let d = &self.data;
        let names = self.session_names();
        let mut rows: Vec<&Row> = d.terminals.iter().filter(|r| r.totals.working_secs + r.totals.waiting_secs > 0.).collect();
        rows.sort_by(|a, b| (b.totals.working_secs + b.totals.waiting_secs).total_cmp(&(a.totals.working_secs + a.totals.waiting_secs)));
        rows.truncate(4);
        let hrows: Vec<HRow> = rows
            .iter()
            .map(|r| HRow { label: names.get(&r.key).cloned().unwrap_or_else(|| r.label.clone()).into(), sub: "".into(), values: vec![r.totals.working_secs, r.totals.waiting_secs] })
            .collect();
        let series = vec![Series { key: "working".into(), label: "Working".into(), color: p.agents }, Series { key: "waiting".into(), label: "Waiting on you".into(), color: p.waiting }];
        head(t, "Working vs waiting on you", "per agent terminal", None).child(legend(t, &[(p.agents, "Working".into()), (p.waiting, "Waiting on you".into())])).child(
            if hrows.is_empty() { empty(t, "No working or waiting time in this range") } else { div().flex_1().min_h_0().flex().flex_col().child(div().id("ww-rows").flex_1().min_h_0().overflow_y_scroll().child(charts::hbars("ww", &hrows, &series, Unit::Secs, t, self.term_hover, super::term_hover, None, cx))) },
        )
    }

    fn approved(&self, t: &Theme, d: &InsightsDetail, size: Size, cx: &mut Context<Self>) -> Div {
        let total: f64 = d.approved.iter().map(|c| c.value).sum();
        let h = head(t, "You approved most", &format!("{} this {}", total as i64, self.range.noun()), None);
        if d.approved.is_empty() {
            return h.child(empty(t, "No approvals in this range"));
        }
        let max = d.approved.first().map(|c| c.value).unwrap_or(1.).max(1.);
        // large: every one, under its tool ("Bash(cargo test)" → Bash)
        let tool = |l: &str| l.split('(').next().unwrap_or(l).trim().to_string();
        let mut rows: Vec<&midna_proto::InsightsCount> = d.approved.iter().take(if size == Size::Large { 12 } else { 5 }).collect();
        if size == Size::Large {
            rows.sort_by_key(|c| tool(&c.label));
        }
        let mut list = div().flex_1().flex().flex_col().when(size == Size::Large, |l| l.gap(px(8.))).when(size != Size::Large, |l| l.justify_around());
        let mut last_tool = String::new();
        for (i, c) in rows.into_iter().enumerate() {
            if size == Size::Large && tool(&c.label) != last_tool {
                last_tool = tool(&c.label);
                list = list.child(div().pt(px(4.)).text_size(px(10.5)).font_weight(FontWeight::BOLD).text_color(t.dim).child(last_tool.to_uppercase()));
            }
            let title = c.label.clone();
            let main = self.main.clone();
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().flex_1().min_w_0().flex().flex_col().gap(px(3.)).child(div().text_size(px(12.)).truncate().child(c.label.clone())).child(track(t, (c.value / max) as f32, t.dim, 5.)))
                    .child(div().w(px(28.)).text_right().font_weight(FontWeight::BOLD).child((c.value as i64).to_string()))
                    .child(
                        div()
                            .id(SharedString::from(format!("rule-{i}")))
                            .h(px(24.))
                            .px(px(8.))
                            .flex()
                            .items_center()
                            .rounded(px(6.))
                            .border_1()
                            .border_color(t.accent)
                            .text_size(px(11.))
                            .text_color(t.accent)
                            .cursor_pointer()
                            .hover(|s| s.bg(t.accent_soft))
                            .tooltip(crate::ui::header::tip("Ask an agent to add a rule so this stops asking"))
                            .on_click(cx.listener(move |_, _, w, cx| crate::ui::rules::ask(&main, format!("Add a midna rule: allow \"{title}\" without asking"), w, cx)))
                            .child("+ Rule"),
                    ),
            );
        }
        h.child(list)
    }

    fn projects_widget(&self, t: &Theme, d: &InsightsDetail) -> Div {
        let total: f64 = d.projects.iter().map(|c| c.value).sum();
        let h = head(t, "Project mix", &format!("agent hours · {}", Unit::Secs.fmt(total)), None);
        if total <= 0. {
            return h.child(empty(t, "No agent work in this range"));
        }
        let rows: Vec<_> = d.projects.iter().take(5).collect();
        let mut bar = div().flex().gap(px(2.)).h(px(14.));
        let mut list = div().flex_1().flex().flex_col().justify_around();
        for c in &rows {
            let color = self.project_color(t, &c.key);
            bar = bar.child(div().h_full().w(relative((c.value / total) as f32)).rounded(px(2.)).bg(color));
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(swatch(color))
                    .child(div().flex_1().min_w_0().truncate().child(c.label.clone()))
                    .child(div().font_weight(FontWeight::BOLD).child(Unit::Secs.fmt(c.value)))
                    .child(div().w(px(36.)).text_right().text_size(px(11.)).text_color(t.dim).child(format!("{:.0}%", c.value / total * 100.))),
            );
        }
        h.child(bar).child(list)
    }
}

impl Range {
    fn noun(self) -> &'static str {
        match self {
            Range::Today => "day",
            Range::Week => "week",
            Range::Month => "month",
        }
    }
}

fn head(t: &Theme, title: &str, sub: &str, right: Option<AnyElement>) -> Div {
    div().flex_1().min_h_0().flex().flex_col().gap(px(10.)).child(card_head(t, title, sub, right))
}

fn parallelism(t: &Theme, p: &Palette, d: &InsightsDetail, range: Range, plot_h: f32) -> Div {
    let c = &d.concurrency;
    let h = head(t, "Agents working at once", if c.step_secs == 600 { "10-minute steps" } else { "hourly steps" }, None);
    if c.peak == 0 {
        return h.child(empty(t, "No agent worked in this range"));
    }
    let samples = c.samples.clone();
    let top = (c.peak as f64 + 1.).max(2.);
    let color = p.agents;
    let fillc = charts::alpha(p.agents, 0.28);
    let grid = charts::alpha(t.dim, 0.18);
    let plot = canvas(
        |_, _, _| {},
        move |b, _, window, _| {
            let n = samples.len().max(1) as f32;
            let (x0, y0) = (f32::from(b.origin.x), f32::from(b.origin.y));
            let (w, hgt) = (f32::from(b.size.width), f32::from(b.size.height));
            for k in 1..=(top as i32 - 1) {
                let y = y0 + hgt - (k as f32 / top as f32) * hgt;
                window.paint_quad(fill(Bounds::new(point(px(x0), px(y)), size(px(w), px(1.))), grid));
            }
            let sw = w / n;
            for (i, v) in samples.iter().enumerate() {
                if *v <= 0. {
                    continue;
                }
                let hh = (*v / top) as f32 * hgt;
                let x = x0 + i as f32 * sw;
                window.paint_quad(fill(Bounds::new(point(px(x), px(y0 + hgt - hh)), size(px(sw + 0.5), px(hh))), fillc));
                window.paint_quad(fill(Bounds::new(point(px(x), px(y0 + hgt - hh)), size(px(sw + 0.5), px(2.))), color));
            }
        },
    )
    .w_full()
    .h(px(plot_h));
    let n = c.samples.len().max(1);
    let start = parse_rfc3339(&d.from).unwrap_or(0);
    let mut axis = div().relative().h(px(14.)).text_size(px(10.5)).text_color(t.dim);
    let marks: Vec<(usize, String)> = if c.step_secs == 600 {
        (0..n).step_by(36).map(|i| (i, hour_label(local_tm(start + i as i64 * 600).tm_hour))).collect()
    } else {
        let per_day = 24;
        (0..n).step_by(per_day * if range == Range::Month { 5 } else { 1 }).map(|i| (i, short_day(&midna_proto::time::format_unix(start + i as i64 * 3600)))).collect()
    };
    for (i, label) in marks {
        axis = axis.child(div().absolute().left(relative(i as f32 / n as f32)).child(label));
    }
    let peak_when = c.peak_at.as_deref().map(|a| {
        let tm = local_tm(parse_rfc3339(a).unwrap_or(0));
        format!("{}:{:02}", hour_label(tm.tm_hour).trim_end_matches(['a', 'p']), tm.tm_min) + if tm.tm_hour < 12 { " AM" } else { " PM" }
    });
    h.child(
        div().flex_1().flex().gap(px(16.)).child(div().flex_1().min_w_0().flex().flex_col().gap(px(4.)).child(plot).child(axis)).child(side_stats(
            t,
            vec![
                stat(t, format!("{:.1}", c.avg_while_working), "average while any agent works"),
                stat(t, dur(c.multi_secs), "with 2 or more at once"),
                stat(t, c.peak.to_string(), &peak_when.map(|w| format!("peak, at {w}")).unwrap_or_else(|| "peak".into())),
            ],
        )),
    )
}

fn autonomy(t: &Theme, p: &Palette, d: &InsightsDetail) -> Div {
    let tl = &d.turns;
    let total: i64 = tl.bins.iter().sum();
    let h = head(t, "Autonomy", &format!("{total} turns · median {}", dur(tl.median_secs)), None);
    if total == 0 {
        return h.child(empty(t, "No finished turns in this range"));
    }
    let labels = ["<1m", "1–5m", "5–15", "15–30", "30–60", "1h+"];
    let max = tl.bins.iter().copied().max().unwrap_or(1).max(1) as f32;
    let last = tl.bins.iter().rposition(|n| *n > 0).unwrap_or(0);
    let mut cols = div().flex_1().flex().items_end().gap(px(6.)).border_b_1().border_color(t.line);
    let mut xl = div().flex().gap(px(6.)).text_size(px(10.5)).text_color(t.dim);
    for (i, n) in tl.bins.iter().enumerate() {
        let color = if i == last { p.agents } else { charts::alpha(p.agents, 0.55) };
        cols = cols.child(
            div()
                .flex_1()
                .h_full()
                .flex()
                .flex_col()
                .justify_end()
                .items_center()
                .gap(px(2.))
                .child(div().text_size(px(10.5)).text_color(t.dim).child(n.to_string()))
                .child(div().w_full().h(relative((*n as f32 / max) * 0.8)).min_h(px(if *n > 0 { 3. } else { 0. })).rounded_t(px(3.)).bg(color)),
        );
        xl = xl.child(div().flex_1().text_center().child(labels.get(i).copied().unwrap_or("")));
    }
    h.child(stat(t, dur(tl.longest_secs), "longest turn without you")).child(cols).child(xl)
}

fn latency(t: &Theme, p: &Palette, d: &InsightsDetail) -> Div {
    let n = d.waits.len();
    let h = head(t, "How fast you answer", &format!("{n} needs-you items"), None);
    if n == 0 {
        return h.child(empty(t, "Nothing needed you in this range"));
    }
    let (lo, hi) = (5f32.ln(), 3600f32.ln());
    let x = |s: i64| ((s.max(5) as f32).ln() - lo) / (hi - lo);
    let mut sorted: Vec<i64> = d.waits.iter().map(|w| w.secs).collect();
    sorted.sort();
    let median = sorted[n / 2];
    let slow = sorted.iter().filter(|s| **s > 900).count();
    let mut plot = div().relative().h(px(120.)).w_full();
    plot = plot.child(div().absolute().top_0().bottom(px(16.)).left(relative(x(900))).right_0().bg(charts::alpha(p.waiting, 0.08)));
    for (s, l) in [(5, "5s"), (30, "30s"), (60, "1m"), (300, "5m"), (900, "15m"), (3600, "1h")] {
        plot = plot
            .child(div().absolute().top_0().bottom(px(16.)).left(relative(x(s))).w(px(1.)).bg(charts::alpha(t.dim, 0.18)))
            .child(div().absolute().bottom_0().left(relative(x(s))).ml(px(-8.)).text_size(px(10.)).text_color(t.dim).child(l));
    }
    plot = plot.child(div().absolute().top_0().bottom(px(16.)).left(relative(x(median))).w(px(1.5)).bg(t.fg));
    let mut lanes = [f32::MIN; 8];
    let order = [3usize, 4, 2, 5, 1, 6, 0, 7];
    for (i, w) in d.waits.iter().enumerate() {
        let fx = x(w.secs).clamp(0., 1.);
        let lane = order.iter().copied().find(|l| fx - lanes[*l] > 0.016).unwrap_or(3);
        lanes[lane] = fx;
        plot = plot.child(
            div()
                .id(SharedString::from(format!("wait-{i}")))
                .absolute()
                .top(px(4. + lane as f32 * 12.))
                .left(relative(fx))
                .ml(px(-5.))
                .size(px(10.))
                .rounded_full()
                .border_1()
                .border_color(t.panel)
                .bg(p.waiting)
                .tooltip(crate::ui::header::tip(&format!("{} · {}", dur(w.secs), w.title))),
        );
    }
    h.child(div().flex_1().flex().gap(px(16.)).child(div().flex_1().min_w_0().child(plot)).child(side_stats(
        t,
        vec![stat(t, dur(median), "median wait"), stat(t, slow.to_string(), "waited over 15 min"), stat(t, dur(*sorted.last().unwrap_or(&0)), "longest")],
    )))
}

impl InsightsView {
    fn agent_time(&self, t: &Theme, p: &Palette, d: &InsightsDetail, size: Size) -> Div {
        let (w, b, i) = d.agent_time.iter().fold((0, 0, 0), |a, r| (a.0 + r.working_secs, a.1 + r.blocked_secs, a.2 + r.idle_secs));
        let idle_c = charts::alpha(t.dim, 0.45);
        if size == Size::Small {
            let h = head(t, "Agents waited on you", "blocked + done with no next prompt", None);
            if d.agent_time.is_empty() {
                return h.child(empty(t, "No agent terminals in this range"));
            }
            let top = d.agent_time.first().map(|r| format!("most: {} ({})", r.label, dur(r.blocked_secs + r.idle_secs))).unwrap_or_default();
            let tot = (w + b + i).max(1) as f32;
            return h.child(stat(t, dur(b + i), &format!("blocked {} · idle {}", dur(b), dur(i)))).child(
                div().h(px(14.)).flex().gap(px(2.)).rounded(px(3.)).overflow_hidden().child(div().h_full().w(relative(w as f32 / tot)).bg(p.agents)).child(div().h_full().w(relative(b as f32 / tot)).bg(p.waiting)).child(div().h_full().w(relative(i as f32 / tot)).bg(idle_c)),
            ).child(div().text_size(px(11.5)).text_color(t.dim).truncate().child(top));
        }
        let h = head(t, "Where agent time went", "per agent terminal", None).child(legend(
            t,
            &[(p.agents, format!("Working {}", dur(w))), (p.waiting, format!("Blocked on you {}", dur(b))), (idle_c, format!("Done, no next prompt {}", dur(i)))],
        ));
        if d.agent_time.is_empty() {
            return h.child(empty(t, "No agent terminals in this range"));
        }
        let large = size == Size::Large;
        let max = d.agent_time.iter().map(|r| r.working_secs + r.blocked_secs + r.idle_secs).max().unwrap_or(1).max(1) as f32;
        let mut list = div().flex_1().flex().flex_col().when(large, |l| l.gap(px(10.))).when(!large, |l| l.justify_around());
        for r in d.agent_time.iter().take(if large { 12 } else { 5 }) {
            let seg = |v: i64, c: Hsla| div().h_full().w(relative(v as f32 / max)).bg(c);
            let project = r.project_id.as_deref().map(|p| self.project_name(p)).unwrap_or_default();
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(
                        div().w(px(150.)).flex_none().flex().flex_col().child(div().truncate().child(r.label.clone())).when(large && !project.is_empty(), |c| c.child(div().text_size(px(10.5)).text_color(t.dim).truncate().child(project))),
                    )
                    .child(div().flex_1().min_w_0().h(px(14.)).flex().gap(px(2.)).overflow_hidden().rounded(px(3.)).child(seg(r.working_secs, p.agents)).child(seg(r.blocked_secs, p.waiting)).child(seg(r.idle_secs, idle_c)))
                    .child(div().w(px(90.)).flex_none().text_right().text_size(px(11.)).text_color(t.dim).child(format!("{} waiting", dur(r.blocked_secs + r.idle_secs)))),
            );
        }
        h.child(list)
    }
}

fn parallelism_small(t: &Theme, p: &Palette, d: &InsightsDetail) -> Div {
    let c = &d.concurrency;
    let h = head(t, "Agents at once", "", None);
    if c.peak == 0 {
        return h.child(empty(t, "No agent worked in this range"));
    }
    let samples = c.samples.clone();
    let top = c.peak as f64 + 0.5;
    let (fillc, color) = (charts::alpha(p.agents, 0.28), p.agents);
    let spark = canvas(
        |_, _, _| {},
        move |b, _, window, _| {
            let n = samples.len().max(1) as f32;
            let sw = f32::from(b.size.width) / n;
            let (x0, y0, hgt) = (f32::from(b.origin.x), f32::from(b.origin.y), f32::from(b.size.height));
            for (i, v) in samples.iter().enumerate().filter(|(_, v)| **v > 0.) {
                let hh = (*v / top) as f32 * hgt;
                let x = x0 + i as f32 * sw;
                window.paint_quad(fill(Bounds::new(point(px(x), px(y0 + hgt - hh)), size(px(sw + 0.5), px(hh))), fillc));
                window.paint_quad(fill(Bounds::new(point(px(x), px(y0 + hgt - hh)), size(px(sw + 0.5), px(1.5))), color));
            }
        },
    )
    .w_full()
    .h(px(64.));
    h.child(stat(t, c.peak.to_string(), "most at once")).child(div().text_size(px(12.)).text_color(t.dim).child(format!("{:.1} on average · {} with 2+", c.avg_while_working, dur(c.multi_secs)))).child(div().flex_1()).child(spark)
}

fn latency_small(t: &Theme, p: &Palette, d: &InsightsDetail) -> Div {
    let n = d.waits.len();
    let h = head(t, "How fast you answer", &format!("{n} needs-you items"), None);
    if n == 0 {
        return h.child(empty(t, "Nothing needed you in this range"));
    }
    let mut sorted: Vec<i64> = d.waits.iter().map(|w| w.secs).collect();
    sorted.sort();
    let slow = sorted.iter().filter(|s| **s > 900).count();
    let fast = sorted.iter().filter(|s| **s <= 60).count();
    let total = n as f32;
    h.child(stat(t, dur(sorted[n / 2]), "median wait")).child(div().flex_1()).child(
        div().h(px(10.)).flex().gap(px(2.)).rounded(px(3.)).overflow_hidden()
            .child(div().h_full().w(relative(fast as f32 / total)).bg(charts::alpha(p.waiting, 0.4)))
            .child(div().h_full().w(relative((n - fast - slow) as f32 / total)).bg(charts::alpha(p.waiting, 0.7)))
            .child(div().h_full().w(relative(slow as f32 / total)).bg(p.waiting)),
    ).child(div().text_size(px(11.5)).text_color(t.dim).child(format!("{fast} within a minute · {slow} over 15 min")))
}

/// Large "how fast you answer": your slowest hours of the day and the longest waits.
fn latency_more(t: &Theme, p: &Palette, d: &InsightsDetail) -> Div {
    let mut by_hour: Vec<Vec<i64>> = vec![vec![]; 24];
    for w in &d.waits {
        let hr = local_tm(parse_rfc3339(&w.resolved_at).unwrap_or(0)).tm_hour as usize % 24;
        by_hour[hr].push(w.secs);
    }
    let med: Vec<i64> = by_hour.iter_mut().map(|v| { v.sort(); v.get(v.len() / 2).copied().unwrap_or(0) }).collect();
    let max = med.iter().copied().max().unwrap_or(1).max(1) as f32;
    let mut cols = div().h(px(90.)).flex().items_end().gap(px(3.)).border_b_1().border_color(t.line);
    for (hr, m) in med.iter().enumerate() {
        cols = cols.child(
            div().id(SharedString::from(format!("lat-hour-{hr}"))).flex_1().h(relative(*m as f32 / max)).min_h(px(if *m > 0 { 2. } else { 0. })).rounded_t(px(2.)).bg(p.waiting)
                .tooltip(crate::ui::header::tip(format!("{} · median {}", hour_label(hr as i32), dur(*m)))),
        );
    }
    let mut axis = div().flex().gap(px(3.));
    for hr in 0..24 {
        axis = axis.child(div().flex_1().text_size(px(10.)).text_color(t.dim).child(if hr % 6 == 0 { hour_label(hr) } else { String::new() }));
    }
    let mut longest: Vec<&midna_proto::InsightsWait> = d.waits.iter().collect();
    longest.sort_by_key(|w| std::cmp::Reverse(w.secs));
    let mut list = div().flex().flex_col().gap(px(4.));
    for w in longest.into_iter().take(3) {
        list = list.child(div().flex().gap(px(10.)).child(div().w(px(60.)).flex_none().font_weight(FontWeight::BOLD).child(dur(w.secs))).child(div().flex_1().min_w_0().truncate().text_color(t.dim).child(w.title.clone())));
    }
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .pt(px(6.))
        .child(div().text_size(px(10.5)).font_weight(FontWeight::BOLD).text_color(t.dim).child("MEDIAN WAIT BY HOUR OF DAY"))
        .child(cols)
        .child(axis)
        .child(div().pt(px(4.)).text_size(px(10.5)).font_weight(FontWeight::BOLD).text_color(t.dim).child("LONGEST WAITS"))
        .child(list)
}

fn corrections(t: &Theme, p: &Palette, d: &InsightsDetail) -> Div {
    let (den, stop) = d.corrections.iter().fold((0, 0), |a, c| (a.0 + c.denied, a.1 + c.stopped));
    let soft = charts::alpha(p.waiting, 0.45);
    let h = head(t, "Course corrections", &format!("{} denied · {} stopped", den, stop), None);
    if den + stop == 0 {
        return h.child(empty(t, "No denials or stopped turns in this range"));
    }
    let days: Vec<_> = d.corrections.iter().rev().take(14).rev().collect();
    let max = days.iter().map(|c| c.denied + c.stopped).max().unwrap_or(1).max(1) as f32;
    let mut cols = div().flex_1().flex().items_end().gap(px(6.)).border_b_1().border_color(t.line);
    let mut xl = div().flex().gap(px(6.)).text_size(px(10.5)).text_color(t.dim);
    for c in &days {
        cols = cols.child(
            div()
                .flex_1()
                .h_full()
                .flex()
                .flex_col()
                .justify_end()
                .gap(px(2.))
                .child(div().w_full().h(relative(c.stopped as f32 / max * 0.9)).rounded_t(px(3.)).bg(soft))
                .child(div().w_full().h(relative(c.denied as f32 / max * 0.9)).bg(p.waiting)),
        );
        xl = xl.child(div().flex_1().text_center().child(short_day(&c.day)));
    }
    h.child(cols).child(xl).child(legend(t, &[(p.waiting, "Denied".into()), (soft, "Stopped".into())]))
}

fn heatmap_small(t: &Theme, p: &Palette, d: &InsightsDetail) -> Div {
    let h = head(t, "When work happens", "last 7 days", None);
    let mut by_hour = [0i64; 24];
    let mut best = (0i64, String::new(), 0usize);
    for day in &d.heatmap {
        for (hr, s) in day.working_secs.iter().enumerate().take(24) {
            by_hour[hr] += s;
            if *s > best.0 {
                best = (*s, when(&day.day).split(' ').next().unwrap_or("").to_string(), hr);
            }
        }
    }
    if best.0 == 0 {
        return h.child(empty(t, "No agent work in the last 7 days"));
    }
    let max = by_hour.iter().copied().max().unwrap_or(1).max(1) as f32;
    let mut cols = div().h(px(70.)).flex().items_end().gap(px(2.)).border_b_1().border_color(t.line);
    for v in by_hour {
        cols = cols.child(div().flex_1().h(relative(v as f32 / max)).rounded_t(px(2.)).bg(charts::alpha(p.agents, 0.3 + 0.7 * v as f32 / max)));
    }
    h.child(stat(t, format!("{} {}", best.1, hour_label(best.2 as i32)), "busiest hour")).child(div().flex_1()).child(cols).child(
        div().flex().justify_between().text_size(px(10.)).text_color(t.dim).child("12a").child("12p").child("11p"),
    )
}

/// `cell_h` 18 (wide) or 40 (large, with a total per day and per hour).
fn heatmap(t: &Theme, p: &Palette, d: &InsightsDetail, cell_h: f32, totals: bool) -> Div {
    let h = head(t, "When work happens", "last 7 days", None);
    if d.heatmap.iter().all(|day| day.working_secs.iter().all(|s| *s == 0)) {
        return h.child(empty(t, "No agent work in the last 7 days"));
    }
    let max = d.heatmap.iter().flat_map(|day| day.working_secs.iter().copied()).max().unwrap_or(1).max(1) as f32;
    let mut rows = div().flex().flex_col().gap(px(2.));
    for day in &d.heatmap {
        let mut r = div().flex().items_center().gap(px(2.)).child(div().w(px(30.)).flex_none().text_size(px(10.5)).text_color(t.dim).child(when(&day.day).split(' ').next().unwrap_or("").to_string()));
        for hr in 0..24 {
            let s = day.working_secs.get(hr).copied().unwrap_or(0);
            let you = day.you.get(hr).copied().unwrap_or(false);
            let step = if s == 0 { 0. } else { 0.25 + 0.75 * (s as f32 / max).min(1.) };
            r = r.child(
                div()
                    .flex_1()
                    .h(px(cell_h))
                    .rounded(px(3.))
                    .bg(if s == 0 { t.raised } else { charts::alpha(p.agents, step) })
                    // every cell gets the border, so the outlined ones aren't wider and the columns line up
                    .border_2()
                    .border_color(if you { p.you } else { transparent_black() }),
            );
        }
        if totals {
            r = r.child(div().w(px(54.)).flex_none().text_right().text_size(px(11.)).text_color(t.dim).child(dur(day.working_secs.iter().sum())));
        }
        rows = rows.child(r);
    }
    let mut axis = div().flex().gap(px(2.)).child(div().w(px(30.)).flex_none());
    for hr in 0..24 {
        axis = axis.child(div().flex_1().text_size(px(10.)).text_color(t.dim).child(if hr % 6 == 0 { hour_label(hr) } else { String::new() }));
    }
    if totals {
        axis = axis.child(div().w(px(54.)).flex_none());
        let by_hour: Vec<i64> = (0..24).map(|hr| d.heatmap.iter().map(|day| day.working_secs.get(hr).copied().unwrap_or(0)).sum()).collect();
        let hmax = by_hour.iter().copied().max().unwrap_or(1).max(1) as f32;
        let mut bars = div().h(px(56.)).flex().items_end().gap(px(2.)).child(div().w(px(30.)).flex_none().text_size(px(10.)).text_color(t.dim).child("all"));
        for v in by_hour {
            bars = bars.child(div().flex_1().h(relative(v as f32 / hmax)).rounded_t(px(2.)).bg(charts::alpha(p.agents, 0.6)));
        }
        rows = rows.child(bars.child(div().w(px(54.)).flex_none()));
    }
    let ramp: Vec<Div> = [0.25f32, 0.45, 0.65, 0.85, 1.].iter().map(|a| div().w(px(14.)).h(px(10.)).rounded(px(2.)).bg(charts::alpha(p.agents, *a))).collect();
    h.child(rows).child(axis).child(
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(11.))
            .text_color(t.dim)
            .child("Agent time per hour")
            .children(ramp)
            .child(div().ml(px(14.)).w(px(14.)).h(px(10.)).rounded(px(2.)).bg(t.raised).border_2().border_color(p.you))
            .child("you were active"),
    )
}

fn models(t: &Theme, p: &Palette, d: &InsightsDetail) -> Div {
    let total: f64 = d.models.iter().map(|c| c.value).sum();
    let h = head(t, "Model mix", &format!("{} spent", Unit::Usd.fmt(total)), None);
    if total <= 0. {
        return h.child(empty(t, "No spend recorded in this range"));
    }
    let max = d.models.first().map(|c| c.value).unwrap_or(1.).max(1e-9);
    let mut list = div().flex_1().flex().flex_col().justify_around();
    for c in d.models.iter().take(4) {
        list = list.child(
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(div().flex().child(div().flex_1().truncate().child(c.label.clone())).child(div().font_weight(FontWeight::BOLD).child(Unit::Usd.fmt(c.value))))
                .child(track(t, (c.value / max) as f32, p.agents, 8.)),
        );
    }
    h.child(list)
}

fn bests(t: &Theme, d: &InsightsDetail) -> Div {
    let b = &d.bests;
    let h = head(t, "Personal bests", "all time", None);
    if b.peak_agents == 0 && b.longest_turn_secs == 0 {
        return h.child(empty(t, "Records show up once agents have worked"));
    }
    let row = |label: &str, value: String, sub: String| {
        div()
            .flex()
            .flex_col()
            .pb(px(6.))
            .border_b_1()
            .border_color(t.line)
            .child(div().flex().child(div().flex_1().child(label.to_string())).child(div().text_size(px(15.)).font_weight(FontWeight::BOLD).child(value)))
            .child(div().text_size(px(11.)).text_color(t.dim).child(sub))
    };
    h.child(
        div()
            .flex_1()
            .flex()
            .flex_col()
            .justify_around()
            .child(row("Busiest day", dur(b.busiest_day_secs), b.busiest_day.as_deref().map(when).map(|w| format!("{w} · agent working time")).unwrap_or_default()))
            .child(row("Longest turn", dur(b.longest_turn_secs), b.longest_turn_at.as_deref().map(when).unwrap_or_default()))
            .child(row("Most agents at once", b.peak_agents.to_string(), b.peak_at.as_deref().map(when).unwrap_or_default())),
    )
}
