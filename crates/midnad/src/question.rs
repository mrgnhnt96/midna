//! Answering Claude Code's AskUserQuestion picker for the human (`session.answer_question`).
//!
//! The picker (Claude Code 2.1.301), one tab per question:
//!
//! ```text
//! ←  ☐ Color  ☐ Pets  ✔ Submit  →        (one single-select question: " ☐ Size", no arrows)
//!
//! Pick a color?
//!
//! ❯ 1. Red                               (multi-select rows read "1. [ ] Cat" / "[✔]")
//!      Warm, bold red
//!   2. Green
//!   3. Type something.                   (typing here replaces the label with the text)
//!      Submit                            (multi-select only)
//! ──────────
//!   4. Chat about this
//!
//! Enter to select · Tab/Arrow keys to navigate · Esc to cancel
//! ```
//!
//! Enter picks a single-select option (or toggles a multi-select one; Enter on `Submit` ends a
//! multi-select question) and moves to the next tab. Left/Right move between tabs without
//! answering; Left on the first tab does nothing, and neither works while the cursor sits in
//! the "Type something" text field (the footer then offers "ctrl+g to edit in Vim"). After the
//! last question a picker with several questions, or a multi-select one, shows "Review your
//! answers" with `1. Submit answers` / `2. Cancel`; a lone single-select question submits at once.
//!
//! [`answer`] checks every question before it answers any, re-reads the screen after each step
//! and stops at the first surprise, leaving the picker to the human. It never presses Escape
//! (that cancels the whole picker).
use crate::term::Key;
use midna_proto::methods::{AnsweredQuestion, QuestionAnswer};
use std::time::{Duration, Instant};

/// One tab of the picker's tab bar.
#[derive(Clone, Debug, PartialEq)]
pub struct Tab {
    pub label: String,
    pub answered: bool,
}

/// A numbered option row (not "Type something").
#[derive(Clone, Debug, PartialEq)]
pub struct Opt {
    pub label: String,
    pub checked: bool,
}

/// Where the cursor (`❯`) is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Opt(usize),
    Other,
    Submit,
    Chat,
}

/// The question picker as the screen shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Picker {
    pub tabs: Vec<Tab>,
    /// The shown question's text, wrapped lines joined.
    pub question: String,
    pub multi: bool,
    pub options: Vec<Opt>,
    /// The "Type something" row's label (the typed text once the human or midna typed).
    pub other: String,
    pub other_checked: bool,
    pub cursor: Option<Row>,
    /// The cursor is in the "Type something" text field (arrows left/right edit text there).
    pub text_mode: bool,
}

