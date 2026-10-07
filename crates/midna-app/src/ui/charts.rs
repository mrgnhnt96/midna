//! Hand-drawn GPUI charts for Insights: stacked columns, line/area, and horizontal stacked
//! bars. Marks are painted with `canvas()` quads and paths; axes, legends and tooltips are
//! ordinary elements. Hover is tracked per chart in a [`ChartState`] that lives in the view.
//!
//! Mark specs follow the dataviz guide: thin marks with 3–4px rounded data ends anchored to
//! the baseline, a 2px surface gap between stacked segments, 2px lines, recessive gridlines,
//! one y axis, text in text colors (series colors only on marks and swatches).
use crate::theme::{Theme, ThemeMode};
use gpui_kit::prelude::*;
use gpui_kit::*;
use std::cell::Cell;
use std::rc::Rc;

/// Width of the y-axis label column.
pub const AXIS_W: f32 = 52.;
/// Space above the top gridline inside the plot.
const PAD_TOP: f32 = 10.;

// ------------------------------------------------------------------ palette

/// Categorical slots (dataviz reference palette, validated for adjacent-pair CVD separation
/// in both modes). Assigned by entity (project order), never by rank.
const CAT_DARK: [u32; 8] = [0x3987e5, 0xd95926, 0x199e70, 0xc98500, 0xd55181, 0x2f9e2f, 0x9085e9, 0xe66767];
const CAT_LIGHT: [u32; 8] = [0x2a78d6, 0xeb6834, 0x1baf7a, 0xeda100, 0xe87ba4, 0x008300, 0x4a3aa7, 0xe34948];

/// Color for categorical slot `i`; slots past the palette fold into a neutral "other".
pub fn cat_color(t: &Theme, i: usize) -> Hsla {
    let set = if t.mode == ThemeMode::Dark { &CAT_DARK } else { &CAT_LIGHT };
    match set.get(i) {
        Some(c) => rgb(*c).into(),
        None => other_color(t),
    }
}

pub fn other_color(t: &Theme) -> Hsla {
    let mut c = t.dim;
    c.a = 0.55;
    c
}

pub fn alpha(mut c: Hsla, a: f32) -> Hsla {
    c.a = a;
    c
}

// ------------------------------------------------------------------ data

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Count,
    Usd,
    Secs,
}

impl Unit {
    pub fn from_str(s: &str) -> Unit {
        match s {
            "usd" => Unit::Usd,
            "secs" => Unit::Secs,
            _ => Unit::Count,
        }
    }

    /// Exact value for tooltips and headlines.
    pub fn fmt(self, v: f64) -> String {
        match self {
            Unit::Count => format!("{}", v.round() as i64),
            Unit::Usd => format!("${v:.2}"),
            Unit::Secs => duration(v),
        }
    }

    /// Compact value for axis ticks.
    fn fmt_axis(self, v: f64) -> String {
        match self {
            Unit::Count => {
                if v >= 1000. {
                    format!("{:.1}k", v / 1000.).replace(".0k", "k")
                } else {
                    format!("{}", v.round() as i64)
                }
            }
            Unit::Usd => {
                if v == 0. {
                    "$0".into()
                } else if v < 1. {
                    format!("${v:.2}")
                } else if v.fract().abs() < 1e-9 {
                    format!("${}", v as i64)
                } else {
                    format!("${v:.1}")
                }
            }
            Unit::Secs => {
                if v == 0. {
                    "0".into()
                } else if v < 3600. {
                    format!("{}m", (v / 60.).round() as i64)
                } else {
                    let h = v / 3600.;
                    if (h.fract()).abs() < 1e-9 { format!("{}h", h as i64) } else { format!("{h:.1}h") }
                }
            }
        }
    }
}

/// "2h 14m", "38m", "45s".
pub fn duration(secs: f64) -> String {
    let s = secs.round().max(0.) as i64;
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        _ => {
            let (h, m) = (s / 3600, s % 3600 / 60);
            if m == 0 { format!("{h}h") } else { format!("{h}h {m}m") }
        }
    }
}

