//! A folded project's status bar and its peek card (B5 on the folded-project canvas,
//! https://claude.ai/artifact/EnDFMkgLvG94frymsMGCkT). Under a folded project's heading, a thin
//! bar shows how its terminals stand: one color per status, most urgent first, each as wide as
//! its count. Pointing at a color grows it (a small spring) and opens a card beside the sidebar
//! listing that status's terminals, each with its time and, for needs-you and failed, its
//! reason; click one to open it.
//!
//! Getting to the card: once it's open, another color (this project's or another's) takes over
//! only after the pointer rests on it for `SWITCH`; and while the pointer moves inside the aim
//! zone, a triangle from where it left the bar to the card's near edge, no bar reacts at all.
//! Leaving the bar gives `GRACE` to reach the card. In the lower half of the window the card
//! grows upward from the bar. Its list scrolls past `LIST_MAX`.
use crate::app::MainWindow;
use crate::model::*;
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// With a card open, how long the pointer must rest on another color to switch to it.
const SWITCH: Duration = Duration::from_millis(140);
/// From leaving the bar to reaching the card.
const GRACE: Duration = Duration::from_millis(300);
/// From leaving the card to it closing.
const CARD_GRACE: Duration = Duration::from_millis(200);
/// A move inside the aim zone keeps the bars quiet this long.
const AIM_HOLD: Duration = Duration::from_millis(120);
/// A color growing or shrinking back.
const GROW_MS: f32 = 180.;
/// The bar's height: the hit area around the line.
pub const BAR_H: f32 = 14.;
const LINE: f32 = 3.;
const GROWN: f32 = 7.;
/// How far a grown color reaches over the gap on each side.
const REACH: f32 = 1.5;
const CARD_W: f32 = 300.;
const LIST_MAX: f32 = 264.;
/// The card's title sits level with the bar: its edge this far from the bar's middle.
const TITLE_OFF: f32 = 21.;
/// Between the sidebar's edge and the card.
const CARD_GAP: f32 = 6.;

/// A color on the bar. Exited (and unknown) terminals count as idle: their dots are the same.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Bucket {
    NeedsYou,
    Failed,
    Working,
    Done,
    Idle,
}

impl Bucket {
    /// Bar order: most urgent first.
    pub const ORDER: [Bucket; 5] = [Bucket::NeedsYou, Bucket::Failed, Bucket::Working, Bucket::Done, Bucket::Idle];

    pub fn of(state: StatusState) -> Self {
        match state {
            StatusState::NeedsYou => Bucket::NeedsYou,
            StatusState::Failed => Bucket::Failed,
            StatusState::Working => Bucket::Working,
            StatusState::Done => Bucket::Done,
            _ => Bucket::Idle,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Bucket::NeedsYou => "needs you",
            Bucket::Failed => "failed",
            Bucket::Working => "working",
            Bucket::Done => "finished",
            Bucket::Idle => "idle",
        }
    }

    /// Text and dot color.
    fn color(self, t: &Theme) -> Hsla {
        match self {
            Bucket::NeedsYou => t.need,
            Bucket::Failed => t.err,
            Bucket::Working => t.work,
            Bucket::Done => t.ok,
            Bucket::Idle => t.dim,
        }
    }

    /// Its part of the bar (idle is quieter than its text).
    fn fill(self, t: &Theme) -> Hsla {
        if self == Bucket::Idle { t.dim.opacity(0.35) } else { self.color(t) }
    }

    fn state(self) -> StatusState {
        match self {
            Bucket::NeedsYou => StatusState::NeedsYou,
            Bucket::Failed => StatusState::Failed,
            Bucket::Working => StatusState::Working,
            Bucket::Done => StatusState::Done,
            Bucket::Idle => StatusState::Idle,
        }
    }
}

/// How many terminals are in each color, in bar order, leaving out empty ones.
pub fn counts(states: impl IntoIterator<Item = StatusState>) -> Vec<(Bucket, usize)> {
    let mut n = [0usize; 5];
    for s in states {
        n[Bucket::ORDER.iter().position(|b| *b == Bucket::of(s)).unwrap_or(4)] += 1;
    }
    Bucket::ORDER.iter().zip(n).filter(|(_, n)| *n > 0).map(|(b, n)| (*b, n)).collect()
}

