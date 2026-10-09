//! The sidebar collapsing to its rail and back, ⌘B (design B, "Cascade", on the sidebar
//! collapse canvas). Collapsing: each line's text wipes back toward its status dot, top to
//! bottom one after another, then the panel closes to the rail and the rail's dots pop as they
//! land. Expanding: the panel opens first, then the lines write themselves back in.
//!
//! While the panel's width moves it draws over a rail-wide slot and the pane beside it slides
//! along as one block (`Frame::pane_offset`), so a terminal's grid changes size once (collapsing:
//! at the start; expanding: at the end), never every frame.
//!
//! The direction is `MainWindow::sidebar_collapsed` (already flipped); `MainWindow::sidebar_anim`
//! is when it flipped. Reduce Motion skips it.
use crate::app::MainWindow;
use gpui_kit::*;

/// Between one line starting to wipe (or write) and the next; shortened so the whole cascade
/// spans at most `SPREAD_MS`, however many terminals there are.
const STEP_MS: f32 = 22.;
const SPREAD_MS: f32 = 160.;
/// One line's text wiping back.
const WIPE_MS: f32 = 130.;
/// The panel closing starts this far into the wipe (as a share of it), so the two overlap.
const CLOSE_AT: f32 = 0.75;
/// The panel opening or closing.
const WIDTH_MS: f32 = 240.;
/// A rail dot's pop when it lands, and how much it grows at its peak.
const POP_MS: f32 = 280.;
const POP_GROW: f32 = 0.6;
/// A rail icon fading in beside its dot.
const FADE_MS: f32 = 120.;
/// Expanding: the first line starts writing back in this far into the opening.
const WRITE_AT_MS: f32 = 140.;
const WRITE_MS: f32 = 170.;

/// Where the animation is at one moment. `n` = how many lines cascade.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    ms: f32,
    collapsing: bool,
    step: f32,
    /// When the panel starts closing (collapsing only).
    close_at: f32,
    /// The panel's width now.
    pub panel_w: f32,
    /// The panel draws over a rail-wide slot (its width is moving, or about to).
    pub overlay: bool,
    /// Collapsing and the panel has closed: draw the rail (dots popping).
    pub rail: bool,
    pub done: bool,
}

impl Frame {
    pub fn at(ms: f32, collapsing: bool, n: usize, full_w: f32, rail_w: f32) -> Frame {
        let last = n.saturating_sub(1) as f32;
        let step = if last > 0. { STEP_MS.min(SPREAD_MS / last) } else { 0. };
        let ease = |v: f32| 1. - (1. - v.clamp(0., 1.)).powi(3);
        if collapsing {
            let close_at = (last * step + WIPE_MS) * CLOSE_AT;
            let p = ease((ms - close_at) / WIDTH_MS);
            let landed = close_at + WIDTH_MS;
            Frame {
                ms,
                collapsing,
                step,
                close_at,
                panel_w: full_w + (rail_w - full_w) * p,
                overlay: ms < landed,
                rail: ms >= landed,
                done: ms >= landed + last * step + POP_MS,
            }
        } else {
            Frame {
                ms,
                collapsing,
                step,
                close_at: 0.,
                panel_w: rail_w + (full_w - rail_w) * ease(ms / WIDTH_MS),
                overlay: ms < WIDTH_MS,
                rail: false,
                done: ms >= (WRITE_AT_MS + last * step + WRITE_MS).max(WIDTH_MS),
            }
        }
    }

    /// How much of line `k`'s text is wiped away: 0 = all there, 1 = gone.
    pub fn wipe(&self, k: usize) -> f32 {
        let start = k as f32 * self.step;
        if self.collapsing {
            let v = ((self.ms - start) / WIPE_MS).clamp(0., 1.);
            v * v
        } else {
            let v = ((self.ms - WRITE_AT_MS - start) / WRITE_MS).clamp(0., 1.);
            (1. - v).powi(3)
        }
    }

    /// Rail line `k` once the panel has closed: how much its dot has grown (0 at rest) and how
    /// far its icon has faded in (1 = fully).
    pub fn pop(&self, k: usize) -> (f32, f32) {
        if !self.rail {
            return (0., 1.);
        }
        let since = self.ms - self.close_at - WIDTH_MS - k as f32 * self.step;
        let q = (since / POP_MS).clamp(0., 1.);
        let grow = if q < 1. { (std::f32::consts::PI * q).sin() * POP_GROW } else { 0. };
        (grow, (since / FADE_MS).clamp(0., 1.))
    }