/// The "Review your answers" screen.
#[derive(Clone, Debug, PartialEq)]
pub struct Review {
    /// (question, answer) pairs, wrapped lines joined.
    pub entries: Vec<(String, String)>,
    /// "You have not answered all questions".
    pub incomplete: bool,
    /// The cursor is on `Submit answers` (Some(true)) or `Cancel` (Some(false)).
    pub on_submit: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Screen {
    Question(Picker),
    Review(Review),
    None,
}

const SEPARATOR: char = '─';

fn is_separator(l: &str) -> bool {
    let t = l.trim();
    t.chars().count() >= 3 && t.chars().all(|c| c == SEPARATOR)
}

/// `❯ 3. [✔] Fish` -> (cursor, 3, Some(true), "Fish"); `  4. Type something.` -> (false, 4, None, …).
fn numbered(l: &str) -> Option<(bool, usize, Option<bool>, String)> {
    let (cursor, rest) = match l.strip_prefix("❯ ") {
        Some(r) => (true, r),
        None => (false, l.strip_prefix("  ")?),
    };
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let n: usize = digits.parse().ok()?;
    let rest = rest[digits.len()..].strip_prefix(". ")?;
    let (checked, label) = if let Some(r) = rest.strip_prefix("[ ] ") {
        (Some(false), r)
    } else if let Some(r) = rest.strip_prefix("[✔] ") {
        (Some(true), r)
    } else if let Some(r) = rest.strip_prefix("[✓] ") {
        (Some(true), r)
    } else {
        (None, rest)
    };
    Some((cursor, n, checked, label.trim().to_string()))
}

/// `←  ☐ Color  ☒ Pets  ✔ Submit  →` -> the question tabs. None when `l` isn't a tab bar.
fn tab_bar(l: &str) -> Option<Vec<Tab>> {
    let t = l.trim().trim_start_matches('←').trim_end_matches('→').trim();
    if !t.starts_with(['☐', '☒']) {
        return None;
    }
    let mut tabs = vec![];
    let mut rest = t;
    while let Some(c) = rest.chars().next() {
        let after = &rest[c.len_utf8()..];
        let end = after.find(['☐', '☒', '✔']).unwrap_or(after.len());
        let label = after[..end].trim().to_string();
        match c {
            '☐' | '☒' => tabs.push(Tab { label, answered: c == '☒' }),
            '✔' if label == "Submit" => {}
            _ => return None,
        }
        rest = &after[end..];
    }
    (!tabs.is_empty()).then_some(tabs)
}

/// The last tab bar on the screen, at or above `before`.
fn last_tab_bar(lines: &[String], before: usize) -> Option<(usize, Vec<Tab>)> {
    (0..before.min(lines.len())).rev().find_map(|i| tab_bar(&lines[i]).map(|t| (i, t)))
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Text equal but for whitespace (wrapping may split or join words differently).
pub fn same_text(a: &str, b: &str) -> bool {
    let strip = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    strip(a) == strip(b)
}

/// Read the picker off a screen.
pub fn parse(screen: &[String]) -> Screen {
    let lines: Vec<String> = screen.iter().map(|l| l.trim_end().to_string()).collect();
    if let Some(p) = parse_question(&lines) {
        return Screen::Question(p);
    }
    if let Some(r) = parse_review(&lines) {
        return Screen::Review(r);
    }
    Screen::None
}

fn parse_question(lines: &[String]) -> Option<Picker> {
    // The footer is among the last few non-empty lines, like `screen_shows_question`.
    let nonempty: Vec<usize> = (0..lines.len()).filter(|&i| !lines[i].trim().is_empty()).collect();
    let footer = nonempty.iter().rev().take(4).copied().find(|&i| {
        let t = lines[i].trim_start();
        t.starts_with("Enter to select") && t.contains("Esc to cancel")
    })?;
    let text_mode = lines[footer].contains("ctrl+g");
    // "N. Chat about this" just above the footer, the separator just above that.
    let chat = (0..footer).rev().find(|&i| !lines[i].trim().is_empty())?;
    let (chat_cursor, chat_n, _, chat_label) = numbered(&lines[chat])?;
    if chat_label != "Chat about this" || chat == 0 || !is_separator(&lines[chat - 1]) {
        return None;
    }
    let sep = chat - 1;
    let (bar, tabs) = last_tab_bar(lines, sep)?;
    let mut question: Vec<&str> = vec![];
    let mut rows: Vec<(bool, usize, Option<bool>, String)> = vec![];
    let mut submit: Option<bool> = None;
    for l in &lines[bar + 1..sep] {
        if let Some(r) = numbered(l) {
            rows.push(r);
        } else if l.trim_start_matches('❯').trim() == "Submit" && !rows.is_empty() {
            submit = Some(l.starts_with('❯'));
        } else if rows.is_empty() && !l.trim().is_empty() {
            question.push(l.trim());
        }
        // Anything else is an option's description (indented under its row).
    }
    // At least one option, then "Type something"; numbered 1.. in order, and Chat right after.
    if rows.len() < 2 || rows.iter().enumerate().any(|(i, r)| r.1 != i + 1) || chat_n != rows.len() + 1 {
        return None;
    }
    let multi = rows[0].2.is_some();
    if rows.iter().any(|r| r.2.is_some() != multi) || submit.is_some() != multi {
        return None;
    }
    let other = rows.pop()?;
    let mut cursor = rows.iter().position(|r| r.0).map(Row::Opt);
    if other.0 {
        cursor = Some(Row::Other);
    }
    if submit == Some(true) {
        cursor = Some(Row::Submit);
    }
    if chat_cursor {
        cursor = Some(Row::Chat);
    }
    Some(Picker {
        tabs,
        question: squash(&question.join(" ")),
        multi,
        options: rows.into_iter().map(|r| Opt { label: r.3, checked: r.2 == Some(true) }).collect(),
        other: other.3,
        other_checked: other.2 == Some(true),
        cursor,
        text_mode,
    })
}

fn parse_review(lines: &[String]) -> Option<Review> {
    let (bar, _) = last_tab_bar(lines, lines.len())?;
    let rest = &lines[bar + 1..];
    let start = rest.iter().position(|l| l.trim() == "Review your answers")?;
    let mut entries: Vec<(String, String)> = vec![];
    let mut on_submit = None;
    let (mut submit_row, mut cancel_row) = (false, false);
    let mut incomplete = false;
    // Which part of an entry the current line continues.
    let mut in_answer = false;
    for l in &rest[start + 1..] {
        let t = l.trim();
        if let Some(q) = t.strip_prefix("● ") {
            entries.push((q.to_string(), String::new()));
            in_answer = false;
        } else if let Some(a) = t.strip_prefix("→ ") {
            let Some(e) = entries.last_mut() else { return None };
            e.1 = a.to_string();
            in_answer = true;
        } else if let Some((cursor, n, None, label)) = numbered(l) {
            match (n, label.as_str()) {
                (1, "Submit answers") => submit_row = true,
                (2, "Cancel") => cancel_row = true,
                _ => return None,
            }
            if cursor {
                on_submit = Some(n == 1);
            }
            in_answer = false;
        } else if t.contains('⚠') {
            incomplete = true;
        } else if t.is_empty() || !l.starts_with("  ") {
            in_answer = false;
        } else if let Some(e) = entries.last_mut() {
            // A wrapped question or answer line.
            let part = if in_answer { &mut e.1 } else { &mut e.0 };
            part.push(' ');
            part.push_str(t);
        }
    }
    if !submit_row || !cancel_row {
        return None;
    }
    let entries = entries.into_iter().map(|(q, a)| (squash(&q), squash(&a))).collect();
    Some(Review { entries, incomplete, on_submit })
}

/// The terminal the picker is in.
pub trait Term {
    /// The visible screen, None if the terminal is gone.
    fn screen(&mut self) -> Option<Vec<String>>;
    fn key(&mut self, k: Key) -> bool;
    fn text(&mut self, s: &str) -> bool;
    fn pause(&mut self, d: Duration);
}

/// Gap between key presses, as in [`crate::trust`].
const KEY_GAP: Duration = Duration::from_millis(80);
/// How often, and how long, to re-read the screen waiting for a key's effect.
const POLL: Duration = Duration::from_millis(40);
const STEP_TIMEOUT: Duration = Duration::from_secs(3);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);
/// Longest free-text answer.
const MAX_OTHER: usize = 1000;

/// Why nothing (more) was pressed. `Refused` before any key that changes an answer.
#[derive(Debug, PartialEq)]
pub enum Fail {
    /// The screen or the answers don't fit; nothing was answered.
    Refused(String),
    /// Something went wrong part way; the picker is left as it is for the human.
    Stopped(String),
    /// The terminal stopped answering.
    Gone,
}

struct Driver<'a> {
    t: &'a mut dyn Term,
}