/// A nice axis top and tick step covering `max` with about `ticks` intervals.
pub fn nice_scale(max: f64, unit: Unit, ticks: usize) -> (f64, f64) {
    let ticks = ticks.max(1) as f64;
    if max <= 0. {
        return match unit {
            Unit::Count => (4., 1.),
            Unit::Usd => (1., 0.25),
            Unit::Secs => (3600., 900.),
        };
    }
    let raw = max / ticks;
    let step = match unit {
        Unit::Secs => {
            const STEPS: [f64; 12] = [60., 120., 300., 600., 900., 1800., 3600., 7200., 10800., 14400., 21600., 43200.];
            STEPS.iter().copied().find(|s| *s >= raw).unwrap_or_else(|| (raw / 3600.).ceil() * 3600.)
        }
        _ => {
            let mag = 10f64.powf(raw.log10().floor());
            let norm = raw / mag;
            let n = if norm <= 1. {
                1.
            } else if norm <= 2. {
                2.
            } else if norm <= 2.5 {
                2.5
            } else if norm <= 5. {
                5.
            } else {
                10.
            };
            let s = n * mag;
            if unit == Unit::Count { s.max(1.).ceil() } else { s }
        }
    };
    let top = (max / step).ceil().max(1.) * step;
    (top, step)
}

#[derive(Clone)]
pub struct Series {
    pub key: String,
    pub label: SharedString,
    pub color: Hsla,
}

/// Column data: `values[bucket][series]`. Buckets at or past `reached` are in the future
/// (drawn empty, never hoverable).
#[derive(Clone)]
pub struct ChartData {
    pub unit: Unit,
    pub series: Vec<Series>,
    pub values: Vec<Vec<f64>>,
    /// Sparse x-axis labels (bucket index, text).
    pub x_labels: Vec<(usize, SharedString)>,
    /// Tooltip title per bucket.
    pub titles: Vec<SharedString>,
    pub reached: usize,
}

impl ChartData {
    fn totals(&self) -> Vec<f64> {
        self.values.iter().map(|v| v.iter().sum()).collect()
    }
    pub fn is_empty(&self) -> bool {
        self.values.iter().all(|v| v.iter().all(|x| *x <= 0.))
    }
}

/// Per-chart hover state, owned by the view.
#[derive(Default)]
pub struct ChartState {
    pub hover: Option<usize>,
    plot: Rc<Cell<Option<Bounds<Pixels>>>>,
}

type Getter<V> = fn(&mut V) -> &mut ChartState;
pub type OnPick<V> = Rc<dyn Fn(&mut V, usize, Option<usize>, &mut Window, &mut Context<V>)>;
/// Click on a row of a bar list (view, row index).
pub type RowPick<V> = Rc<dyn Fn(&mut V, usize, &mut Window, &mut Context<V>)>;

fn slot_at(b: Bounds<Pixels>, n: usize, pos: Point<Pixels>) -> Option<usize> {
    if n == 0 || !b.contains(&pos) {
        return None;
    }
    let x = f32::from(pos.x - b.origin.x);
    let slot = f32::from(b.size.width) / n as f32;
    Some(((x / slot).floor() as usize).min(n - 1))
}

// ------------------------------------------------------------------ shared frame