    /// How far right the pane beside the sidebar is pushed, so its left edge follows the panel's.
    pub fn pane_offset(&self, rail_w: f32) -> f32 {
        if self.overlay { self.panel_w - rail_w } else { 0. }
    }
}

/// This frame of the animation, if one is running (and asks for the next frame).
pub fn frame(m: &MainWindow, window: &mut Window) -> Option<Frame> {
    let at = m.sidebar_anim?;
    let f = Frame::at(at.elapsed().as_secs_f32() * 1000., m.sidebar_collapsed, lines(m), m.sidebar_width, crate::ui::sidebar::RAIL_WIDTH);
    if f.done {
        return None;
    }
    window.request_animation_frame();
    Some(f)
}

/// How many lines cascade: the top (strip and needs-you button), each project heading and
/// shown row, and the bottom (Background group and footer).
fn lines(m: &MainWindow) -> usize {
    let mut n = 2;
    for g in m.groups() {
        let folded = g.project.is_some_and(|p| m.collapsed.contains(&p.id));
        n += usize::from(g.project.is_some());
        // A pinned project with no terminals has its ghost "New terminal" row.
        n += usize::from(g.sessions.is_empty());
        n += g.sessions.iter().filter(|s| !folded || m.selected.as_deref() == Some(&s.id)).count();
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    const FULL: f32 = 264.;
    const RAIL: f32 = 76.;

    fn collapse(ms: f32, n: usize) -> Frame {
        Frame::at(ms, true, n, FULL, RAIL)
    }

    fn expand(ms: f32, n: usize) -> Frame {
        Frame::at(ms, false, n, FULL, RAIL)
    }

    #[test]
    fn collapsing_wipes_the_lines_before_the_panel_closes() {
        let f = collapse(0., 8);
        assert_eq!((f.panel_w, f.wipe(0), f.wipe(7)), (FULL, 0., 0.));
        assert!(f.overlay && !f.rail);
        // the first line is gone while the last is still wiping, and the panel hasn't moved
        let f = collapse(WIPE_MS, 8);
        assert_eq!(f.wipe(0), 1.);
        assert!(f.wipe(7) < 1.);
        assert_eq!(f.panel_w, FULL);
    }

    #[test]
    fn collapsing_ends_on_the_rail_with_the_dots_at_rest() {
        let f = collapse(5000., 8);
        assert!(f.done && f.rail && !f.overlay);
        assert_eq!((f.panel_w, f.pane_offset(RAIL), f.pop(3)), (RAIL, 0., (0., 1.)));
    }

    #[test]
    fn a_dot_pops_as_it_lands_one_after_another() {
        let landed = (7. * STEP_MS + WIPE_MS) * CLOSE_AT + WIDTH_MS;
        let f = collapse(landed + POP_MS / 2., 8);
        assert!(f.rail);
        let (first, _) = f.pop(0);
        let (later, fade) = f.pop(7);
        assert!(first > later, "{first} {later}");
        assert!((first - POP_GROW).abs() < 0.01);
        assert!(fade < 1.);
    }

    #[test]
    fn expanding_opens_the_panel_before_writing_the_lines() {
        let f = expand(0., 8);
        assert_eq!((f.panel_w, f.wipe(0)), (RAIL, 1.));
        assert!(f.overlay && !f.rail);
        let f = expand(WRITE_AT_MS, 8);
        assert!(f.panel_w > RAIL && f.panel_w < FULL);
        assert_eq!(f.wipe(0), 1.);
        let f = expand(WIDTH_MS, 8);
        assert_eq!(f.panel_w, FULL);
        assert!(!f.overlay);
        assert!(f.wipe(0) < f.wipe(7));
        assert!(expand(5000., 8).done);
    }

    #[test]
    fn the_pane_follows_the_panel_edge() {
        assert_eq!(collapse(0., 8).pane_offset(RAIL), FULL - RAIL);
        assert_eq!(expand(0., 8).pane_offset(RAIL), 0.);
        let f = expand(WIDTH_MS / 2., 8);
        assert_eq!(f.pane_offset(RAIL), f.panel_w - RAIL);
    }

    #[test]
    fn many_terminals_cascade_within_the_spread() {
        let f = collapse(SPREAD_MS + WIPE_MS, 200);
        assert_eq!(f.wipe(199), 1.);
        assert!(expand(WRITE_AT_MS + SPREAD_MS + WRITE_MS, 200).done);
    }
}
