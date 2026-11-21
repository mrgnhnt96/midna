//! Plain ↑ pressed fast in Claude Code's input. ↑ on the text's first row recalls history, and
//! a frame can be behind the keys (a busy Claude can take a second to draw), so judging each ↑
//! by the screen lets a burst run past the first row. Here the ↑ on their way are counted, and
//! those that would go past the first row are held until the screen shows whether there's a row
//! for them. A burst that reaches the first row stops there: the ↑ after it are dropped. A fresh
//! ↑ on the first row still goes to Claude, so history is a press away.

use crate::frame::RowData;
use crate::term_edit::{self, LongSel, Pos, Send};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::{Duration, Instant};

/// ↑ this soon after the last one belongs to the same burst (key repeat is ~30 ms apart).
const BURST_GAP: Duration = Duration::from_millis(250);

/// How long held ↑ wait for the screen to show the ones sent before they're dropped.
pub const WAIT: Duration = Duration::from_millis(1500);

/// What to do with a plain ↑.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Up {
    /// Send it now: there's a row above for it.
    Send,
    /// Keep it until the screen catches up ([`UpBurst::frame`] sends it, or drops it).
    Hold,
    /// Not this guard's: send it the usual way.
    Pass,
}

#[derive(Debug)]
pub struct UpBurst {
    last: Instant,
    /// None while this burst goes straight to Claude: it started on the text's first row (for
    /// history), or its ↑ went to a menu.
    guard: Option<Guard>,
}

#[derive(Debug)]
struct Guard {
    track: LongSel,
    held: u16,
    /// The cursor, and the rows under the box, in the last frame seen.
    cursor: Pos,
    below: u64,
    /// When the screen last showed a ↑ landing (or the first was sent).
    since: Instant,
}

impl Guard {
    /// ↑ are held, or on their way.
    fn busy(&self) -> bool {
        self.held > 0 || !self.track.settled()
    }
}

impl UpBurst {
    /// A plain ↑ in Claude's input with the cursor at `cursor`.
    pub fn key(slot: &mut Option<UpBurst>, grid: &[RowData], cursor: Pos, now: Instant) -> Up {
        let goes_on = slot.as_ref().is_some_and(|b| now.duration_since(b.last) < BURST_GAP || b.guard.as_ref().is_some_and(Guard::busy));
        if !goes_on {
            let guard = LongSel::track(grid, cursor).filter(|_| !at_top(grid, cursor));
            let guard = guard.map(|track| Guard { track, held: 0, cursor, below: below_box(grid, cursor), since: now });
            *slot = Some(UpBurst { last: now, guard });
        }
        let Some(burst) = slot.as_mut() else { return Up::Pass };
        burst.last = now;
        let Some(g) = burst.guard.as_mut() else { return Up::Pass };
        // Held ones go first, so this one waits behind them.
        if g.held > 0 || at_top(grid, g.track.aimed(grid, cursor)) {
            g.held = g.held.saturating_add(1);
            return Up::Hold;
        }
        if g.track.settled() {
            g.since = now;
        }
        g.track.pressed("up");
        Up::Send
    }

    /// A new frame (`cursor` None while scrolled back): how many held ↑ to send now.
    pub fn frame(&mut self, grid: &[RowData], cursor: Option<Pos>, now: Instant) -> u16 {
        let Some(g) = self.guard.as_mut() else { return 0 };
        let Some(c) = cursor else {
            self.guard = None;
            return 0;
        };
        let below = below_box(grid, c);
        if !g.track.settled() && c == g.cursor && below != g.below {
            // The ↑ went to a menu under the box (`@`'s file list), which takes them itself:
            // let the rest through.
            let held = g.held;
            self.guard = None;
            return held;
        }
        g.track.see(grid, c);
        if c != g.cursor {
            g.since = now;
        }
        (g.cursor, g.below) = (c, below);
        let mut n = 0;
        while g.held > 0 && !at_top(grid, g.track.aimed(grid, c)) {
            g.held -= 1;
            g.track.pressed("up");
            n += 1;
        }
        if g.held > 0 && g.track.settled() {
            // Every ↑ sent has shown and the cursor is on the first row: the rest would
            // recall history.
            g.held = 0;
        }
        n
    }

    /// ↑ are held, or on their way.
    pub fn busy(&self) -> bool {
        self.guard.as_ref().is_some_and(Guard::busy)
    }

    /// The screen hasn't shown a ↑ landing for [`WAIT`] while some are held or on their way:
    /// it may never, so give up on them.
    pub fn stale(&self, now: Instant) -> bool {
        self.guard.as_ref().is_some_and(|g| g.busy() && now.duration_since(g.since) >= WAIT)
    }
}

/// `at` is on the first row of the text in Claude's box, where ↑ recalls history. Claude keeps
/// the cursor mid-window while there's text above, so the box's top row is the text's.
fn at_top(grid: &[RowData], at: Pos) -> bool {
    !matches!(term_edit::vertical("up", true, grid, at), Some(Send::Arrow(_)))
}

