//! macOS text-editing keys in a terminal's input line (the shell prompt, Claude Code's or
//! Codex's input box), outside full-screen apps:
//!
//! - ⌘← ⌘→ line start/end, ⌥← ⌥→ word left/right, ⌘⌫ delete to line start, ⌥⌫ delete the
//!   word left, ⌥⌦ the word right, ⌘⌦ to line end. These send the readline bytes Ghostty and
//!   Terminal.app send (ctrl-a, esc-b, ctrl-u, …), which zsh, bash, Claude Code and Codex all
//!   understand. In a shell, ⌘⌫ jumps to the line start and erases what it passed over, since
//!   zsh's ctrl-u clears the whole line.
//! - ⇧ with any of those moves (and plain ⇧← ⇧→ ⇧↑ ⇧↓) selects. The app moves its own cursor,
//!   so word and line edges are the app's and the prompt can't be selected; the selection is
//!   drawn from where the cursor started to where it is now. ⇧↑ on an input's first line (and
//!   in a shell, where ↑ is history) selects to the line start instead, ⇧↓ on its last line to
//!   the line end, as in a text field: Claude Code recalls history on ↑ from the first line.
//! - ⌘A in an agent's input box selects all its text and puts the cursor at the end (the
//!   ⇧⌘↓ moves, anchored at the text's start). Elsewhere it selects the whole screen.
//! - Typing, ⌫, ⌦ or pasting with a selection replaces it. On one row (from the keyboard, or a
//!   mouse selection such as a double-clicked word) the cursor is moved to its right edge with
//!   arrows, the selected characters are erased with ⌫, then the text is typed. Across rows
//!   the line breaks and indents aren't visible as characters, so an [`EraseJob`] erases a
//!   count that can't be too many, then one at a time until the screen shows the text that
//!   surrounded the selection meeting at the cursor. A mouse selection elsewhere in the same
//!   input (dragged across rows, say) is reached first by a [`MoveJob`], which walks the
//!   cursor to its nearer edge the same way, checking the cursor's position on screen.
//!
//! Full-screen apps (vim, less, htop) get their keys untouched.
use crate::frame::{Cell, F_SPACER, RowData};

/// (column, row) on the screen.
pub type Pos = (u16, u16);

/// Indent of an agent's continuation rows (`❯ `/`› ` on the first row, two spaces after).
pub const AGENT_INDENT: u16 = 2;

/// A keyboard selection: where the cursor was when ⇧ first moved it. The other end is wherever
/// the cursor is now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KbdSel {
    pub anchor: Pos,
    /// ⌘⌫ in a shell: erase this selection as soon as the cursor reaches the line start
    /// (zsh's ctrl-u would clear the whole line, not just what's left of the cursor).
    pub kill: bool,
}

