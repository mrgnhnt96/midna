//! macOS text-editing keys in a terminal's input line (the shell prompt, Claude Code's or
//! Codex's input box), outside full-screen apps:
//!
//! - ⌘← ⌘→ line start/end, ⌥← ⌥→ word left/right, ⌘⌫ delete to line start, ⌥⌫ delete the
//!   word left, ⌥⌦ the word right, ⌘⌦ to line end. These send the readline bytes Ghostty and
//!   Terminal.app send (ctrl-a, esc-b, ctrl-u, …), which zsh, bash, Claude Code and Codex all
//!   understand. In a shell, ⌘⌫ jumps to the line start and erases what it passed over, since
//!   zsh's ctrl-u clears the whole line. On a wrapped row of Claude Code's input, ⌘← ⌘→ stop
//!   at the screen row's edge instead, as in a text field.
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
//! - A long input in Claude Code shows only a window on its text, with `❯` on the window's top
//!   row whether or not that's where the text starts, so the screen alone can't say how much
//!   there is. ⌘A there remembers it meant all of it: ⌘C and ⌘X read the rest with a
//!   [`ReadJob`], and ⌫ or typing erases until the box is empty ([`Pending::Clear`]). Claude
//!   can take seconds to redraw and can be caught halfway through one, so these wait out a
//!   screen that doesn't fit, and say so when they give up.
//! - A ⇧ selection there runs past the window, so its ends can't be screen positions: the text
//!   scrolls under them. A [`LongSel`] keeps the rows the window has shown and both ends as rows
//!   of those. ⌘C and ⌘X copy from them, and ⌫ or typing erases the characters they count.
//! - Codex's long input is a window too, but it keeps the cursor at the window's edge and
//!   scrolls one row per ↑ or ↓, and on a recalled prompt ↑ or ↓ at the very start or end of
//!   the text swap in another history entry. It does say what's hidden: `↑` ends the window's
//!   top row while there are rows above, `↓` its last row while there are rows below. So the
//!   walk to the end uses ctrl-e, which never touches history, and a [`ReadJob`] climbs one ↑
//!   at a time for as long as the top row says there's more.
//!
//! Full-screen apps (vim, less, htop) get their keys untouched.
use crate::frame::{Cell, F_INPUT, F_SPACER, RowData};

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
    /// ⌘A in a box at its tallest: the selection is all the text, rows out of view included.
    pub all: bool,
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
    let rows = if agent { input_rows(grid, cursor) } else { None };
    match key {
        "up" if rows.is_some_and(|(top, _)| cursor.1 > top) => Some(Send::Arrow("up")),
        "up" => Some(Send::Bytes(b"\x01")),
        "down" if rows.is_some_and(|(_, bottom)| cursor.1 < bottom) => Some(Send::Arrow("down")),
        "down" => Some(Send::Bytes(b"\x05")),
        _ => None,
    }
}

/// ⌘↑ / ⌘↓ in an agent's input: to the start or end of all its text. ↑ ↓ move a screen row at
/// a time in Claude Code and Codex (wrapped lines and blank lines included), so the rows up to
/// the prompt row (or down to the last input row) are exact; ↑ on the first row would recall
/// history. None when the cursor isn't in a recognisable input box.
pub fn text_edge(key: &str, grid: &[RowData], cursor: Pos) -> Option<Vec<Send>> {
    let (top, bottom) = input_rows(grid, cursor)?;
    let (n, arrow, edge) = match key {
        "up" => (cursor.1 - top, "up", b"\x01"),
        "down" => (bottom - cursor.1, "down", b"\x05"),
        _ => return None,
    };
    let mut v = vec![Send::Arrow(arrow); n as usize];
    v.push(Send::Bytes(edge));
    Some(v)
}

/// ⌘A in an agent's input: where its text starts (past the prompt glyph and its space on the
/// prompt row), and the moves that put the cursor at its end. None when the cursor isn't in a
/// recognisable input box.
pub fn select_all(grid: &[RowData], cursor: Pos) -> Option<(Pos, Vec<Send>)> {
    let (top, _) = input_rows(grid, cursor)?;
    let glyph = grid.get(top as usize)?.cells.iter().position(|c| matches!(c.ch, '❯' | '›' | '>'))? as u16;
    Some(((glyph + 2, top), text_edge("down", grid, cursor)?))
}

/// Whether the agent's input box around the cursor may be a window on longer text. Codex
/// marks one. Claude Code doesn't, so its box counts once it's as tall as it gets: half the
/// terminal less five rows.
pub fn box_is_full(grid: &[RowData], cursor: Pos) -> bool {
    let Some((top, bottom)) = claude_box(grid, cursor) else { return false };
    if grid.get(top as usize).is_some_and(is_codex_prompt) {
        return grid.get(top as usize).is_some_and(hides_rows_above) || grid.get(bottom as usize).is_some_and(hides_rows_below);
    }
    (bottom - top + 1) as usize >= (grid.len() / 2).saturating_sub(5)
}

/// How many ↓ reach the last row on screen of the input around the cursor (none on it). Claude
/// keeps the cursor mid-window while there's text beyond it, so being on the window's last row
/// means being on the text's. Never press more than this: ↓ past the last row goes to the next
/// history entry, and on a recalled prompt that replaces the text.
pub fn rows_below(grid: &[RowData], cursor: Pos) -> Option<u16> {
    let (_, bottom) = claude_box(grid, cursor)?;
    Some(bottom.saturating_sub(cursor.1))
}

/// The ends on screen of the text in Claude's input around the cursor.
pub fn box_text_span(grid: &[RowData], cursor: Pos) -> Option<(Pos, Pos)> {
    let (start, _) = select_all(grid, cursor)?;
    let (_, bottom) = claude_box(grid, cursor)?;
    Some((start, (text_end(grid.get(bottom as usize)?), bottom)))
}

/// The text of the cursor's row of Claude's input, to tell whether a move has shown yet.
pub fn box_row_at(grid: &[RowData], cursor: Pos) -> String {
    claude_box(grid, cursor).map_or(String::new(), |(top, _)| box_row(grid, top, cursor.1))
}

/// The prompt row and last row of an agent's input around the cursor. Inside Claude Code's
/// box (closed by a rule below) and Codex's (see [`above_footer`]) blank rows are empty lines
/// of the text; anywhere else a blank row ends the input.
fn input_rows(grid: &[RowData], cursor: Pos) -> Option<(u16, u16)> {
    let row = |y: u16| grid.get(y as usize);
    if let Some((top, bottom)) = claude_box(grid, cursor)
        && (row(bottom + 1).is_some_and(is_rule) || row(top).is_some_and(is_codex_prompt))
    {
        return Some((top, bottom));
    }
    let mut top = cursor.1;
    while !row(top).is_some_and(is_prompt_row) {
        if top == 0 || !row(top).is_some_and(is_continuation) {
            return None;
        }
        top -= 1;
    }
    if !row(cursor.1).is_some_and(is_input_row) {
        return None;
    }
    let bottom = cursor.1 + (cursor.1 + 1..).take_while(|&y| row(y).is_some_and(is_continuation)).count() as u16;
    Some((top, bottom))
}

/// Whether the cursor is in an agent's input box, placeholder or not.
pub fn in_input(grid: &[RowData], cursor: Pos) -> bool {
    input_rows(grid, cursor).is_some()
}

fn is_rule(row: &RowData) -> bool {
    text_of(row, 0, text_end(row)).trim_start().starts_with("──")
}

/// The last row above what Codex draws under its box: a blank row, then a status row and a
/// hint row, either of which can be blank and both of which look like rows of text. None when
/// the screen doesn't end that way.
fn above_footer(grid: &[RowData]) -> Option<u16> {
    let blank = |r: &&RowData| text_end(r) == 0;
    let from_end = grid.iter().rev().skip_while(blank).position(|r| text_end(r) == 0)?;
    let trailing = grid.iter().rev().take_while(blank).count();
    (grid.len() - trailing - from_end).checked_sub(2).map(|y| y as u16)
}

/// An agent's input box around the cursor: its prompt row and last row. Rows run from the
/// prompt row down to Claude Code's bottom rule or the blank row over Codex's footer; blank
/// rows are empty lines. None when the cursor isn't in a box.
fn claude_box(grid: &[RowData], cursor: Pos) -> Option<(u16, u16)> {
    let row = |y: u16| grid.get(y as usize);
    let inside = |r: &RowData| !is_rule(r) && (is_continuation(r) || text_end(r) == 0);
    let mut top = cursor.1;
    while !row(top).is_some_and(is_prompt_row) {
        if top == 0 || !row(top).is_some_and(inside) {
            return None;
        }
        top -= 1;
    }
    let mut bottom = top;
    while row(bottom + 1).is_some_and(inside) {
        bottom += 1;
    }
    if !row(bottom + 1).is_some_and(is_rule) {
        if row(top).is_some_and(is_codex_prompt) {
            bottom = above_footer(grid).filter(|&y| y >= top).map_or(bottom, |y| y.min(bottom));
            if cursor.1 > bottom {
                return None;
            }
        }
        // Trailing blank rows aren't text.
        while bottom > top.max(cursor.1) && row(bottom).is_some_and(|r| text_end(r) == 0 && !hides_rows_below(r)) {
            bottom -= 1;
        }
    }
    Some((top, bottom))
}

/// The text in an agent's input box around the cursor, rebuilt from the screen (for ⇧⌘Z,
/// which types it back after ⌘Z). Agents wrap at words ([`Wrap`]) and drop the space
/// they wrapped at, so a row ends in a wrap when the next row's first word wouldn't have fit
/// after it, and in a line break when it would. Some("") for the dim placeholder; None when
/// the cursor isn't in a box.
pub fn input_text(grid: &[RowData], cursor: Pos) -> Option<String> {
    let (top, bottom) = claude_box(grid, cursor)?;
    let first = grid.get(top as usize)?;
    let glyph = first.cells.iter().position(|c| matches!(c.ch, '❯' | '›' | '>'))? as u16;
    if placeholder(first, glyph + 2) {
        return Some(String::new());
    }
    let lines: Vec<String> = (top..=bottom).map(|y| box_row(grid, top, y)).collect();
    Some(join_rows(&lines, Wrap::of(first)))
}