impl Driver<'_> {
    fn read(&mut self) -> Result<Screen, Fail> {
        self.t.screen().map(|l| parse(&l)).ok_or(Fail::Gone)
    }

    fn picker(&mut self, stage: &str) -> Result<Picker, Fail> {
        match self.read()? {
            Screen::Question(p) => Ok(p),
            _ => Err(Fail::Stopped(format!("the question picker disappeared {stage}"))),
        }
    }

    fn press(&mut self, k: Key) -> Result<(), Fail> {
        if !self.t.key(k) {
            return Err(Fail::Gone);
        }
        self.t.pause(KEY_GAP);
        Ok(())
    }

    /// Re-read until `ok` holds (or the time runs out); returns the last screen.
    fn wait(&mut self, timeout: Duration, ok: impl Fn(&Screen) -> bool) -> Result<(Screen, bool), Fail> {
        let start = Instant::now();
        let mut polls = 0;
        loop {
            let s = self.read()?;
            if ok(&s) {
                return Ok((s, true));
            }
            // A fake terminal answers at once; bound the polls as well as the time.
            polls += 1;
            if start.elapsed() >= timeout || polls > timeout.as_millis() / POLL.as_millis() {
                return Ok((s, false));
            }
            self.t.pause(POLL);
        }
    }

    /// Move the cursor to `to`, one key at a time. `m` = number of real options.
    fn move_to(&mut self, p: &Picker, to: Row, fail: fn(String) -> Fail) -> Result<Picker, Fail> {
        let idx = |r: Row, m: usize| match r {
            Row::Opt(i) => i,
            Row::Other => m,
            Row::Submit => m + 1,
            Row::Chat => m + 2,
        };
        let m = p.options.len();
        let from = p.cursor.ok_or_else(|| fail("the picker shows no cursor".into()))?;
        // Single-select has no Submit row between "Type something" and Chat.
        let pos = |r: Row| {
            let i = idx(r, m);
            if !p.multi && r == Row::Chat { i - 1 } else { i }
        };
        let (a, b) = (pos(from), pos(to));
        let k = if b < a { Key::Up } else { Key::Down };
        for _ in 0..a.abs_diff(b) {
            self.press(k)?;
        }
        let (s, ok) = self.wait(STEP_TIMEOUT, |s| matches!(s, Screen::Question(q) if q.cursor == Some(to)))?;
        match s {
            Screen::Question(q) if ok => Ok(q),
            Screen::Question(q) => Err(fail(format!("the cursor went to {:?} instead of {to:?}", q.cursor))),
            _ => Err(fail("the question picker disappeared while moving the cursor".into())),
        }
    }

    /// Cursor on the first option, out of the text field, so Left/Right switch tabs.
    fn park(&mut self, p: Picker, fail: fn(String) -> Fail) -> Result<Picker, Fail> {
        let p = if p.cursor == Some(Row::Opt(0)) { p } else { self.move_to(&p, Row::Opt(0), fail)? };
        if p.text_mode {
            return Err(fail("the cursor is stuck in the \"Type something\" field".into()));
        }
        Ok(p)
    }

    /// Switch tab with Left/Right (cursor parked first).
    fn tab(&mut self, p: Picker, k: Key, fail: fn(String) -> Fail) -> Result<Picker, Fail> {
        self.park(p, fail)?;
        self.press(k)?;
        // Let the picker redraw; the new tab is checked by its caller.
        self.t.pause(KEY_GAP);
        match self.read()? {
            Screen::Question(q) => Ok(q),
            Screen::Review(_) => Err(fail("the picker went to its review screen while switching questions".into())),
            Screen::None => Err(fail("the question picker disappeared while switching questions".into())),
        }
    }
}