/// What the app should be sent for a move: readline bytes, or a plain arrow key (which the
/// daemon encodes for the app's keyboard mode).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Send {
    Bytes(&'static [u8]),
    Arrow(&'static str),
}

/// A cursor move: ⌘/⌥/plain ← →, with or without ⇧ (the caller handles ⇧).
pub fn movement(key: &str, cmd: bool, alt: bool) -> Option<Send> {
    Some(match (key, cmd, alt) {
        ("left", true, false) => Send::Bytes(b"\x01"),
        ("right", true, false) => Send::Bytes(b"\x05"),
        ("left", false, true) => Send::Bytes(b"\x1bb"),
        ("right", false, true) => Send::Bytes(b"\x1bf"),
        ("left", false, false) => Send::Arrow("left"),
        ("right", false, false) => Send::Arrow("right"),
        _ => return None,
    })
}

/// ⇧↑ / ⇧↓: a line up or down inside a multi-line input, else to this line's start or end.
/// `agent`: the input is Claude Code's or Codex's box; a shell's ↑ ↓ are history.
pub fn vertical(key: &str, agent: bool, grid: &[RowData], cursor: Pos) -> Option<Send> {
    let row = |y: u16| grid.get(y as usize);
    match key {
        "up" if agent && !row(cursor.1).is_some_and(is_prompt_row) && cursor.1 > 0 && row(cursor.1 - 1).is_some_and(|r| is_input_row(r)) => Some(Send::Arrow("up")),
        "up" => Some(Send::Bytes(b"\x01")),
        "down" if agent && row(cursor.1 + 1).is_some_and(is_continuation) => Some(Send::Arrow("down")),
        "down" => Some(Send::Bytes(b"\x05")),
        _ => None,
    }
}

/// ⌘↑ / ⌘↓ in an agent's input: to the start or end of all its text. ↑ ↓ move a screen row at
/// a time in Claude Code and Codex (wrapped lines included), so the rows up to the prompt row
/// (or down to the last input row) are exact; ↑ on the first row would recall history. None
/// when the cursor isn't in a recognisable input box.
pub fn text_edge(key: &str, grid: &[RowData], cursor: Pos) -> Option<Vec<Send>> {
    let row = |y: u16| grid.get(y as usize);
    match key {
        "up" => {
            let mut y = cursor.1;
            while !row(y).is_some_and(is_prompt_row) {
                if y == 0 || !row(y).is_some_and(is_continuation) {
                    return None;
                }
                y -= 1;
            }
            let mut v = vec![Send::Arrow("up"); (cursor.1 - y) as usize];
            v.push(Send::Bytes(b"\x01"));
            Some(v)
        }
        "down" => {
            if !row(cursor.1).is_some_and(is_input_row) {
                return None;
            }
            let n = (cursor.1 + 1..).take_while(|&y| row(y).is_some_and(is_continuation)).count();
            let mut v = vec![Send::Arrow("down"); n];
            v.push(Send::Bytes(b"\x05"));
            Some(v)
        }
        _ => None,
    }
}

/// ⌘A in an agent's input: where its text starts (past the prompt glyph and its space on the
/// prompt row), and the moves that put the cursor at its end. None when the cursor isn't in a
/// recognisable input box.
pub fn select_all(grid: &[RowData], cursor: Pos) -> Option<(Pos, Vec<Send>)> {
    let row = |y: u16| grid.get(y as usize);
    let mut y = cursor.1;
    while !row(y).is_some_and(is_prompt_row) {
        if y == 0 || !row(y).is_some_and(is_continuation) {
            return None;
        }
        y -= 1;
    }
    let glyph = row(y)?.cells.iter().position(|c| matches!(c.ch, '❯' | '›' | '>'))? as u16;
    Some(((glyph + 2, y), text_edge("down", grid, cursor)?))
}

/// Which agent runs in a terminal ("claude" or "codex"), from `session.processes` (a list;
/// the CLI's `--json` wraps it as `{"processes": …}`). They may be started from a shell rather
/// than as an agent terminal.
pub fn agent_running(processes: &serde_json::Value) -> Option<&'static str> {
    let ps = processes.as_array().or_else(|| processes["processes"].as_array())?;
    ps.iter().filter(|p| p["depth"].as_u64().unwrap_or(0) > 0).find_map(|p| {
        let words = p["command"].as_str().unwrap_or("").split_whitespace().take(2);
        words.map(|w| w.rsplit('/').next().unwrap_or(w)).find_map(|w| match w {
            "claude" => Some("claude"),
            "codex" => Some("codex"),
            _ => None,
        })
    })
}

/// The line start / end moves (ctrl-a / ctrl-e) in the keys `agent` keeps on the current line.
/// Codex's ctrl-a / ctrl-e at a line's start / end go on to the line above / below, but its
/// Home / End don't; Claude Code is the other way round.
pub fn for_agent(mv: Send, agent: Option<&str>) -> Send {
    match (mv, agent) {
        (Send::Bytes(b"\x01"), Some("codex")) => Send::Arrow("home"),
        (Send::Bytes(b"\x05"), Some("codex")) => Send::Arrow("end"),
        _ => mv,
    }
}

