//! Prompt fast travel: the human's prompts in an agent terminal (`session.prompts`) and
//! scrolling the agent to one (`session.jump_prompt`).
//!
//! Prompts come from `agent.prompt_submitted` events; each carries the agent's conversation id,
//! so prompts sent before a /clear (or a fresh start) are known to be gone from its screen.
//!
//! Claude Code and Codex draw full-screen and scroll their own view, so a jump drives them the
//! way the human would: page keys until the prompt's row shows (read with
//! `midna_proto::prompts`), then wheel ticks to bring it to the top. Claude Code 2.1.289 moves
//! half a screen per PageUp/PageDown and goes back to the bottom on ctrl-end. A shell's
//! scrollback (an agent not drawing full-screen) is searched with the engine's find instead.
//!
//! A newer jump in the same terminal cancels the one still running (holding ⌥⌘↑).
use crate::daemon::Daemon;
use crate::term::RtHandle;
use midna_proto::frame::{ClientMsg, KeyAction, KeyMsg, MOD_CTRL, ScrollKind, ScrollMsg};
use midna_proto::prompts::{self as scr, Here, ScreenScan};
use midna_proto::*;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Most prompts listed for one terminal.
const MAX_PROMPTS: usize = 2000;
/// A jump gives up after this long.
const JUMP_DEADLINE: Duration = Duration::from_secs(12);

static JUMPS: Mutex<Option<HashMap<String, u64>>> = Mutex::new(None);

fn start_jump(sid: &str) -> u64 {
    let mut g = JUMPS.lock().unwrap_or_else(|e| e.into_inner());
    let n = g.get_or_insert_with(HashMap::new).entry(sid.to_string()).or_insert(0);
    *n += 1;
    *n
}

fn still_current(sid: &str, token: u64) -> bool {
    JUMPS.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(sid)).is_none_or(|&n| n == token)
}

/// Every prompt sent to the terminal, oldest first, numbered from 1.
pub fn list(d: &Daemon, sid: &str) -> Vec<PromptMark> {
    let current = d.core().state.session(sid).and_then(|s| s.agent_info.as_ref()).and_then(|a| a.conversation_id.clone());
    let filter = EventFilter { kinds: Some(vec![kinds::AGENT_PROMPT_SUBMITTED.into()]), session_id: Some(sid.into()), project_id: None };
    let events = d.log.list(0, MAX_PROMPTS, &filter);
    let last_conv = events.iter().rev().find_map(|e| e.data.get("conversation").and_then(|c| c.as_str()).map(str::to_string));
    events
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let conversation = e.data.get("conversation").and_then(|c| c.as_str()).map(str::to_string);
            // Older events carry no conversation: count them as current only while no newer
            // prompt says otherwise.
            let on_screen = match (&conversation, current.as_ref().or(last_conv.as_ref())) {
                (Some(c), Some(cur)) => c == cur,
                (None, cur) => cur.is_none() || last_conv.is_none(),
                (Some(_), None) => true,
            };
            PromptMark { n: i as u32 + 1, text: e.data.get("prompt").and_then(|p| p.as_str()).unwrap_or("").to_string(), at: e.at.clone(), conversation, on_screen }
        })
        .collect()
}

struct View {
    lines: Vec<String>,
    cursor: Option<u16>,
    alt: bool,
}

fn view(rt: &RtHandle) -> Option<View> {
    rt.with(|e| e.view()).map(|(lines, cursor, alt)| View { lines, cursor, alt })
}

impl View {
    fn scan(&self) -> ScreenScan {
        let mut s = scr::scan(&self.lines, self.cursor);
        // A shell's scrollback: scrolled back is the viewport's, not the agent's.
        if !self.alt {
            s.scrolled = self.cursor.is_none();
        }
        s
    }
}

/// The prompts the screen can show (their texts) and their `n`s.
fn visible(marks: &[PromptMark]) -> (Vec<String>, Vec<u32>) {
    marks.iter().filter(|m| m.on_screen).map(|m| (m.text.clone(), m.n)).unzip()
}