/// What one question takes, checked against the screen before anything is pressed.
struct Plan {
    question: String,
    multi: bool,
    options: Vec<usize>,
    other: Option<String>,
    labels: Vec<String>,
}

fn untouched(p: &Picker, n: usize) -> Result<(), Fail> {
    if p.tabs.iter().any(|t| t.answered) || p.options.iter().any(|o| o.checked) || p.other_checked {
        return Err(Fail::Refused("the question picker is already partly answered; finish it in the terminal".into()));
    }
    if p.other.trim_end_matches('.') != "Type something" {
        return Err(Fail::Refused(format!("question {} already has text typed into \"Type something\"; finish it in the terminal", n + 1)));
    }
    Ok(())
}

fn plan(n: usize, p: &Picker, a: &QuestionAnswer) -> Result<Plan, Fail> {
    let refuse = |m: String| Fail::Refused(format!("question {} ({:?}): {m}", n + 1, p.question));
    untouched(p, n)?;
    if let Some(q) = &a.question
        && !same_text(q, &p.question)
    {
        return Err(refuse(format!("the picker shows a different question than {q:?}")));
    }
    let mut options: Vec<usize> = vec![];
    for &i in &a.options {
        let i = i as usize;
        if i >= p.options.len() {
            return Err(refuse(format!("option {i} does not exist; it has {} (0-based)", p.options.len())));
        }
        if !options.contains(&i) {
            options.push(i);
        }
    }
    options.sort_unstable();
    let other = match &a.other {
        None => None,
        Some(t) if t.trim().is_empty() => return Err(refuse("`other` is empty".into())),
        Some(t) if t.chars().any(char::is_control) => return Err(refuse("`other` must be one line of text, without control characters".into())),
        Some(t) if t.chars().count() > MAX_OTHER => return Err(refuse(format!("`other` is longer than {MAX_OTHER} characters"))),
        Some(t) => Some(t.trim().to_string()),
    };
    let picked = options.len() + other.is_some() as usize;
    if !p.multi && picked != 1 {
        return Err(refuse(format!("it is single-select: give exactly one option or `other`, not {picked}")));
    }
    if picked == 0 {
        return Err(refuse("no option picked".into()));
    }
    let mut labels: Vec<String> = options.iter().map(|&i| p.options[i].label.clone()).collect();
    labels.extend(other.clone());
    Ok(Plan { question: p.question.clone(), multi: p.multi, options, other, labels })
}