/// An agent input's first row: the prompt glyph in the leading cells.
fn is_prompt_row(row: &RowData) -> bool {
    row.cells.iter().take(3).map(|c| c.ch).find(|c| !matches!(c, ' ' | '\0' | '\u{a0}')).is_some_and(|c| matches!(c, '❯' | '›' | '>'))
}

/// A row of an agent's input after the first: indented text (borders and blank rows aren't).
fn is_continuation(row: &RowData) -> bool {
    let lead = leading_blanks(row);
    lead >= AGENT_INDENT && text_end(row) > lead
}

fn is_input_row(row: &RowData) -> bool {
    is_prompt_row(row) || is_continuation(row)
}

/// A deleting shortcut: ⌘⌫ ⌥⌫ ⌥⌦ ⌘⌦.
pub fn deletion(key: &str, cmd: bool, alt: bool) -> Option<&'static [u8]> {
    Some(match (key, cmd, alt) {
        ("backspace", true, false) => b"\x15",
        ("backspace", false, true) => b"\x1b\x7f",
        ("delete", false, true) => b"\x1bd",
        ("delete", true, false) => b"\x0b",
        _ => return None,
    })
}

/// Characters (not cells: a wide char's spacer doesn't count) in columns `a..b` of `row`.
pub fn chars_between(row: &RowData, a: u16, b: u16) -> usize {
    let (a, b) = (a.min(b) as usize, a.max(b) as usize);
    row.cells.get(a..b.min(row.cells.len())).map_or(0, |cs| cs.iter().filter(|c| c.flags & F_SPACER == 0).count())
}

/// Columns `a..b` of `row` as text.
fn text_of(row: &RowData, a: u16, b: u16) -> String {
    let (a, b) = (a as usize, (b as usize).min(row.cells.len()));
    row.cells.get(a..b.max(a)).map_or(String::new(), |cs| cs.iter().filter(|c| c.flags & F_SPACER == 0).map(|c| if c.ch == '\0' { ' ' } else { c.ch }).collect())
}

/// Column just past the last non-blank cell of `row`.
fn text_end(row: &RowData) -> u16 {
    row.cells.iter().rposition(|c: &Cell| c.flags & F_SPACER != 0 || !matches!(c.ch, ' ' | '\0' | '\u{a0}')).map_or(0, |i| i as u16 + 1)
}

fn leading_blanks(row: &RowData) -> u16 {
    row.cells.iter().take_while(|c| c.flags & F_SPACER == 0 && matches!(c.ch, ' ' | '\0' | '\u{a0}')).count() as u16
}

/// The two ends in reading order.
pub fn ordered(a: Pos, b: Pos) -> (Pos, Pos) {
    if (a.1, a.0) <= (b.1, b.0) { (a, b) } else { (b, a) }
}

/// The cells a selection from `s` to `e` covers on each row, as (row, first col, last col)
/// for painting. Rows after the first start past the indent (at most `indent` blank cells),
/// rows before the last end at their text.
pub fn spans(grid: &[RowData], s: Pos, e: Pos, indent: u16) -> Vec<(u16, u16, u16)> {
    let mut out = vec![];
    for y in s.1..=e.1 {
        let Some(row) = grid.get(y as usize) else { break };
        let x0 = if y == s.1 { s.0 } else { leading_blanks(row).min(indent) };
        let x1 = if y == e.1 { e.0 } else { text_end(row) };
        if x1 > x0 {
            out.push((y, x0, x1 - 1));
        }
    }
    out
}

/// Characters a selection certainly holds: what [`spans`] covers, trimmed to each row's text.
/// The true count adds the line breaks and any indent that is really text, never less.
pub fn lower_bound(grid: &[RowData], s: Pos, e: Pos, indent: u16) -> usize {
    spans(grid, s, e, indent).iter().map(|&(y, x0, x1)| grid.get(y as usize).map_or(0, |r| chars_between(r, x0, (x1 + 1).min(text_end(r))))).sum()
}

