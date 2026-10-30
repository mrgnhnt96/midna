//! Markdown lists while typing, for the composer (`composer.rs`) and Claude Code's and Codex's
//! input boxes (`terminal.rs`):
//!
//! - ⇧↩ on a list item (`- `, `* `, `+ `, `- [ ] `, `1. `, `1) `) starts the next one: the same
//!   bullet (an unticked box for a checklist) or the next number, at the same indent. ⇧↩ on an
//!   empty item ends the list instead: its marker is cleared and no line is added.
//! - After an edit that removes text (or a new item in the middle), the numbered list at the
//!   cursor is renumbered from its first item's number. Only the digits change, so the cursor
//!   stays where it was. A number being edited (the cursor inside its marker) is left alone.
//!
//! Offsets are byte offsets into the text.
use std::ops::Range;

/// What ⇧↩ does on a list item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Newline {
    /// Insert this at the cursor (a line break, then the next item's marker).
    Continue(String),
    /// The item is empty: erase this range (the line's indent and marker) and add no line.
    End(Range<usize>),
}

/// A replacement of `range` (a number's digits) with `text`.
pub type Edit = (Range<usize>, String);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Marker {
    Bullet(char),
    Number(u64, char),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Item {
    /// Leading spaces (a tab counts as one).
    indent: usize,
    marker: Marker,
    /// The marker's digits, for a number.
    digits: Range<usize>,
    /// Where the item's text starts (past the marker and its spaces, and a checkbox).
    body: usize,
    checkbox: bool,
}

fn item(line: &str) -> Option<Item> {
    let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
    let rest = &line[indent..];
    let (marker, digits, after) = match rest.chars().next()? {
        c @ ('-' | '*' | '+') => (Marker::Bullet(c), indent..indent, indent + 1),
        '0'..='9' => {
            let n = rest.bytes().take_while(u8::is_ascii_digit).count();
            let delim = rest[n..].chars().next().filter(|c| matches!(c, '.' | ')'))?;
            (Marker::Number(rest[..n].parse().ok().filter(|_| n <= 9)?, delim), indent..indent + n, indent + n + 1)
        }
        _ => return None,
    };
    let spaces = line[after..].len() - line[after..].trim_start_matches(' ').len();
    if spaces == 0 {
        return None;
    }
    let mut body = after + spaces;
    let checkbox = matches!(marker, Marker::Bullet(_)) && ["[ ]", "[x]", "[X]"].iter().any(|b| line[body..].starts_with(b));
    if checkbox {
        body += 3 + (line[body + 3..].len() - line[body + 3..].trim_start_matches(' ').len());
    }
    Some(Item { indent, marker, digits, body, checkbox })
}

/// The line around `at`: its range in `text`.
fn line_at(text: &str, at: usize) -> Range<usize> {
    let start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let end = text[at..].find('\n').map_or(text.len(), |i| at + i);
    start..end
}

/// What ⇧↩ does with the cursor at `cursor`; None where it's a plain line break (not on a list
/// item, or the cursor is in its indent or marker).
pub fn newline(text: &str, cursor: usize) -> Option<Newline> {
    let line = line_at(text, cursor);
    let it = item(&text[line.clone()])?;
    if cursor < line.start + it.body {
        return None;
    }
    if text[line.start + it.body..line.end].trim().is_empty() {
        return Some(Newline::End(line));
    }
    let marker = match it.marker {
        Marker::Bullet(c) if it.checkbox => format!("{c} [ ]"),
        Marker::Bullet(c) => c.to_string(),
        Marker::Number(n, d) => format!("{}{d}", n + 1),
    };
    Some(Newline::Continue(format!("\n{}{marker} ", &text[line.start..line.start + it.indent])))
}

/// The digits to change so the numbered list at the cursor counts up by one from its first
/// item, in order. A list is the numbered items at one indent with nothing between them but
/// deeper lines (a nested list, say); a blank line or a shallower line ends it. The cursor's own
/// line doesn't end a list when it isn't an item, so a list whose middle item just lost its
/// marker still counts through it.
pub fn renumber(text: &str, cursor: usize) -> Vec<Edit> {
    let here = line_at(text, cursor);
    // Open lists by indent: (indent, delimiter, next number, whether it touches the cursor, edits).
    let mut open: Vec<(usize, char, u64, bool, Vec<Edit>)> = vec![];
    let mut out = vec![];
    let mut close = |open: &mut Vec<(usize, char, u64, bool, Vec<Edit>)>, keep: usize| {
        for (_, _, _, touches, edits) in open.drain(keep..) {
            if touches {
                out.extend(edits);
            }
        }
    };
    let mut start = 0;
    let mut prev_here = false;
    for line in text.split('\n') {
        let range = start..start + line.len();
        start = range.end + 1;
        let is_here = range == here;
        let after_here = std::mem::replace(&mut prev_here, is_here);
        let Some(it) = item(line) else {
            if line.trim().is_empty() {
                close(&mut open, 0);
            } else if !is_here {
                let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
                let keep = open.iter().take_while(|o| o.0 < indent).count();
                close(&mut open, keep);
            }
            continue;
        };
        let keep = open.iter().take_while(|o| o.0 < it.indent || (o.0 == it.indent && matches!(it.marker, Marker::Number(_, d) if d == o.1))).count();
        close(&mut open, keep);
        let Marker::Number(n, delim) = it.marker else {
            // A bullet at a list's indent ends it.
            let keep = open.iter().take_while(|o| o.0 < it.indent).count();
            close(&mut open, keep);
            continue;
        };
        let touches = is_here || after_here;
        match open.last_mut().filter(|o| o.0 == it.indent) {
            Some(o) => {
                let editing = is_here && (range.start + it.digits.start + 1..range.start + it.body).contains(&cursor);
                if n != o.2 && !editing {
                    let d = it.digits.clone();
                    o.4.push((range.start + d.start..range.start + d.end, o.2.to_string()));
                }
                o.2 += 1;
                o.3 |= touches;
            }
            None => open.push((it.indent, delim, n + 1, touches, vec![])),
        }
    }
    close(&mut open, 0);
    out.sort_by_key(|e| e.0.start);
    out
}

/// `text` with `edits` (in order, not overlapping) applied.
pub fn apply(text: &str, edits: &[Edit]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (r, s) in edits {
        out.push_str(&text[at..r.start]);
        out.push_str(s);
        at = r.end;
    }
    out.push_str(&text[at..]);
    out
}

/// Where `cursor` ends up once `edits` are applied: shifted by the ones before it.
pub fn shift(cursor: usize, edits: &[Edit]) -> usize {
    let delta: isize = edits.iter().filter(|(r, _)| r.end <= cursor).map(|(r, s)| s.len() as isize - r.len() as isize).sum();
    cursor.saturating_add_signed(delta)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `text` with `|` as the cursor: (text, cursor).
    fn at(s: &str) -> (String, usize) {
        let i = s.find('|').unwrap();
        (s.replacen('|', "", 1), i)
    }

    fn cont(s: &str) -> Option<String> {
        let (t, c) = at(s);
        match newline(&t, c)? {
            Newline::Continue(s) => Some(s),
            Newline::End(_) => panic!("ended"),
        }
    }

    fn renumbered(s: &str) -> String {
        let (t, c) = at(s);
        let edits = renumber(&t, c);
        let mut out = apply(&t, &edits);
        out.insert(shift(c, &edits), '|');
        out
    }

    #[test]
    fn continues_bullets_and_numbers() {
        assert_eq!(cont("- one|").as_deref(), Some("\n- "));
        assert_eq!(cont("intro\n  * nested|").as_deref(), Some("\n  * "));
        assert_eq!(cont("1. one\n2. two|").as_deref(), Some("\n3. "));
        assert_eq!(cont("9) nine|").as_deref(), Some("\n10) "));
        assert_eq!(cont("- [x] done|").as_deref(), Some("\n- [ ] "));
        // Mid-item splits it.
        assert_eq!(cont("- fo|o").as_deref(), Some("\n- "));
    }

    #[test]
    fn plain_lines_and_markers_are_left_alone() {
        assert_eq!(cont("hello|"), None);
        assert_eq!(cont("-no space|"), None);
        assert_eq!(cont("2024.10 release|"), None);
        assert_eq!(cont("-| one"), None);
        assert_eq!(cont("1234567890. big|"), None);
    }

    #[test]
    fn an_empty_item_ends_the_list() {
        let (t, c) = at("- one\n  - |");
        assert_eq!(newline(&t, c), Some(Newline::End(6..10)));
        let (t, c) = at("1. one\n2. |");
        assert_eq!(newline(&t, c), Some(Newline::End(7..10)));
        let (t, c) = at("- [ ] |");
        assert_eq!(newline(&t, c), Some(Newline::End(0..6)));
    }

    #[test]
    fn renumbers_after_a_removed_item() {
        assert_eq!(renumbered("1. a|\n3. c\n4. d"), "1. a|\n2. c\n3. d");
        assert_eq!(renumbered("1. a\n|3. c\n4. d"), "1. a\n|2. c\n3. d");
        // The cursor stays put when a number before it gets shorter or longer.
        assert_eq!(renumbered("9. a\n11. b|"), "9. a\n10. b|");
        assert_eq!(renumbered("8. a\n9. b\n9. c|"), "8. a\n9. b\n10. c|");
    }

    #[test]
    fn a_new_item_in_the_middle_pushes_the_rest_down() {
        assert_eq!(renumbered("1. a\n2. |\n2. b\n3. c"), "1. a\n2. |\n3. b\n4. c");
    }

    #[test]
    fn a_line_that_lost_its_marker_doesnt_split_the_list() {
        assert_eq!(renumbered("1. a\nb|\n3. c"), "1. a\nb|\n2. c");
    }

    #[test]
    fn nested_lists_count_on_their_own() {
        assert_eq!(renumbered("1. a\n   1. x\n   3. y|\n2. b"), "1. a\n   1. x\n   2. y|\n2. b");
        assert_eq!(renumbered("1. a|\n   - x\n3. b"), "1. a|\n   - x\n2. b");
    }

    #[test]
    fn only_the_list_at_the_cursor_changes() {
        assert_eq!(renumbered("4. a\n6. b\n\n1. x|\n3. y"), "4. a\n6. b\n\n1. x|\n2. y");
        assert_eq!(renumbered("4. a\n6. b\n\nplain|"), "4. a\n6. b\n\nplain|");
    }

    #[test]
    fn a_number_being_typed_is_left_alone() {
        assert_eq!(renumbered("1. a\n1|. b"), "1. a\n1|. b");
        assert_eq!(renumbered("1. a\n2. b\n1|. c"), "1. a\n2. b\n1|. c");
    }

    #[test]
    fn paragraphs_and_bullets_end_a_list() {
        assert_eq!(renumbered("1. a|\nnote\n5. b"), "1. a|\nnote\n5. b");
        assert_eq!(renumbered("1. a|\n- x\n5. b"), "1. a|\n- x\n5. b");
        assert_eq!(renumbered("1) a|\n5. b"), "1) a|\n5. b");
    }
}