/// `session.prompts`: the list, and where the view is.
pub fn locate(d: &Daemon, sid: &str) -> Option<SessionPromptsResult> {
    let rt = d.rt(sid)?;
    let prompts = list(d, sid);
    let (texts, ns) = visible(&prompts);
    let v = view(&rt)?;
    let sc = v.scan();
    let a = scr::assign(&sc.rows, &texts, None);
    let here = match scr::here(&sc.rows, &a) {
        Here::At(i) => ns.get(i).copied(),
        _ => None,
    };
    Some(SessionPromptsResult { prompts, here, scrolled: sc.scrolled })
}

pub enum To {
    N(u32),
    Prev,
    Next,
    Latest,
    Live,
}

impl To {
    pub fn parse(n: Option<u32>, to: Option<&str>) -> Result<To, String> {
        Ok(match (n, to) {
            (Some(n), None) => To::N(n),
            (None, Some("prev" | "previous")) => To::Prev,
            (None, Some("next")) => To::Next,
            (None, Some("latest" | "last")) => To::Latest,
            (None, Some("live" | "bottom")) => To::Live,
            (None, Some(t)) => return Err(format!("to must be prev, next, latest or live, not {t}")),
            _ => return Err("give n or to".into()),
        })
    }
}

fn fail(n: Option<u32>, why: impl Into<String>) -> JumpPromptResult {
    JumpPromptResult { ok: false, n, found: false, reason: Some(why.into()) }
}

/// Which prompt `to` means from where the view is now: an index into the visible prompts, or
/// None for the live end.
fn resolve(to: &To, marks: &[PromptMark], ns: &[u32], sc: &ScreenScan, here: Here) -> Result<Option<usize>, JumpPromptResult> {
    let last = ns.len().checked_sub(1);
    match to {
        To::Live => Ok(None),
        To::Latest => last.map(Some).ok_or_else(|| fail(None, "no prompts on screen")),
        To::N(n) => match ns.iter().position(|x| x == n) {
            Some(i) => Ok(Some(i)),
            None if marks.iter().any(|m| m.n == *n) => Err(fail(Some(*n), "sent before a /clear or a fresh start: the agent no longer shows it")),
            None => Err(fail(Some(*n), format!("no prompt {n}"))),
        },
        To::Prev if !sc.scrolled => last.map(Some).ok_or_else(|| fail(None, "no prompts on screen")),
        To::Prev => match here {
            Here::At(i) if i > 0 => Ok(Some(i - 1)),
            Here::At(_) | Here::BeforeFirst => Err(fail(ns.first().copied(), "already at the first prompt")),
            Here::Unknown => Err(fail(None, "can't tell which prompt the view is in")),
        },
        To::Next if !sc.scrolled => Err(fail(None, "already at the live end")),
        To::Next => match here {
            Here::At(i) if i + 1 < ns.len() => Ok(Some(i + 1)),
            Here::At(_) => Ok(None),
            Here::BeforeFirst => ns.first().map(|_| Some(0)).ok_or_else(|| fail(None, "no prompts on screen")),
            Here::Unknown => Err(fail(None, "can't tell which prompt the view is in")),
        },
    }
}

fn press(rt: &RtHandle, key: &str, mods: u8) {
    rt.client(ClientMsg::Key(KeyMsg { action: KeyAction::Press, mods, key: key.into(), text: String::new() }));
}

/// Wheel ticks toward the live end (positive) or history (negative).
fn wheel(rt: &RtHandle, ticks: i32) {
    rt.client(ClientMsg::Scroll(ScrollMsg { kind: ScrollKind::Wheel, amount: ticks.clamp(-20, 20), x: 2., y: 2., mods: 0 }));
}