/// Y labels, the plot (with mouse handling), x labels and the tooltip.
#[allow(clippy::too_many_arguments)] // one shared layout for every chart kind
fn frame<V: 'static>(
    id: &str,
    data: &ChartData,
    height: f32,
    top: f64,
    step: f64,
    t: &Theme,
    state: &ChartState,
    get: Getter<V>,
    on_pick: Option<OnPick<V>>,
    plot: AnyElement,
    cx: &mut Context<V>,
) -> Div {
    let plot_h = height - PAD_TOP;
    let n = data.values.len();
    let reached = data.reached.min(n);
    let mut axis = div().relative().w(px(AXIS_W)).h(px(height)).flex_none();
    let mut v = 0.;
    while v <= top + 1e-9 {
        let y = PAD_TOP + (1. - (v / top) as f32) * plot_h;
        axis =
            axis.child(div().absolute().top(px(y - 7.)).right(px(10.)).text_size(px(11.)).line_height(px(14.)).text_color(t.dim).whitespace_nowrap().child(data.unit.fmt_axis(v)));
        v += step;
    }
    let cell = state.plot.clone();
    let cell2 = state.plot.clone();
    let cell3 = state.plot.clone();
    let mut plot_box = div()
        .id(SharedString::from(format!("{id}-plot")))
        .relative()
        .flex_1()
        .min_w_0()
        .h(px(height))
        .child(plot)
        .child(canvas(move |b, _, _| cell.set(Some(b)), |_, _, _, _| {}).absolute().top_0().left_0().size_full())
        .on_mouse_move(cx.listener(move |view, ev: &MouseMoveEvent, _, cx| {
            let Some(b) = cell2.get() else { return };
            let hit = slot_at(b, n, ev.position).filter(|i| *i < reached);
            let st = get(view);
            if st.hover != hit {
                st.hover = hit;
                cx.notify();
            }
        }))
        .on_hover(cx.listener(move |view, hovered: &bool, _, cx| {
            if !*hovered && get(view).hover.is_some() {
                get(view).hover = None;
                cx.notify();
            }
        }));
    if let Some(pick) = on_pick {
        let values = data.values.clone();
        let top_v = top;
        plot_box = plot_box.cursor_pointer().on_mouse_down(
            MouseButton::Left,
            cx.listener(move |view, ev: &MouseDownEvent, window, cx| {
                let Some(b) = cell3.get() else { return };
                let Some(i) = slot_at(b, values.len(), ev.position).filter(|i| *i < reached) else {
                    return;
                };
                // which stacked segment is under the pointer
                let bottom = f32::from(b.origin.y + b.size.height);
                let ph = f32::from(b.size.height) - PAD_TOP;
                let y = f32::from(ev.position.y);
                let mut acc = 0.;
                let mut hit = None;
                for (s, v) in values[i].iter().enumerate() {
                    let lo = bottom - (acc / top_v) as f32 * ph;
                    acc += v;
                    let hi = bottom - (acc / top_v) as f32 * ph;
                    if *v > 0. && y <= lo && y >= hi - 2. {
                        hit = Some(s);
                    }
                }
                pick(view, i, hit, window, cx);
            }),
        );
    }
    // x labels, positioned at slot centers
    let mut xl = div().relative().h(px(18.)).ml(px(AXIS_W)).mt(px(6.));
    for (i, text) in &data.x_labels {
        if n == 0 {
            break;
        }
        let frac = (*i as f32 + 0.5) / n as f32;
        xl = xl.child(
            div()
                .absolute()
                .top_0()
                .left(relative(frac))
                .ml(px(-30.))
                .w(px(60.))
                .flex()
                .justify_center()
                .text_size(px(11.))
                .text_color(t.dim)
                .whitespace_nowrap()
                .child(text.clone()),
        );
    }
    let tooltip = state.hover.and_then(|i| {
        let b = state.plot.get()?;
        let slot = f32::from(b.size.width) / n.max(1) as f32;
        let x = f32::from(b.origin.x) + slot * (i as f32 + 0.5);
        let right_half = (i as f32 + 0.5) / n.max(1) as f32 > 0.6;
        let pos = point(px(if right_half { x - slot / 2. - 10. } else { x + slot / 2. + 10. }), b.origin.y + px(4.));
        Some(tooltip_card(data, i, t, pos, right_half))
    });
    div().flex().flex_col().w_full().child(div().flex().w_full().child(axis).child(plot_box)).child(xl).children(tooltip)
}

