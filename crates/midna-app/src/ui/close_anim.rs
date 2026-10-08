//! Closing terminals with ⌘W (design C, "Conveyor", on the tab close canvas). The closed row
//! stays as a ghost while it folds away: its content lifts and fades, and the rows below slide
//! up into its room. The selection moves with the list: the terminal shown next comes from the
//! row below (it slides up into the highlight, which stays put) or the row above (the highlight
//! glides up to it). The terminal pane slides the same way, in step: the closed terminal leaves
//! the way the list moves and the next one comes in behind it.
//!
//! `start` runs before the sessions leave `MainWindow::sessions`; the pane's slide starts when
//! the next terminal is promoted (`on_show`), which waits for its first frame.
//!
//! Opening a terminal (⌘T and the like, `open`) runs it in reverse: the new row unfolds where
//! it lands, its content dropping in and fading up, and the rows below slide down. The
//! highlight glides down to it from the row right above, or stays put for it to unfold into
//! from the row right below; further away it glides to the new row from where the shown row
//! was laid out (`rows`), or, with nothing to go by, fills the new row's room, solid, as it opens.
//! The pane slides the new terminal in the same way.
//!
//! A terminal that turns up on its own (an agent, a trigger, the CLI, `arrive`) unfolds the
//! same way where it lands, with no highlight: the selection and the pane stay put. The first
//! one in a project brings the project's whole group in with it, heading and all.
//!
//! Moving to the terminal above or below (⌥⌘↑ / ⌥⌘↓, `switch`) slides too: the highlight glides
//! to it, across project headings too, from where each row was last laid out (`rows`), and the
//! pane slides the same way, once the terminal has drawn (`on_show`). With Reduce Motion on, nothing moves.
use crate::app::MainWindow;
use crate::model::*;
use crate::terminal::TerminalView;
use gpui_kit::prelude::*;
use gpui_kit::*;
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The fold, the glide and the pane's slide, in ms.
pub const SLIDE_MS: f32 = 320.;
/// The ghost's content lifting and fading.
const LIFT_MS: f32 = 220.;
/// How far the ghost's content lifts, in px.
pub const LIFT: f32 = 14.;
/// The pane waits this long at most for the next terminal's first frame.
const PANE_WAIT: Duration = Duration::from_secs(1);

/// Where the terminal shown next sits, from the closed one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Above,
    Below,
}

/// A closed row, folding away where it was.
pub struct Ghost {
    pub session: Session,
    /// The sidebar group's key (a project id, or "root").
    pub group: String,
    /// The row it sits above (`None`: the group's last).
    pub next: Option<String>,
    /// It draws the selection highlight: the next terminal slides up into it.
    pub slot: bool,
    at: Instant,
    /// Its measured height (0 until the first frame).
    height: Rc<Cell<f32>>,
}

/// A row just opened, unfolding where it landed.
pub struct Opening {
    pub id: String,
    /// The terminal shown before it.
    from: Option<String>,
    /// Where it sits from `from`, when they're side by side: the highlight moves to it.
    pub glide: Option<Dir>,
    /// Further away: the highlight travels to it from `from`.
    travel: Option<Travel>,
    at: Instant,
    /// Its measured height (0 until the first frame).
    height: Rc<Cell<f32>>,
}

/// A row (a session id) or a whole group (its key) that turned up on its own, unfolding where
/// it landed.
pub struct Arrival {
    key: String,
    at: Instant,
    /// Its measured height (0 until the first frame).
    height: Rc<Cell<f32>>,
}

/// A highlight travelling to a row just opened from a row further away, as laid out before it
/// opened.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Travel {
    /// Where the new row sits from `from`.
    dir: Dir,
    /// `from`'s top and height.
    from_at: (f32, f32),
    /// The new row's top (it unfolds there: the rows above it stay put).
    to_top: f32,
}

/// How a row draws its selection highlight.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Highlight {
    /// As usual.
    Own,
    /// Not at all: the ghost above draws it.
    None,
    /// Shifted down by this fraction of the row's height (the highlight gliding up to it).
    Shifted(f32),
    /// At `top` px from the row's top, `h` px tall (gliding between rows further apart).
    At { top: f32, h: f32 },
}