/// How an agent wraps its input: at `cols - 4` in Claude Code and `cols - 3` in Codex. Codex also
/// breaks after a `-` or `/` inside a word, dropping nothing there.
#[derive(Clone, Copy)]
struct Wrap {
    width: usize,
    codex: bool,
}

impl Wrap {
    /// From the box's prompt row.
    fn of(first: &RowData) -> Wrap {
        let codex = is_codex_prompt(first);
        Wrap { width: first.cells.len().saturating_sub(if codex { 3 } else { 4 }), codex }
    }

    /// The first thing on `row` that couldn't be parted from itself, in cells.
    fn first_piece(self, row: &str) -> usize {
        let word = row.chars().take_while(|c| *c != ' ');
        if !self.codex {
            return word.count();
        }
        let mut n = 0;
        for c in word {
            n += 1;
            if matches!(c, '-' | '/') {
                break;
            }
        }
        n
    }
}

/// The rows of an agent's input as its text.
fn join_rows(rows: &[String], wrap: Wrap) -> String {
    let mut text = rows.first().cloned().unwrap_or_default();
    for pair in rows.windows(2) {
        text.push_str(row_break(&pair[0], &pair[1], wrap));
        text.push_str(&pair[1]);
    }
    text
}

/// Whether the prompt row `first`, its text starting at column `start`, shows the dim
/// placeholder ("Try …"). The cursor's cell on it is drawn plainly.
fn placeholder(first: &RowData, start: u16) -> bool {
    let shown: Vec<&Cell> = first.cells.get(start as usize + 1..text_end(first) as usize).unwrap_or_default().iter().filter(|c| !matches!(c.ch, ' ' | '\0')).collect();
    !shown.is_empty() && shown.iter().all(|c| is_dim(c))
}

/// The text in an agent's input box and the cursor's byte offset in it, for markdown lists
/// (`list_edit`). Rows are joined as [`input_text`] joins them. Spaces between the row's text
/// and the cursor count (the agent has typed them; the screen shows none). None when the cursor
/// isn't in a box or the box shows its placeholder.
pub fn input_cursor(grid: &[RowData], cursor: Pos) -> Option<(String, usize)> {
    let (top, bottom) = input_rows(grid, cursor)?;
    let first = grid.get(top as usize)?;
    let glyph = first.cells.iter().position(|c| matches!(c.ch, '❯' | '›' | '>'))?;
    if placeholder(first, glyph as u16 + 2) {
        return None;
    }
    let wrap = Wrap::of(first);
    let mut text = String::new();
    let mut at = 0;
    let mut prev: Option<String> = None;
    for y in top..=bottom {
        let mut line = box_row(grid, top, y);
        let whole = line.clone();
        if let Some(p) = &prev {
            text.push_str(row_break(p, &line, wrap));
        }
        if y == cursor.1 {
            let r = grid.get(y as usize)?;
            let start = if y == top { glyph as u16 + 2 } else { AGENT_INDENT };
            let n = chars_between(r, start, cursor.0.max(start));
            let shown = line.chars().count();
            line.extend(std::iter::repeat_n(' ', n.saturating_sub(shown)));
            at = text.len() + line.char_indices().nth(n).map_or(line.len(), |(i, _)| i);
        }
        text.push_str(&line);
        prev = Some(whole);
    }
    Some((text, at))
}

/// Row `y` of the text in an agent's box whose prompt row is `top`: past the prompt glyph
/// and its space on the prompt row, past the indent below it.
fn box_row(grid: &[RowData], top: u16, y: u16) -> String {
    let Some(r) = grid.get(y as usize) else { return String::new() };
    let start = if y == top { r.cells.iter().position(|c| matches!(c.ch, '❯' | '›' | '>')).map_or(AGENT_INDENT, |g| g as u16 + 2) } else { AGENT_INDENT };
    text_of(r, start, text_end(r).max(start))
}

/// A row of Claude's input as text, past the indent `❯` also sits in.
fn box_text(row: &RowData) -> String {
    text_of(row, AGENT_INDENT, text_end(row).max(AGENT_INDENT))
}

/// Claude's box around the cursor: its top row on screen, and its rows with `❯` blanked.
fn box_window(grid: &[RowData], cursor: Pos) -> Option<(u16, Vec<RowData>)> {
    let (top, bottom) = claude_box(grid, cursor)?;
    let mut rows = grid.get(top as usize..=bottom as usize)?.to_vec();
    for cell in rows[0].cells.iter_mut().take(AGENT_INDENT as usize) {
        cell.ch = ' ';
    }
    Some((top, rows))
}

/// ⌘← / ⌘→ in Claude Code's input on a row that wraps: arrows to that screen row's start or
/// end, as in a text field (Claude's ctrl-a / ctrl-e go to the ends of the whole wrapped line).
/// None where the row's edge is the line's own, so ctrl-a / ctrl-e do (and reach any trailing
/// spaces), or when the cursor isn't in a box.
pub fn row_edge(key: &str, grid: &[RowData], cursor: Pos) -> Option<Vec<Send>> {
    let (top, bottom) = claude_box(grid, cursor)?;
    let (y, row) = (cursor.1, grid.get(cursor.1 as usize)?);
    let wrap = Wrap::of(grid.get(top as usize)?);
    let joined = |a: u16| row_break(&box_row(grid, top, a), &box_row(grid, top, a + 1), wrap) != "\n";
    let n = match key {
        "left" if y > top && joined(y - 1) => chars_between(row, AGENT_INDENT, cursor.0.max(AGENT_INDENT)),
        "right" if y < bottom && joined(y) => chars_between(row, cursor.0.min(text_end(row)), text_end(row)),
        _ => return None,
    };
    Some(vec![Send::Arrow(if key == "left" { "left" } else { "right" }); n])
}

/// What joins two rows of an agent's input text: nothing, the space it wrapped at, or a line
/// break (see [`input_text`]). A full row in Claude was cut mid-word (a path, a URL), and in
/// Codex only one that's a single word was: words that fill a row exactly lose the space after
/// them.
fn row_break(prev: &str, next: &str, wrap: Wrap) -> &'static str {
    let split_word = wrap.codex && prev.ends_with(['-', '/']);
    let cut = !wrap.codex || !prev.contains(' ');
    let (prev, piece) = (prev.chars().count(), wrap.first_piece(next));
    if split_word && piece > 0 && prev + piece > wrap.width {
        ""
    } else if prev >= wrap.width {
        if cut { "" } else { " " }
    } else if !split_word && piece > 0 && prev > 0 && prev + 1 + piece > wrap.width {
        " "
    } else {
        "\n"
    }
}

/// The text a keyboard selection from `s` to `e` (in reading order) holds, for ⌘C and ⌘X.
/// In an agent's box rows are joined as [`input_text`] joins them; a shell's rows are one
/// soft-wrapped line.
pub fn selected_text(grid: &[RowData], s: Pos, e: Pos, agent: bool) -> String {
    let indent = if agent { AGENT_INDENT } else { 0 };
    let prompt = claude_box(grid, s).and_then(|(top, _)| grid.get(top as usize));
    let row = prompt.or(grid.get(s.1 as usize));
    let wrap = row.map_or(Wrap { width: 0, codex: false }, Wrap::of);
    let mut text = String::new();
    let mut prev: Option<String> = None;
    for y in s.1..=e.1 {
        let Some(row) = grid.get(y as usize) else { break };
        let x0 = if y == s.1 { s.0 } else { leading_blanks(row).min(indent) };
        let x1 = if y == e.1 { e.0.min(text_end(row)) } else { text_end(row) };
        let whole = text_of(row, indent, text_end(row).max(indent));
        if let (Some(p), true) = (&prev, agent) {
            text.push_str(row_break(p, &whole, wrap));
        }
        text.push_str(&text_of(row, x0, x1));
        prev = Some(whole);
    }
    text
}

/// Whether the cursor is past the last character of its row.
pub fn at_row_end(grid: &[RowData], cursor: Pos) -> bool {
    grid.get(cursor.1 as usize).is_some_and(|r| cursor.0 >= text_end(r))
}

/// The moves to the end of Claude Code's input from the cursor: down to its last row, then
/// to that row's end (↓ moves a screen row; on the last row it would recall history).
pub fn input_end(grid: &[RowData], cursor: Pos) -> Option<Vec<Send>> {
    let (_, bottom) = claude_box(grid, cursor)?;
    let mut v = vec![Send::Arrow("down"); bottom.saturating_sub(cursor.1) as usize];
    v.push(Send::Bytes(b"\x05"));
    Some(v)
}