/// What's on screen under the box around `cursor`: a menu there changes it.
fn below_box(grid: &[RowData], cursor: Pos) -> u64 {
    let mut h = DefaultHasher::new();
    let from = cursor.1 as usize + term_edit::rows_below(grid, cursor).unwrap_or(0) as usize + 1;
    for row in grid.iter().skip(from) {
        for c in &row.cells {
            c.ch.hash(&mut h);
        }
    }
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Cell;

    fn row(s: &str) -> RowData {
        let cells = format!("{s:<30}").chars().map(|ch| Cell { ch, fg: [0; 3], bg: [0; 3], flags: 0 }).collect();
        RowData { cells, extras: vec![], ..Default::default() }
    }

    /// Claude's box holding `lines` (all in view), a footer under it, the cursor on line `at`.
    fn screen(lines: &[&str], at: u16, footer: &str) -> (Vec<RowData>, Pos) {
        let rule = "─".repeat(30);
        let mut g = vec![row("output"), row(&rule)];
        for (i, l) in lines.iter().enumerate() {
            g.push(row(&format!("{}{l}", if i == 0 { "❯ " } else { "  " })));
        }
        g.push(row(&rule));
        g.push(row(footer));
        (g, (4, 2 + at))
    }

    const LINES: [&str; 4] = ["one", "two", "three", "four"];

    #[test]
    fn a_burst_stops_on_the_first_row() {
        let t0 = Instant::now();
        let (g, c) = screen(&LINES, 3, "? for shortcuts");
        let mut slot = None;
        // Four ↑ before any frame: three rows to go up, so the fourth waits.
        let keys: Vec<Up> = (0..4).map(|i| UpBurst::key(&mut slot, &g, c, t0 + Duration::from_millis(30 * i))).collect();
        assert_eq!(keys, [Up::Send, Up::Send, Up::Send, Up::Hold]);
        let burst = slot.as_mut().unwrap();
        // The screen catches up: the cursor is on the first row, so the held one is dropped.
        let (g, c) = screen(&LINES, 0, "? for shortcuts");
        assert_eq!(burst.frame(&g, Some(c), t0 + Duration::from_millis(200)), 0);
        assert!(!burst.busy());
        // A fresh ↑ there is Claude's, for history.
        assert_eq!(UpBurst::key(&mut slot, &g, c, t0 + Duration::from_secs(1)), Up::Pass);
    }

    #[test]
    fn held_keys_wait_for_the_keys_on_their_way() {
        let t0 = Instant::now();
        let (g, c) = screen(&LINES, 3, "");
        let mut slot = None;
        for i in 0..3 {
            UpBurst::key(&mut slot, &g, c, t0 + Duration::from_millis(30 * i));
        }
        assert_eq!(UpBurst::key(&mut slot, &g, c, t0 + Duration::from_millis(90)), Up::Hold);
        // One ↑ has shown so far: the other two are still on their way to the first row, so
        // the held one is neither sent nor dropped yet, and the next ↑ waits behind it.
        let (g, c) = screen(&LINES, 2, "");
        let burst = slot.as_mut().unwrap();
        assert_eq!(burst.frame(&g, Some(c), t0 + Duration::from_millis(120)), 0);
        assert!(burst.busy());
        assert_eq!(UpBurst::key(&mut slot, &g, c, t0 + Duration::from_millis(500)), Up::Hold, "still the same burst");
    }

    #[test]
    fn a_fresh_up_on_the_first_row_is_claudes() {
        let (g, c) = screen(&LINES, 0, "");
        let mut slot = None;
        let t0 = Instant::now();
        assert_eq!(UpBurst::key(&mut slot, &g, c, t0), Up::Pass);
        assert_eq!(UpBurst::key(&mut slot, &g, c, t0 + Duration::from_millis(30)), Up::Pass, "the rest of its burst too");
    }

    #[test]
    fn a_menu_under_the_box_takes_the_burst() {
        let t0 = Instant::now();
        let (g, c) = screen(&LINES, 1, "    + crates/");
        let mut slot = None;
        assert_eq!(UpBurst::key(&mut slot, &g, c, t0), Up::Send);
        assert_eq!(UpBurst::key(&mut slot, &g, c, t0 + Duration::from_millis(30)), Up::Hold);
        // The cursor stays put and the menu's highlight moves: the held ↑ go to the menu too.
        let (g, c) = screen(&LINES, 1, "    ◇ crates/midna-app");
        assert_eq!(slot.as_mut().unwrap().frame(&g, Some(c), t0 + Duration::from_millis(60)), 1);
        assert_eq!(UpBurst::key(&mut slot, &g, c, t0 + Duration::from_millis(90)), Up::Pass);
    }

    #[test]
    fn gives_up_when_the_screen_stays_behind() {
        let t0 = Instant::now();
        let (g, c) = screen(&LINES, 1, "");
        let mut slot = None;
        UpBurst::key(&mut slot, &g, c, t0);
        assert_eq!(UpBurst::key(&mut slot, &g, c, t0 + Duration::from_millis(30)), Up::Hold);
        let burst = slot.as_ref().unwrap();
        assert!(!burst.stale(t0 + Duration::from_millis(500)));
        assert!(burst.stale(t0 + WAIT));
    }

    #[test]
    fn a_long_input_window_is_followed() {
        // Ten rows of text in a four-row window: Claude keeps the cursor on its second row while
        // the window slides, so a row on screen isn't the text's first until the window stops.
        let text: Vec<String> = (0..10).map(|i| format!("line {i}")).collect();
        let window = |first: usize, at: u16| {
            let lines: Vec<&str> = text[first..first + 4].iter().map(String::as_str).collect();
            screen(&lines, at, "")
        };
        let t0 = Instant::now();
        let (g, c) = window(6, 1);
        let mut slot = None;
        // ↑ to row 1 of the window is known to be text; the next waits.
        assert_eq!(UpBurst::key(&mut slot, &g, c, t0), Up::Send);
        assert_eq!(UpBurst::key(&mut slot, &g, c, t0 + Duration::from_millis(30)), Up::Hold);
        // The window slid a row and the cursor stayed on its second row: room again.
        let (g, c) = window(5, 1);
        assert_eq!(slot.as_mut().unwrap().frame(&g, Some(c), t0 + Duration::from_millis(60)), 1);
    }
}