/// The screen once it has changed from `before` and stopped changing; None when it didn't
/// change at all (the agent is at the end it was scrolled toward).
fn changed(rt: &RtHandle, before: &[String]) -> Option<View> {
    let t0 = Instant::now();
    let mut seen: Option<View> = None;
    loop {
        std::thread::sleep(Duration::from_millis(30));
        let v = view(rt)?;
        match &seen {
            Some(s) if s.lines == v.lines => return Some(v),
            _ if v.lines != before => seen = Some(v),
            _ if t0.elapsed() > Duration::from_millis(700) => return None,
            _ => {}
        }
        if t0.elapsed() > Duration::from_millis(1500) {
            return seen;
        }
    }
}

/// `session.jump_prompt`, waiting until the prompt shows (or the jump gives up).
pub fn jump(d: &Daemon, sid: &str, to: To) -> JumpPromptResult {
    let Some(rt) = d.rt(sid) else { return fail(None, "no such terminal") };
    let token = start_jump(sid);
    let marks = list(d, sid);
    let (texts, ns) = visible(&marks);
    let Some(v) = view(&rt) else { return fail(None, "the terminal did not answer") };
    let sc = v.scan();
    let here = scr::here(&sc.rows, &scr::assign(&sc.rows, &texts, None));
    let target = match resolve(&to, &marks, &ns, &sc, here) {
        Ok(t) => t,
        Err(r) => return r,
    };
    let Some(t) = target else {
        if v.alt {
            press(&rt, "end", MOD_CTRL);
        } else {
            rt.client(ClientMsg::Scroll(ScrollMsg { kind: ScrollKind::Bottom, amount: 0, x: 0., y: 0., mods: 0 }));
        }
        return JumpPromptResult { ok: true, n: None, found: true, reason: None };
    };
    let n = ns[t];
    if !v.alt {
        return find_in_scrollback(&rt, &texts, t, n);
    }
    let deadline = Instant::now() + JUMP_DEADLINE;
    let mut v = v;
    let mut last_dir: Option<bool> = None; // true = up
    let mut turns = 0;
    let mut pages = 0; // page presses that moved the view
    let mut aligning = false; // its row has been on screen
    loop {
        if !still_current(sid, token) {
            return fail(Some(n), "a newer jump took over");
        }
        if Instant::now() > deadline {
            return fail(Some(n), "gave up scrolling");
        }
        let sc = v.scan();
        let a = scr::assign(&sc.rows, &texts, Some(t));
        if std::env::var_os("MIDNA_DEBUG_JUMP").is_some() {
            eprintln!("midnad jump {sid} -> {n}: rows {:?} assigned {a:?} scrolled {} aligning {aligning}", sc.rows.iter().map(|r| r.0).collect::<Vec<_>>(), sc.scrolled);
        }
        // Row 0 doesn't count: Claude Code pins the prompt being read there, while its real
        // row is further up.
        let row = sc.rows.iter().zip(&a).find(|(r, a)| **a == Some(t) && r.0 >= 1).map(|(r, _)| r.0 as i32);
        if let Some(row) = row {
            // On screen: bring it up to row 1, under the prompt pinned on row 0. PageDown
            // moves half the view; a lone wheel tick moves one row (a burst of ticks
            // accelerates, so they go one at a time).
            aligning = true;
            if row <= 1 {
                return JumpPromptResult { ok: true, n: Some(n), found: true, reason: None };
            }
            let half = v.cursor.map_or(v.lines.len() as i32, |c| c as i32) / 2;
            if row - 1 > half + 1 {
                press(&rt, "pagedown", 0);
            } else {
                wheel(&rt, 1);
            }
            let Some(nv) = changed(&rt, &v.lines) else {
                // The live end: it can't come any higher.
                return JumpPromptResult { ok: true, n: Some(n), found: true, reason: None };
            };
            v = nv;
            continue;
        }
        if aligning && sc.rows.iter().zip(&a).any(|(r, a)| *a == Some(t) && r.0 == 0) {
            // Overshot: it went past the top and is only the pinned row. Back a row at a time.
            wheel(&rt, -1);
            match changed(&rt, &v.lines) {
                Some(nv) => v = nv,
                None => return JumpPromptResult { ok: true, n: Some(n), found: true, reason: None },
            }
            continue;
        }
        let here = scr::here(&sc.rows, &a);
        let up = match here {
            Here::At(i) => t <= i,
            Here::BeforeFirst => false,
            Here::Unknown => last_dir.unwrap_or(true),
        };
        if last_dir.is_some_and(|d| d != up) {
            turns += 1;
            if turns > 3 {
                return fail(Some(n), "its row never showed (the agent may draw it differently)");
            }
        }
        last_dir = Some(up);
        press(&rt, if up { "pageup" } else { "pagedown" }, 0);
        match changed(&rt, &v.lines) {
            Some(nv) => {
                v = nv;
                pages += 1;
            }
            // At the very top the first row is real, not pinned.
            None if up && sc.rows.iter().zip(&a).any(|(r, a)| *a == Some(t) && r.0 == 0) => {
                return JumpPromptResult { ok: true, n: Some(n), found: true, reason: None };
            }
            None if last_dir.is_some() && pages == 0 => return fail(Some(n), "the agent's view didn't scroll (is a dialog or menu open?)"),
            None => return fail(Some(n), format!("reached the {} without finding it", if up { "top" } else { "bottom" })),
        }
    }
}