/// Dim text: faint, or grey (Claude draws its placeholder in a grey).
fn is_dim(c: &Cell) -> bool {
    let [r, g, b] = c.fg;
    let (lo, hi) = (r.min(g).min(b), r.max(g).max(b));
    c.flags & crate::frame::F_FAINT != 0 || (hi - lo <= 16 && (70..=185).contains(&hi))
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

/// The first thing drawn in a row's leading cells.
fn lead_glyph(row: &RowData) -> Option<char> {
    row.cells.iter().take(3).map(|c| c.ch).find(|c| !matches!(c, ' ' | '\0' | '\u{a0}'))
}

/// An agent input's first row: the prompt glyph in the leading cells.
fn is_prompt_row(row: &RowData) -> bool {
    lead_glyph(row).is_some_and(|c| matches!(c, '❯' | '›' | '>'))
}

/// The first row of Codex's input, whose glyph is its own.
fn is_codex_prompt(row: &RowData) -> bool {
    lead_glyph(row) == Some('›')
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

/// Whether Codex marks `row`, the top row of its window, as having rows above it. Its text
/// never reaches the last column, so what's there is only ever a mark.
fn hides_rows_above(row: &RowData) -> bool {
    row.cells.last().is_some_and(|c| c.ch == '↑')
}

/// Whether Codex marks `row`, the last row of its window, as having rows below it.
fn hides_rows_below(row: &RowData) -> bool {
    row.cells.last().is_some_and(|c| c.ch == '↓')
}

/// Column just past the last non-blank cell of `row`, leaving out a mark of Codex's.
fn text_end(row: &RowData) -> u16 {
    let marked = hides_rows_above(row) || hides_rows_below(row);
    let cells = &row.cells[..row.cells.len() - marked as usize];
    cells.iter().rposition(|c: &Cell| c.flags & F_SPACER != 0 || !matches!(c.ch, ' ' | '\0' | '\u{a0}')).map_or(0, |i| i as u16 + 1)
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
/// continuation row on), or a shell line soft-wrapped onto the rows below its first (the
/// terminal says so, or the row is full to its edge).
pub fn one_input(grid: &[RowData], top: u16, bottom: u16, agent: bool) -> bool {
    (top..=bottom).all(|y| {
        let Some(row) = grid.get(y as usize) else { return false };
        if agent {
            is_continuation(row) || (y == top && is_prompt_row(row))
        } else {
            y == bottom || row.wrapped || text_end(row) as usize >= row.cells.len()
        }
    })
}

/// A plain click in a shell's input: the arrow and how many presses move the cursor to the
/// clicked cell. With prompt marks (OSC 133) the click can be anywhere in the input, line breaks
/// included; without, only on the cursor's line (its row, or a row the line soft-wraps across).
/// Past a line's text it goes to the line's end. None when there's nothing to move.
pub fn click_move(grid: &[RowData], cursor: Pos, click: Pos) -> Option<(&'static str, usize)> {
    if let Some(input) = MarkedInput::around(grid, cursor) {
        let (from, to) = (input.offset(grid, cursor)?, input.offset(grid, click)?);
        return (from != to).then(|| if to < from { ("left", from - to) } else { ("right", to - from) });
    }
    let (a, b) = ordered(cursor, click);
    if !one_input(grid, a.1, b.1, false) {
        return None;
    }
    let row = grid.get(click.1 as usize)?;
    let end = if click.1 == cursor.1 { text_end(row).max(cursor.0) } else { text_end(row) };
    let click = (click.0.min(end), click.1);
    let (a, b) = ordered(cursor, click);
    let n: usize = (a.1..=b.1)
        .filter_map(|y| {
            let row = grid.get(y as usize)?;
            let x0 = if y == a.1 { a.0 } else { 0 };
            let x1 = if y == b.1 { b.0 } else { row.cells.len() as u16 };
            Some(chars_between(row, x0, x1))
        })
        .sum();
    let key = if (click.1, click.0) < (cursor.1, cursor.0) { "left" } else { "right" };
    (n > 0).then_some((key, n))
}

/// The rows of a shell's input the shell marked (OSC 133): from the prompt row above the cursor to
/// the last row of input below it.
struct MarkedInput {
    top: u16,
    bottom: u16,
}

impl MarkedInput {
    fn around(grid: &[RowData], cursor: Pos) -> Option<MarkedInput> {
        // Up to the prompt's row: marked (OSC 133 A), or where input follows other text (a
        // prompt redrawn without its mark, after ctrl-l). A row of other text ends it too.
        let mut top = 0;
        for y in (0..=cursor.1).rev() {
            let row = grid.get(y as usize)?;
            let input = row.cells.iter().any(is_input);
            let first = row.cells.iter().find(|c| c.flags & F_SPACER != 0 || !matches!(c.ch, ' ' | '\0' | '\u{a0}'));
            if row.prompt || (input && !first.is_some_and(is_input)) || (first.is_some() && !input && y == cursor.1) {
                top = y;
                break;
            }
            if first.is_some() && !input {
                top = y + 1;
                break;
            }
        }
        let marked = |y: u16| grid.get(y as usize).is_some_and(|r| r.prompt || r.cells.iter().any(is_input));
        if !(top..=cursor.1).any(marked) {
            return None;
        }
        // Down to the last row of input below the cursor, over blank lines between.
        let mut bottom = cursor.1;
        for y in cursor.1 + 1..grid.len() as u16 {
            let row = &grid[y as usize];
            if row.prompt || (text_end(row) > 0 && !row.cells.iter().any(is_input)) {
                break;
            }
            if row.cells.iter().any(is_input) {
                bottom = y;
            }
        }
        Some(MarkedInput { top, bottom })
    }

    /// Characters of input before `pos` (a line break is one; the prompt and anything past a
    /// line's text are none). None outside the input's rows.
    fn offset(&self, grid: &[RowData], pos: Pos) -> Option<usize> {
        if !(self.top..=self.bottom).contains(&pos.1) {
            return None;
        }
        let mut off = 0;
        let mut started = false;
        for y in self.top..=pos.1 {
            let row = grid.get(y as usize)?;
            let input = |c: &&Cell| is_input(c) && c.flags & F_SPACER == 0;
            if y == pos.1 {
                return Some(off + row.cells.iter().take(pos.0 as usize).filter(input).count());
            }
            // Rows of the prompt before the input starts (a two-line prompt) hold none of it.
            started |= row.cells.iter().any(is_input);
            if started {
                off += row.cells.iter().filter(input).count() + !row.wrapped as usize;
            }
        }
        None
    }
}

fn is_input(c: &Cell) -> bool {
    c.flags & F_INPUT != 0
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
    /// ⌘C or ⌘X waiting for the screen to show the agent's box (it may be mid-redraw), `tries`
    /// checks left.
    StartRead { cut: bool, tries: u8 },
    /// Reading all of an agent's input for ⌘C or ⌘X.
    Read(ReadJob),
    /// Walking the cursor to the end of an agent's input, `tries` checks left. In Claude's that
    /// many for the screen to show the last ↓ pressed, from where it was then; in Codex's that
    /// many batches of ctrl-e, from what the last one left on screen.
    ToEnd { from: Option<(Pos, String)>, tries: u8, then: AtEnd },
    /// Pressing ⌫ at the end of an agent's input until it's empty, `rounds` batches left. Extra
    /// presses do nothing at its start.
    Clear { rounds: u8 },
    /// Waiting for the screen to catch up with a [`LongSel`], `tries` checks left.
    Follow { tries: u8, then: WithSel },
}

/// What to do with a [`LongSel`] once the screen has caught up with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WithSel {
    Copy,
    Cut,
    Erase,
}

/// What to do once the cursor has walked to the end of an agent's input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AtEnd {
    /// Select it all (⌘A, or a ⌘C that has been read).
    Select,
    /// Erase it all (a ⌘X that has been read).
    Clear,
    /// Read it all: ⌘C or ⌘X pressed while ⌘A was still walking.
    Read { cut: bool },
    /// Nothing: a read gave up, and being at the end is all that's left to do.
    Stay,
}

/// What a step of a [`ReadJob`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Read {
    /// The screen doesn't show the last ↑ pressed yet.
    Wait,
    /// Press ↑ this many times, then look again.
    Up(u16),
    /// Press ← once, then look again.
    Left,
    /// That's the first row: here's all the text.
    Done(String),
    /// The rows no longer line up (the box changed under us): leave the text alone.
    Failed,
}

/// Reading all of Claude's input, from the end up. ↑ goes to the window's top row, which is
/// always safe (↑ past the text's first row would recall history), and Claude then scrolls so
/// the cursor is mid-window, so the row the cursor lands on is how many rows came into view.
/// The rows still in view must match what was read, which also tells a late redraw from a
/// finished one.
///
/// Codex scrolls one row per ↑ only once the cursor is on the window's top row, so from there
/// it's one ↑ at a time ([`Climb`]). The window stays where it is on screen, its height the
/// rows from the prompt row to the cursor at the text's end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadJob {
    /// The text's rows read so far, the last window's first.
    rows: Vec<String>,
    /// The height of the window.
    height: usize,
    /// Checks left for a redraw to show the last ↑ pressed.
    tries: u8,
    /// ⌘X: erase the text once read.
    pub cut: bool,
    /// The window's top row and how far along a read of Codex's input is.
    climb: Option<(u16, Climb)>,
}

/// How far along a read of Codex's input is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Climb {
    /// ← pressed at `from`, off the text's very end, where ↑ can recall history.
    Leaving { from: Pos },
    /// ↑ pressed up to the window's top row, all of them on rows in view.
    ToTop,
    /// One ↑ pressed on the top row, `still` checks ago with nothing to show for it. Only ever
    /// pressed under Codex's mark: ↑ on the text's first row is the way into history.
    Scroll { still: u8 },
}

impl ReadJob {
    /// Checks of the screen allowed between two batches of ↑, about five seconds: a busy
    /// Claude can take seconds to redraw, and ↑ pressed meanwhile are still on their way.
    const TRIES: u8 = 40;
    /// Checks of a window that shows no change before taking it for a row that repeats the
    /// whole window: Codex redraws in milliseconds, far inside these.
    const REPEATS_AFTER: u8 = 2;

    /// The job for the box around the cursor, and the keys to press first. In Codex the cursor
    /// must be at the text's end, which is the window's last row.
    pub fn start(grid: &[RowData], cursor: Pos, cut: bool) -> Option<(ReadJob, Read)> {
        let (top, bottom) = claude_box(grid, cursor)?;
        let codex = grid.get(top as usize).is_some_and(is_codex_prompt);
        let bottom = if codex { cursor.1 } else { bottom };
        let rows: Vec<String> = (top..=bottom).map(|y| box_row(grid, top, y)).collect();
        let climb = codex.then_some((top, Climb::Leaving { from: cursor }));
        let keys = if codex { Read::Left } else { Read::Up(cursor.1 - top) };
        Some((ReadJob { height: rows.len(), rows, tries: Self::TRIES, cut, climb }, keys))
    }

