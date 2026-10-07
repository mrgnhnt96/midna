//! "Needs you" coming and going in the sidebar (design C, "Sweep", on the needs-you animations
//! canvas). A terminal that starts needing you: a band of the need color sweeps across its row
//! left to right, the "N need you" button opens above the list and the band carries on across
//! it, a bar grows on the row's left edge (it stays while the item waits) and the reason line
//! drops in. Still waiting: a softer sweep every `REMIND`. Handled: the reverse. The band
//! crosses the button right to left and the button folds away (when nothing else waits), then
//! crosses the row, the edge bar shrinks to its middle, and the reason line says what you did
//! ("Approved once", "Denied", ...) before it folds up.
//!
//! `sync` diffs what needs you against the last call (after every refresh and on `resolve`).
//! The first lists after launch, and every change while Reduce Motion is on, show as they are.
use crate::app::MainWindow;
use crate::model::*;
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

// Each part of a timeline is (start, length) in ms.
/// Coming in: the row's sweep, its reason line and edge bar; the button opening and its sweep.
const IN_SWEEP: (f32, f32) = (0., 900.);
const IN_LINE: (f32, f32) = (450., 400.);
const IN_EDGE: (f32, f32) = (620., 360.);
const IN_OPEN: (f32, f32) = (380., 380.);
const IN_BTN_SWEEP: (f32, f32) = (520., 900.);
const IN_MS: f32 = 1420.;
/// Going out: the button's sweep and fold; the row's sweep, edge bar and reason line (which
/// holds what you did until it folds).
const OUT_BTN_SWEEP: (f32, f32) = (0., 800.);
const OUT_CLOSE: (f32, f32) = (520., 360.);
const OUT_SWEEP: (f32, f32) = (380., 800.);
const OUT_EDGE: (f32, f32) = (520., 340.);
const OUT_LINE: (f32, f32) = (900., 380.);
const OUT_MS: f32 = 1280.;
/// A softer sweep across each row that still waits (and the button, half a second later).
const REMIND: Duration = Duration::from_secs(60);
const REMIND_SWEEP: (f32, f32) = (0., 900.);
const REMIND_BTN_SWEEP: (f32, f32) = (500., 900.);
const REMIND_MS: f32 = 1400.;
/// The band's brightest point (alpha of the need color) and its width (of the row).
const SWEEP_ALPHA: f32 = 0.28;
const REMIND_ALPHA: f32 = 0.16;
pub const BAND: f32 = 0.45;
/// The reason line's height, for folding it in and out.
pub const LINE_H: f32 = 16.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dir {
    In,
    Out,
}

fn len(dir: Dir) -> f32 {
    if dir == Dir::In { IN_MS } else { OUT_MS }
}

/// A row leaving: what its reason line said, or what you did about it.
#[derive(Clone, Debug, PartialEq)]
pub struct Leaving {
    pub text: String,
    /// `None`: the need color (it went away on its own); else the outcome's color.
    pub tone: Option<Outcome>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Err,
    Dim,
}

#[derive(Default)]
pub struct NeedAnim {
    /// Set once the first lists have been seen; until then changes aren't animated.
    primed: bool,
    /// Terminals needing you at the last `sync`: their reason line and since when.
    waiting: HashMap<String, (String, Instant)>,
    count: usize,
    rows: HashMap<String, (Dir, Instant, Option<Leaving>)>,
    button: Option<(Dir, Instant)>,
    /// What you did about a terminal's item, from `resolve`, until its row leaves.
    outcomes: HashMap<String, Leaving>,
    /// The last reminder sweep; the task that schedules the next.
    remind_at: Option<Instant>,
    remind_task: Option<Task<()>>,
}

/// How a row draws right now.
#[derive(Debug, PartialEq)]
pub struct RowLook {
    /// The band's left edge (fraction of the row, -BAND..1) and its peak alpha; `None`: no band.
    pub sweep: Option<(f32, f32)>,
    /// The left edge bar's height, 0..1 (1 while waiting).
    pub edge: f32,
    /// How far the reason line is unfolded, 0..1.
    pub line: f32,
    /// A leaving row's last line.
    pub leaving: Option<Leaving>,
}