fn tooltip_card(data: &ChartData, i: usize, t: &Theme, pos: Point<Pixels>, flip: bool) -> AnyElement {
    let vals = &data.values[i];
    let total: f64 = vals.iter().sum();
    let mut rows = div().flex().flex_col().gap(px(3.));
    let multi = data.series.len() > 1;
    // top segment first, matching the stack order on screen
    for (s, v) in vals.iter().enumerate().rev() {
        if *v <= 0. && multi {
            continue;
        }
        let ser = &data.series[s];
        rows = rows.child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().size(px(9.)).rounded(px(2.)).bg(ser.color).flex_none())
                .child(div().flex_1().text_color(t.fg).child(ser.label.clone()))
                .child(div().pl(px(14.)).font_weight(FontWeight::BOLD).text_color(t.fg).child(data.unit.fmt(*v))),
        );
    }
    if multi && total <= 0. {
        rows = rows.child(div().text_color(t.dim).child("Nothing"));
    }
    let card = div()
        .min_w(px(170.))
        .max_w(px(280.))
        .flex()
        .flex_col()
        .gap(px(6.))
        .px(px(12.))
        .py(px(9.))
        .rounded(px(9.))
        .bg(t.raised)
        .border_1()
        .border_color(t.line)
        .text_size(px(12.))
        .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.28), offset: point(px(0.), px(8.)), blur_radius: px(22.), spread_radius: px(0.), inset: false }])
        .child(div().text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(t.dim).child(data.titles.get(i).cloned().unwrap_or_default()))
        .child(rows)
        .when(multi, |d| {
            d.child(
                div()
                    .flex()
                    .pt(px(5.))
                    .border_t_1()
                    .border_color(t.line)
                    .child(div().flex_1().text_color(t.dim).child("Total"))
                    .child(div().font_weight(FontWeight::BOLD).child(data.unit.fmt(total))),
            )
        });
    deferred(anchored().position(pos).anchor(if flip { Anchor::TopRight } else { Anchor::TopLeft }).snap_to_window_with_margin(px(8.)).child(card))
        .with_priority(3)
        .into_any_element()
}

fn gridlines(window: &mut Window, b: Bounds<Pixels>, top: f64, step: f64, t: &Theme) {
    let ph = f32::from(b.size.height) - PAD_TOP;
    let bottom = f32::from(b.origin.y + b.size.height);
    let mut v = 0.;
    while v <= top + 1e-9 {
        let y = bottom - (v / top) as f32 * ph;
        let color = if v == 0. { alpha(t.dim, 0.45) } else { alpha(t.line, if t.mode == ThemeMode::Dark { 0.8 } else { 0.9 }) };
        window.paint_quad(fill(Bounds::new(point(b.origin.x, px(y - 0.5)), size(b.size.width, px(1.))), color));
        v += step;
    }
}

fn empty_note(t: &Theme, text: &str) -> AnyElement {
    div().absolute().top_0().left_0().size_full().flex().items_center().justify_center().pb(px(10.)).text_size(px(12.)).text_color(t.dim).child(text.to_string()).into_any_element()
}

// ------------------------------------------------------------------ stacked columns