/// Answer the picker on `t` with `answers` (one per question), submit, and wait for it to close.
pub fn answer(t: &mut dyn Term, answers: &[QuestionAnswer]) -> Result<Vec<AnsweredQuestion>, Fail> {
    let mut d = Driver { t };
    let first = match d.read()? {
        Screen::Question(p) => p,
        Screen::Review(_) => return Err(Fail::Refused("the question picker is on its review screen; finish it in the terminal".into())),
        Screen::None => {
            return Err(Fail::Refused(
                "the terminal does not show Claude's question picker (numbered options, \"Type something\", \"Chat about this\", \"Enter to select\")".into(),
            ));
        }
    };
    let n = first.tabs.len();
    if answers.len() != n {
        return Err(Fail::Refused(format!("the picker has {n} question(s) but {} answer(s) were given", answers.len())));
    }
    untouched(&first, 0)?;

    // Back to the first question (Left on it does nothing), then check each one in turn.
    let refused: fn(String) -> Fail = Fail::Refused;
    let mut p = first;
    for _ in 1..n {
        p = d.tab(p, Key::Left, refused)?;
    }
    let mut plans = vec![];
    for (i, a) in answers.iter().enumerate() {
        if i > 0 {
            p = d.tab(p, Key::Right, refused)?;
        }
        if p.tabs.len() != n {
            return Err(Fail::Refused("the picker changed while it was being checked".into()));
        }
        plans.push(plan(i, &p, a)?);
    }
    for _ in 1..n {
        p = d.tab(p, Key::Left, refused)?;
    }
    if !same_text(&p.question, &plans[0].question) {
        return Err(Fail::Refused("could not get back to the first question".into()));
    }

    // Answer. From here on a surprise stops, leaving the picker as it is.
    let stopped: fn(String) -> Fail = Fail::Stopped;
    for (i, pl) in plans.iter().enumerate() {
        if !same_text(&p.question, &pl.question) {
            return Err(Fail::Stopped(format!("expected question {} ({:?}) but the picker shows {:?}", i + 1, pl.question, p.question)));
        }
        for &o in &pl.options {
            p = d.move_to(&p, Row::Opt(o), stopped)?;
            if pl.multi {
                d.press(Key::Enter)?;
                let (s, ok) = d.wait(STEP_TIMEOUT, |s| matches!(s, Screen::Question(q) if q.options.get(o).is_some_and(|x| x.checked)))?;
                let Screen::Question(q) = s else { return Err(Fail::Stopped("the question picker disappeared while ticking an option".into())) };
                if !ok {
                    return Err(Fail::Stopped(format!("option {:?} did not get ticked", pl.labels[0])));
                }
                p = q;
            }
        }
        if let Some(text) = &pl.other {
            d.move_to(&p, Row::Other, stopped)?;
            if !d.t.text(text) {
                return Err(Fail::Gone);
            }
            let multi = pl.multi;
            let (s, ok) = d.wait(STEP_TIMEOUT, |s| matches!(s, Screen::Question(q) if same_text(&q.other, text) && (!multi || q.other_checked)))?;
            let Screen::Question(q) = s else { return Err(Fail::Stopped("the question picker disappeared while typing".into())) };
            if !ok {
                return Err(Fail::Stopped(format!("the typed text did not show in \"Type something\" (it shows {:?})", q.other)));
            }
            p = q;
        }
        if pl.multi {
            d.move_to(&p, Row::Submit, stopped)?;
        }
        // Enter picks (single-select) or ends (multi-select) the question.
        d.press(Key::Enter)?;
        let last = i + 1 == n;
        let next = plans.get(i + 1).map(|x| x.question.clone());
        let (s, ok) = d.wait(STEP_TIMEOUT, |s| match s {
            Screen::Question(q) => next.as_deref().is_some_and(|nq| same_text(&q.question, nq) && q.tabs.get(i).is_some_and(|t| t.answered)),
            Screen::Review(_) | Screen::None => last,
        })?;
        if !ok {
            return Err(Fail::Stopped(format!("question {} did not take the answer", i + 1)));
        }
        match s {
            Screen::Question(q) => p = q,
            Screen::Review(r) => return submit(&mut d, &plans, r),
            Screen::None => return Ok(done(&plans)),
        }
    }
    let _ = d.picker("after the last question")?;
    Err(Fail::Stopped("the picker did not move on after the last question".into()))
}

fn done(plans: &[Plan]) -> Vec<AnsweredQuestion> {
    plans.iter().map(|p| AnsweredQuestion { question: p.question.clone(), labels: p.labels.clone() }).collect()
}