/// `p` inside the triangle `abc` (edges included).
pub fn in_triangle(p: Point<Pixels>, [a, b, c]: [Point<Pixels>; 3]) -> bool {
    let f = |q: Point<Pixels>| (f32::from(q.x), f32::from(q.y));
    let (p, a, b, c) = (f(p), f(a), f(b), f(c));
    let side = |(px, py): (f32, f32), (qx, qy): (f32, f32), (rx, ry): (f32, f32)| (px - rx) * (qy - ry) - (qx - rx) * (py - ry);
    let (d1, d2, d3) = (side(p, a, b), side(p, b, c), side(p, c, a));
    !((d1 < 0. || d2 < 0. || d3 < 0.) && (d1 > 0. || d2 > 0. || d3 > 0.))
}

/// `cubic-bezier(.34, 1.56, .64, 1)`-like: overshoots a little, then settles.
fn spring(x: f32) -> f32 {
    let (c1, c3) = (1.70158, 2.70158);
    let x = x.clamp(0., 1.) - 1.;
    1. + c3 * x * x * x + c1 * x * x
}

type Key = (String, Bucket);

/// The open card: which project and color, and the color's middle (window px).
#[derive(Clone, Debug, PartialEq)]
pub struct Spot {
    pub project: String,
    pub bucket: Bucket,
    pub mid_y: f32,
}

/// One color growing or shrinking: since when, from and to (0 = a line, 1 = grown).
#[derive(Clone, Copy)]
struct Grow {
    at: Instant,
    from: f32,
    to: f32,
}

impl Grow {
    fn value(&self) -> f32 {
        let x = self.at.elapsed().as_secs_f32() * 1000. / GROW_MS;
        if x >= 1. { self.to } else { self.from + (self.to - self.from) * spring(x) }
    }
}

#[derive(Default)]
pub struct FoldPeek {
    pub open: Option<Spot>,
    /// A switch waiting on the pointer to rest; a close waiting out the grace period.
    pending: Option<Task<()>>,
    closing: Option<Task<()>>,
    /// Where the pointer left the bar, and the card's near corners (window px).
    aim: Option<[Point<Pixels>; 3]>,
    aim_until: Option<Instant>,
    /// Each color's bounds (window px) and the card's, from the last paint.
    bars: Rc<RefCell<HashMap<Key, Bounds<Pixels>>>>,
    card: Rc<Cell<Option<Bounds<Pixels>>>>,
    grow: HashMap<Key, Grow>,
}

impl FoldPeek {
    fn grown(&self, key: &Key) -> f32 {
        self.grow.get(key).map_or(0., Grow::value)
    }

    fn growing(&self) -> bool {
        self.grow.values().any(|g| g.at.elapsed().as_secs_f32() * 1000. < GROW_MS)
    }

    fn set_grow(&mut self, key: Key, to: f32, reduce: bool) {
        let from = self.grown(&key);
        let at = if reduce { Instant::now() - Duration::from_secs(1) } else { Instant::now() };
        self.grow.insert(key, Grow { at, from, to });
    }

    fn aiming(&self) -> bool {
        self.aim_until.is_some_and(|t| Instant::now() < t)
    }
}

/// Dev (screenshots): `MIDNA_DEBUG_PEEK=<project id>:<color>[,<project id>…]` folds every
/// listed project and shows the first one's card open on that color (`needs-you`, `failed`,
/// `working`, `finished`, `idle`).
fn debug_preset() -> Option<Vec<String>> {
    crate::dev::var("MIDNA_DEBUG_PEEK").ok().map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
}

pub fn debug_folded() -> Vec<String> {
    debug_preset().unwrap_or_default().iter().map(|s| s.split(':').next().unwrap_or_default().to_string()).collect()
}

fn debug_key() -> Option<Key> {
    let first = debug_preset()?.into_iter().next()?;
    let (project, color) = first.split_once(':')?;
    let bucket = Bucket::ORDER.into_iter().find(|b| b.word().replace(' ', "-") == color)?;
    Some((project.to_string(), bucket))
}

/// The open card, or the dev preset's once its bar has been laid out.
fn spot(m: &MainWindow) -> Option<Spot> {
    m.fold_peek.open.clone().or_else(|| {
        let (project, bucket) = debug_key()?;
        let b = m.fold_peek.bars.borrow().get(&(project.clone(), bucket)).copied()?;
        Some(Spot { project, bucket, mid_y: f32::from(b.center().y) })
    })
}