/// Stacked columns (one segment per series). `on_pick(view, bucket, series)` on click.
#[allow(clippy::too_many_arguments)]
pub fn stacked_bars<V: 'static>(
    id: &str,
    data: &ChartData,
    height: f32,
    empty_text: &str,
    t: &Theme,
    state: &ChartState,
    get: Getter<V>,
    on_pick: Option<OnPick<V>>,
    cx: &mut Context<V>,
) -> Div {
    let max = data.totals().into_iter().fold(0., f64::max);
    let (top, step) = nice_scale(max, data.unit, 4);
    let d = data.clone();
    let th = t.clone();
    let hover = state.hover;
    let plot = div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .child(
            canvas(
                |_, _, _| {},
                move |b, _, window, _| {
                    gridlines(window, b, top, step, &th);
                    let n = d.values.len().max(1);
                    let slot = f32::from(b.size.width) / n as f32;
                    let ph = f32::from(b.size.height) - PAD_TOP;
                    let bottom = f32::from(b.origin.y + b.size.height);
                    let bar_w = (slot * 0.64).clamp(2., 34.);
                    if let Some(i) = hover {
                        let x = f32::from(b.origin.x) + slot * i as f32;
                        window.paint_quad(fill(Bounds::new(point(px(x), b.origin.y), size(px(slot), b.size.height)), alpha(th.fg, 0.05)).corner_radii(px(4.)));
                    }
                    let gap = if bar_w > 6. { 2. } else { 1. };
                    for (i, vals) in d.values.iter().enumerate() {
                        let x = f32::from(b.origin.x) + slot * i as f32 + (slot - bar_w) / 2.;
                        let last = vals.iter().rposition(|v| *v > 0.);
                        let mut y = bottom;
                        let dimmed = hover.is_some_and(|h| h != i);
                        for (s, v) in vals.iter().enumerate() {
                            if *v <= 0. {
                                continue;
                            }
                            let h = (*v / top) as f32 * ph;
                            let is_top = Some(s) == last;
                            let seg_top = y - h;
                            let seg_h = if is_top { h } else { (h - gap).max(0.5) };
                            let r = if is_top { (bar_w / 2.).min(3.5) } else { 0. };
                            let mut c = d.series[s].color;
                            if dimmed {
                                c.a *= 0.55;
                            }
                            let q = fill(Bounds::new(point(px(x), px(seg_top + if is_top { 0. } else { gap })), size(px(bar_w), px(seg_h))), c).corner_radii(Corners {
                                top_left: px(r),
                                top_right: px(r),
                                bottom_left: px(0.),
                                bottom_right: px(0.),
                            });
                            window.paint_quad(q);
                            y = seg_top;
                        }
                    }
                },
            )
            .size_full(),
        )
        .when(data.is_empty(), |el| el.child(empty_note(t, empty_text)))
        .into_any_element();
    frame(id, data, height, top, step, t, state, get, on_pick, plot, cx)
}

// ------------------------------------------------------------------ line / area

/// One line per series with a soft area under the first; crosshair + point on hover.
#[allow(clippy::too_many_arguments)]
pub fn area_line<V: 'static>(id: &str, data: &ChartData, height: f32, empty_text: &str, t: &Theme, state: &ChartState, get: Getter<V>, cx: &mut Context<V>) -> Div {
    let max = data.values.iter().flat_map(|v| v.iter().copied()).fold(0., f64::max);
    let (top, step) = nice_scale(max, data.unit, 4);
    let d = data.clone();
    let th = t.clone();
    let hover = state.hover;
    let plot = div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .child(
            canvas(
                |_, _, _| {},
                move |b, _, window, _| {
                    gridlines(window, b, top, step, &th);
                    let n = d.values.len().max(1);
                    let reached = d.reached.min(d.values.len());
                    let slot = f32::from(b.size.width) / n as f32;
                    let ph = f32::from(b.size.height) - PAD_TOP;
                    let bottom = f32::from(b.origin.y + b.size.height);
                    let xy = |i: usize, v: f64| point(px(f32::from(b.origin.x) + slot * (i as f32 + 0.5)), px(bottom - (v / top) as f32 * ph));
                    if let Some(i) = hover {
                        let x = f32::from(b.origin.x) + slot * (i as f32 + 0.5);
                        window.paint_quad(fill(Bounds::new(point(px(x - 0.5), b.origin.y), size(px(1.), b.size.height)), alpha(th.dim, 0.6)));
                    }
                    for (s, ser) in d.series.iter().enumerate().rev() {
                        if reached == 0 {
                            break;
                        }
                        let pts: Vec<Point<Pixels>> = (0..reached).map(|i| xy(i, d.values[i].get(s).copied().unwrap_or(0.))).collect();
                        if s == 0 && pts.len() > 1 {
                            let mut area = PathBuilder::fill();
                            area.move_to(point(pts[0].x, px(bottom)));
                            for p in &pts {
                                area.line_to(*p);
                            }
                            area.line_to(point(pts[pts.len() - 1].x, px(bottom)));
                            area.close();
                            if let Ok(path) = area.build() {
                                window.paint_path(path, alpha(ser.color, if th.mode == ThemeMode::Dark { 0.20 } else { 0.14 }));
                            }
                        }
                        if pts.len() > 1 {
                            let mut line = PathBuilder::stroke(px(2.));
                            line.move_to(pts[0]);
                            for p in &pts[1..] {
                                line.line_to(*p);
                            }
                            if let Ok(path) = line.build() {
                                window.paint_path(path, ser.color);
                            }
                        }
                        if let Some(i) = hover.filter(|i| *i < pts.len()) {
                            let p = pts[i];
                            let ring = 2.;
                            let r = 4.5;
                            window.paint_quad(
                                fill(Bounds::new(point(p.x - px(r + ring), p.y - px(r + ring)), size(px(2. * (r + ring)), px(2. * (r + ring)))), th.panel)
                                    .corner_radii(px(r + ring)),
                            );
                            window.paint_quad(fill(Bounds::new(point(p.x - px(r), p.y - px(r)), size(px(2. * r), px(2. * r))), ser.color).corner_radii(px(r)));
                        }
                    }
                },
            )
            .size_full(),
        )
        .when(data.is_empty(), |el| el.child(empty_note(t, empty_text)))
        .into_any_element();
    frame(id, data, height, top, step, t, state, get, None, plot, cx)
}

