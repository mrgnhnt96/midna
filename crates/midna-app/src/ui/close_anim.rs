//! Closing terminals with ⌘W (design C, "Conveyor", on the tab close canvas). The closed row
//! stays as a ghost while it folds away: its content lifts and fades, and the rows below slide
//! up into its room. The selection moves with the list: the terminal shown next comes from the
//! row below (it slides up into the highlight, which stays put) or the row above (the highlight
//! glides up to it). The terminal pane slides the same way, in step: the closed terminal leaves
//! the way the list moves and the next one comes in behind it.
//!
//! `start` runs before the sessions leave `MainWindow::sessions`; the pane's slide starts when
//! the next terminal is promoted (`on_show`), which waits for its first frame. With Reduce
//! Motion on, nothing moves.
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

/// How a row draws its selection highlight.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Highlight {
    /// As usual.
    Own,
    /// Not at all: the ghost above draws it.
    None,
    /// Shifted down by this fraction of the row's height (the highlight gliding up to it).
    Shifted(f32),
}

#[derive(Default)]
pub struct CloseAnim {
    ghosts: Vec<Ghost>,
    /// The terminal shown next, when it's the row right above or below the closed one.
    glide: Option<(String, Dir, Instant)>,
    /// Closed terminals whose pane slides when the next one shows, and which way.
    pane_due: Option<(Vec<String>, Dir, Instant)>,
    pane: Option<(Entity<TerminalView>, Dir, Instant)>,
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
    let groups: Vec<PlanGroup> = m
        .groups()
        .into_iter()
        .map(|g| {
            let pid = g.project.map(|p| p.id.clone());
            PlanGroup {
                folded: pid.as_ref().is_some_and(|p| m.collapsed.contains(p)),
                key: pid.unwrap_or_else(|| "root".into()),
                rows: g.sessions.iter().map(|s| s.id.clone()).collect(),
            }
        })
        .collect();
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
    }

    /// The sidebar is moving (it asks for the next frame).
    pub fn moving(&self) -> bool {
        self.ghosts.iter().any(|g| ms(g.at) < SLIDE_MS) || self.glide.as_ref().is_some_and(|(_, _, at)| ms(*at) < SLIDE_MS)
    }

    /// Ghosts in `group` sitting above `next` (`None`: at the group's end).
    pub fn ghosts<'a>(&'a self, group: &'a str, next: Option<&'a str>) -> impl Iterator<Item = &'a Ghost> + 'a {
        self.ghosts.iter().filter(move |g| g.group == group && g.next.as_deref() == next && ms(g.at) < SLIDE_MS)
    }

    pub fn highlight(&self, id: &str) -> Highlight {
        match &self.glide {
            Some((g, dir, at)) if g == id && ms(*at) < SLIDE_MS => match dir {
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
}