/// A highlight gliding between two rows (`switch`).
struct Step {
    from: String,
    to: String,
    /// Where `to` sits from `from`.
    dir: Dir,
    /// Each row's top and height, laid out.
    from_at: (f32, f32),
    to_at: (f32, f32),
    at: Instant,
}

#[derive(Default)]
pub struct CloseAnim {
    ghosts: Vec<Ghost>,
    /// The terminal shown next, when it's the row right above or below the closed one.
    glide: Option<(String, Dir, Instant)>,
    /// Closed terminals whose pane slides when the next one shows, and which way.
    pane_due: Option<(Vec<String>, Dir, Instant)>,
    pane: Option<(Entity<TerminalView>, Dir, Instant)>,
    opening: Option<Opening>,
    arrivals: Vec<Arrival>,
    step: Option<Step>,
    /// Each sidebar row's top and height in the window, as last laid out (`sidebar::measured`).
    pub rows: Rc<std::cell::RefCell<std::collections::HashMap<String, (f32, f32)>>>,
}

/// A sidebar group for `plan`: its key, whether it's folded, its rows in order.
pub struct PlanGroup {
    pub key: String,
    pub folded: bool,
    pub rows: Vec<String>,
}

#[derive(Debug, PartialEq)]
pub struct Plan {
    /// (closed id, group key, the row it sits above, it draws the highlight).
    pub ghosts: Vec<(String, String, Option<String>, bool)>,
    pub glide: Option<(String, Dir)>,
    pub pane: Option<Dir>,
}

/// What closing `closing` animates. `order`: every row in sidebar order; `next`: the terminal
/// shown next. Folded groups get no ghosts. The highlight moves only for a single row whose
/// next terminal is right beside it.
pub fn plan(groups: &[PlanGroup], order: &[String], closing: &[String], next: Option<&str>) -> Plan {
    let mut ghosts = vec![];
    for g in groups.iter().filter(|g| !g.folded) {
        for (i, id) in g.rows.iter().enumerate().filter(|(_, id)| closing.contains(id)) {
            let after = g.rows[i + 1..].iter().find(|r| !closing.contains(r)).cloned();
            ghosts.push((id.clone(), g.key.clone(), after, false));
        }
    }
    let last = order.iter().rposition(|x| closing.contains(x));
    let dir = next.and_then(|n| Some((order.iter().position(|x| x == n)?, last?))).map(|(n, l)| if n < l { Dir::Above } else { Dir::Below });
    let mut glide = None;
    if let ([id], Some(n), Some(d)) = (closing, next, dir) {
        let beside = groups.iter().filter(|g| !g.folded).any(|g| {
            let at = |x: &str| g.rows.iter().position(|r| r == x);
            matches!((at(id), at(n)), (Some(a), Some(b)) if a.abs_diff(b) == 1)
        });
        if beside {
            glide = Some((n.to_string(), d));
            if d == Dir::Below {
                ghosts.iter_mut().for_each(|g| g.3 = true);
            }
        }
    }
    Plan { ghosts, glide, pane: dir }
}

/// What opening or moving to `new` animates, from `from` (the terminal shown before): whether
/// the highlight glides (`from` right beside it in an unfolded group), and which way the pane
/// slides.
pub fn plan_move(groups: &[PlanGroup], order: &[String], from: Option<&str>, new: &str) -> (Option<Dir>, Option<Dir>) {
    let at = |x: &str| order.iter().position(|r| r == x);
    let Some((from, f, n)) = from.and_then(|f| Some((f, at(f)?, at(new)?))).filter(|(_, f, n)| f != n) else { return (None, None) };
    let dir = if n < f { Dir::Above } else { Dir::Below };
    let beside = groups.iter().filter(|g| !g.folded).any(|g| {
        let at = |x: &str| g.rows.iter().position(|r| r == x);
        matches!((at(from), at(new)), (Some(a), Some(b)) if a.abs_diff(b) == 1)
    });
    (beside.then_some(dir), Some(dir))
}

