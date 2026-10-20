//! GitHub pull request links in the grid show their state (canvas "PR status in terminal links",
//! A + B): the URL takes the state's color over a faint underline (struck through when closed,
//! dashed when a draft), and GitHub's icon for the state sits one cell after it when those two
//! cells are blank. The state comes from the link preview's `gh pr view`, which shares its cache;
//! merged and closed pull requests are asked again less often, as they rarely change.
//! `terminal.pr_status` turns it off.
use super::link_preview;
use super::{LINE_H, PAD_X, PAD_Y, TerminalView};
use crate::frame::{F_SPACER, RowData};
use crate::icons::Icon;
use crate::theme::Theme;
use gpui_kit::*;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// How long a state is trusted before it's asked again (open, draft, or not known).
const LIVE_TTL: Duration = Duration::from_secs(90);
const DONE_TTL: Duration = Duration::from_secs(600);

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum State {
    Open,
    Draft,
    Merged,
    Closed,
}

impl State {
    fn parse(s: &str) -> Option<State> {
        Some(match s {
            "OPEN" => State::Open,
            "DRAFT" => State::Draft,
            "MERGED" => State::Merged,
            "CLOSED" => State::Closed,
            _ => return None,
        })
    }

    /// The colors of the preview card's state pill.
    fn color(self, t: &Theme) -> Hsla {
        match self {
            State::Open => t.ok,
            State::Draft => t.dim,
            State::Merged => t.accent,
            State::Closed => t.err,
        }
    }

    fn icon(self) -> Icon {
        match self {
            State::Open => Icon::Pr,
            State::Draft => Icon::PrDraft,
            State::Merged => Icon::PrMerged,
            State::Closed => Icon::PrClosed,
        }
    }
}

/// A pull request link on screen.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Found {
    /// `https://github.com/<owner>/<repo>/pull/<n>`, whatever page of it the link was.
    pub url: String,
    /// The cells it covers, as (row, first col, last col): more than one when it wraps.
    pub spans: Vec<(u16, u16, u16)>,
    /// Where its icon goes, when the cells after it are blank.
    pub icon: Option<(u16, u16)>,
}

fn url_char(c: char) -> bool {
    c.is_ascii_graphic() && !matches!(c, '<' | '>' | '"' | '\'' | '`' | '{' | '}' | '|' | '\\' | '^')
}

fn blank(row: &RowData, x: usize) -> bool {
    row.cells.get(x).is_some_and(|c| (c.ch == ' ' || c.ch == '\0') && c.flags & F_SPACER == 0)
}

/// The pull request links on screen; a soft-wrapped line is read as one.
pub(super) fn find(grid: &[RowData]) -> Vec<Found> {
    let mut out = vec![];
    let mut y = 0;
    while y < grid.len() {
        // (char, row, col) along the line, spacer cells left out
        let mut line: Vec<(char, u16, u16)> = vec![];
        loop {
            let row = &grid[y];
            line.extend(row.cells.iter().enumerate().filter(|(_, c)| c.flags & F_SPACER == 0).map(|(x, c)| (c.ch, y as u16, x as u16)));
            y += 1;
            if !row.wrapped || y >= grid.len() {
                break;
            }
        }
        let at = |i: usize, p: &str| p.chars().enumerate().all(|(k, pc)| line.get(i + k).is_some_and(|c| c.0 == pc));
        let mut i = 0;
        while i < line.len() {
            if !(at(i, "https://github.com/") || at(i, "http://github.com/")) || (i > 0 && (line[i - 1].0.is_ascii_alphanumeric() || line[i - 1].0 == '/')) {
                i += 1;
                continue;
            }
            let mut j = i;
            while j < line.len() && url_char(line[j].0) {
                j += 1;
            }
            while j > i && matches!(line[j - 1].0, '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']') {
                j -= 1;
            }
            let url: String = line[i..j].iter().map(|c| c.0).collect();
            if let Some((owner, repo, n)) = link_preview::github_pr(&url) {
                let mut spans: Vec<(u16, u16, u16)> = vec![];
                for &(_, r, x) in &line[i..j] {
                    match spans.last_mut() {
                        Some(s) if s.0 == r && s.2 + 1 == x => s.2 = x,
                        _ => spans.push((r, x, x)),
                    }
                }
                let (_, r, x) = line[j - 1];
                let row = &grid[r as usize];
                let icon = (blank(row, x as usize + 1) && blank(row, x as usize + 2)).then_some((r, x + 2));
                out.push(Found { url: format!("https://github.com/{owner}/{repo}/pull/{n}"), spans, icon });
            }
            i = j.max(i + 1);
        }
    }
    out
}

/// Each pull request's last known state (`None`: not one `gh` could read), when it was asked,
/// and whether an ask is out now.
static STATES: LazyLock<Mutex<HashMap<String, (Instant, Option<State>, bool)>>> = LazyLock::new(Default::default);

fn known(url: &str) -> Option<State> {
    STATES.lock().ok()?.get(url)?.1
}