/// How the "N need you" button draws right now.
#[derive(Debug, PartialEq)]
pub struct ButtonLook {
    /// Shown at all (it stays through its fold after the last item goes).
    pub shown: bool,
    /// How far it's open, 0..1.
    pub open: f32,
    pub sweep: Option<(f32, f32)>,
    /// Folding away after the last item went: it reads "0 need you", dimmed.
    pub leaving: bool,
}

/// Where a part of the timeline is: 0 before `(start, len)`, 1 after.
fn progress(ms: f32, (start, len): (f32, f32)) -> f32 {
    ((ms - start) / len).clamp(0., 1.)
}

fn ease_out(x: f32) -> f32 {
    1. - (1. - x).powi(3)
}

fn ease_in_out(x: f32) -> f32 {
    if x < 0.5 { 4. * x * x * x } else { 1. - (-2. * x + 2.).powi(3) / 2. }
}

fn ms(since: Instant) -> f32 {
    since.elapsed().as_secs_f32() * 1000.
}

/// A band crossing left to right over `span`, or right to left; `None` outside it.
fn band(ms: f32, span: (f32, f32), ltr: bool, alpha: f32) -> Option<(f32, f32)> {
    let p = (ms - span.0) / span.1;
    if !(0. ..1.).contains(&p) {
        return None;
    }
    let x = ease_in_out(p);
    let x = if ltr { x } else { 1. - x };
    Some((-BAND + x * (1. + BAND), alpha))
}

/// What `resolve` did, in words, for the row's last line.
pub fn outcome(res: &Resolution) -> Leaving {
    let (text, tone) = match res {
        Resolution::Approve { scope } => (
            match scope {
                ApprovalScope::Once => "Approved once".to_string(),
                ApprovalScope::Minutes { minutes } => format!("Approved for {minutes} min"),
                ApprovalScope::Session => "Approved for this session".into(),
                ApprovalScope::Always => "Approved always".into(),
            },
            Outcome::Ok,
        ),
        Resolution::Done => ("Done".into(), Outcome::Ok),
        Resolution::Restart => ("Restarting".into(), Outcome::Ok),
        Resolution::Deny => ("Denied".into(), Outcome::Err),
        Resolution::Dismiss => ("Dismissed".into(), Outcome::Dim),
    };
    Leaving { text, tone: Some(tone) }
}

/// Remember what you did about `session`'s item, for its row's last line.
pub fn note_outcome(m: &mut MainWindow, session: &str, res: &Resolution) {
    m.need_anim.outcomes.insert(session.to_string(), outcome(res));
}

/// The reason line a waiting terminal shows (as `sidebar::row` words it, without the age).
fn reason(m: &MainWindow, s: &Session) -> String {
    s.status.reason.clone().filter(|r| !r.is_empty()).or_else(|| m.need_for_session(&s.id).map(|n| n.title.clone())).unwrap_or_else(|| "needs you".into())
}

/// Diff what needs you against the last call and start the animations for what changed.
pub fn sync(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    let now: HashMap<String, String> = m.sessions.iter().filter(|s| m.effective_state(s) == StatusState::NeedsYou).map(|s| (s.id.clone(), reason(m, s))).collect();
    let live: HashSet<String> = m.sessions.iter().map(|s| s.id.clone()).collect();
    gpui_kit::base::apply_system_reduce_motion(cx);
    let animate = !cx.reduce_motion();
    m.need_anim.diff(now, m.needs.len(), &live, animate, Instant::now());
    remind(m, cx);
    if m.need_anim.moving() {
        cx.notify();
    }
}

/// While anything waits, a softer sweep every `REMIND` (one task per window).
fn remind(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    if m.need_anim.waiting.is_empty() {
        m.need_anim.remind_task = None;
        m.need_anim.remind_at = None;
        return;
    }
    if m.need_anim.remind_task.is_some() {
        return;
    }
    m.need_anim.remind_task = Some(cx.spawn(async move |this, cx| {
        loop {
            cx.background_executor().timer(REMIND).await;
            let Ok(more) = this.update(cx, |m, cx| {
                gpui_kit::base::apply_system_reduce_motion(cx);
                if !cx.reduce_motion() {
                    m.need_anim.remind_at = Some(Instant::now());
                    cx.notify();
                }
                !m.need_anim.waiting.is_empty()
            }) else { return };
            if !more {
                return;
            }
        }
    }));
}