/// The pointer came onto a color.
fn enter(m: &mut MainWindow, key: Key, cx: &mut Context<MainWindow>) {
    let p = &mut m.fold_peek;
    let open = p.open.as_ref().map(|o| (o.project.clone(), o.bucket));
    let same = open.as_ref() == Some(&key);
    // Heading for the card: colors crossed on the way don't react.
    if open.is_some() && !same && p.aiming() {
        return;
    }
    p.closing = None;
    p.pending = None;
    if same {
        return;
    }
    let Some(b) = p.bars.borrow().get(&key).copied() else { return };
    let spot = Spot { project: key.0.clone(), bucket: key.1, mid_y: f32::from(b.center().y) };
    if open.is_none() {
        show(m, spot, cx);
        return;
    }
    // A card is open: switch only if the pointer rests here.
    m.fold_peek.pending = Some(cx.spawn(async move |this, cx| {
        cx.background_executor().timer(SWITCH).await;
        let _ = this.update(cx, |m, cx| {
            m.fold_peek.pending = None;
            show(m, spot, cx);
        });
    }));
}

fn show(m: &mut MainWindow, spot: Spot, cx: &mut Context<MainWindow>) {
    let reduce = cx.reduce_motion();
    let p = &mut m.fold_peek;
    if let Some(o) = p.open.take() {
        p.set_grow((o.project, o.bucket), 0., reduce);
    }
    p.set_grow((spot.project.clone(), spot.bucket), 1., reduce);
    p.open = Some(spot);
    p.aim = None;
    p.aim_until = None;
    p.card.set(None);
    cx.notify();
}

/// The pointer left a color: the aim zone runs from here to the card.
fn leave(m: &mut MainWindow, window: &Window, cx: &mut Context<MainWindow>) {
    let p = &mut m.fold_peek;
    p.pending = None;
    if p.open.is_none() {
        return;
    }
    if let Some(c) = p.card.get() {
        let from = window.mouse_position();
        p.aim = Some([from, c.origin, c.bottom_left()]);
    }
    close_after(m, GRACE, cx);
}

/// The pointer moved over the sidebar: inside the aim zone, the card holds and bars stay quiet.
pub fn moved(m: &mut MainWindow, pos: Point<Pixels>, cx: &mut Context<MainWindow>) {
    let p = &mut m.fold_peek;
    let Some(aim) = p.aim.filter(|_| p.open.is_some()) else { return };
    if in_triangle(pos, aim) {
        p.aim_until = Some(Instant::now() + AIM_HOLD);
        p.pending = None;
        close_after(m, GRACE, cx);
    }
}

fn close_after(m: &mut MainWindow, d: Duration, cx: &mut Context<MainWindow>) {
    m.fold_peek.closing = Some(cx.spawn(async move |this, cx| {
        cx.background_executor().timer(d).await;
        let _ = this.update(cx, close);
    }));
}

pub fn close(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    let reduce = cx.reduce_motion();
    let p = &mut m.fold_peek;
    p.pending = None;
    p.closing = None;
    p.aim = None;
    p.aim_until = None;
    if let Some(o) = p.open.take() {
        p.set_grow((o.project, o.bucket), 0., reduce);
        cx.notify();
    }
}