/// How to erase columns `lo..hi` of `row` with the cursor at column `cursor`: arrows to put
/// the cursor at the right edge (negative = left), then this many ⌫. Blank cells past the end
/// of the line's text aren't characters, so the range is trimmed to it first.
pub fn erase_plan(row: &RowData, cursor: u16, lo: u16, hi: u16) -> Option<(i64, usize)> {
    let hi = hi.min(text_end(row)).max(lo.min(hi));
    let n = chars_between(row, lo, hi);
    if n == 0 {
        return None;
    }
    let moves = if cursor <= hi { chars_between(row, cursor, hi) as i64 } else { -(chars_between(row, hi, cursor) as i64) };
    Some((moves, n))
}

/// The selection to replace, start and end: the keyboard selection, else the mouse selection.
pub fn replace_range(kbd: Option<KbdSel>, mouse: &[(u16, u16, u16)], cursor: Pos) -> Option<(Pos, Pos)> {
    if let Some(k) = kbd.filter(|k| k.anchor != cursor) {
        return Some(ordered(k.anchor, cursor));
    }
    match (mouse.first(), mouse.last()) {
        (Some(&(y0, x0, _)), Some(&(y1, _, x1))) => Some(((x0, y0), (x1 + 1, y1))),
        _ => None,
    }
}

/// Whether rows `top..=bottom` are all one input: an agent's box (from its prompt row or a
/// continuation row on), or a shell line soft-wrapped onto the rows below its first.
pub fn one_input(grid: &[RowData], top: u16, bottom: u16, agent: bool) -> bool {
    (top..=bottom).all(|y| {
        let Some(row) = grid.get(y as usize) else { return false };
        if agent {
            is_continuation(row) || (y == top && is_prompt_row(row))
        } else {
            y == bottom || text_end(row) as usize >= row.cells.len()
        }
    })
}

/// A mouse selection's ends pulled onto the input's text, where the cursor can go: past an
/// agent's prompt glyph and its space, past a continuation row's indent, and not beyond a
/// row's text.
pub fn clamp_to_text(grid: &[RowData], s: Pos, e: Pos, indent: u16) -> (Pos, Pos) {
    let first_col = |(x, y): Pos| {
        let Some(row) = grid.get(y as usize) else { return x };
        let glyph = row.cells.iter().take(3).position(|c| matches!(c.ch, '❯' | '›' | '>'));
        let start = match glyph {
            Some(g) if indent > 0 && is_prompt_row(row) => g as u16 + 2,
            _ => leading_blanks(row).min(indent),
        };
        x.max(start).min(text_end(row).max(start))
    };
    let s = (first_col(s), s.1);
    let e = (first_col(e), e.1);
    if s.1 == e.1 { (s, e.max(s)) } else { (s, e) }
}

/// Walking the cursor to an edge of a mouse selection before erasing it, checked against the
/// screen like an [`EraseJob`]: arrows over the characters certainly in between, then one at
/// a time until the cursor is there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoveJob {
    /// "left" or "right".
    pub key: &'static str,
    pub target: Pos,
    /// The selection to erase once there.
    pub sel: (Pos, Pos),
    /// Single keys still allowed past the first batch (line breaks and indents that are text).
    pub budget: usize,
}

impl MoveJob {
    /// The move to the nearer edge of `s..e` (to its end from inside it), and the arrows to
    /// press first. None when the cursor is already at an edge.
    pub fn plan(grid: &[RowData], s: Pos, e: Pos, cursor: Pos, indent: u16) -> Option<(MoveJob, usize)> {
        let at = |p: Pos| (p.1, p.0);
        let (key, target) = if at(cursor) < at(s) {
            ("right", s)
        } else if at(cursor) < at(e) && cursor != s {
            ("right", e)
        } else if at(cursor) > at(e) {
            ("left", e)
        } else {
            return None;
        };
        let (a, b) = ordered(cursor, target);
        let budget = (b.1 - a.1) as usize * (1 + indent as usize) + 1;
        Some((MoveJob { key, target, sel: (s, e), budget }, lower_bound(grid, a, b, indent)))
    }