// ------------------------------------------------------------------ horizontal breakdown

#[derive(Clone)]
pub struct HRow {
    pub label: SharedString,
    pub sub: SharedString,
    /// One value per series.
    pub values: Vec<f64>,
}

/// Horizontal stacked bars, one row per item, sharing one scale. Hover a row for exact
/// values; click calls `on_pick(view, row)`.
#[allow(clippy::too_many_arguments)]
pub fn hbars<V: 'static>(
    id: &str,
    rows: &[HRow],
    series: &[Series],
    unit: Unit,
    t: &Theme,
    hover: Option<usize>,
    get_hover: fn(&mut V) -> &mut Option<usize>,
    on_pick: Option<RowPick<V>>,
    cx: &mut Context<V>,
) -> Div {
    let max = rows.iter().map(|r| r.values.iter().sum::<f64>()).fold(0., f64::max);
    let (top, _) = nice_scale(max, unit, 4);
    let mut list = div().flex().flex_col().gap(px(2.));
    for (i, row) in rows.iter().enumerate() {
        let vals = row.values.clone();
        let colors: Vec<Hsla> = series.iter().map(|s| s.color).collect();
        let total: f64 = vals.iter().sum();
        let hovered = hover == Some(i);
        let pick = on_pick.clone();
        let track = alpha(t.fg, if t.mode == ThemeMode::Dark { 0.05 } else { 0.06 });
        let bar = canvas(
            |_, _, _| {},
            move |b, _, window, _| {
                let w = f32::from(b.size.width);
                let h = 12.;
                let y = f32::from(b.origin.y) + (f32::from(b.size.height) - h) / 2.;
                window.paint_quad(fill(Bounds::new(point(b.origin.x, px(y)), size(b.size.width, px(h))), track).corner_radii(px(3.)));
                let mut x = f32::from(b.origin.x);
                let last = vals.iter().rposition(|v| *v > 0.);
                for (s, v) in vals.iter().enumerate() {
                    if *v <= 0. {
                        continue;
                    }
                    let seg = ((*v / top) as f32 * w).max(2.);
                    let is_last = Some(s) == last;
                    let first = x == f32::from(b.origin.x);
                    let seg_w = if is_last { seg } else { (seg - 2.).max(1.) };
                    let r = |on: bool| px(if on { 3. } else { 0. });
                    window.paint_quad(fill(Bounds::new(point(px(x), px(y)), size(px(seg_w), px(h))), colors[s]).corner_radii(Corners {
                        top_left: r(first),
                        bottom_left: r(first),
                        top_right: r(is_last),
                        bottom_right: r(is_last),
                    }));
                    x += seg;
                }
            },
        )
        .flex_1()
        .h(px(22.));
        let mut row_el = div()
            .id(SharedString::from(format!("{id}-row-{i}")))
            .relative()
            .flex()
            .items_center()
            .gap(px(12.))
            .px(px(8.))
            .py(px(4.))
            .rounded(px(6.))
            .when(hovered, |d| d.bg(alpha(t.fg, 0.05)))
            .child(
                div()
                    .w(px(150.))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .child(div().text_size(px(12.5)).text_color(t.fg).whitespace_nowrap().overflow_hidden().text_ellipsis().child(row.label.clone()))
                    .child(div().text_size(px(11.)).text_color(t.dim).whitespace_nowrap().overflow_hidden().text_ellipsis().child(row.sub.clone())),
            )
            .child(bar)
            .child(div().w(px(64.)).flex_none().flex().justify_end().text_size(px(12.)).text_color(t.fg).child(unit.fmt(total)))
            .on_hover(cx.listener(move |view, h: &bool, _, cx| {
                let slot = get_hover(view);
                let next = if *h {
                    Some(i)
                } else if *slot == Some(i) {
                    None
                } else {
                    *slot
                };
                if *slot != next {
                    *slot = next;
                    cx.notify();
                }
            }));
        if let Some(p) = pick {
            row_el = row_el.cursor_pointer().on_click(cx.listener(move |view, _, window, cx| p(view, i, window, cx)));
        }
        if hovered {
            // exact values beside the row
            let mut tip = div()
                .flex()
                .flex_col()
                .gap(px(3.))
                .min_w(px(170.))
                .px(px(12.))
                .py(px(9.))
                .rounded(px(9.))
                .bg(t.raised)
                .border_1()
                .border_color(t.line)
                .text_size(px(12.))
                .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.28), offset: point(px(0.), px(8.)), blur_radius: px(22.), spread_radius: px(0.), inset: false }])
                .child(div().text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(t.dim).child(row.label.clone()));
            for (s, ser) in series.iter().enumerate() {
                tip = tip.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(div().size(px(9.)).rounded(px(2.)).bg(ser.color))
                        .child(div().flex_1().child(ser.label.clone()))
                        .child(div().pl(px(14.)).font_weight(FontWeight::BOLD).child(unit.fmt(row.values.get(s).copied().unwrap_or(0.)))),
                );
            }
            row_el = row_el.child(deferred(div().absolute().top(px(-4.)).right(px(72.)).child(tip)).with_priority(3));
        }
        list = list.child(row_el);
    }
    list
}