/// Where `new`, just added to its group's rows, will sit: the top of the row it pushes down,
/// or the bottom of the row above it, as last laid out.
fn slot_top(groups: &[PlanGroup], rows: &std::collections::HashMap<String, (f32, f32)>, new: &str) -> Option<f32> {
    let g = groups.iter().find(|g| !g.folded && g.rows.iter().any(|r| r == new))?;
    let i = g.rows.iter().position(|r| r == new)?;
    match g.rows.get(i + 1).and_then(|r| rows.get(r)) {
        Some(&(top, _)) => Some(top),
        None => g.rows[..i].last().and_then(|r| rows.get(r)).map(|&(top, h)| top + h),
    }
}

fn plan_groups(m: &MainWindow) -> Vec<PlanGroup> {
    m.groups()
        .into_iter()
        .map(|g| {
            let pid = g.project.map(|p| p.id.clone());
            PlanGroup {
                folded: pid.as_ref().is_some_and(|p| m.collapsed.contains(p)),
                key: pid.unwrap_or_else(|| "root".into()),
                rows: g.sessions.iter().map(|s| s.id.clone()).collect(),
            }
        })
        .collect()
}

fn ease_out(x: f32) -> f32 {
    1. - (1. - x.clamp(0., 1.)).powi(3)
}

fn ms(since: Instant) -> f32 {
    since.elapsed().as_secs_f32() * 1000.
}

/// ⌘W closed `ids`, still in `m.sessions`; `next` is about to be selected.
pub fn start(m: &mut MainWindow, ids: &[String], next: Option<&str>, cx: &mut Context<MainWindow>) {
    gpui_kit::base::apply_system_reduce_motion(cx);
    if cx.reduce_motion() {
        return;
    }
    let groups = plan_groups(m);
    let order: Vec<String> = m.ordered_sessions().iter().map(|s| s.id.clone()).collect();
    let p = plan(&groups, &order, ids, next);
    let now = Instant::now();
    let a = &mut m.close_anim;
    a.ghosts = p
        .ghosts
        .into_iter()
        .filter_map(|(id, group, next, slot)| {
            let session = m.sessions.iter().find(|s| s.id == id)?.clone();
            Some(Ghost { session, group, next, slot, at: now, height: Rc::new(Cell::new(0.)) })
        })
        .collect();
    a.glide = p.glide.map(|(id, d)| (id, d, now));
    a.pane_due = p.pane.map(|d| (ids.to_vec(), d, now));
    cx.notify();
}

/// `new` just opened, already in `m.sessions` but not yet selected; `old` is the terminal on
/// screen, which slides out as `new` comes in (shown straight away, so keys reach it).
pub fn open(m: &mut MainWindow, new: &str, old: Option<Entity<TerminalView>>, cx: &mut Context<MainWindow>) {
    gpui_kit::base::apply_system_reduce_motion(cx);
    if cx.reduce_motion() {
        return;
    }
    let order: Vec<String> = m.ordered_sessions().iter().map(|s| s.id.clone()).collect();
    let from = m.selected.clone();
    let groups = plan_groups(m);
    let (glide, pane) = plan_move(&groups, &order, from.as_deref(), new);
    let shown = |id: &str| groups.iter().any(|g| !g.folded && g.rows.iter().any(|r| r == id));
    let travel = {
        let rows = m.close_anim.rows.borrow();
        match (glide, pane, from.as_deref().filter(|f| shown(f)).and_then(|f| rows.get(f)), slot_top(&groups, &rows, new)) {
            (None, Some(dir), Some(&from_at), Some(to_top)) => Some(Travel { dir, from_at, to_top }),
            _ => None,
        }
    };
    let now = Instant::now();
    let a = &mut m.close_anim;
    a.opening = Some(Opening { id: new.to_string(), from, glide, travel, at: now, height: Rc::new(Cell::new(0.)) });
    a.pane_due = None;
    a.pane = old.filter(|o| o.read(cx).session_id != new).zip(pane).map(|(o, d)| (o, d, now));
    cx.notify();
}

/// The sidebar's groups, by key, each with its rows: what `arrive` compares against.
pub fn sidebar_rows(m: &MainWindow) -> Vec<(String, Vec<String>)> {
    plan_groups(m).into_iter().map(|g| (g.key, g.rows)).collect()
}

