//! The human's prompts on an agent's screen (prompt fast travel). Shared by midnad, which
//! scrolls the agent to a prompt (`session.jump_prompt`), and the app, which reads each frame
//! to show where the view is.
//!
//! Claude Code and Codex draw full-screen (alternate screen), so their conversation never
//! reaches the terminal's scrollback: the agent scrolls it, and only the visible screen can be
//! read. What Claude Code (2.1.289) draws there:
//! - each prompt in the transcript as a row starting `❯ ` (the first line of it; continuation
//!   rows are indented). Slash commands look the same but fire no `UserPromptSubmit`;
//! - while scrolled back, the prompt the top of the view belongs to pinned on row 0, and a hint
//!   drawn over the right of the last transcript row: `Jump to bottom: fn+↓ to scroll`
//!   (2.1.291: `Jump to bottom (click) ↓`), or `3 new messages (click) ↓` once output arrives
//!   below the view;
//! - the input box below the transcript, between two `─` rules, its first row also `❯ `.
//!
//! Codex marks prompts with `› `.

/// Characters that start a prompt row: Claude Code's and Codex's.
pub const MARKERS: [char; 2] = ['❯', '›'];

/// Where Claude Code's scrolled-back hint starts in a row, if the row has one: `Jump to
/// bottom…`, or `N new message(s) (click) ↓` (the count must lead, and an arrow follow, so prose
/// saying "new message" doesn't count).
fn scrolled_hint(line: &str) -> Option<usize> {
    if let Some(at) = line.find("Jump to bottom") {
        return Some(at);
    }
    let at = line.find(" new message")?;
    if !line[at..].contains('↓') {
        return None;
    }
    let head = &line[..at];
    let digits = head.len() - head.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    let start = at - digits;
    (digits > 0 && line[..start].ends_with("  ")).then_some(start)
}

/// What one screen shows of the prompts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScreenScan {
    /// Prompt rows above the input box, top to bottom: (row, text after the marker).
    pub rows: Vec<(u16, String)>,
    /// The agent says its view is scrolled back.
    pub scrolled: bool,
}

/// Where the top of the view is in the conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Here {
    /// No known prompt on screen (in the middle of a long reply).
    Unknown,
    /// Above the first prompt (the agent's banner).
    BeforeFirst,
    /// In this prompt or its reply (an index into the prompt list).
    At(usize),
}