    /// How long after pressing keys to look at the screen, and how long the app must have been
    /// quiet by then, in milliseconds. A read of Codex looks after every row.
    pub fn quiet_ms(&self) -> u64 {
        if self.climb.is_some() { 30 } else { 250 }
    }

    /// Rows of text read so far.
    pub fn rows(&self) -> usize {
        self.rows.len()
    }

    /// Take in the screen once the keys pressed have had time to show. A screen that doesn't
    /// fit may be a redraw caught halfway, so it only uses up a try.
    pub fn step(&mut self, grid: &[RowData], cursor: Option<Pos>) -> Read {
        if let Some((top, climb)) = self.climb {
            return self.climb_step(top, climb, grid, cursor);
        }
        let Some((cursor, (top, bottom))) = cursor.and_then(|c| Some((c, claude_box(grid, c)?))) else { return self.wait() };
        let window: Vec<String> = (top..=bottom).map(|y| box_row(grid, top, y)).collect();
        let new = (cursor.1 - top) as usize;
        if window.len() != self.height || window[new..] != self.rows[..self.height - new] {
            return self.wait();
        }
        if new > 0 {
            self.rows.splice(0..0, window[..new].iter().cloned());
            self.tries = Self::TRIES;
            return Read::Up(new as u16);
        }
        Read::Done(join_rows(&self.rows, grid.get(top as usize).map_or(Wrap { width: 0, codex: false }, Wrap::of)))
    }

    /// A step of reading Codex's input (see [`Climb`]).
    fn climb_step(&mut self, top: u16, climb: Climb, grid: &[RowData], cursor: Option<Pos>) -> Read {
        let Some(c) = cursor.filter(|_| grid.get(top as usize).is_some_and(is_codex_prompt)) else { return self.wait() };
        let window: Vec<String> = (top..top + self.height as u16).map(|y| box_row(grid, top, y)).collect();
        let same = window[..] == self.rows[..self.height];
        let shifted = window[1..] == self.rows[..self.height - 1];
        let more = grid.get(top as usize).is_some_and(hides_rows_above);
        match climb {
            Climb::Leaving { from } if !same || c == from => self.wait(),
            Climb::Leaving { .. } => {
                self.climb = Some((top, Climb::ToTop));
                Read::Up(c.1 - top)
            }
            Climb::ToTop if !same || c.1 != top => self.wait(),
            Climb::ToTop => self.climb_on(top, grid),
            Climb::Scroll { .. } if c.1 != top => self.wait(),
            // A window of rows that all repeat looks the same once it has moved.
            Climb::Scroll { still } if same && more && !(shifted && still >= Self::REPEATS_AFTER) => {
                self.climb = Some((top, Climb::Scroll { still: still + 1 }));
                self.wait()
            }
            Climb::Scroll { .. } if shifted => {
                self.rows.insert(0, window[0].clone());
                self.climb_on(top, grid)
            }
            Climb::Scroll { .. } => self.wait(),
        }
    }

    /// One more ↑ while Codex says there are rows above the window, else all of it is read.
    fn climb_on(&mut self, top: u16, grid: &[RowData]) -> Read {
        let Some(first) = grid.get(top as usize) else { return self.wait() };
        if !hides_rows_above(first) {
            return Read::Done(join_rows(&self.rows, Wrap::of(first)));
        }
        self.climb = Some((top, Climb::Scroll { still: 0 }));
        self.tries = Self::TRIES;
        Read::Up(1)
    }

    fn wait(&mut self) -> Read {
        self.tries = self.tries.saturating_sub(1);
        if self.tries == 0 { Read::Failed } else { Read::Wait }
    }
}

/// A ⇧ selection in Claude's long input. The window moves with the cursor, so each frame is
/// lined up with the rows seen so far, which gives both ends as rows of the text. Only rows the
/// window has shown are kept: the selection grew to its ends through them, so that's all it holds.
#[derive(Clone, Debug)]
pub struct LongSel {
    /// The rows seen, in order, with `❯` blanked: it's on whichever row is on top of the window.
    rows: Vec<RowData>,
    height: usize,
    /// The row of `rows` on top of the window.
    first: usize,
    /// Where the selection started and where the cursor is: (column, row of `rows`).
    anchor: Pos,
    cursor: Pos,
    /// The ↑ and ↓ pressed that haven't shown yet. Rows that repeat can look the same after a
    /// move, and only these tell. They're counted apart, not as the row they'll end on, because
    /// a key pressed before the selection can land after it starts: that row would never come.
    ups: u16,
    downs: u16,
    /// The last frame lined up. One that doesn't may be a redraw caught halfway.
    fits: bool,
}

impl LongSel {
    /// Starts at the cursor, in a box that may be a window on longer text (None in any other).
    pub fn start(grid: &[RowData], cursor: Pos) -> Option<LongSel> {
        if !box_is_full(grid, cursor) {
            return None;
        }
        let (top, rows) = box_window(grid, cursor)?;
        let at = (cursor.0, cursor.1 - top);
        Some(LongSel { height: rows.len(), rows, first: 0, anchor: at, cursor: at, ups: 0, downs: 0, fits: true })
    }

    /// An arrow is on its way to Claude.
    pub fn pressed(&mut self, key: &str) {
        match key {
            "up" => self.ups += 1,
            "down" => self.downs += 1,
            _ => {}
        }
    }

    /// Take in a frame.
    pub fn see(&mut self, grid: &[RowData], cursor: Pos) {
        self.fits = self.line_up(grid, cursor).is_some();
    }

    fn line_up(&mut self, grid: &[RowData], cursor: Pos) -> Option<()> {
        let (top, window) = box_window(grid, cursor)?;
        if window.len() != self.height {
            return None;
        }
        let (at, old, height) = ((cursor.1 - top) as i64, self.first as i64, self.height as i64);
        let want = self.cursor.1 as i64 + self.ahead() - at;
        let first = (old - height + 1..old + height).filter(|&f| self.matches(&window, f)).min_by_key(|f| (f - want).abs())?;
        if first < 0 {
            let new = -first as usize;
            self.rows.splice(0..0, window[..new].iter().cloned());
            self.anchor.1 += new as u16;
            self.cursor.1 += new as u16;
        }
        self.first = first.max(0) as usize;
        let known = self.rows.len() - self.first;
        self.rows.extend(window.into_iter().skip(known));
        let row = self.first as u16 + at as u16;
        self.answered(row as i64 - self.cursor.1 as i64);
        self.cursor = (cursor.0, row);
        Some(())
    }

    /// The cursor moved this many rows: that many of the arrows pressed have shown. A move past
    /// those was a key from before the selection, and answers nothing.
    fn answered(&mut self, moved: i64) {
        let keys = if moved < 0 { &mut self.ups } else { &mut self.downs };
        *keys = keys.saturating_sub(moved.unsigned_abs().min(u16::MAX as u64) as u16);
        if self.ahead() == 0 {
            (self.ups, self.downs) = (0, 0);
        }
    }

    /// How many rows the arrows on their way will still move the cursor, down being positive.
    fn ahead(&self) -> i64 {
        self.downs as i64 - self.ups as i64
    }

    /// Whether `window` shows the rows seen, with its top row at `first`.
    fn matches(&self, window: &[RowData], first: i64) -> bool {
        window.iter().zip(first..).all(|(row, y)| usize::try_from(y).ok().and_then(|y| self.rows.get(y)).is_none_or(|seen| box_text(seen) == box_text(row)))
    }

    /// Every arrow pressed has shown, and the screen makes sense.
    pub fn settled(&self) -> bool {
        self.fits && self.ahead() == 0
    }

    /// Nothing is selected, and no arrow is on its way to change that.
    pub fn is_empty(&self) -> bool {
        self.anchor == self.cursor && self.ahead() == 0
    }

    /// Where the cursor would be on screen with every arrow pressed shown, kept in the box: the
    /// last row anyone has seen is as far as a key can go.
    pub fn aimed(&self, grid: &[RowData], cursor: Pos) -> Pos {
        let Some((top, bottom)) = claude_box(grid, cursor) else { return cursor };
        (cursor.0, (cursor.1 as i64 + self.ahead()).clamp(top as i64, bottom as i64) as u16)
    }

    /// The selection's fixed end on screen. Out of view, the window's edge on that side, so what
    /// the window shows of the selection is painted. None when the cursor isn't in a box.
    pub fn anchor_on_screen(&self, grid: &[RowData], cursor: Pos) -> Option<Pos> {
        let (top, bottom) = claude_box(grid, cursor)?;
        let y = top as i64 + self.anchor.1 as i64 - self.first as i64;
        Some(if y < top as i64 {
            (AGENT_INDENT, top)
        } else if y > bottom as i64 {
            (text_end(grid.get(bottom as usize)?).max(AGENT_INDENT), bottom)
        } else {
            (self.anchor.0, y as u16)
        })
    }

    pub fn text(&self) -> String {
        let (s, e) = ordered(self.anchor, self.cursor);
        selected_text(&self.rows, s, e, true)
    }