/// The sidebar was just refreshed from `before` (`sidebar_rows`): terminals that turned up
/// without being opened here unfold where they land, or, in a group that wasn't there, the
/// whole group does.
pub fn arrive(m: &mut MainWindow, before: &[(String, Vec<String>)], cx: &mut Context<MainWindow>) {
    gpui_kit::base::apply_system_reduce_motion(cx);
    if cx.reduce_motion() {
        return;
    }
    let keys = arrivals(&plan_groups(m), before);
    if keys.is_empty() {
        return;
    }
    let now = Instant::now();
    let a = &mut m.close_anim;
    a.arrivals.retain(|x| !keys.contains(&x.key));
    a.arrivals.extend(keys.into_iter().map(|key| Arrival { key, at: now, height: Rc::new(Cell::new(0.)) }));
    cx.notify();
}

/// What unfolds, `before` → `groups`: a group that wasn't there (by key), else each row that
/// wasn't (in an unfolded group; a folded one doesn't show it).
fn arrivals(groups: &[PlanGroup], before: &[(String, Vec<String>)]) -> Vec<String> {
    let mut keys = vec![];
    for g in groups {
        match before.iter().find(|(k, _)| k == &g.key) {
            None => keys.push(g.key.clone()),
            Some((_, rows)) if !g.folded => keys.extend(g.rows.iter().filter(|r| !rows.contains(r)).cloned()),
            Some(_) => {}
        }
    }
    keys
}

/// ⌥⌘↑ / ⌥⌘↓ moved from the selected terminal to `to`, not yet selected.
pub fn switch(m: &mut MainWindow, to: &str, cx: &mut Context<MainWindow>) {
    gpui_kit::base::apply_system_reduce_motion(cx);
    let Some(from) = m.selected.clone().filter(|f| f != to && !cx.reduce_motion()) else { return };
    let order: Vec<String> = m.ordered_sessions().iter().map(|s| s.id.clone()).collect();
    let groups = plan_groups(m);
    let (_, pane) = plan_move(&groups, &order, Some(&from), to);
    // Both rows stay on screen: neither is in a folded project.
    let shown = |id: &str| groups.iter().any(|g| !g.folded && g.rows.iter().any(|r| r == id));
    let now = Instant::now();
    let a = &mut m.close_anim;
    let rows = a.rows.borrow();
    a.step = match (pane, rows.get(&from), rows.get(to)) {
        (Some(dir), Some(&from_at), Some(&to_at)) if shown(&from) && shown(to) => Some(Step { from: from.clone(), to: to.to_string(), dir, from_at, to_at, at: now }),
        _ => None,
    };
    drop(rows);
    a.pane_due = pane.map(|d| (vec![from], d, now));
    cx.notify();
}

/// The pane is about to show `next` in place of `old`: slide them if `old` was just closed.
pub fn on_show(m: &mut MainWindow, old: &Entity<TerminalView>, next: &Entity<TerminalView>, cx: &App) {
    let Some((ids, dir, at)) = m.close_anim.pane_due.take() else { return };
    let old_id = &old.read(cx).session_id;
    if at.elapsed() < PANE_WAIT && ids.contains(old_id) && old_id != &next.read(cx).session_id {
        m.close_anim.pane = Some((old.clone(), dir, Instant::now()));
    }
}

impl CloseAnim {
    /// Drop what has finished.
    pub fn prune(&mut self) {
        self.ghosts.retain(|g| ms(g.at) < SLIDE_MS);
        if self.glide.as_ref().is_some_and(|(_, _, at)| ms(*at) >= SLIDE_MS) {
            self.glide = None;
        }
        if self.pane.as_ref().is_some_and(|(_, _, at)| ms(*at) >= SLIDE_MS) {
            self.pane = None;
        }
        if self.opening.as_ref().is_some_and(|o| ms(o.at) >= SLIDE_MS) {
            self.opening = None;
        }
        if self.step.as_ref().is_some_and(|s| ms(s.at) >= SLIDE_MS) {
            self.step = None;
        }
        self.arrivals.retain(|a| ms(a.at) < SLIDE_MS);
    }