/// A prompt's text as the screen shows its first row: first line, whitespace collapsed.
pub fn key(prompt: &str) -> String {
    let first = prompt.trim_start().lines().next().unwrap_or("");
    first.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Does a prompt row's text (`row`, after the marker) show this prompt? The row may be cut
/// short by the screen width or by the agent's own overlay, so a long enough prefix counts.
pub fn matches(row: &str, prompt: &str) -> bool {
    let (a, b) = (key(row), key(prompt));
    let a = a.trim_end_matches('…').trim_end();
    // Claude shows a prompt of several lines on one row, its lines joined.
    let all = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    !a.is_empty() && (a == b || a == all || (a.chars().count() >= 8 && (b.starts_with(a) || all.starts_with(a))))
}

fn is_rule(line: &str) -> bool {
    let t = line.trim();
    let n = t.chars().count();
    n >= 10 && t.chars().filter(|&c| c == '─').count() * 10 >= n * 8
}

/// Read the prompt rows off a screen (`lines`, one per row). `cursor_row` is the cursor's row
/// (inside the input box), which bounds the transcript like the input box's top rule does.
pub fn scan(lines: &[String], cursor_row: Option<u16>) -> ScreenScan {
    let rules: Vec<usize> = lines.iter().enumerate().filter(|(_, l)| is_rule(l)).map(|(i, _)| i).collect();
    let mut end = lines.len();
    if rules.len() >= 2 {
        end = rules[rules.len() - 2];
    }
    if let Some(c) = cursor_row {
        end = end.min(c as usize);
    }
    let mut out = ScreenScan::default();
    for (i, line) in lines.iter().enumerate().take(end) {
        let mut text = line.as_str();
        if let Some(at) = scrolled_hint(text) {
            out.scrolled = true;
            text = &text[..at];
        }
        let mut chars = text.chars();
        if chars.next().is_some_and(|c| MARKERS.contains(&c)) {
            let rest = chars.as_str();
            if rest.is_empty() || rest.starts_with(' ') {
                out.rows.push((i as u16, rest.trim().to_string()));
            }
        }
    }
    out
}

/// Which prompt each scanned row shows: an index into `prompts` (oldest first), or None for a
/// row that is no known prompt (a slash command). Rows go down the screen in conversation
/// order; a repeated prompt text resolves to the occurrence nearest `hint` (the newest when
/// None), then each later row to the next match after the one above it.
pub fn assign(rows: &[(u16, String)], prompts: &[String], hint: Option<usize>) -> Vec<Option<usize>> {
    let mut last: Option<usize> = None;
    let hint = hint.unwrap_or(prompts.len());
    rows.iter()
        .map(|(_, text)| {
            let cands = prompts.iter().enumerate().filter(|(i, p)| last.is_none_or(|l| *i > l) && matches(text, p)).map(|(i, _)| i);
            let pick = if last.is_none() { cands.min_by_key(|i| i.abs_diff(hint)) } else { cands.min() };
            if pick.is_some() {
                last = pick;
            }
            pick
        })
        .collect()
}

/// Where the top of the view is, from the assigned rows: a prompt on the top two rows (row 0
/// is the one Claude Code pins while scrolled back) is the one being read; else the reply at
/// the top belongs to the prompt before the first one visible.
pub fn here(rows: &[(u16, String)], assigned: &[Option<usize>]) -> Here {
    let known: Vec<(u16, usize)> = rows.iter().zip(assigned).filter_map(|((r, _), a)| a.map(|a| (*r, a))).collect();
    if let Some(&(_, i)) = known.iter().rev().find(|(r, _)| *r <= 1) {
        return Here::At(i);
    }
    match known.first() {
        Some(&(_, 0)) => Here::BeforeFirst,
        Some(&(_, i)) => Here::At(i - 1),
        None => Here::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(s: &str) -> Vec<String> {
        s.lines().map(str::to_string).collect()
    }

    fn prompts() -> Vec<String> {
        ["alpha: list the numbers 1 to 30, one per line, no tools", "bravo: list 30 fruits one per line, no tools", "charlie: list 30 countries one per line, no tools"]
            .map(String::from)
            .to_vec()
    }

    const RULE: &str = "────────────────────────────────────────────────────────────";

    #[test]
    fn keys_and_matching() {
        assert_eq!(key("  fix   the\ttests\nand more"), "fix the tests");
        assert!(matches("fix the tests", "fix the tests\nplease"));
        // cut short by the width or by Claude's "Jump to bottom" hint
        assert!(matches("charlie: list 30 countries one p", "charlie: list 30 countries one per line"));
        assert!(!matches("ship", "ship it"), "too short a prefix to trust");
        assert!(matches("ship it", "ship it"));
        assert!(!matches("ship it now", "ship it"));
        assert!(!matches("", "x"));
        assert!(matches("[Image #5] Annotations (coordinates are perc…", "[Image #5]\nAnnotations (coordinates are percentages)"), "lines joined on one row");
    }

    #[test]
    fn scrolled_back_claude_screen() {
        // Claude Code 2.1.289 after one PageUp: bravo pinned on row 0, charlie's prompt below.
        let s = screen(&format!(
            "❯ bravo: list 30 fruits one per line, no tools\n  23. Grapefruit\n\n✻ Cooked for 5s\n\n❯ charlie: list 30 countries one per line, no tools\n\n⏺ 1. Afghanistan\n  10. Chile                        Jump to bottom: fn+↓ to scroll\n\n{RULE}\n❯ \n{RULE}\n  Haiku 4.5 · $0.04"
        ));
        let sc = scan(&s, Some(11));
        assert!(sc.scrolled);
        assert_eq!(sc.rows.iter().map(|r| r.0).collect::<Vec<_>>(), [0, 5], "the input box's ❯ is not a prompt");
        let a = assign(&sc.rows, &prompts(), None);
        assert_eq!(a, [Some(1), Some(2)]);
        assert_eq!(here(&sc.rows, &a), Here::At(1));
    }

    #[test]
    fn hint_cut_prompt_and_live_screen() {
        let s = screen(&format!("  29. Fig\n\n❯ charlie: list 30 countries one p Jump to bottom: fn+↓ to scroll\n\n{RULE}\n❯ typing\n{RULE}"));
        let sc = scan(&s, Some(5));
        assert!(sc.scrolled);
        let a = assign(&sc.rows, &prompts(), None);
        assert_eq!(a, [Some(2)]);
        assert_eq!(here(&sc.rows, &a), Here::At(1), "the reply above charlie's prompt is bravo's");

        let live = screen(&format!("  30. Norway\n\n{RULE}\n❯\n{RULE}"));
        let sc = scan(&live, Some(3));
        assert!(!sc.scrolled);
        assert!(sc.rows.is_empty());
        assert_eq!(here(&sc.rows, &[]), Here::Unknown);
    }

    #[test]
    fn new_messages_hint_means_scrolled() {
        // Claude Code 2.1.291, scrolled back while it works: the hint counts the new output.
        let s = screen(&format!(
            "❯ alpha: list the numbers 1 to 30, one per line, no tools\n❯ bravo: list 30 fruits one per line, no tools\n\n⏺ Apple\n  Coconut                                       1 new message (click) ↓\n\n{RULE}\n❯\n{RULE}"
        ));
        let sc = scan(&s, Some(7));
        assert!(sc.scrolled);
        assert_eq!(here(&sc.rows, &assign(&sc.rows, &prompts(), None)), Here::At(1));
        let s = screen(&format!("  20                                  Jump to bottom (click) ↓\n\n{RULE}\n❯\n{RULE}"));
        assert!(scan(&s, Some(3)).scrolled);
        // a prompt row cut by the hint
        let s = screen(&format!("❯ charlie: list 30 countries  12 new messages (click) ↓\n{RULE}\n❯\n{RULE}"));
        assert_eq!(scan(&s, Some(2)).rows, [(0, "charlie: list 30 countries".to_string())]);
        // prose about new messages is not the hint
        let s = screen(&format!("⏺ The 1 new message ↓ hint, and 3 new messages here\n{RULE}\n❯\n{RULE}"));
        assert!(!scan(&s, Some(2)).scrolled);
    }

    #[test]
    fn top_of_history_and_slash_commands() {
        let s = screen(&format!(
            " ▐▛███▛█   Claude Code v2.1.289\n\n❯ /model haiku\n  ⎿  Set model\n\n❯ alpha: list the numbers 1 to 30, one per line, no tools\n\n⏺ 1\n{RULE}\n❯\n{RULE}"
        ));
        let sc = scan(&s, None);
        let a = assign(&sc.rows, &prompts(), None);
        assert_eq!(a, [None, Some(0)]);
        assert_eq!(here(&sc.rows, &a), Here::BeforeFirst);
    }

    #[test]
    fn repeated_prompts_follow_the_hint_then_order() {
        let ps: Vec<String> = ["ship it", "fix the build", "ship it", "fix the build"].map(String::from).to_vec();
        let rows = vec![(3, "ship it".to_string()), (9, "fix the build".to_string())];
        assert_eq!(assign(&rows, &ps, None), [Some(2), Some(3)], "newest by default");
        assert_eq!(assign(&rows, &ps, Some(0)), [Some(0), Some(1)]);
    }

    #[test]
    fn codex_marker() {
        let sc = scan(&screen("› explain the relay\n\n• It forwards webhooks"), None);
        assert_eq!(sc.rows, [(0, "explain the relay".to_string())]);
    }
}