impl NeedAnim {
    /// `now`: the terminals needing you and their reason lines; `count`: the needs-you items;
    /// `live`: every terminal. `animate` false (Reduce Motion) shows changes as they are.
    fn diff(&mut self, now: HashMap<String, String>, count: usize, live: &HashSet<String>, animate: bool, t: Instant) {
        self.rows.retain(|id, (dir, at, _)| live.contains(id) && ms(*at) < len(*dir));
        if self.button.is_some_and(|(dir, at)| ms(at) >= len(dir)) {
            self.button = None;
        }
        if animate && self.primed {
            for id in now.keys().filter(|id| !self.waiting.contains_key(*id)) {
                self.rows.insert(id.clone(), (Dir::In, t, None));
            }
            for (id, (text, _)) in self.waiting.iter().filter(|(id, _)| !now.contains_key(*id) && live.contains(*id)) {
                let leaving = self.outcomes.remove(id).unwrap_or_else(|| Leaving { text: text.clone(), tone: None });
                self.rows.insert(id.clone(), (Dir::Out, t, Some(leaving)));
            }
            if self.count == 0 && count > 0 {
                self.button = Some((Dir::In, t));
            } else if self.count > 0 && count == 0 {
                self.button = Some((Dir::Out, t));
            }
        } else {
            self.rows.clear();
            self.button = None;
        }
        self.outcomes.retain(|id, _| now.contains_key(id));
        let old = std::mem::take(&mut self.waiting);
        self.waiting = now.into_iter().map(|(id, text)| {
            let since = old.get(&id).map_or(t, |(_, since)| *since);
            (id, (text, since))
        }).collect();
        self.count = count;
        self.primed |= !live.is_empty();
    }

    /// Whether anything is still moving (the sidebar asks for the next frame).
    pub fn moving(&self) -> bool {
        self.rows.values().any(|(dir, at, _)| ms(*at) < len(*dir))
            || self.button.is_some_and(|(dir, at)| ms(at) < len(dir))
            || self.remind_at.is_some_and(|at| ms(at) < REMIND_MS)
    }

    /// `waiting`: the terminal needs you now.
    pub fn row(&self, sid: &str, waiting: bool) -> RowLook {
        // A row that started waiting since the last reminder sits this one out.
        let due = self.waiting.get(sid).is_some_and(|(_, since)| self.remind_at.is_some_and(|r| *since < r));
        let remind = self.remind_at.filter(|_| waiting && due).and_then(|at| band(ms(at), REMIND_SWEEP, true, REMIND_ALPHA));
        match self.rows.get(sid) {
            Some((Dir::In, at, _)) if waiting => {
                let t = ms(*at);
                RowLook { sweep: band(t, IN_SWEEP, true, SWEEP_ALPHA), edge: ease_out(progress(t, IN_EDGE)), line: ease_out(progress(t, IN_LINE)), leaving: None }
            }
            Some((Dir::Out, at, leaving)) if !waiting && ms(*at) < OUT_MS => {
                let t = ms(*at);
                RowLook { sweep: band(t, OUT_SWEEP, false, SWEEP_ALPHA), edge: 1. - ease_out(progress(t, OUT_EDGE)), line: 1. - ease_in_out(progress(t, OUT_LINE)), leaving: leaving.clone() }
            }
            _ => RowLook { sweep: remind, edge: if waiting { 1. } else { 0. }, line: 1., leaving: None },
        }
    }