    /// The sidebar is moving (it asks for the next frame).
    pub fn moving(&self) -> bool {
        self.ghosts.iter().any(|g| ms(g.at) < SLIDE_MS)
            || self.glide.as_ref().is_some_and(|(_, _, at)| ms(*at) < SLIDE_MS)
            || self.opening.as_ref().is_some_and(|o| ms(o.at) < SLIDE_MS)
            || self.step.as_ref().is_some_and(|s| ms(s.at) < SLIDE_MS)
            || self.arrivals.iter().any(|a| ms(a.at) < SLIDE_MS)
    }

    /// The row or group `key`, when it's unfolding after turning up on its own.
    pub fn arrival(&self, key: &str) -> Option<&Arrival> {
        self.arrivals.iter().find(|a| a.key == key && ms(a.at) < SLIDE_MS)
    }

    /// `id`, when it's the row unfolding.
    pub fn opening(&self, id: &str) -> Option<&Opening> {
        self.opening.as_ref().filter(|o| o.id == id && ms(o.at) < SLIDE_MS)
    }

    /// Ghosts in `group` sitting above `next` (`None`: at the group's end).
    pub fn ghosts<'a>(&'a self, group: &'a str, next: Option<&'a str>) -> impl Iterator<Item = &'a Ghost> + 'a {
        self.ghosts.iter().filter(move |g| g.group == group && g.next.as_deref() == next && ms(g.at) < SLIDE_MS)
    }

    /// How row `id` (`selected` or not) draws the highlight.
    pub fn highlight(&self, id: &str, selected: bool) -> Highlight {
        if let Some(o) = self.opening.as_ref().filter(|o| ms(o.at) < SLIDE_MS) {
            // The unfolding row's highlight is drawn around it (`sidebar::unfold`), not faded
            // with its content, or comes down from the row above.
            if o.id == id {
                return Highlight::None;
            }
            if o.glide == Some(Dir::Below) && o.from.as_deref() == Some(id) && !selected {
                return Highlight::Shifted(ease_out(ms(o.at) / SLIDE_MS));
            }
            // Travelling down from further up: the row it leaves draws it, so the rows and
            // headings below draw over it (`Opening::travel_from`).
            if o.from.as_deref() == Some(id) && !selected {
                if let Some(at) = o.travel_from() {
                    return at;
                }
            }
        }
        // Moved with ⌥⌘↑ / ⌥⌘↓: the upper of the two rows draws the highlight on its way, so
        // the rows and headings below it draw over it.
        if let Some(s) = self.step.as_ref().filter(|s| ms(s.at) < SLIDE_MS) {
            let e = ease_out(ms(s.at) / SLIDE_MS);
            let h = s.from_at.1 + (s.to_at.1 - s.from_at.1) * e;
            match s.dir {
                Dir::Below if s.to == id && selected => return Highlight::None,
                Dir::Below if s.from == id && !selected => return Highlight::At { top: (s.to_at.0 - s.from_at.0) * e, h },
                Dir::Above if s.to == id && selected => return Highlight::At { top: (s.from_at.0 - s.to_at.0) * (1. - e), h },
                _ => {}
            }
        }
        match &self.glide {
            Some((g, dir, at)) if selected && g == id && ms(*at) < SLIDE_MS => match dir {
                Dir::Below => Highlight::None,
                Dir::Above => Highlight::Shifted(1. - ease_out(ms(*at) / SLIDE_MS)),
            },
            _ => Highlight::Own,
        }
    }
}

impl Ghost {
    /// How far it has folded and its content lifted, 0..1 each.
    pub fn progress(&self) -> (f32, f32) {
        let t = ms(self.at);
        (ease_out(t / SLIDE_MS), (t / LIFT_MS).clamp(0., 1.).powi(2))
    }

    /// Its full height, once measured.
    pub fn height(&self) -> Option<f32> {
        Some(self.height.get()).filter(|h| *h > 0.)
    }

    pub fn measure(&self) -> Rc<Cell<f32>> {
        self.height.clone()
    }
}