/// Check the review screen against the plans, then press `Submit answers`.
fn submit(d: &mut Driver, plans: &[Plan], r: Review) -> Result<Vec<AnsweredQuestion>, Fail> {
    if r.incomplete {
        return Err(Fail::Stopped("the review screen says not every question is answered".into()));
    }
    if r.entries.len() != plans.len() {
        return Err(Fail::Stopped(format!("the review screen lists {} answers, expected {}", r.entries.len(), plans.len())));
    }
    for (pl, (q, a)) in plans.iter().zip(&r.entries) {
        if !same_text(q, &pl.question) || !same_text(a, &pl.labels.join(", ")) {
            return Err(Fail::Stopped(format!("the review screen shows {q:?} → {a:?}, expected {:?} → {:?}", pl.question, pl.labels.join(", "))));
        }
    }
    if r.on_submit != Some(true) {
        d.press(if r.on_submit == Some(false) { Key::Up } else { Key::Down })?;
        let (_, ok) = d.wait(STEP_TIMEOUT, |s| matches!(s, Screen::Review(r) if r.on_submit == Some(true)))?;
        if !ok {
            return Err(Fail::Stopped("could not put the cursor on \"Submit answers\"".into()));
        }
    }
    d.press(Key::Enter)?;
    let (_, ok) = d.wait(CLOSE_TIMEOUT, |s| *s == Screen::None)?;
    if !ok {
        return Err(Fail::Stopped("the picker did not close after \"Submit answers\"".into()));
    }
    Ok(done(plans))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<String> {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
        std::fs::read_to_string(p).unwrap().lines().map(str::to_string).collect()
    }

    #[test]
    fn parses_first_of_two_questions() {
        let Screen::Question(p) = parse(&fixture("claude-2.1.301-question-1of2.screen.txt")) else { panic!("not a picker") };
        assert_eq!(p.tabs, vec![Tab { label: "Color".into(), answered: false }, Tab { label: "Pets".into(), answered: false }]);
        assert_eq!(p.question, "Pick a color?");
        assert!(!p.multi);
        assert_eq!(p.options.iter().map(|o| o.label.as_str()).collect::<Vec<_>>(), ["Red", "Green", "Blue"]);
        assert_eq!(p.other, "Type something.");
        assert_eq!(p.cursor, Some(Row::Opt(0)));
        assert!(!p.text_mode);
    }

    #[test]
    fn parses_multi_select() {
        let Screen::Question(p) = parse(&fixture("claude-2.1.301-question-2of2.screen.txt")) else { panic!("not a picker") };
        assert_eq!(p.question, "Which pets?");
        assert!(p.multi);
        assert_eq!(p.options.iter().map(|o| o.label.as_str()).collect::<Vec<_>>(), ["Cat", "Dog", "Fish"]);
        assert_eq!(p.other, "Type something");
        assert_eq!(p.cursor, Some(Row::Opt(0)));
    }

    #[test]
    fn parses_older_single_multi_select() {
        let Screen::Question(p) = parse(&fixture("claude-2.1.292-question.screen.txt")) else { panic!("not a picker") };
        assert_eq!(p.tabs.len(), 1);
        assert_eq!(p.tabs[0].label, "Icon test");
        assert!(p.multi);
        assert_eq!(p.options.len(), 4);
        assert!(p.question.starts_with("Right-click any folder"));
    }

    #[test]
    fn parses_review() {
        let Screen::Review(r) = parse(&fixture("claude-2.1.301-question-review.screen.txt")) else { panic!("not a review") };
        assert_eq!(r.entries, vec![("Pick a color?".into(), "Green".into()), ("Which pets?".into(), "Cat, Fish".into())]);
        assert!(!r.incomplete);
        assert_eq!(r.on_submit, Some(true));
    }

    #[test]
    fn other_screens_are_not_pickers() {
        for f in ["claude-2.1.288-permission.screen.txt", "claude-2.1.288-trust-folder.screen.txt", "codex-0.160.0-idle.screen.txt"] {
            assert_eq!(parse(&fixture(f)), Screen::None, "{f}");
        }
    }

    #[test]
    fn cursor_rows() {
        let mut s = fixture("claude-2.1.301-question-2of2.screen.txt");
        for l in s.iter_mut() {
            if l.starts_with("❯ 1.") {
                *l = l.replacen("❯", " ", 1);
            }
            if l.trim() == "Submit" {
                *l = "❯    Submit".into();
            }
        }
        let Screen::Question(p) = parse(&s) else { panic!() };
        assert_eq!(p.cursor, Some(Row::Submit));
    }

    /// A simulated picker that behaves like Claude Code 2.1.301's.
    struct Fake {
        qs: Vec<(String, bool, Vec<String>)>,
        tab: usize,
        cursor: Vec<usize>,
        checked: Vec<Vec<bool>>,
        other: Vec<Option<String>>,
        answered: Vec<bool>,
        review: Option<usize>,
        closed: bool,
        cancelled: bool,
        keys: Vec<Key>,
        /// Clear text field state once the cursor leaves the "Type something" row via Up.
        text_mode: bool,
    }

    impl Fake {
        fn new(qs: &[(&str, bool, &[&str])]) -> Fake {
            let n = qs.len();
            Fake {
                qs: qs.iter().map(|(q, m, o)| (q.to_string(), *m, o.iter().map(|s| s.to_string()).collect())).collect(),
                tab: 0,
                cursor: vec![0; n],
                checked: qs.iter().map(|q| vec![false; q.2.len()]).collect(),
                other: vec![None; n],
                answered: vec![false; n],
                review: None,
                closed: false,
                cancelled: false,
                keys: vec![],
                text_mode: false,
            }
        }
        fn rows(&self) -> usize {
            let (_, multi, o) = &self.qs[self.tab];
            o.len() + if *multi { 3 } else { 2 }
        }
        fn has_review(&self) -> bool {
            self.qs.len() > 1 || self.qs[0].1
        }
        fn advance(&mut self) {
            self.answered[self.tab] = true;
            if self.tab + 1 < self.qs.len() {
                self.tab += 1;
            } else if self.has_review() {
                self.review = Some(0);
            } else {
                self.closed = true;
            }
        }
        fn render(&self) -> Vec<String> {
            let mut l = vec!["❯ some earlier prompt".to_string(), "─".repeat(40)];
            if self.closed {
                return vec!["⏺ User answered Claude's questions:".into(), "".into(), "❯ ".into()];
            }
            let tabs: Vec<String> = self.qs.iter().enumerate().map(|(i, q)| format!("{} {}", if self.answered[i] { '☒' } else { '☐' }, q.0.split(' ').next().unwrap())).collect();
            if self.has_review() {
                l.push(format!("←  {}  ✔ Submit  →", tabs.join("  ")));
            } else {
                l.push(format!(" {}", tabs[0]));
            }
            l.push("".into());
            if let Some(c) = self.review {
                l.push("Review your answers".into());
                l.push("".into());
                for (i, (q, _, o)) in self.qs.iter().enumerate() {
                    l.push(format!(" ● {q}"));
                    let mut a: Vec<String> = o.iter().enumerate().filter(|(j, _)| self.checked[i][*j]).map(|(_, s)| s.clone()).collect();
                    a.extend(self.other[i].clone());
                    if !a.is_empty() {
                        l.push(format!("   → {}", a.join(", ")));
                    }
                }
                l.push("".into());
                l.push("Ready to submit your answers?".into());
                l.push("".into());
                l.push(format!("{} 1. Submit answers", if c == 0 { '❯' } else { ' ' }));
                l.push(format!("{} 2. Cancel", if c == 1 { '❯' } else { ' ' }));
                return l;
            }
            let (q, multi, o) = &self.qs[self.tab];
            l.push(q.clone());
            l.push("".into());
            let c = self.cursor[self.tab];
            let mark = |i: usize| if c == i { "❯ " } else { "  " };
            let boxed = |on: bool| if *multi { if on { "[✔] " } else { "[ ] " } } else { "" };
            for (i, s) in o.iter().enumerate() {
                l.push(format!("{}{}. {}{s}", mark(i), i + 1, boxed(self.checked[self.tab][i])));
                l.push(format!("         about {s}"));
            }
            let m = o.len();
            let other = self.other[self.tab].clone();
            let label = other.clone().unwrap_or_else(|| if *multi { "Type something".into() } else { "Type something.".into() });
            l.push(format!("{}{}. {}{label}", mark(m), m + 1, boxed(other.is_some())));
            if *multi {
                l.push(format!("{}   Submit", if c == m + 1 { "❯ " } else { "  " }));
            }
            l.push("─".repeat(40));
            l.push(format!("{}{}. Chat about this", mark(self.rows() - 1), m + 2));
            l.push("".into());
            let vim = if self.text_mode { " · ctrl+g to edit in Vim" } else { "" };
            l.push(format!("Enter to select · Tab/Arrow keys to navigate{vim} · Esc to cancel"));
            l
        }
    }

    impl Term for Fake {
        fn screen(&mut self) -> Option<Vec<String>> {
            Some(self.render())
        }
        fn key(&mut self, k: Key) -> bool {
            self.keys.push(k);
            if self.closed {
                return true;
            }
            if let Some(c) = self.review {
                match k {
                    Key::Up => self.review = Some(0),
                    Key::Down => self.review = Some(1),
                    Key::Enter if c == 0 => {
                        self.review = None;
                        self.closed = true;
                    }
                    Key::Enter => self.cancelled = true,
                    Key::Escape => self.cancelled = true,
                    Key::Left => {
                        self.review = None;
                        self.tab = self.qs.len() - 1;
                    }
                    Key::Right => {}
                }
                return true;
            }
            let m = self.qs[self.tab].2.len();
            let multi = self.qs[self.tab].1;
            let c = self.cursor[self.tab];
            match k {
                Key::Up => {
                    self.cursor[self.tab] = c.saturating_sub(1);
                    if self.cursor[self.tab] < m {
                        self.text_mode = false;
                    }
                }
                Key::Down => {
                    self.cursor[self.tab] = (c + 1).min(self.rows() - 1);
                    if self.cursor[self.tab] == m {
                        self.text_mode = true;
                    }
                }
                Key::Left | Key::Right if self.text_mode => {}
                Key::Left => self.tab = self.tab.saturating_sub(1),
                Key::Right => {
                    if self.tab + 1 < self.qs.len() {
                        self.tab += 1;
                    } else if self.has_review() {
                        self.review = Some(0);
                    }
                }
                Key::Escape => self.cancelled = true,
                Key::Enter if c < m && multi => self.checked[self.tab][c] ^= true,
                Key::Enter if c < m => {
                    self.checked[self.tab][c] = true;
                    self.advance();
                }
                Key::Enter if c == m && !multi && self.other[self.tab].is_some() => self.advance(),
                Key::Enter if c == m + 1 && multi => self.advance(),
                Key::Enter => {}
            }
            true
        }
        fn text(&mut self, s: &str) -> bool {
            if self.review.is_none() && self.cursor[self.tab] == self.qs[self.tab].2.len() {
                self.other[self.tab] = Some(s.to_string());
            }
            true
        }
        fn pause(&mut self, _: Duration) {}
    }

    fn ans(options: &[u32]) -> QuestionAnswer {
        QuestionAnswer { options: options.to_vec(), ..Default::default() }
    }

    fn two() -> Fake {
        Fake::new(&[("Color pick?", false, &["Red", "Green", "Blue"]), ("Pets which?", true, &["Cat", "Dog", "Fish"])])
    }

    #[test]
    fn answers_two_questions_through_review() {
        let mut f = two();
        let got = answer(&mut f, &[ans(&[1]), ans(&[2, 0])]).unwrap();
        assert!(f.closed && !f.cancelled);
        assert_eq!(got[0].labels, ["Green"]);
        assert_eq!(got[1].labels, ["Cat", "Fish"]);
        assert!(!f.keys.contains(&Key::Escape));
        assert_eq!(f.checked, vec![vec![false, true, false], vec![true, false, true]]);
    }

    #[test]
    fn free_text_answers() {
        let mut f = two();
        let other = |t: &str, o: &[u32]| QuestionAnswer { options: o.to_vec(), other: Some(t.into()), question: None };
        let got = answer(&mut f, &[other("Teal", &[]), other("Zebra", &[1])]).unwrap();
        assert!(f.closed);
        assert_eq!(got[0].labels, ["Teal"]);
        assert_eq!(got[1].labels, ["Dog", "Zebra"]);
    }

    #[test]
    fn single_question_submits_on_enter() {
        let mut f = Fake::new(&[("Size?", false, &["Small", "Medium", "Large"])]);
        answer(&mut f, &[ans(&[2])]).unwrap();
        assert!(f.closed);
        assert_eq!(f.checked[0], [false, false, true]);
    }

    #[test]
    fn starts_from_any_tab_and_cursor() {
        let mut f = two();
        f.tab = 1;
        f.cursor = vec![2, 3];
        f.text_mode = true;
        answer(&mut f, &[ans(&[0]), ans(&[1])]).unwrap();
        assert!(f.closed);
        assert_eq!(f.checked, vec![vec![true, false, false], vec![false, true, false]]);
    }

    #[test]
    fn refuses_before_answering_anything() {
        let cases: Vec<(Vec<QuestionAnswer>, &str)> = vec![
            (vec![ans(&[0])], "2 question(s) but 1 answer(s)"),
            (vec![ans(&[0]), ans(&[3])], "option 3 does not exist"),
            (vec![ans(&[0, 1]), ans(&[0])], "single-select"),
            (vec![ans(&[0]), ans(&[])], "no option picked"),
            (vec![ans(&[0]), QuestionAnswer { other: Some("a\nb".into()), ..Default::default() }], "one line"),
            (vec![QuestionAnswer { options: vec![0], question: Some("Other?".into()), other: None }, ans(&[0])], "different question"),
        ];
        for (a, want) in cases {
            let mut f = two();
            match answer(&mut f, &a) {
                Err(Fail::Refused(m)) => assert!(m.contains(want), "{m:?} lacks {want:?}"),
                r => panic!("{want}: {r:?}"),
            }
            assert!(!f.keys.contains(&Key::Enter) && f.checked.iter().flatten().all(|c| !c), "{want}: pressed {:?}", f.keys);
        }
    }

    #[test]
    fn matching_question_text_is_accepted() {
        let mut f = two();
        let a = QuestionAnswer { options: vec![0], question: Some("  Color   pick?".into()), other: None };
        answer(&mut f, &[a, ans(&[0])]).unwrap();
        assert!(f.closed);
    }

    #[test]
    fn refuses_a_partly_answered_picker() {
        let mut f = two();
        f.answered[0] = true;
        f.checked[0][0] = true;
        f.tab = 1;
        assert!(matches!(answer(&mut f, &[ans(&[0]), ans(&[0])]), Err(Fail::Refused(m)) if m.contains("partly answered")));
        let mut f = two();
        f.checked[1][2] = true;
        assert!(matches!(answer(&mut f, &[ans(&[0]), ans(&[0])]), Err(Fail::Refused(m)) if m.contains("partly answered")));
        assert!(!f.keys.contains(&Key::Enter));
    }

    #[test]
    fn refuses_review_and_plain_screens() {
        let mut f = two();
        f.review = Some(0);
        assert!(matches!(answer(&mut f, &[ans(&[0]), ans(&[0])]), Err(Fail::Refused(m)) if m.contains("review screen")));
        let mut f = two();
        f.closed = true;
        assert!(matches!(answer(&mut f, &[ans(&[0]), ans(&[0])]), Err(Fail::Refused(m)) if m.contains("does not show")));
    }

    /// A picker that ignores Enter: midna stops without cancelling it.
    #[test]
    fn stops_when_an_answer_does_not_take() {
        struct Deaf(Fake);
        impl Term for Deaf {
            fn screen(&mut self) -> Option<Vec<String>> {
                self.0.screen()
            }
            fn key(&mut self, k: Key) -> bool {
                if k != Key::Enter { self.0.key(k) } else { true }
            }
            fn text(&mut self, s: &str) -> bool {
                self.0.text(s)
            }
            fn pause(&mut self, _: Duration) {}
        }
        let mut f = Deaf(two());
        assert!(matches!(answer(&mut f, &[ans(&[0]), ans(&[0])]), Err(Fail::Stopped(m)) if m.contains("did not take")));
        assert!(!f.0.cancelled && !f.0.keys.contains(&Key::Escape));
    }
}