/// The bar under a folded project's heading.
pub fn bar(m: &MainWindow, t: &Theme, project: &str, sessions: &[&Session], window: &mut Window, cx: &mut Context<MainWindow>) -> Div {
    let parts = counts(sessions.iter().map(|s| m.effective_state(s)));
    if m.fold_peek.growing() {
        window.request_animation_frame();
    }
    let keys: Vec<Key> = parts.iter().map(|(b, _)| (project.to_string(), *b)).collect();
    let bars = m.fold_peek.bars.clone();
    let keys_paint = keys.clone();
    div()
        .flex()
        .items_center()
        .gap(px(2.))
        .h(px(BAR_H))
        .ml(px(26.))
        .mr(px(12.))
        .on_children_prepainted(move |b, _, _| {
            let mut bars = bars.borrow_mut();
            for (k, b) in keys_paint.iter().zip(b) {
                bars.insert(k.clone(), b);
            }
        })
        .children(parts.into_iter().zip(keys).map(|((b, n), key)| {
            let v = if m.fold_peek.open.is_none() && debug_key().as_ref() == Some(&key) { 1. } else { m.fold_peek.grown(&key) };
            let h = LINE + (GROWN - LINE) * v;
            div()
                .id(SharedString::from(format!("fold-bar-{project}-{}", b.word())))
                .flex()
                .items_center()
                .h_full()
                .flex_basis(px(0.))
                .min_w(px(4.))
                .map(|mut d| {
                    d.style().flex_grow = Some(n as f32);
                    d
                })
                .on_hover(cx.listener(move |m, over: &bool, w, cx| {
                    if *over { enter(m, key.clone(), cx) } else { leave(m, w, cx) }
                }))
                .child(div().flex_1().h(px(h.max(1.))).mx(px(-REACH * v.max(0.))).rounded(px(h / 2.)).bg(b.fill(t)))
        }))
}

/// The card for the color the pointer is on, beside the sidebar.
pub fn card(m: &MainWindow, t: &Theme, window: &mut Window, cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    let spot = spot(m)?;
    // Only over the sidebar as you see it: not under setup, ⌘K or the annotation sheet.
    let covered = crate::ui::setup_screen::visible(m) || matches!(m.overlay, crate::app::Overlay::CommandBar | crate::app::Overlay::Annotate);
    if !m.collapsed.contains(&spot.project) || m.sidebar_collapsed || covered {
        return None;
    }
    let groups = m.groups();
    let g = groups.iter().find(|g| g.project.is_some_and(|p| p.id == spot.project))?;
    let mine: Vec<&Session> = g.sessions.iter().copied().filter(|s| Bucket::of(m.effective_state(s)) == spot.bucket).collect();
    if mine.is_empty() {
        return None;
    }
    let color = spot.bucket.color(t);
    let vh = f32::from(window.viewport_size().height);
    let up = spot.mid_y > vh / 2.;
    let room = if up { spot.mid_y + TITLE_OFF } else { vh - (spot.mid_y - TITLE_OFF) } - 8.;
    let title = div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(8.))
        .px(px(14.))
        .pt(px(11.))
        .pb(px(10.))
        .border_b_1()
        .border_color(t.line)
        .child(div().text_size(px(14.)).font_weight(FontWeight::BOLD).child(g.name.clone()))
        .child(div().text_size(px(12.)).text_color(t.dim).child(g.sessions.len().to_string()))
        .child(div().text_size(px(12.)).text_color(t.dim).child("·"))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(5.))
                .text_size(px(12.))
                .font_weight(FontWeight::BOLD)
                .text_color(color)
                .whitespace_nowrap()
                .child(super::status_dot(t, spot.bucket.state(), 7.))
                .child(format!("{} {}", mine.len(), spot.bucket.word())),
        );
    let rows = mine.iter().map(|s| peek_row(m, t, s, spot.bucket, cx));
    let list = div().id("fold-peek-list").flex().flex_col().p(px(6.)).max_h(px(LIST_MAX)).overflow_y_scroll().children(rows);
    // A list cut short fades out at the cut.
    let cut = mine.len() as f32 * 36. > LIST_MAX;
    let list = div()
        .relative()
        .min_h_0()
        .child(list)
        .when(cut, |d| d.child(div().absolute().left_0().right_0().bottom_0().h(px(28.)).rounded_b(px(12.)).bg(linear_gradient(180., linear_color_stop(t.raised.opacity(0.), 0.), linear_color_stop(t.raised, 1.)))));
    let card_bounds = m.fold_peek.card.clone();
    let el = div()
        .id("fold-peek")
        .w(px(CARD_W))
        .max_h(px(room.max(120.)))
        .flex()
        .flex_col()
        .rounded(px(12.))
        .border_1()
        .border_color(t.line)
        .bg(t.raised)
        .text_color(t.fg)
        .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.35), offset: point(px(0.), px(12.)), blur_radius: px(32.), spread_radius: px(0.), inset: false }])
        .occlude()
        .on_hover(cx.listener(|m, over: &bool, _, cx| {
            if *over {
                m.fold_peek.closing = None;
                m.fold_peek.pending = None;
            } else {
                close_after(m, CARD_GRACE, cx);
            }
        }))
        .child(title)
        .child(list);
    let el = div()
        .on_children_prepainted(move |b, _, _| card_bounds.set(b.first().copied()))
        .child(el.with_animation(SharedString::from(format!("fold-peek-{}-{}", spot.project, spot.bucket.word())), Animation::new(Duration::from_millis(140)).with_easing(ease_out_quint()), |el, d| el.opacity(d)));
    let at = point(px(m.sidebar_width + CARD_GAP), px(if up { spot.mid_y + TITLE_OFF } else { spot.mid_y - TITLE_OFF }));
    let anchor = if up { Anchor::BottomLeft } else { Anchor::TopLeft };
    Some(deferred(anchored().position(at).anchor(anchor).snap_to_window_with_margin(px(8.)).child(el)).with_priority(1).into_any_element())
}