/// How far a row `at` opened has unfolded and its content dropped in, 0..1 each.
fn unfolded(at: Instant) -> (f32, f32) {
    let t = ms(at);
    (ease_out(t / SLIDE_MS), 1. - (1. - t / LIFT_MS).clamp(0., 1.).powi(2))
}

impl Arrival {
    /// How far it has unfolded and its content dropped in, 0..1 each.
    pub fn progress(&self) -> (f32, f32) {
        unfolded(self.at)
    }

    /// Its full height, once measured.
    pub fn height(&self) -> Option<f32> {
        Some(self.height.get()).filter(|h| *h > 0.)
    }

    pub fn measure(&self) -> Rc<Cell<f32>> {
        self.height.clone()
    }
}

impl Opening {
    /// How far it has unfolded and its content dropped in, 0..1 each.
    pub fn progress(&self) -> (f32, f32) {
        unfolded(self.at)
    }

    /// The travelling highlight's top (from the drawing row's top) and height, eased `e`.
    fn travel(&self, e: f32) -> Option<(Dir, f32, f32)> {
        let t = self.travel?;
        let (from_top, from_h) = t.from_at;
        let h = self.height().unwrap_or(from_h);
        let h_now = from_h + (h - from_h) * e;
        Some(match t.dir {
            // `from` sits above: it stays put and the highlight drops to the new row's top.
            Dir::Below => (Dir::Below, (t.to_top - from_top) * e, h_now),
            // `from` sits below, pushed down as the new row unfolds: the highlight rises from it.
            Dir::Above => (Dir::Above, (from_top + h * e - t.to_top) * (1. - e), h_now),
        })
    }

    /// Travelling down to it: where the row it left draws the highlight.
    fn travel_from(&self) -> Option<Highlight> {
        match self.travel(ease_out(ms(self.at) / SLIDE_MS))? {
            (Dir::Below, top, h) => Some(Highlight::At { top, h }),
            _ => None,
        }
    }

    /// Travelling up to it: where it draws the highlight itself, from its own top.
    pub fn travel_to(&self) -> Option<(f32, f32)> {
        match self.travel(self.progress().0)? {
            (Dir::Above, top, h) => Some((top, h)),
            _ => None,
        }
    }

    /// The highlight comes to it from a row further away.
    pub fn travels(&self) -> bool {
        self.travel.is_some()
    }

    /// Its full height, once measured.
    pub fn height(&self) -> Option<f32> {
        Some(self.height.get()).filter(|h| *h > 0.)
    }

    pub fn measure(&self) -> Rc<Cell<f32>> {
        self.height.clone()
    }
}