/// A terminal that keeps its own scrollback: find the prompt's row with the engine's search,
/// which scrolls to it and selects it. Repeated prompts are counted back from the newest.
fn find_in_scrollback(rt: &RtHandle, texts: &[String], t: usize, n: u32) -> JumpPromptResult {
    let key: String = scr::key(&texts[t]).chars().take(60).collect();
    if key.is_empty() {
        return fail(Some(n), "the prompt has no text to find");
    }
    let later = texts[t + 1..].iter().filter(|p| scr::key(p).starts_with(&key)).count();
    let found = rt
        .with(move |e| {
            e.find("", false);
            let mut h = e.find(&key, true);
            for _ in 0..later {
                h = e.find(&key, true);
            }
            h.total > 0
        })
        .unwrap_or(false);
    if found { JumpPromptResult { ok: true, n: Some(n), found: true, reason: None } } else { fail(Some(n), "not in the scrollback any more") }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark(n: u32, conv: &str) -> PromptMark {
        PromptMark { n, text: format!("prompt {n}"), at: String::new(), conversation: Some(conv.into()), on_screen: true }
    }

    #[test]
    fn prev_and_next_from_where_the_view_is() {
        let marks = vec![mark(1, "a"), mark(2, "a"), mark(3, "a")];
        let ns = vec![1, 2, 3];
        let live = ScreenScan::default();
        let back = ScreenScan { rows: vec![], scrolled: true };
        assert_eq!(resolve(&To::Prev, &marks, &ns, &live, Here::At(2)).ok(), Some(Some(2)), "from live: the last prompt");
        assert_eq!(resolve(&To::Prev, &marks, &ns, &back, Here::At(1)).ok(), Some(Some(0)));
        assert!(resolve(&To::Prev, &marks, &ns, &back, Here::At(0)).is_err());
        assert_eq!(resolve(&To::Next, &marks, &ns, &back, Here::At(1)).ok(), Some(Some(2)));
        assert_eq!(resolve(&To::Next, &marks, &ns, &back, Here::At(2)).ok(), Some(None), "past the last: live");
        assert!(resolve(&To::Next, &marks, &ns, &live, Here::At(2)).is_err());
        assert_eq!(resolve(&To::Next, &marks, &ns, &back, Here::BeforeFirst).ok(), Some(Some(0)));
    }

    #[test]
    fn prompts_before_a_clear_are_refused() {
        let mut old = mark(1, "a");
        old.on_screen = false;
        let marks = vec![old, mark(2, "b")];
        let ns = vec![2];
        let r = resolve(&To::N(1), &marks, &ns, &ScreenScan::default(), Here::Unknown).unwrap_err();
        assert!(r.reason.unwrap().contains("/clear"));
        assert_eq!(resolve(&To::N(2), &marks, &ns, &ScreenScan::default(), Here::Unknown).ok(), Some(Some(0)));
    }
}