    /// The cursor went past the target: something didn't line up, so stop.
    pub fn passed(&self, cursor: Pos) -> bool {
        let (c, t) = ((cursor.1, cursor.0), (self.target.1, self.target.0));
        if self.key == "right" { c > t } else { c < t }
    }
}

/// What a selection being erased is waiting on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pending {
    Move(MoveJob),
    Erase(EraseJob),
}

/// Erasing a selection that spans rows, checked against the screen (see the module docs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EraseJob {
    /// "backspace" with the cursor at the selection's end, "delete" at its start.
    pub key: &'static str,
    /// Where the selection started, and the row's text before it.
    pub start_col: u16,
    pub before: String,
    /// The text after the selection's end, to the end of its row.
    pub after: String,
    /// Single keys still allowed past the first batch (one per line break, plus one).
    pub budget: usize,
}

/// How to erase a selection across rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Erase {
    /// Press `key` this many times; extra presses do nothing (⌫ at the input's start, ⌦ at its
    /// end), so no check is needed. An emptied agent input shows its placeholder, which a
    /// check couldn't tell from text.
    Blind(&'static str, usize),
    /// Press `job.key` this many times, then check (`EraseJob::done`).
    Checked(EraseJob, usize),
}

impl EraseJob {
    /// How to erase `s..e` with the cursor at one end (None when it's at neither).
    pub fn plan(grid: &[RowData], s: Pos, e: Pos, cursor: Pos, indent: u16) -> Option<Erase> {
        let key = if cursor == e {
            "backspace"
        } else if cursor == s {
            "delete"
        } else {
            return None;
        };
        let (first, last) = (grid.get(s.1 as usize)?, grid.get(e.1 as usize)?);
        let budget = (e.1 - s.1) as usize + 1;
        let n = lower_bound(grid, s, e, indent);
        let input_start = is_prompt_row(first) && text_of(first, 0, s.0).chars().filter(|c| !matches!(c, ' ' | '\u{a0}')).count() <= 1;
        let input_end = e.0 >= text_end(last) && !grid.get(e.1 as usize + 1).is_some_and(is_continuation);
        if (key == "backspace" && input_start) || (key == "delete" && input_end) {
            return Some(Erase::Blind(key, n + budget));
        }
        let job = EraseJob { key, start_col: s.0, before: text_of(first, 0, s.0), after: text_of(last, e.0, text_end(last)), budget };
        Some(Erase::Checked(job, n))
    }