/// Marks the ones due an ask as asked, and returns them.
fn due(urls: impl Iterator<Item = String>) -> Vec<String> {
    let Ok(mut m) = STATES.lock() else { return vec![] };
    let mut out = vec![];
    for url in urls {
        let stale = match m.get(&url) {
            None => true,
            Some((_, _, true)) => false,
            Some((at, s, _)) => at.elapsed() >= if matches!(s, Some(State::Merged | State::Closed)) { DONE_TTL } else { LIVE_TTL },
        };
        if stale && !out.contains(&url) {
            let prev = m.get(&url).and_then(|e| e.1);
            m.insert(url.clone(), (Instant::now(), prev, true));
            out.push(url);
        }
    }
    out
}

impl TerminalView {
    /// After a frame: find the pull request links on screen and ask for states not known yet.
    pub(super) fn scan_prs(&mut self, cx: &mut Context<Self>) {
        self.prs = if link_preview::pr_status_on(cx) { find(&self.grid) } else { vec![] };
        for url in due(self.prs.iter().map(|f| f.url.clone())) {
            cx.spawn(async move |this, cx| {
                let u = url.clone();
                let state = cx.background_executor().spawn(async move { link_preview::pr_state(&u) }).await;
                if let Ok(mut m) = STATES.lock() {
                    m.insert(url, (Instant::now(), state.as_deref().and_then(State::parse), false));
                }
                let _ = this.update(cx, |_, cx| cx.notify());
            })
            .detach();
        }
    }

    /// What to paint: the links whose state is known, with its color.
    pub(super) fn pr_marks(&self, t: &Theme, cx: &App) -> Vec<Mark> {
        if !link_preview::pr_status_on(cx) {
            return vec![];
        }
        self.prs.iter().filter_map(|f| known(&f.url).map(|s| Mark { spans: f.spans.clone(), icon: f.icon, state: s, color: s.color(t) })).collect()
    }
}

pub(super) struct Mark {
    pub spans: Vec<(u16, u16, u16)>,
    icon: Option<(u16, u16)>,
    state: State,
    pub color: Hsla,
}

/// The underline (or strike) under each link and the icon after it; the text itself is tinted
/// in `paint_grid`.
pub(super) fn paint(marks: &[Mark], bounds: Bounds<Pixels>, cw: f32, window: &mut Window, cx: &mut App) {
    let ox = f32::from(bounds.origin.x) + PAD_X;
    let oy = f32::from(bounds.origin.y) + PAD_Y;
    for m in marks {
        let line = m.color.opacity(0.45);
        for &(row, a, b) in &m.spans {
            let x0 = ox + cw * a as f32;
            let w = cw * (b - a + 1) as f32;
            let py = oy + LINE_H * row as f32;
            let under = py + LINE_H - 3.;
            if m.state == State::Draft {
                let mut x = 0.;
                while x < w {
                    window.paint_quad(fill(Bounds::new(point(px(x0 + x), px(under)), size(px(3f32.min(w - x)), px(1.))), line));
                    x += 5.;
                }
            } else {
                window.paint_quad(fill(Bounds::new(point(px(x0), px(under)), size(px(w), px(1.))), line));
            }
            if m.state == State::Closed {
                window.paint_quad(fill(Bounds::new(point(px(x0), px(py + LINE_H / 2.)), size(px(w), px(1.))), line));
            }
        }
        if let Some((row, col)) = m.icon {
            let s = 12.;
            let o = point(px(ox + cw * col as f32 + (cw - s) / 2.), px(oy + LINE_H * row as f32 + (LINE_H - s) / 2.));
            let _ = window.paint_svg(Bounds::new(o, size(px(s), px(s))), m.state.icon().path().into(), None, TransformationMatrix::unit(), m.color, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::find;
    use crate::frame::{Cell, RowData};

    fn row(s: &str, cols: usize, wrapped: bool) -> RowData {
        let mut cells: Vec<Cell> = s.chars().map(|ch| Cell { ch, ..Default::default() }).collect();
        cells.resize(cols, Cell { ch: ' ', ..Default::default() });
        RowData { cells, wrapped, ..Default::default() }
    }

    #[test]
    fn finds_pull_request_links() {
        let g = [row("  Open: https://github.com/mrgnhnt96/taskboard/pull/550", 70, false)];
        let f = find(&g);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].url, "https://github.com/mrgnhnt96/taskboard/pull/550");
        assert_eq!(f[0].spans, vec![(0, 8, 54)]);
        assert_eq!(f[0].icon, Some((0, 56)));
    }

    #[test]
    fn text_right_after_leaves_no_room_for_the_icon() {
        let g = [row("see (https://github.com/a/b/pull/7/files). ok", 60, false)];
        let f = find(&g);
        assert_eq!(f[0].url, "https://github.com/a/b/pull/7");
        // the parens and full stop aren't part of it
        assert_eq!(f[0].spans, vec![(0, 5, 39)]);
        assert_eq!(f[0].icon, None);
        let g = [row("https://github.com/a/b/pull/7", 30, false)]; // ends one cell from the edge
        assert_eq!(find(&g)[0].icon, None);
    }

    #[test]
    fn wrapped_links_and_other_pages() {
        let g = [row("x https://github.com/a/", 23, true), row("b/pull/12", 23, false)];
        let f = find(&g);
        assert_eq!(f[0].url, "https://github.com/a/b/pull/12");
        assert_eq!(f[0].spans, vec![(0, 2, 22), (1, 0, 8)]);
        assert_eq!(f[0].icon, Some((1, 10)));
        let g = [row("https://github.com/a/b/issues/3 https://github.com/a/b", 80, false)];
        assert!(find(&g).is_empty());
    }
}