// ------------------------------------------------------------------ legend

/// Legend chips: swatch + label (+ optional value). `active` highlights one key.
pub fn legend_item(t: &Theme, color: Hsla, label: impl Into<SharedString>, value: Option<String>, active: bool) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .px(px(7.))
        .h(px(22.))
        .rounded(px(6.))
        .text_size(px(12.))
        .text_color(if active { t.fg } else { t.dim })
        .when(active, |d| d.bg(t.accent_soft).border_1().border_color(t.accent))
        .child(div().size(px(9.)).rounded(px(2.)).bg(color).flex_none())
        .child(label.into())
        .when_some(value, |d, v| d.child(div().text_color(t.fg).child(v)))
}

#[cfg(test)]
mod tests {
    use super::{Unit, duration, nice_scale};

    #[test]
    fn nice_scales() {
        assert_eq!(nice_scale(0., Unit::Count, 4), (4., 1.));
        assert_eq!(nice_scale(7., Unit::Count, 4), (8., 2.));
        assert_eq!(nice_scale(38., Unit::Count, 4), (40., 10.));
        assert_eq!(nice_scale(3., Unit::Count, 4), (3., 1.));
        let (top, step) = nice_scale(4.12, Unit::Usd, 4);
        assert!(top >= 4.12 && step > 0.);
        assert_eq!(nice_scale(5000., Unit::Secs, 4), (5400., 1800.));
    }

    #[test]
    fn formats() {
        assert_eq!(duration(8040.), "2h 14m");
        assert_eq!(duration(59.), "59s");
        assert_eq!(Unit::Usd.fmt(4.123), "$4.12");
        assert_eq!(Unit::Secs.fmt_axis(5400.), "1.5h");
        assert_eq!(Unit::Count.fmt_axis(1500.), "1.5k");
    }
}