    /// The selection is gone: the cursor is where it started, after the same text, and the
    /// text that followed it comes next.
    pub fn done(&self, grid: &[RowData], cursor: Pos) -> bool {
        let Some(row) = grid.get(cursor.1 as usize) else { return false };
        if cursor.0 != self.start_col || text_of(row, 0, cursor.0) != self.before {
            return false;
        }
        let end = text_end(row);
        let right = text_of(row, cursor.0, end.max(cursor.0));
        // The rest may wrap onto the next row: then a row that is full up to its edge holds the
        // start of it.
        let wraps = end as usize + 1 >= row.cells.len();
        if self.after.is_empty() {
            return right.is_empty();
        }
        right.starts_with(&self.after) || (wraps && !right.is_empty() && self.after.starts_with(&right))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::F_WIDE;

    fn row(s: &str) -> RowData {
        let mut cells = vec![];
        for ch in s.chars() {
            if ch == '世' {
                cells.push(Cell { ch, fg: [0; 3], bg: [0; 3], flags: F_WIDE });
                cells.push(Cell { ch: '\0', fg: [0; 3], bg: [0; 3], flags: F_SPACER });
            } else {
                cells.push(Cell { ch, fg: [0; 3], bg: [0; 3], flags: 0 });
            }
        }
        RowData { cells, extras: vec![] }
    }

    /// Rows padded to 30 columns, as frames are.
    fn grid(rows: &[&str]) -> Vec<RowData> {
        rows.iter().map(|r| row(&format!("{r:<30}"))).collect()
    }

    #[test]
    fn keys_map_to_readline() {
        assert_eq!(movement("left", true, false), Some(Send::Bytes(b"\x01")));
        assert_eq!(movement("right", false, true), Some(Send::Bytes(b"\x1bf")));
        assert_eq!(movement("left", false, false), Some(Send::Arrow("left")));
        assert_eq!(movement("up", true, false), None);
        assert_eq!(deletion("backspace", true, false), Some(&b"\x15"[..]));
        assert_eq!(deletion("backspace", false, true), Some(&b"\x1b\x7f"[..]));
        assert_eq!(deletion("backspace", false, false), None);
    }

    #[test]
    fn up_and_down_stay_inside_the_input() {
        let g = grid(&["────────", "❯\u{a0}one", "  two", "  three", "────────", "  status"]);
        assert_eq!(vertical("up", true, &g, (3, 1)), Some(Send::Bytes(b"\x01")), "first line: never history");
        assert_eq!(vertical("up", true, &g, (3, 2)), Some(Send::Arrow("up")));
        assert_eq!(vertical("down", true, &g, (3, 2)), Some(Send::Arrow("down")));
        assert_eq!(vertical("down", true, &g, (3, 3)), Some(Send::Bytes(b"\x05")), "last line: border below");
        let codex = grid(&["› one", "  two", "", "  GPT-6 status"]);
        assert_eq!(vertical("down", true, &codex, (3, 1)), Some(Send::Bytes(b"\x05")), "blank row ends the input");
        let sh = grid(&["$ echo hi"]);
        assert_eq!(vertical("up", false, &sh, (9, 0)), Some(Send::Bytes(b"\x01")));
        assert_eq!(vertical("down", false, &sh, (3, 0)), Some(Send::Bytes(b"\x05")));
    }

    #[test]
    fn command_up_and_down_reach_the_text_edges() {
        let g = grid(&["────────", "❯\u{a0}one", "  two", "  three", "────────"]);
        let up = text_edge("up", &g, (4, 3)).unwrap();
        assert_eq!(up, vec![Send::Arrow("up"), Send::Arrow("up"), Send::Bytes(b"\x01")]);
        assert_eq!(text_edge("up", &g, (4, 1)).unwrap(), vec![Send::Bytes(b"\x01")], "already on the first row");
        let down = text_edge("down", &g, (4, 1)).unwrap();
        assert_eq!(down, vec![Send::Arrow("down"), Send::Arrow("down"), Send::Bytes(b"\x05")]);
        assert_eq!(text_edge("up", &g, (4, 0)), None, "not in the input");
    }

    #[test]
    fn select_all_spans_the_whole_input() {
        let g = grid(&["────────", "❯\u{a0}one", "  two", "  three", "────────"]);
        let (start, moves) = select_all(&g, (3, 2)).unwrap();
        assert_eq!(start, (2, 1), "past the glyph and its space");
        assert_eq!(moves, vec![Send::Arrow("down"), Send::Bytes(b"\x05")]);
        let codex = grid(&["", " › one", "", "  GPT-6 status"]);
        assert_eq!(select_all(&codex, (4, 1)).unwrap(), ((3, 1), vec![Send::Bytes(b"\x05")]));
        assert_eq!(select_all(&g, (4, 0)), None, "not in the input");
    }

    #[test]
    fn spots_agents_started_from_a_shell() {
        let ps = |cmd: &str| {
            serde_json::json!({ "processes": [
                { "depth": 0, "command": "/bin/zsh -l" },
                { "depth": 1, "command": cmd },
            ]})
        };
        assert_eq!(agent_running(&ps("claude")), Some("claude"));
        assert_eq!(agent_running(&ps("/Users/me/.local/bin/codex -c x=1")), Some("codex"));
        assert_eq!(agent_running(&ps("node /opt/homebrew/bin/claude --resume 1")), Some("claude"));
        assert_eq!(agent_running(&ps("vim notes.md")), None);
        assert_eq!(agent_running(&serde_json::json!({ "processes": [{ "depth": 0, "command": "claude" }] })), None, "the terminal's own command is known already");
        // The RPC's own result is the bare list.
        assert_eq!(agent_running(&serde_json::json!([{ "depth": 0, "command": "zsh" }, { "depth": 1, "command": "claude" }])), Some("claude"));
        assert_eq!(for_agent(Send::Bytes(b"\x05"), Some("codex")), Send::Arrow("end"));
        assert_eq!(for_agent(Send::Bytes(b"\x05"), Some("claude")), Send::Bytes(b"\x05"));
    }

    #[test]
    fn counts_characters_not_cells() {
        let r = row("> a世b   ");
        assert_eq!(chars_between(&r, 2, 6), 3);
        assert_eq!(chars_between(&r, 6, 2), 3);
        assert_eq!(text_end(&r), 6);
    }

    #[test]
    fn plans_the_erase() {
        let r = row("> hello brave world   ");
        // "brave" is cols 8..13. Cursor at the end of the line (col 19): 6 lefts, 5 ⌫.
        assert_eq!(erase_plan(&r, 19, 8, 13), Some((-6, 5)));
        // Cursor at the left edge: 5 rights.
        assert_eq!(erase_plan(&r, 8, 8, 13), Some((5, 5)));
        // A selection running into trailing blanks stops at the text.
        assert_eq!(erase_plan(&r, 19, 14, 22), Some((0, 5)));
        assert_eq!(erase_plan(&r, 19, 19, 22), None);
    }

    #[test]
    fn picks_the_range() {
        let k = Some(KbdSel { anchor: (10, 3), kill: false });
        assert_eq!(replace_range(k, &[], (4, 3)), Some(((4, 3), (10, 3))));
        assert_eq!(replace_range(k, &[], (4, 5)), Some(((10, 3), (4, 5))), "spans rows");
        assert_eq!(replace_range(k, &[], (10, 3)), None, "empty keyboard selection");
        assert_eq!(replace_range(None, &[(3, 5, 8)], (20, 3)), Some(((5, 3), (9, 3))));
        assert_eq!(replace_range(None, &[(2, 5, 8)], (20, 3)), Some(((5, 2), (9, 2))), "not the cursor's row");
        assert_eq!(replace_range(None, &[(2, 3, 9), (3, 0, 4)], (20, 3)), Some(((3, 2), (5, 3))), "mouse selection across rows");
        assert_eq!(replace_range(None, &[], (20, 3)), None);
    }

    #[test]
    fn mouse_selections_stay_in_the_input() {
        let g = grid(&["  output", "────────", "❯ line one alpha", "  line two beta", "────────"]);
        assert!(one_input(&g, 2, 3, true));
        assert!(!one_input(&g, 0, 3, true), "starts in the output above");
        assert!(!one_input(&g, 2, 4, true), "runs into the border");
        let sh = grid(&["$ echo aaaaaaaaaaaaaaaaaaaaaaaaa", "bbb", "next"]);
        assert!(one_input(&sh, 0, 1, false), "a soft-wrapped command");
        assert!(!one_input(&sh, 1, 2, false), "the row above isn't full");
        // Dragged from the prompt glyph to the left margin of the next row.
        assert_eq!(clamp_to_text(&g, (0, 2), (0, 3), AGENT_INDENT), ((2, 2), (2, 3)));
        // Past the text on its row.
        assert_eq!(clamp_to_text(&g, (7, 2), (25, 3), AGENT_INDENT), ((7, 2), (15, 3)));
    }

    #[test]
    fn move_job_heads_for_the_nearer_edge() {
        let g = grid(&["❯ line one alpha", "  line two beta", "  line three gamma"]);
        let (s, e) = ((7, 0), (6, 1));
        // Cursor at the end of the input: left to the selection's end.
        let (job, n) = MoveJob::plan(&g, s, e, (18, 2), AGENT_INDENT).unwrap();
        assert_eq!((job.key, job.target, n), ("left", e, 9 + 16));
        assert_eq!(job.budget, 3 + 1);
        // Before it: right to its start. Inside it: right to its end.
        assert_eq!(MoveJob::plan(&g, s, e, (2, 0), AGENT_INDENT).unwrap().0.target, s);
        let (inside, n) = MoveJob::plan(&g, s, e, (10, 0), AGENT_INDENT).unwrap();
        assert_eq!((inside.key, inside.target, n), ("right", e, 6 + 4));
        assert_eq!(MoveJob::plan(&g, s, e, e, AGENT_INDENT), None, "already at an edge");
        assert!(job.passed((5, 1)) && !job.passed((7, 1)));
    }

    #[test]
    fn multi_row_spans_and_bound() {
        let g = grid(&["❯ line one alpha", "  line two beta", "  line three gamma"]);
        // From "one" (col 7, row 0) to before "gamma" (col 13, row 2).
        let sp = spans(&g, (7, 0), (13, 2), AGENT_INDENT);
        assert_eq!(sp, vec![(0, 7, 15), (1, 2, 14), (2, 2, 12)]);
        // "one alpha" 9 + "line two beta" 13 + "line three " 11; the two line breaks aren't seen.
        assert_eq!(lower_bound(&g, (7, 0), (13, 2), AGENT_INDENT), 33);
    }

    #[test]
    fn erase_job_knows_when_it_is_done() {
        let g = grid(&["❯ line one alpha", "  line two beta", "  line three gamma"]);
        let checked = |p: Option<Erase>| match p {
            Some(Erase::Checked(j, n)) => (j, n),
            other => panic!("want a checked erase, got {other:?}"),
        };
        let (job, first) = checked(EraseJob::plan(&g, (7, 0), (13, 2), (13, 2), AGENT_INDENT));
        assert_eq!((job.key, first, job.budget), ("backspace", 33, 3));
        assert_eq!((job.before.as_str(), job.after.as_str()), ("❯ line ", "gamma"));
        // After the first batch two characters are left ("e" and one line break).
        let partial = grid(&["❯ line o", "  gamma"]);
        assert!(!job.done(&partial, (8, 0)));
        let joined = grid(&["", "", "❯ line gamma"]);
        assert!(job.done(&joined, (7, 2)), "rows above may shift; position in the text is what counts");
        assert!(!job.done(&grid(&["❯ line gamm"]), (7, 0)), "deleted past the end");
        // Cursor in the middle: no job.
        assert!(EraseJob::plan(&g, (7, 0), (13, 2), (3, 1), AGENT_INDENT).is_none());
        let (fwd, _) = checked(EraseJob::plan(&g, (7, 0), (13, 2), (7, 0), AGENT_INDENT));
        assert_eq!(fwd.key, "delete");
        // Selected to the end of the last line, then a line after it: nothing may be left after
        // the cursor on its row.
        let g4 = grid(&["❯ line one alpha", "  line two beta", "  line three gamma", "  four"]);
        let (to_eol, first) = checked(EraseJob::plan(&g4, (15, 1), (18, 2), (15, 1), AGENT_INDENT));
        assert_eq!(first, 16, "\"line three gamma\"; the line break is unseen");
        assert!(!to_eol.done(&grid(&["❯ line one alpha", "  line two betaa"]), (15, 1)), "one left");
        assert!(to_eol.done(&grid(&["❯ line one alpha", "  line two beta", "  four"]), (15, 1)));
        // To the end of the whole input with ⌦, or from its start with ⌫: extra presses are
        // harmless, so they all go at once.
        assert_eq!(EraseJob::plan(&g, (15, 1), (18, 2), (15, 1), AGENT_INDENT), Some(Erase::Blind("delete", 16 + 2)));
        assert_eq!(EraseJob::plan(&g, (2, 0), (6, 1), (6, 1), AGENT_INDENT), Some(Erase::Blind("backspace", 14 + 4 + 2)));
    }
}