/// One terminal in the card: name with its time beside it, a reason line for needs-you and
/// failed, its agent icon. Click to open it.
fn peek_row(m: &MainWindow, t: &Theme, s: &Session, bucket: Bucket, cx: &mut Context<MainWindow>) -> Stateful<Div> {
    let color = bucket.color(t);
    let need = m.need_for_session(&s.id).filter(|_| bucket == Bucket::NeedsYou);
    let since = need.map(|n| since_short(&n.created_at)).or_else(|| s.status.since.as_deref().map(since_short)).unwrap_or_default();
    let reason = match bucket {
        Bucket::NeedsYou | Bucket::Failed => s
            .status
            .reason
            .clone()
            .filter(|r| !r.is_empty())
            .or_else(|| need.map(|n| n.title.clone()))
            .or_else(|| s.status.exit_code.map(|c| format!("exit {c}"))),
        _ => None,
    };
    let id = s.id.clone();
    div()
        .id(SharedString::from(format!("fold-peek-{}", s.id)))
        .flex()
        .flex_col()
        .gap(px(1.))
        .px(px(8.))
        .py(px(7.))
        .rounded(px(6.))
        .cursor_pointer()
        .when(bucket == Bucket::NeedsYou, |d| d.bg(t.need_soft))
        .hover(|st| st.bg(t.panel))
        .on_click(cx.listener(move |m, _, w, cx| {
            close(m, cx);
            m.select_only(id.clone(), w, cx);
        }))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap(px(8.))
                        .flex_1()
                        .min_w_0()
                        .child(div().font_weight(FontWeight::MEDIUM).truncate().child(crate::ui::rename::shown_name(m, &s.id, &s.name, cx)))
                        .when(!since.is_empty(), |d| d.child(div().flex_none().text_size(px(11.5)).text_color(color).child(since))),
                )
                .child(super::terminal_icon(m, t, s, 13., t.dim)),
        )
        .when_some(reason, |d, r| d.child(div().text_size(px(11.5)).text_color(color).truncate().child(r)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn counts_follow_bar_order_and_skip_empty_colors() {
        use StatusState as S;
        let got = counts([S::Idle, S::Working, S::NeedsYou, S::Exited, S::Working, S::Failed]);
        assert_eq!(got, vec![(Bucket::NeedsYou, 1), (Bucket::Failed, 1), (Bucket::Working, 2), (Bucket::Idle, 2)]);
        assert!(counts([]).is_empty());
    }

    #[test]
    fn aim_zone_is_the_triangle_to_the_cards_edge() {
        let p = |x: f32, y: f32| point(px(x), px(y));
        // Left the bar at (200, 500); the card's near edge runs from (270, 200) to (270, 520).
        let aim = [p(200., 500.), p(270., 200.), p(270., 520.)];
        assert!(in_triangle(p(240., 420.), aim), "heading up and right to the card");
        assert!(in_triangle(p(200., 500.), aim), "where it left");
        assert!(!in_triangle(p(150., 420.), aim), "back across the sidebar");
        assert!(!in_triangle(p(240., 200.), aim), "too steep: that's the bar above, not the card");
    }

    #[test]
    fn spring_overshoots_then_settles() {
        assert_eq!(spring(0.), 0.);
        assert!((spring(1.) - 1.).abs() < 1e-6);
        assert!([0.4, 0.6, 0.8].iter().map(|x| spring(*x)).fold(0., f32::max) > 1.);
    }
}