    /// `has`: something needs you now.
    pub fn button(&self, has: bool) -> ButtonLook {
        let remind = self.remind_at.filter(|_| has).and_then(|at| band(ms(at), REMIND_BTN_SWEEP, true, REMIND_ALPHA));
        match self.button {
            Some((Dir::In, at)) if has => {
                let t = ms(at);
                ButtonLook { shown: true, open: ease_out(progress(t, IN_OPEN)), sweep: band(t, IN_BTN_SWEEP, true, SWEEP_ALPHA), leaving: false }
            }
            Some((Dir::Out, at)) if !has && ms(at) < OUT_MS => {
                let t = ms(at);
                ButtonLook { shown: true, open: 1. - ease_in_out(progress(t, OUT_CLOSE)), sweep: band(t, OUT_BTN_SWEEP, false, SWEEP_ALPHA), leaving: true }
            }
            _ => ButtonLook { shown: has, open: 1., sweep: remind, leaving: false },
        }
    }
}

/// The band itself: an absolutely placed strip, clear at both ends and `alpha` in the middle.
/// Its parent must be `relative` and clip its overflow.
pub fn band_el(t: &Theme, (left, alpha): (f32, f32)) -> Div {
    let c = t.need;
    div()
        .absolute()
        .top_0()
        .bottom_0()
        .left(relative(left))
        .w(relative(BAND))
        .flex()
        .child(div().flex_1().h_full().bg(linear_gradient(90., linear_color_stop(c.opacity(0.), 0.), linear_color_stop(c.opacity(alpha), 1.))))
        .child(div().flex_1().h_full().bg(linear_gradient(90., linear_color_stop(c.opacity(alpha), 0.), linear_color_stop(c.opacity(0.), 1.))))
}

/// The bar on a waiting row's left edge, `v` of its full height, centered.
pub fn edge_el(t: &Theme, v: f32) -> Div {
    let gap = (1. - v.clamp(0., 1.)) / 2.;
    div().absolute().left_0().w(px(2.)).top(relative(gap)).bottom(relative(gap)).bg(t.need)
}