    /// The erase for the selection and how many times to press its key. A burst that long
    /// can leave Claude quiet with presses still to come, so counting on the screen to say when
    /// to stop could run past the selection: the count never goes over. Two row breaks can't be
    /// told from the rows, so they're left to single keys, checked one at a time: a break before
    /// a word that fills its row may be a space or a cut, and so may the end of a full row.
    /// Spaces at a line's end don't show, and aren't counted: they stay.
    pub fn erase(&self) -> (EraseJob, usize) {
        let (s, e) = ordered(self.anchor, self.cursor);
        let wrap = Wrap::of(&self.rows[0]);
        let (mut counted, mut uncounted) = (0, 0);
        for y in s.1 as usize..e.1 as usize {
            let (row, next) = (box_text(&self.rows[y]), box_text(&self.rows[y + 1]));
            match row_break(&row, &next, wrap) {
                "" => uncounted += 1,
                " " if next.chars().take_while(|c| *c != ' ').count() >= wrap.width => counted += 1,
                _ => {}
            }
        }
        let (first, last) = (&self.rows[s.1 as usize], &self.rows[e.1 as usize]);
        let job = EraseJob {
            key: if self.cursor == e { "backspace" } else { "delete" },
            start_col: s.0,
            indent: AGENT_INDENT,
            before: text_of(first, AGENT_INDENT, s.0),
            after: text_of(last, e.0, text_end(last).max(e.0)),
            budget: counted + uncounted,
        };
        (job, self.text().chars().count() - counted)
    }
}