/// The terminal pane's body, `el`: while a closed terminal slides out, it slides out beside
/// the next one coming in.
pub fn pane(m: &MainWindow, el: AnyElement, window: &mut Window) -> AnyElement {
    let Some((old, dir, at)) = m.close_anim.pane.as_ref().filter(|(_, _, at)| ms(*at) < SLIDE_MS) else { return el };
    window.request_animation_frame();
    let e = ease_out(ms(*at) / SLIDE_MS);
    // Next below: everything moves up. Next above: down.
    let (old_top, new_top) = match dir {
        Dir::Below => (-e, 1. - e),
        Dir::Above => (e, e - 1.),
    };
    div()
        .relative()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .overflow_hidden()
        .child(div().flex().flex_col().flex_1().min_h_0().relative().top(relative(new_top)).child(el))
        .child(div().absolute().left_0().size_full().top(relative(old_top)).flex().flex_col().occlude().child(old.clone()))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn groups() -> Vec<PlanGroup> {
        vec![
            PlanGroup { key: "a".into(), folded: false, rows: s(&["a1", "a2", "a3"]) },
            PlanGroup { key: "b".into(), folded: false, rows: s(&["b1"]) },
        ]
    }

    fn order() -> Vec<String> {
        s(&["a1", "a2", "a3", "b1"])
    }

    #[test]
    fn next_above_glides_the_highlight_up() {
        let p = plan(&groups(), &order(), &s(&["a2"]), Some("a1"));
        assert_eq!(p.ghosts, vec![("a2".into(), "a".into(), Some("a3".into()), false)]);
        assert_eq!(p.glide, Some(("a1".into(), Dir::Above)));
        assert_eq!(p.pane, Some(Dir::Above));
    }

    #[test]
    fn next_below_slides_into_the_ghosts_highlight() {
        let p = plan(&groups(), &order(), &s(&["a1"]), Some("a2"));
        assert_eq!(p.ghosts, vec![("a1".into(), "a".into(), Some("a2".into()), true)]);
        assert_eq!(p.glide, Some(("a2".into(), Dir::Below)));
        assert_eq!(p.pane, Some(Dir::Below));
    }

    #[test]
    fn next_in_another_group_moves_no_highlight() {
        let p = plan(&groups(), &order(), &s(&["b1"]), Some("a3"));
        assert_eq!(p.ghosts, vec![("b1".into(), "b".into(), None, false)]);
        assert_eq!(p.glide, None);
        assert_eq!(p.pane, Some(Dir::Above));
    }

    #[test]
    fn several_sit_above_the_first_row_left() {
        let p = plan(&groups(), &order(), &s(&["a1", "a2"]), Some("a3"));
        assert_eq!(p.ghosts, vec![("a1".into(), "a".into(), Some("a3".into()), false), ("a2".into(), "a".into(), Some("a3".into()), false)]);
        assert_eq!(p.glide, None);
        assert_eq!(p.pane, Some(Dir::Below));
    }

    #[test]
    fn folded_groups_and_the_last_terminal() {
        let mut g = groups();
        g[1].folded = true;
        let p = plan(&g, &order(), &s(&["b1"]), None);
        assert!(p.ghosts.is_empty());
        assert_eq!((p.glide, p.pane), (None, None));
    }

    #[test]
    fn a_new_project_unfolds_whole_and_a_new_row_on_its_own() {
        let g = |key: &str, folded: bool, rows: &[&str]| PlanGroup { key: key.into(), folded, rows: rows.iter().map(|r| r.to_string()).collect() };
        let before = vec![("p_a".to_string(), vec!["s1".to_string()]), ("p_f".to_string(), vec!["s3".to_string()])];
        let now = [g("p_a", false, &["s1", "s2"]), g("p_f", true, &["s3", "s4"]), g("p_new", false, &["s5"])];
        assert_eq!(arrivals(&now, &before), ["s2", "p_new"]);
        assert!(arrivals(&now[..1], &[("p_a".into(), vec!["s1".into(), "s2".into()])]).is_empty());
    }

    #[test]
    fn opening_beside_glides_the_highlight() {
        assert_eq!(plan_move(&groups(), &order(), Some("a2"), "a3"), (Some(Dir::Below), Some(Dir::Below)));
        assert_eq!(plan_move(&groups(), &order(), Some("a2"), "a1"), (Some(Dir::Above), Some(Dir::Above)));
    }

    #[test]
    fn a_row_opens_where_the_row_it_pushes_down_was() {
        let rows: std::collections::HashMap<String, (f32, f32)> = [("a1", (0., 30.)), ("a3", (30., 30.)), ("b1", (100., 30.))].into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        let g = groups();
        assert_eq!(slot_top(&g, &rows, "a2"), Some(30.));
        let mut g2 = groups();
        g2[0].rows.push("a4".into());
        assert_eq!(slot_top(&g2, &rows, "a4"), Some(60.));
        g2[0].folded = true;
        assert_eq!(slot_top(&g2, &rows, "a4"), None);
    }

    #[test]
    fn opening_elsewhere_only_slides_the_pane() {
        assert_eq!(plan_move(&groups(), &order(), Some("a1"), "b1"), (None, Some(Dir::Below)));
        assert_eq!(plan_move(&groups(), &order(), Some("a3"), "b1"), (None, Some(Dir::Below)));
        assert_eq!(plan_move(&groups(), &order(), None, "b1"), (None, None));
        let mut g = groups();
        g[0].folded = true;
        assert_eq!(plan_move(&g, &order(), Some("a2"), "a3"), (None, Some(Dir::Below)));
    }
}