pub fn outcome_color(t: &Theme, tone: Option<Outcome>) -> Hsla {
    match tone {
        None => t.need,
        Some(Outcome::Ok) => t.ok,
        Some(Outcome::Err) => t.err,
        Some(Outcome::Dim) => t.dim,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn ids(v: &[&str]) -> HashSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn waiting(v: &[(&str, &str)]) -> HashMap<String, String> {
        v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    fn ago(ms: u64) -> Instant {
        Instant::now() - Duration::from_millis(ms)
    }

    /// Primed with `live` terminals and nothing waiting.
    fn primed(live: &HashSet<String>) -> NeedAnim {
        let mut a = NeedAnim::default();
        a.diff(HashMap::new(), 0, live, true, Instant::now());
        a
    }

    #[test]
    fn first_list_after_launch_is_not_animated() {
        let live = ids(&["s1"]);
        let mut a = NeedAnim::default();
        a.diff(waiting(&[("s1", "Approve: ls")]), 1, &live, true, Instant::now());
        assert!(!a.moving());
        let row = a.row("s1", true);
        assert_eq!((row.sweep, row.edge, row.line), (None, 1., 1.));
        assert_eq!(a.button(true), ButtonLook { shown: true, open: 1., sweep: None, leaving: false });
    }

    #[test]
    fn coming_in_sweeps_the_row_then_settles_with_the_edge_bar() {
        let live = ids(&["s1", "s2"]);
        let mut a = primed(&live);
        a.diff(waiting(&[("s1", "Approve: ls")]), 1, &live, true, ago(0));
        assert!(a.moving());
        let start = a.row("s1", true);
        assert!(start.sweep.is_some_and(|(x, _)| x < 0.), "the band starts off the row's left edge: {start:?}");
        assert!(start.edge < 0.05 && start.line < 0.05);
        assert_eq!(a.row("s2", false).edge, 0.);
        // The button opens: nothing needed you before.
        let b = a.button(true);
        assert!(b.shown && b.open < 0.05 && !b.leaving);

        a.rows.insert("s1".into(), (Dir::In, ago(IN_MS as u64 + 50), None));
        a.button = Some((Dir::In, ago(IN_MS as u64 + 50)));
        assert!(!a.moving());
        assert_eq!(a.row("s1", true), RowLook { sweep: None, edge: 1., line: 1., leaving: None });
        assert_eq!(a.button(true), ButtonLook { shown: true, open: 1., sweep: None, leaving: false });
    }

    #[test]
    fn a_second_item_leaves_the_button_alone() {
        let live = ids(&["s1", "s2"]);
        let mut a = primed(&live);
        a.diff(waiting(&[("s1", "a")]), 1, &live, true, ago(5000));
        a.diff(waiting(&[("s1", "a"), ("s2", "b")]), 2, &live, true, ago(0));
        assert!(a.button.is_none_or(|(_, at)| ms(at) > IN_MS));
        assert!(matches!(a.rows.get("s2"), Some((Dir::In, _, None))));
    }

    #[test]
    fn handled_says_what_you_did_and_the_button_folds_away() {
        let live = ids(&["s1"]);
        let mut a = primed(&live);
        a.diff(waiting(&[("s1", "Approve: ls")]), 1, &live, true, ago(5000));
        a.outcomes.insert("s1".into(), outcome(&Resolution::Approve { scope: ApprovalScope::Once }));
        a.diff(HashMap::new(), 0, &live, true, ago(0));
        let row = a.row("s1", false);
        assert_eq!(row.leaving, Some(Leaving { text: "Approved once".into(), tone: Some(Outcome::Ok) }));
        assert!(row.edge > 0.95 && row.line > 0.95, "the edge and line hold before they go: {row:?}");
        let b = a.button(false);
        assert!(b.shown && b.leaving && b.open > 0.95);
        assert!(b.sweep.is_some_and(|(x, _)| x > 0.9), "the band starts at the button's right: {b:?}");
        assert!(a.outcomes.is_empty());

        a.rows.insert("s1".into(), (Dir::Out, ago(OUT_MS as u64 + 50), a.rows["s1"].2.clone()));
        a.button = Some((Dir::Out, ago(OUT_MS as u64 + 50)));
        assert_eq!(a.row("s1", false).edge, 0.);
        assert!(!a.button(false).shown);
    }

    #[test]
    fn going_away_on_its_own_keeps_the_reason_in_the_need_color() {
        let live = ids(&["s1"]);
        let mut a = primed(&live);
        a.diff(waiting(&[("s1", "Approve: ls")]), 1, &live, true, ago(5000));
        a.diff(HashMap::new(), 0, &live, true, ago(0));
        assert_eq!(a.row("s1", false).leaving, Some(Leaving { text: "Approve: ls".into(), tone: None }));
    }

    #[test]
    fn a_closed_terminal_just_goes() {
        let mut a = primed(&ids(&["s1"]));
        a.diff(waiting(&[("s1", "a")]), 1, &ids(&["s1"]), true, ago(5000));
        a.diff(HashMap::new(), 0, &ids(&[]), true, ago(0));
        assert!(a.rows.is_empty());
    }

    #[test]
    fn reduce_motion_shows_changes_as_they_are() {
        let live = ids(&["s1"]);
        let mut a = primed(&live);
        a.diff(waiting(&[("s1", "a")]), 1, &live, false, ago(0));
        assert!(!a.moving());
        assert_eq!(a.row("s1", true).edge, 1.);
        a.diff(HashMap::new(), 0, &live, false, ago(0));
        assert!(!a.button(false).shown);
    }

    #[test]
    fn reminders_skip_rows_that_just_started_waiting() {
        let live = ids(&["old", "new"]);
        let mut a = primed(&live);
        a.diff(waiting(&[("old", "a")]), 1, &live, true, ago(90_000));
        a.diff(waiting(&[("old", "a"), ("new", "b")]), 2, &live, true, ago(0));
        a.rows.clear();
        a.remind_at = Some(ago(100));
        assert!(a.row("old", true).sweep.is_some_and(|(_, alpha)| alpha == REMIND_ALPHA));
        assert_eq!(a.row("new", true).sweep, None);
    }

    #[test]
    fn the_band_crosses_the_whole_row() {
        assert_eq!(band(0., (0., 900.), true, 1.), Some((-BAND, 1.)));
        assert_eq!(band(900., (0., 900.), true, 1.), None);
        assert!(band(899., (0., 900.), true, 1.).is_some_and(|(x, _)| x > 0.99));
        assert!(band(899., (0., 900.), false, 1.).is_some_and(|(x, _)| x < -BAND + 0.01));
    }
}