/// Erasing a selection that spans rows, checked against the screen (see the module docs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EraseJob {
    /// "backspace" with the cursor at the selection's end, "delete" at its start.
    pub key: &'static str,
    /// Where the selection started, and the row's text before it, past `indent` cells: in
    /// Claude's window the row may gain or lose the `❯` of the window's top row.
    pub start_col: u16,
    pub indent: u16,
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
        let job = EraseJob { key, start_col: s.0, indent: 0, before: text_of(first, 0, s.0), after: text_of(last, e.0, text_end(last)), budget };
        Some(Erase::Checked(job, n))
    }

    /// The selection is gone: the cursor is where it started, after the same text, and the
    /// text that followed it comes next.
    pub fn done(&self, grid: &[RowData], cursor: Pos) -> bool {
        let Some(row) = grid.get(cursor.1 as usize) else { return false };
        if cursor.0 != self.start_col || text_of(row, self.indent, cursor.0) != self.before {
            return false;
        }
        let end = text_end(row);
        let right = text_of(row, cursor.0, end.max(cursor.0));
        // The rest may wrap onto the next row: then a row that is full up to its edge holds the
        // start of it. An agent wraps at words, short of the edge.
        let wraps = self.indent > 0 || end as usize + 1 >= row.cells.len();
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
        RowData { cells, extras: vec![], ..Default::default() }
    }

    /// Rows padded to 30 columns, as frames are.
    fn grid(rows: &[&str]) -> Vec<RowData> {
        rows.iter().map(|r| row(&format!("{r:<30}"))).collect()
    }

    /// Rows of a marked shell input: `>` ends the prompt, the rest of the row is input.
    fn marked(rows: &[&str]) -> Vec<RowData> {
        let mut g = grid(rows);
        for (y, r) in rows.iter().enumerate() {
            let from = r.find('>').map_or(0, |i| i + 1);
            g[y].prompt = r.contains('>');
            for c in g[y].cells.iter_mut().take(r.chars().count()).skip(from) {
                c.flags |= F_INPUT;
            }
        }
        g
    }

    /// Claude's box showing `window` of `text` rows, the rest of the terminal around it, and the
    /// cursor on window row `at`.
    fn claude_window(text: &[String], first: usize, height: usize, at: u16) -> (Vec<RowData>, Pos) {
        let rule = "─".repeat(30);
        let rows: Vec<String> = text[first..first + height].iter().enumerate().map(|(i, r)| format!("{}{r}", if i == 0 { "❯ " } else { "  " })).collect();
        let mut g = vec!["output"];
        g.push(&rule);
        g.extend(rows.iter().map(String::as_str));
        g.push(&rule);
        (grid(&g), (4, 2 + at))
    }

    /// Run a job over `text` the way Claude scrolls: ↑ to the window's top row recentres it.
    fn read_through(text: &[String], height: usize) -> String {
        let mut first = text.len() - height;
        let (g, c) = claude_window(text, first, height, height as u16 - 1);
        let (mut job, first_keys) = ReadJob::start(&g, c, false).unwrap();
        let Read::Up(mut ups) = first_keys else { panic!("{first_keys:?}") };
        let mut at = height as u16 - 1;
        loop {
            let line = first + at as usize - ups as usize;
            first = line.saturating_sub(height / 2);
            at = (line - first) as u16;
            let (g, c) = claude_window(text, first, height, at);
            match job.step(&g, Some(c)) {
                Read::Up(n) => ups = n,
                Read::Done(t) => return t,
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn a_read_joins_the_rows_out_of_view() {
        let text: Vec<String> = (0..37).map(|i| format!("line {i}")).collect();
        assert_eq!(read_through(&text, 10), text.join("\n"));
    }

    #[test]
    fn a_read_keeps_rows_that_repeat() {
        let mut text: Vec<String> = (0..14).map(|i| format!("line {i}")).collect();
        text.extend(std::iter::repeat_n("same".to_string(), 9));
        text.extend(["", "", "last"].map(String::from));
        assert_eq!(read_through(&text, 10), text.join("\n"));
    }

    #[test]
    fn a_read_of_a_box_with_nothing_hidden_takes_one_step() {
        let text: Vec<String> = (0..10).map(|i| format!("line {i}")).collect();
        assert_eq!(read_through(&text, 10), text.join("\n"));
    }

    #[test]
    fn a_read_waits_for_the_redraw_until_its_tries_run_out() {
        let text: Vec<String> = (0..30).map(|i| format!("line {i}")).collect();
        let (g, c) = claude_window(&text, 20, 10, 9);
        let (mut job, ups) = ReadJob::start(&g, c, true).unwrap();
        assert_eq!(ups, Read::Up(9));
        for _ in 0..ReadJob::TRIES - 1 {
            assert_eq!(job.step(&g, Some(c)), Read::Wait, "the screen hasn't moved yet");
        }
        assert_eq!(job.step(&g, Some(c)), Read::Failed, "it never moved");
    }

    #[test]
    fn a_read_rides_out_a_redraw_caught_halfway() {
        let text: Vec<String> = (0..30).map(|i| format!("line {i}")).collect();
        let (g, c) = claude_window(&text, 20, 10, 9);
        let (mut job, _) = ReadJob::start(&g, c, false).unwrap();
        assert_eq!(job.step(&g, None), Read::Wait, "no cursor");
        assert_eq!(job.step(&g, Some((0, 0))), Read::Wait, "the cursor parked off the box");
        let (shorter, at) = claude_window(&text, 20, 9, 4);
        assert_eq!(job.step(&shorter, Some(at)), Read::Wait, "the box half drawn");
        let (moved, at) = claude_window(&text, 15, 10, 5);
        assert_eq!(job.step(&moved, Some(at)), Read::Up(5), "the redraw landed");
    }

    /// Codex's input as measured on 0.163.0: the cursor sits at the window's edge and scrolls it
    /// one row at a time, ↑ keeps the column, the window's rows carry its marks, and on a
    /// recalled prompt ↑ at the text's very start or end swaps in another history entry.
    struct CodexInput {
        text: Vec<String>,
        first: usize,
        height: usize,
        y: usize,
        col: usize,
        want: usize,
        recalled: bool,
    }

    impl CodexInput {
        fn at_end(text: &[String], height: usize) -> CodexInput {
            let y = text.len() - 1;
            let col = text[y].chars().count();
            CodexInput { text: text.to_vec(), first: text.len() - height, height, y, col, want: col, recalled: true }
        }

        fn left(&mut self) {
            if self.col > 0 {
                self.col -= 1;
            } else if self.y > 0 {
                self.y -= 1;
                self.col = self.text[self.y].chars().count();
            }
            self.want = self.col;
        }

        fn up(&mut self) {
            let very_end = self.y == self.text.len() - 1 && self.col == self.text[self.y].chars().count();
            assert!(!self.recalled || !(very_end || (self.y, self.col) == (0, 0)), "↑ recalls history at row {} column {}", self.y, self.col);
            if self.y == 0 {
                self.col = 0;
                return;
            }
            self.y -= 1;
            self.first = self.first.min(self.y);
            self.col = self.want.min(self.text[self.y].chars().count());
        }

        fn screen(&self) -> (Vec<RowData>, Pos) {
            let last = self.first + self.height;
            let rows = self.text[self.first..last].iter().enumerate().map(|(i, r)| {
                let mark = match i {
                    0 if self.first > 0 => "↑",
                    i if i == self.height - 1 && last < self.text.len() => "↓",
                    _ => "",
                };
                format!("{:<29}{mark}", format!("{}{r}", if i == 0 { "› " } else { "  " }))
            });
            let mut g = vec!["output".to_string(), String::new()];
            g.extend(rows);
            g.extend(["", "  GPT-6 status", "  ? for shortcuts"].map(String::from));
            let g: Vec<&str> = g.iter().map(String::as_str).collect();
            (grid(&g), (AGENT_INDENT + self.col as u16, 2 + (self.y - self.first) as u16))
        }

        /// Run a read to its end, the screen showing each key before the next look.
        fn read(&mut self) -> Read {
            let (g, c) = self.screen();
            let (mut job, mut keys) = ReadJob::start(&g, c, false).unwrap();
            loop {
                match keys {
                    Read::Left => self.left(),
                    Read::Up(n) => (0..n).for_each(|_| self.up()),
                    Read::Wait => {}
                    done => return done,
                }
                let (g, c) = self.screen();
                keys = job.step(&g, Some(c));
            }
        }
    }

    fn numbered(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("line {i}")).collect()
    }

    #[test]
    fn a_codex_read_joins_the_rows_out_of_view() {
        let text = numbered(37);
        assert_eq!(CodexInput::at_end(&text, 12).read(), Read::Done(text.join("\n")));
    }

    #[test]
    fn a_codex_read_with_nothing_hidden_stops_at_the_first_row() {
        let text = numbered(12);
        assert_eq!(CodexInput::at_end(&text, 12).read(), Read::Done(text.join("\n")));
    }

    #[test]
    fn a_codex_read_keeps_blank_rows_and_rows_that_repeat() {
        let mut text = numbered(5);
        text.extend(["", "gap", ""].map(String::from));
        text.extend(std::iter::repeat_n("same".to_string(), 30));
        text.extend(["", "", "last"].map(String::from));
        assert_eq!(CodexInput::at_end(&text, 12).read(), Read::Done(text.join("\n")));
    }

    #[test]
    fn a_codex_read_stops_where_the_mark_does_and_presses_no_further() {
        for tail in [&["x"][..], &["ab", "x"], &["ab", ""], &["ab", "", "x"]] {
            let mut text = numbered(20);
            text.extend(tail.iter().map(|s| s.to_string()));
            let mut input = CodexInput::at_end(&text, 12);
            assert_eq!(input.read(), Read::Done(text.join("\n")), "ending {tail:?}");
            assert_eq!((input.y, input.first), (0, 0), "ending {tail:?}");
        }
    }

    #[test]
    fn a_codex_read_takes_text_that_starts_blank_or_has_a_window_of_blanks() {
        let mut text = vec!["hidden".to_string()];
        text.extend(std::iter::repeat_n(String::new(), 20));
        text.push("last".into());
        assert_eq!(CodexInput::at_end(&text, 12).read(), Read::Done(text.join("\n")));
        let mut text = vec![String::new()];
        text.extend(numbered(20));
        assert_eq!(CodexInput::at_end(&text, 12).read(), Read::Done(text.join("\n")));
    }

    #[test]
    fn a_codex_read_waits_for_each_key_to_show() {
        let text = numbered(20);
        let mut input = CodexInput::at_end(&text, 12);
        let (end, c) = input.screen();
        let (mut job, keys) = ReadJob::start(&end, c, false).unwrap();
        assert_eq!(keys, Read::Left);
        assert_eq!(job.step(&end, Some(c)), Read::Wait, "the ← hasn't shown");
        input.left();
        let (g, c) = input.screen();
        assert_eq!(job.step(&g, Some(c)), Read::Up(11));
        assert_eq!(job.step(&g, Some(c)), Read::Wait, "the ↑ haven't shown");
        (0..11).for_each(|_| input.up());
        let (g, c) = input.screen();
        assert_eq!(job.step(&g, Some(c)), Read::Up(1));
        assert_eq!(job.step(&g, Some(c)), Read::Wait, "the window hasn't moved yet");
        assert_eq!(job.step(&end, Some(c)), Read::Wait, "a stale screen");
        input.up();
        let (g, c) = input.screen();
        assert_eq!(job.step(&g, Some(c)), Read::Up(1));
        assert_eq!(job.rows(), 13);
    }

    #[test]
    fn codexs_box_ends_above_its_footer() {
        let g = grid(&["output", "", "› one", "", "  two", "  three", "", "  GPT-6 status", "  ? for shortcuts"]);
        assert_eq!(input_rows(&g, (4, 4)), Some((2, 5)), "a blank row is a line of the text");
        assert_eq!(input_rows(&g, (2, 3)), Some((2, 5)), "the cursor can be on one");
        assert_eq!(input_text(&g, (4, 4)), Some("one\n\ntwo\nthree".into()));
        assert!(!in_input(&g, (4, 7)), "the status row");
        assert!(!in_input(&g, (4, 8)), "the hint row");
        let no_hint = grid(&["› one", "  two", "", "  GPT-6 status", ""]);
        assert_eq!(input_rows(&no_hint, (4, 1)), Some((0, 1)));
        let no_status = grid(&["› one", "  two", "", "", "  ? for shortcuts"]);
        assert_eq!(input_rows(&no_status, (4, 1)), Some((0, 1)));
        assert_eq!(input_rows(&grid(&["› one", "  two"]), (4, 1)), Some((0, 1)), "nothing under it");
    }

    #[test]
    fn codexs_marks_say_when_its_box_is_a_window() {
        let text = numbered(30);
        let (g, c) = CodexInput::at_end(&text, 12).screen();
        assert!(box_is_full(&g, c), "rows above");
        let mut top = CodexInput::at_end(&text, 12);
        (top.first, top.y, top.col) = (0, 0, 2);
        let (g, c) = top.screen();
        assert!(box_is_full(&g, c), "rows below");
        let (g, c) = CodexInput::at_end(&numbered(12), 12).screen();
        assert!(!box_is_full(&g, c), "as tall, nothing hidden");
    }

    #[test]
    fn codexs_rows_join_the_way_it_broke_them() {
        let codex = Wrap { width: 20, codex: true };
        assert_eq!(row_break("aaaa bbbb cccc dddd ", "eeee", codex), " ", "a space at the wrap");
        assert_eq!(row_break("aaaa bbbb cccc ddddd", "eeee", codex), " ", "words that fill the row exactly");
        assert_eq!(row_break("xxxxxxxxxxxxxxxxxxxx", "xxxx", codex), "", "one word cut at the edge");
        assert_eq!(row_break("well00-well01-well2-", "well03-well04", codex), "", "after a hyphen in a word");
        assert_eq!(row_break("example.com/path001/", "path002/path003", codex), "", "after a slash in a word");
        assert_eq!(row_break("well-", "known", codex), "\n", "a line that ends in a hyphen");
        assert_eq!(row_break("short", "row", codex), "\n");
        assert_eq!(row_break("aaaa bbbb cccc dddd", "well-known", codex), " ", "only the word's first part has to not fit");
    }

    #[test]
    fn a_full_row_in_claude_is_still_cut_mid_word() {
        let claude = Wrap { width: 20, codex: false };
        assert_eq!(row_break("aaaa bbbb cccc ddddd", "eeee", claude), "");
        assert_eq!(row_break("well00-well01-well2-", "well03", claude), "");
    }

    #[test]
    fn codexs_marks_are_not_text() {
        let text = numbered(30);
        let mut input = CodexInput::at_end(&text, 3);
        input.first = 5;
        input.y = 6;
        let (g, _) = input.screen();
        assert_eq!(input_rows(&g, (2, 3)), Some((2, 4)));
        assert_eq!(input_text(&g, (2, 3)), Some("line 5\nline 6\nline 7".into()));
        assert!(at_row_end(&g, (8, 2)), "on the row marked ↑");
        assert!(at_row_end(&g, (8, 4)), "on the row marked ↓");
    }

    #[test]
    fn a_blank_row_marked_with_more_below_stays_in_codexs_box() {
        let mut text = numbered(5);
        text.extend([String::new(), "more".into()]);
        let mut input = CodexInput::at_end(&text, 6);
        (input.first, input.y, input.col) = (0, 0, 2);
        let (g, c) = input.screen();
        assert_eq!(input_rows(&g, c), Some((2, 7)));
    }

    #[test]
    fn reads_codexs_input_back_at_its_own_width() {
        let g = grid(&["› aaaa bbbb cccc dddd ee", "  ffff", "", "  GPT-6 status"]);
        assert_eq!(input_text(&g, (6, 1)), Some("aaaa bbbb cccc dddd ee\nffff".into()));
        let g = grid(&["› aaaa bbbb cccc dddd eeeee", "  ffff", "", "  GPT-6 status"]);
        assert_eq!(input_text(&g, (6, 1)), Some("aaaa bbbb cccc dddd eeeee ffff".into()));
    }

    #[test]
    fn a_selection_in_codex_joins_rows_as_a_read_does() {
        let g = grid(&["› aaaa bbbb cccc dddd eeeee", "  ffff", "", "  GPT-6 status"]);
        assert_eq!(selected_text(&g, (2, 0), (6, 1), true), "aaaa bbbb cccc dddd eeeee ffff");
    }

    #[test]
    fn at_row_end_counts_trailing_spaces() {
        let g = grid(&["› one"]);
        assert!(at_row_end(&g, (5, 0)));
        assert!(at_row_end(&g, (7, 0)));
        assert!(!at_row_end(&g, (4, 0)));
    }

    #[test]
    fn rows_below_stops_at_the_last_row_on_screen() {
        let text: Vec<String> = (0..30).map(|i| format!("line {i}")).collect();
        let (g, c) = claude_window(&text, 10, 10, 5);
        assert_eq!(rows_below(&g, c), Some(4));
        let (g, c) = claude_window(&text, 20, 10, 9);
        assert_eq!(rows_below(&g, c), Some(0));
    }

    #[test]
    fn a_box_is_full_at_half_the_terminal_less_five_rows() {
        let text: Vec<String> = (0..30).map(|i| format!("line {i}")).collect();
        let pad = |mut g: Vec<RowData>, rows: usize| {
            g.resize(rows, row(&" ".repeat(30)));
            g
        };
        let (g, c) = claude_window(&text, 0, 10, 9);
        assert!(box_is_full(&pad(g.clone(), 30), c), "the tallest it gets in 30 rows");
        assert!(!box_is_full(&pad(g, 50), c), "room to grow in 50 rows");
    }

    #[test]
    fn in_input_is_the_agents_box_only() {
        let rule = "─".repeat(30);
        let g = grid(&["some output", &rule, "❯ hello", "  world", &rule, "  ? for shortcuts"]);
        assert!(in_input(&g, (7, 2)), "the prompt row");
        assert!(in_input(&g, (7, 3)), "a later line of the input");
        assert!(!in_input(&g, (3, 0)), "the transcript above");
        assert!(!in_input(&g, (3, 5)), "the hints below");
        let empty = grid(&[&rule, "❯ ", &rule]);
        assert!(in_input(&empty, (2, 1)), "an empty box");
    }

    #[test]
    fn a_click_reaches_other_lines_of_a_marked_input() {
        let mut g = grid(&["$ ls", "a b"]);
        g.extend(marked(&["> hello hows it going?", "I dunnot", "", "end"]));
        // The cursor at the end of "I dunnot" (row 3).
        assert_eq!(click_move(&g, (8, 3), (4, 2)), Some(("left", 27)), "up a line: the rest of the line, its break, and this one");
        assert_eq!(click_move(&g, (8, 3), (0, 2)), Some(("left", 30)), "on the prompt: the input's start");
        assert_eq!(click_move(&g, (8, 3), (12, 4)), Some(("right", 1)), "a blank line is its line break");
        assert_eq!(click_move(&g, (8, 3), (20, 5)), Some(("right", 5)), "past the text: the end");
        assert_eq!(click_move(&g, (8, 3), (1, 1)), None, "output above the prompt");
        assert_eq!(click_move(&g, (2, 3), (9, 3)), Some(("right", 6)));
        let mut two = grid(&["~/src main"]);
        two[0].prompt = true;
        two.extend(marked(&["> one", "two"]));
        two[1].prompt = false;
        assert_eq!(click_move(&two, (3, 2), (2, 1)), Some(("left", 7)), "a two-line prompt's first row holds no input");
        let mut unmarked = grid(&["$ ls", "a b"]);
        unmarked.extend(marked(&["> one", "two"]));
        unmarked[2].prompt = false;
        assert_eq!(click_move(&unmarked, (3, 3), (3, 2)), Some(("left", 6)), "a prompt redrawn without its mark");
        assert_eq!(click_move(&unmarked, (3, 3), (1, 1)), None, "still not the output above");
        assert_eq!(click_move(&grid(&["$ ls", "a b"]), (3, 1), (0, 0)), None, "no marks: only the cursor's line");
    }

    #[test]
    fn a_click_moves_the_shell_cursor() {
        let g = grid(&["$ echo hello world", "done"]);
        assert_eq!(click_move(&g, (18, 0), (7, 0)), Some(("left", 11)));
        assert_eq!(click_move(&g, (7, 0), (12, 0)), Some(("right", 5)));
        assert_eq!(click_move(&g, (7, 0), (25, 0)), Some(("right", 11)), "past the text: to its end");
        assert_eq!(click_move(&g, (7, 0), (7, 0)), None);
        assert_eq!(click_move(&g, (7, 0), (2, 1)), None, "another line");
        let wide = grid(&["$ 世界 ok"]);
        assert_eq!(click_move(&wide, (8, 0), (2, 0)), Some(("left", 5)), "characters, not cells");
        let full = format!("$ {}", "x".repeat(28));
        let wrapped = grid(&[&full, "yz"]);
        assert_eq!(click_move(&wrapped, (2, 1), (10, 0)), Some(("left", 22)), "onto the row the line wraps from");
        assert_eq!(click_move(&wrapped, (10, 0), (1, 1)), Some(("right", 21)));
        assert_eq!(click_move(&grid(&["$ ls", "a b"]), (4, 0), (1, 1)), None, "output below isn't the line");
        let mut at_space = grid(&["$ wraps at a space", "here"]);
        assert_eq!(click_move(&at_space, (2, 1), (4, 0)), None, "a short row ends the line");
        at_space[0].wrapped = true;
        assert_eq!(click_move(&at_space, (2, 1), (4, 0)), Some(("left", 28)), "unless the terminal says it wraps");
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
        let pasted = grid(&["────────", "❯\u{a0}one", "", "  two", "", "    three", "────────"]);
        let (start, moves) = select_all(&pasted, (7, 5)).unwrap();
        assert_eq!(start, (2, 1), "blank lines don't end the input");
        assert_eq!(moves, vec![Send::Bytes(b"\x05")]);
        let (_, moves) = select_all(&pasted, (0, 2)).unwrap();
        assert_eq!(moves, vec![Send::Arrow("down"); 3].into_iter().chain([Send::Bytes(b"\x05")]).collect::<Vec<_>>(), "from a blank line");
        assert_eq!(text_edge("up", &pasted, (7, 5)).unwrap().len(), 5, "four rows up, then the line start");
        assert_eq!(vertical("up", true, &pasted, (5, 3)), Some(Send::Arrow("up")), "onto a blank line");
        assert_eq!(vertical("down", true, &pasted, (5, 3)), Some(Send::Arrow("down")));
    }

    #[test]
    fn reads_claudes_input_back() {
        // 30 columns: Claude wraps at 26.
        let rule = "─".repeat(30);
        let g = grid(&[&rule, "❯ the quick brown fox jumps", "  over the lazy dog", &rule, "  status"]);
        assert_eq!(input_text(&g, (10, 2)).as_deref(), Some("the quick brown fox jumps over the lazy dog"), "\"over\" didn't fit: a wrap");
        let g = grid(&[&rule, "❯ first line", "    indented", "", "  after blank", &rule]);
        assert_eq!(input_text(&g, (13, 4)).as_deref(), Some("first line\n  indented\n\nafter blank"));
        let g = grid(&[&rule, "❯ abcdefghijklmnopqrstuvwxyz", "  0123", &rule]);
        assert_eq!(input_text(&g, (6, 2)).as_deref(), Some("abcdefghijklmnopqrstuvwxyz0123"), "cut mid-word");
        assert_eq!(input_text(&g, (0, 0)), None, "not in the box");
        let mut empty = grid(&[&rule, "❯ Try \"edit\"", &rule]);
        for c in &mut empty[1].cells[2..12] {
            c.fg = [128, 128, 128];
        }
        assert_eq!(input_text(&empty, (2, 1)).as_deref(), Some(""), "the placeholder");
        let g = grid(&[&rule, "❯ one", "", "  two", &rule]);
        assert_eq!(row_edge("left", &g, (3, 3)), None, "after a blank line: a line of its own");
        assert_eq!(input_end(&g, (0, 2)), Some(vec![Send::Arrow("down"), Send::Bytes(b"\x05")]), "from a blank line");
    }

    #[test]
    fn finds_the_cursor_in_claudes_input() {
        let rule = "─".repeat(30);
        let g = grid(&[&rule, "❯ 1. a", "  2. b", &rule]);
        assert_eq!(input_cursor(&g, (6, 2)), Some(("1. a\n2. b".into(), 9)));
        assert_eq!(input_cursor(&g, (2, 1)), Some(("1. a\n2. b".into(), 0)));
        // "3. " typed: the space after the marker isn't drawn.
        let g = grid(&[&rule, "❯ 1. a", "  3.", &rule]);
        assert_eq!(input_cursor(&g, (5, 2)), Some(("1. a\n3. ".into(), 8)));
        let g = grid(&[&rule, "❯ the quick brown fox jumps", "  over", &rule]);
        assert_eq!(input_cursor(&g, (4, 2)), Some(("the quick brown fox jumps over".into(), 28)));
        assert_eq!(input_cursor(&g, (0, 0)), None, "not in the box");
    }

    #[test]
    fn finds_the_cursor_in_codexs_input() {
        let g = grid(&["› 1. alpha", "  2. beta", "", "  GPT-6 status"]);
        assert_eq!(input_cursor(&g, (9, 1)), Some(("1. alpha\n2. beta".into(), 16)), "the status line isn't text");
        // Codex wraps at `cols - 3`: "ffff" would have fit after this row, so it's a line break.
        let g = grid(&["› aaaa bbbb cccc dddd ee", "  ffff", "", "  GPT-6 status"]);
        assert_eq!(input_cursor(&g, (6, 1)).map(|t| t.0), Some("aaaa bbbb cccc dddd ee\nffff".into()));
        let g = grid(&["› aaaa bbbb cccc dddd eeeee", "  ffff", "", "  GPT-6 status"]);
        assert_eq!(input_cursor(&g, (6, 1)).map(|t| t.0), Some("aaaa bbbb cccc dddd eeeee ffff".into()));
    }

    #[test]
    fn line_keys_stop_at_a_wrapped_rows_edge() {
        let rule = "─".repeat(30);
        let g = grid(&[&rule, "❯ the quick brown fox jumps", "  over the lazy dog", "  next line", &rule]);
        assert_eq!(row_edge("right", &g, (6, 1)), Some(vec![Send::Arrow("right"); 21]), "to the end of \"jumps\"");
        assert_eq!(row_edge("right", &g, (27, 1)), Some(vec![]), "already there");
        assert_eq!(row_edge("left", &g, (7, 2)), Some(vec![Send::Arrow("left"); 5]), "to the start of \"over\"");
        assert_eq!(row_edge("left", &g, (6, 1)), None, "the prompt row starts the line");
        assert_eq!(row_edge("right", &g, (7, 2)), None, "a line break ends the row");
        assert_eq!(row_edge("left", &g, (4, 3)), None);
        assert_eq!(row_edge("right", &g, (4, 3)), None, "the last row");
        assert_eq!(row_edge("right", &g, (0, 0)), None, "not in the box");
        let g = grid(&[&rule, "❯ abcdefghijklmnopqrstuvwxyz", "  0123", &rule]);
        assert_eq!(row_edge("right", &g, (2, 1)), Some(vec![Send::Arrow("right"); 26]), "cut mid-word");
    }

    #[test]
    fn reads_a_keyboard_selection() {
        let rule = "─".repeat(30);
        let g = grid(&[&rule, "❯ the quick brown fox jumps", "  over the lazy dog", &rule]);
        assert_eq!(selected_text(&g, (6, 1), (11, 1), true), "quick");
        assert_eq!(selected_text(&g, (22, 1), (10, 2), true), "jumps over the", "a wrap is a space");
        assert_eq!(selected_text(&g, (2, 1), (19, 2), true), "the quick brown fox jumps over the lazy dog", "⌘A");
        let g = grid(&[&rule, "❯ first line", "    indented", "", "  after blank", &rule]);
        assert_eq!(selected_text(&g, (8, 1), (7, 4), true), "line\n  indented\n\nafter");
        let g = grid(&["$ echo abcdefghij", "klmnop"]);
        assert_eq!(selected_text(&g, (7, 0), (3, 1), false), "abcdefghijklm", "a shell's wrapped line");
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
        let k = Some(KbdSel { anchor: (10, 3), kill: false, all: false });
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

    /// Claude's window with the cursor on `line`, by the rule in docs/DECISIONS.md: centred on
    /// the cursor's line, short of the text's ends.
    fn on_line(text: &[String], height: usize, line: usize, col: u16) -> (Vec<RowData>, Pos) {
        let first = line.saturating_sub(height / 2).min(text.len() - height);
        let (g, c) = claude_window(text, first, height, (line - first) as u16);
        (g, (col, c.1))
    }

    /// A selection from `line`, then `n` presses of `key` with a frame after each.
    fn select(text: &[String], height: usize, line: usize, col: u16, key: &str, n: usize) -> (LongSel, usize) {
        let (g, c) = on_line(text, height, line, col);
        let mut sel = LongSel::start(&g, c).unwrap();
        let mut at = line;
        for _ in 0..n {
            sel.pressed(key);
            at = if key == "up" { at - 1 } else { at + 1 };
            let (g, c) = on_line(text, height, at, col);
            sel.see(&g, c);
        }
        (sel, at)
    }

    fn lines(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("line {i:02}")).collect()
    }

    #[test]
    fn a_selection_up_keeps_the_rows_that_scroll_away() {
        let text = lines(40);
        let (sel, _) = select(&text, 10, 39, 9, "up", 25);
        assert!(sel.settled());
        assert_eq!(sel.text(), format!("{}\n{}", &text[14][7..], text[15..=38].join("\n")) + "\nline 39");
    }

    #[test]
    fn a_selection_down_keeps_the_rows_that_scroll_away() {
        let text = lines(40);
        let (sel, _) = select(&text, 10, 3, 2, "down", 30);
        assert!(sel.settled());
        assert_eq!(sel.text(), text[3..33].join("\n") + "\n");
    }

    #[test]
    fn a_selection_can_turn_back_past_where_it_started() {
        let text = lines(40);
        let (mut sel, mut at) = select(&text, 10, 20, 2, "up", 12);
        for _ in 0..17 {
            sel.pressed("down");
            at += 1;
            let (g, c) = on_line(&text, 10, at, 2);
            sel.see(&g, c);
        }
        assert!(sel.settled());
        assert_eq!(sel.text(), text[20..25].join("\n") + "\n");
    }

    #[test]
    fn a_selection_back_at_its_start_is_empty() {
        let text = lines(40);
        let (mut sel, at) = select(&text, 10, 20, 2, "up", 3);
        assert!(!sel.is_empty());
        for i in 1..=3 {
            sel.pressed("down");
            let (g, c) = on_line(&text, 10, at + i, 2);
            sel.see(&g, c);
        }
        assert!(sel.is_empty());
    }

    #[test]
    fn an_arrow_on_its_way_counts_as_a_selection() {
        let text = lines(40);
        let (g, c) = on_line(&text, 10, 20, 2);
        let mut sel = LongSel::start(&g, c).unwrap();
        sel.pressed("up");
        sel.see(&g, c);
        assert!(!sel.is_empty());
        assert!(!sel.settled());
    }

    #[test]
    fn rows_that_repeat_are_told_apart_by_the_arrows_pressed() {
        // The window looks the same after each press, so no frame comes at all.
        let text = vec!["same".to_string(); 40];
        let (g, c) = on_line(&text, 10, 20, 2);
        let mut sel = LongSel::start(&g, c).unwrap();
        for _ in 0..3 {
            sel.pressed("up");
        }
        sel.see(&g, c);
        assert!(sel.settled());
        assert_eq!(sel.text(), "same\n".repeat(3));
    }

    #[test]
    fn a_frame_from_before_the_arrows_isnt_settled() {
        let text = lines(40);
        let (g, c) = on_line(&text, 10, 20, 2);
        let mut sel = LongSel::start(&g, c).unwrap();
        sel.pressed("up");
        sel.see(&g, c);
        assert!(!sel.settled());
        let (g, c) = on_line(&text, 10, 19, 2);
        sel.see(&g, c);
        assert!(sel.settled());
        assert_eq!(sel.text(), "line 19\n");
    }

    #[test]
    fn a_key_from_before_the_selection_can_land_after_it_starts() {
        let text = lines(40);
        let (g, c) = on_line(&text, 10, 20, 2);
        let mut sel = LongSel::start(&g, c).unwrap();
        sel.pressed("down");
        let (g, c) = on_line(&text, 10, 19, 2);
        sel.see(&g, c);
        assert!(!sel.settled());
        let (g, c) = on_line(&text, 10, 20, 2);
        sel.see(&g, c);
        assert!(sel.settled());
        assert!(sel.is_empty());
        sel.pressed("down");
        let (g, c) = on_line(&text, 10, 21, 2);
        sel.see(&g, c);
        assert!(sel.settled());
        assert_eq!(sel.text(), "line 20\n");
    }

    #[test]
    fn a_frame_caught_halfway_through_a_redraw_changes_nothing() {
        let text = lines(40);
        let (mut sel, at) = select(&text, 10, 20, 2, "up", 8);
        let before = sel.text();
        sel.pressed("up");
        let mut torn = text.clone();
        torn[at] = "line 99".into();
        let (g, c) = on_line(&torn, 10, at - 1, 2);
        sel.see(&g, c);
        assert!(!sel.settled());
        assert_eq!(sel.text(), before);
        let (g, c) = on_line(&text, 10, at - 1, 2);
        sel.see(&g, c);
        assert!(sel.settled());
        assert_eq!(sel.text(), format!("{}\n{before}", text[at - 1]));
    }

    #[test]
    fn a_window_of_another_height_doesnt_fit() {
        let text = lines(40);
        let (mut sel, _) = select(&text, 10, 20, 2, "up", 2);
        let (g, c) = on_line(&text, 12, 18, 2);
        sel.see(&g, c);
        assert!(!sel.settled());
    }

    #[test]
    fn keys_never_run_past_the_rows_on_screen() {
        let text = lines(40);
        let (g, c) = on_line(&text, 10, 20, 2);
        let mut sel = LongSel::start(&g, c).unwrap();
        for _ in 0..20 {
            sel.pressed("up");
        }
        let aimed = sel.aimed(&g, c);
        assert_eq!(aimed.1, 2, "the box's top row");
        assert_eq!(vertical("up", true, &g, aimed), Some(Send::Bytes(b"\x01")));
    }

    #[test]
    fn the_selection_paints_from_the_edge_of_the_window() {
        let text = lines(40);
        let (sel, at) = select(&text, 10, 39, 9, "up", 25);
        let (g, c) = on_line(&text, 10, at, 9);
        assert_eq!(sel.anchor_on_screen(&g, c), Some((9, 11)), "below the window: its last row's end");
        let (sel, at) = select(&text, 10, 3, 5, "down", 25);
        let (g, c) = on_line(&text, 10, at, 5);
        assert_eq!(sel.anchor_on_screen(&g, c), Some((AGENT_INDENT, 2)), "above: its first row's start");
        let (sel, at) = select(&text, 10, 20, 5, "up", 2);
        let (g, c) = on_line(&text, 10, at, 5);
        assert_eq!(sel.anchor_on_screen(&g, c), Some((5, c.1 + 2)), "in view: where it is");
    }

    #[test]
    fn a_selection_up_is_erased_forward_and_one_down_backward() {
        let text = lines(40);
        let (sel, _) = select(&text, 10, 30, 4, "up", 20);
        let (job, n) = sel.erase();
        assert_eq!((job.key, n, job.budget), ("delete", sel.text().chars().count(), 0));
        let (sel, _) = select(&text, 10, 10, 4, "down", 20);
        let (job, n) = sel.erase();
        assert_eq!((job.key, n, job.budget), ("backspace", sel.text().chars().count(), 0));
    }

    #[test]
    fn a_row_cut_inside_a_long_word_is_erased_one_key_at_a_time() {
        // A URL, say. `selected_text` joins these with a space, which may have been a cut. The
        // word fills its row, so the break after it gets a key of its own as well.
        let mut text = lines(40);
        text[20] = "see".into();
        text[21] = "b".repeat(26);
        let (sel, _) = select(&text, 10, 22, 2, "up", 2);
        let (job, n) = sel.erase();
        assert_eq!((job.budget, n), (2, sel.text().chars().count() - 1));
    }

    #[test]
    fn a_full_row_may_hide_a_space_so_a_key_is_kept_for_it() {
        let mut text = lines(40);
        text[20] = "a".repeat(26);
        let (sel, _) = select(&text, 10, 22, 2, "up", 2);
        let (job, n) = sel.erase();
        assert_eq!((job.budget, n), (1, sel.text().chars().count()));
    }

    #[test]
    fn an_erase_past_the_window_ends_where_the_prompt_has_moved_to() {
        let text = lines(40);
        let (sel, _) = select(&text, 10, 30, 6, "up", 20);
        let (job, _) = sel.erase();
        assert_eq!((job.start_col, job.before.as_str(), job.after.as_str()), (6, "line", " 30"));
        let mut left = text[..10].to_vec();
        left.push("line 30".into());
        left.extend_from_slice(&text[31..]);
        for first in [5, 10] {
            let (g, c) = claude_window(&left, first, 10, 10 - first as u16);
            assert!(job.done(&g, (6, c.1)), "the row may be the window's top one: first {first}");
        }
    }

    #[test]
    fn only_a_full_box_starts_a_long_selection() {
        let (g, c) = claude_window(&lines(4), 0, 4, 2);
        let tall: Vec<RowData> = g.iter().cloned().chain((0..30).map(|_| row(""))).collect();
        assert!(LongSel::start(&tall, c).is_none());
        assert!(LongSel::start(&g, c).is_some());
    }
}
