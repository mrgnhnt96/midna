//! Prompt fast travel in an agent terminal: a bar pinned over the top row while the agent's
//! view is scrolled back (which prompt you're reading, ‹ › to step, the title scrolls to that
//! prompt, ▾ opens the searchable list in the command bar, Live to go back), a rail of ticks on the right edge (one
//! per prompt; hover for the text, click to jump), and ⌥⌘↑ ⌥⌘↓.
//!
//! The prompts come from `session.prompts`, fetched when the view opens and again whenever the
//! screen shows a prompt row the list doesn't know (a new prompt, or a new conversation after
//! /clear). Where the view is comes from reading each frame with `midna_proto::prompts`; the
//! scrolling itself is midnad's (`session.jump_prompt`), since the agent scrolls its own view.
use super::{LINE_H, PAD_Y, TerminalView};
use crate::actions::{NextPrompt, OpenPrompts, PrevPrompt};
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use midna_proto::PromptMark;
use midna_proto::prompts::{self as scr, Here};
use serde_json::json;
use std::collections::HashSet;
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct PromptNav {
    pub prompts: Vec<PromptMark>,
    /// The prompt the top of the view belongs to (index into `prompts`).
    here: Option<usize>,
    before_first: bool,
    /// The agent's view (or the scrollback) is scrolled back from the live end.
    scrolled: bool,
    /// Rail tick under the pointer.
    hover: Option<usize>,
    /// Prompt rows the list didn't know, already refetched for.
    asked: HashSet<String>,
    fetching: bool,
    /// The jump last asked for and when: ⌥⌘↑ pressed again before the screen catches up steps
    /// from there, not from wherever the scrolling has got to.
    pending: Option<(usize, Instant)>,
}

impl PromptNav {
    /// Indices of the prompts the agent still shows (not before a /clear).
    fn on_screen(&self) -> Vec<usize> {
        self.prompts.iter().enumerate().filter(|(_, p)| p.on_screen).map(|(i, _)| i).collect()
    }

    /// Read the screen: where the view is, and whether it shows prompts the list lacks.
    fn scan(&mut self, lines: &[String], cursor: Option<u16>, alt: bool, at_bottom: bool) -> bool {
        let vis = self.on_screen();
        let texts: Vec<String> = vis.iter().map(|&i| self.prompts[i].text.clone()).collect();
        let sc = scr::scan(lines, cursor);
        let hint = self.here.and_then(|h| vis.iter().position(|&i| i == h));
        let a = scr::assign(&sc.rows, &texts, hint);
        self.scrolled = if alt { sc.scrolled } else { !at_bottom };
        match scr::here(&sc.rows, &a) {
            Here::At(i) => {
                self.here = vis.get(i).copied();
                self.before_first = false;
            }
            Here::BeforeFirst => {
                self.here = None;
                self.before_first = true;
            }
            Here::Unknown if !self.scrolled => {
                self.here = None;
                self.before_first = false;
            }
            Here::Unknown => {}
        }
        if let Some((n, at)) = self.pending
            && (self.here == Some(n) || at.elapsed() > Duration::from_secs(3))
        {
            self.pending = None;
        }
        let mut unknown = false;
        for ((_, text), a) in sc.rows.iter().zip(&a) {
            // Slash commands never become prompts: ask once per text.
            if a.is_none() && !text.starts_with('/') && self.asked.insert(text.clone()) {
                unknown = true;
            }
        }
        unknown
    }

    /// The prompt ⌥⌘↑ goes to: from the live end, the last one.
    fn prev(&self) -> Option<usize> {
        let vis = self.on_screen();
        let from = self.pending.map(|p| p.0).or(self.here.filter(|_| self.scrolled));
        match from {
            Some(i) => vis.iter().rev().find(|&&j| j < i).copied(),
            None if self.before_first => None,
            None => vis.last().copied(),
        }
    }

    /// The prompt ⌥⌘↓ goes to, or Err(()) for the live end; None when already live.
    fn next(&self) -> Option<Result<usize, ()>> {
        let vis = self.on_screen();
        if self.before_first && self.pending.is_none() {
            return vis.first().map(|&i| Ok(i));
        }
        let from = self.pending.map(|p| p.0).or(self.here.filter(|_| self.scrolled))?;
        Some(vis.iter().find(|&&j| j > from).map(|&j| Ok(j)).unwrap_or(Err(())))
    }
}

impl TerminalView {
    /// Fetch the prompt list (agent terminals only).
    pub(super) fn refresh_prompts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.nav.fetching {
            return;
        }
        self.nav.fetching = true;
        self.call("session.prompts", json!({ "id": self.session_id }), window, cx, |t, r, _, cx| {
            t.nav.fetching = false;
            if let Ok(v) = r
                && let Ok(list) = serde_json::from_value::<Vec<PromptMark>>(v["prompts"].clone())
            {
                t.nav.prompts = list;
                cx.notify();
            }
        });
    }

    /// Per render: read the screen; refetch when it shows a prompt the list doesn't have.
    pub(super) fn update_nav(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_agent() {
            return;
        }
        let lines: Vec<String> = self.grid.iter().map(|r| r.text()).collect();
        let at_bottom = self.ext.at_bottom();
        let cursor = self.cursor.filter(|_| at_bottom).map(|c| c.1);
        if self.nav.scan(&lines, cursor, self.ext.alt_screen, at_bottom) {
            self.refresh_prompts(window, cx);
        }
    }

    /// Ask midnad to scroll the agent to prompt `i` (an index into the list), or to the live end.
    fn jump_to(&mut self, target: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let params = match target.and_then(|i| self.nav.prompts.get(i).map(|p| (i, p.n))) {
            Some((i, n)) => {
                self.nav.pending = Some((i, Instant::now()));
                json!({ "id": self.session_id, "n": n, "wait": false })
            }
            None => {
                self.nav.pending = None;
                json!({ "id": self.session_id, "to": "live", "wait": false })
            }
        };
        self.call("session.jump_prompt", params, window, cx, |_, _, _, _| {});
        cx.notify();
    }

    pub(super) fn on_prev_prompt(&mut self, _: &PrevPrompt, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(i) = self.nav.prev() {
            self.jump_to(Some(i), window, cx);
        }
    }

    pub(super) fn on_next_prompt(&mut self, _: &NextPrompt, window: &mut Window, cx: &mut Context<Self>) {
        match self.nav.next() {
            Some(Ok(i)) => self.jump_to(Some(i), window, cx),
            Some(Err(())) => self.jump_to(None, window, cx),
            None => {}
        }
    }

    /// The pinned bar and the rail, over the grid.
    pub(super) fn render_nav(&self, theme: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let nav = &self.nav;
        if !self.is_agent() || nav.prompts.is_empty() {
            return vec![];
        }
        let mut out = vec![self.render_rail(theme, cx)];
        if nav.scrolled {
            out.push(self.render_bar(theme, cx));
        }
        out
    }

    fn render_bar(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let nav = &self.nav;
        let shown = nav.pending.map(|p| p.0).or(nav.here);
        let vis = nav.on_screen();
        let (num, title, meta) = match shown.and_then(|i| nav.prompts.get(i).map(|p| (i, p))) {
            Some((i, p)) => {
                let pos = vis.iter().position(|&j| j == i).map(|k| format!(" · {} of {}", k + 1, vis.len())).unwrap_or_default();
                (format!("#{}", p.n), scr::key(&p.text), format!("{}{pos}", hhmm(&p.at)))
            }
            None if nav.before_first => (String::new(), "Before your first prompt".to_string(), String::new()),
            None => (String::new(), "Your prompts".to_string(), String::new()),
        };
        let stop = |d: Stateful<Div>| d.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        let btn = |id: &'static str| stop(div().id(id)).h(px(24.)).min_w(px(24.)).px(px(6.)).flex().items_center().justify_center().rounded(px(5.)).cursor_pointer().text_color(t.fg).hover(|s| s.bg(t.raised));
        let can_prev = nav.prev().is_some();
        stop(div().id("prompt-bar"))
            .absolute()
            .top(px(0.))
            .left(px(0.))
            .right(px(0.))
            .h(px(PAD_Y + LINE_H))
            .flex()
            .items_center()
            .gap(px(4.))
            .px(px(8.))
            .bg(t.panel)
            .border_b_1()
            .border_color(t.line)
            .text_size(px(12.5))
            .font_family(t.ui_font.clone())
            .child(btn("prompt-prev").when(!can_prev, |d| d.opacity(0.35)).child("‹").on_click(cx.listener(|v, _, w, cx| v.on_prev_prompt(&PrevPrompt, w, cx))))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .items_center()
                    .rounded(px(5.))
                    .bg(t.raised)
                    .child(
                        btn("prompt-title")
                            .flex_1()
                            .min_w(px(0.))
                            .justify_start()
                            .gap(px(10.))
                            .px(px(10.))
                            .child(div().font_family(t.mono_font.clone()).text_size(px(11.5)).text_color(t.accent).child(num))
                            .child(div().flex_1().min_w(px(0.)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(title))
                            .on_click(cx.listener(move |v, _, w, cx| match shown {
                                // Scroll so the prompt itself is in view, not just its reply.
                                Some(i) => v.jump_to(Some(i), w, cx),
                                None => w.dispatch_action(Box::new(OpenPrompts), cx),
                            })),
                    )
                    .child(btn("prompt-list").text_color(t.dim).child("▾").on_click(|_, w, cx| w.dispatch_action(Box::new(OpenPrompts), cx))),
            )
            .child(div().px(px(6.)).text_size(px(11.5)).text_color(t.dim).whitespace_nowrap().child(meta))
            .child(btn("prompt-next").child("›").on_click(cx.listener(|v, _, w, cx| v.on_next_prompt(&NextPrompt, w, cx))))
            .child(btn("prompt-live").text_color(t.dim).text_size(px(11.5)).child("Live ↓").on_click(cx.listener(|v, _, w, cx| v.jump_to(None, w, cx))))
            .into_any_element()
    }

    fn render_rail(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let nav = &self.nav;
        let n = nav.prompts.len();
        let current = nav.pending.map(|p| p.0).or(nav.here.filter(|_| nav.scrolled));
        let top = if nav.scrolled { PAD_Y + LINE_H } else { PAD_Y };
        let mut rail = div().id("prompt-rail").absolute().top(px(top)).bottom(px(PAD_Y)).right(px(2.)).w(px(14.));
        for (i, p) in nav.prompts.iter().enumerate() {
            let frac = (i as f32 + 0.5) / n as f32;
            let on = current == Some(i);
            let hovered = nav.hover == Some(i);
            let color = if !p.on_screen {
                t.dim.opacity(0.35)
            } else if on {
                t.fg
            } else if hovered {
                t.accent
            } else {
                t.accent.opacity(0.55)
            };
            let bar = div().w(px(if on || hovered { 12. } else { 8. })).h(px(if on { 4. } else { 3. })).rounded(px(2.)).bg(color);
            let mut tick = div()
                .id(("prompt-tick", i))
                .absolute()
                .top(relative(frac))
                .mt(px(-4.))
                .left(px(0.))
                .w(px(14.))
                .h(px(8.))
                .flex()
                .items_center()
                .justify_end()
                .child(bar)
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_hover(cx.listener(move |v, on: &bool, _, cx| {
                    let h = if *on { Some(i) } else { v.nav.hover.filter(|&x| x != i) };
                    if v.nav.hover != h {
                        v.nav.hover = h;
                        cx.notify();
                    }
                }));
            if p.on_screen {
                tick = tick.cursor_pointer().on_click(cx.listener(move |v, _, w, cx| v.jump_to(Some(i), w, cx)));
            }
            rail = rail.child(tick);
        }
        if let Some(p) = nav.hover.and_then(|i| nav.prompts.get(i).map(|p| (i, p))) {
            let (i, p) = p;
            let frac = (i as f32 + 0.5) / n as f32;
            let sub = if p.on_screen { hhmm(&p.at) } else { format!("{} · before /clear", hhmm(&p.at)) };
            rail = rail.child(
                div()
                    .absolute()
                    .top(relative(frac))
                    .mt(px(-22.))
                    .right(px(20.))
                    .w(px(300.))
                    .px(px(12.))
                    .py(px(8.))
                    .rounded(px(8.))
                    .bg(t.panel)
                    .border_1()
                    .border_color(t.line)
                    .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.35), offset: point(px(0.), px(8.)), blur_radius: px(24.), spread_radius: px(0.), inset: false }])
                    .font_family(t.ui_font.clone())
                    .child(div().flex().justify_between().text_size(px(11.)).text_color(t.dim).child(format!("#{}", p.n)).child(sub))
                    .child(div().mt(px(3.)).text_size(px(12.5)).text_color(t.fg).overflow_hidden().whitespace_nowrap().text_ellipsis().child(scr::key(&p.text))),
            );
        }
        rail.into_any_element()
    }
}

/// "14:17" (local).
fn hhmm(at: &str) -> String {
    let c = crate::ui::screen_kit::clock(at);
    c.get(..5).unwrap_or(&c).to_string()
}

#[cfg(test)]
mod tests {
    use super::PromptNav;
    use midna_proto::PromptMark;
    use std::time::Instant;

    fn nav(texts: &[&str]) -> PromptNav {
        let prompts = texts.iter().enumerate().map(|(i, t)| PromptMark { n: i as u32 + 1, text: t.to_string(), on_screen: true, ..Default::default() }).collect();
        PromptNav { prompts, ..Default::default() }
    }

    const RULE: &str = "────────────────────────────────────────";

    #[test]
    fn steps_from_the_screen_then_from_the_pending_jump() {
        let mut n = nav(&["alpha one two", "bravo one two", "charlie one two"]);
        // live: ⌥⌘↑ goes to the last prompt, ⌥⌘↓ does nothing
        let live: Vec<String> = ["  30. Norway", RULE, "❯", RULE].map(String::from).to_vec();
        assert!(!n.scan(&live, Some(2), true, true));
        assert_eq!(n.prev(), Some(2));
        assert_eq!(n.next(), None);
        // scrolled back into bravo's reply
        let back: Vec<String> = ["❯ bravo one two", "  12. Peach", "  13. Pear  Jump to bottom: fn+↓ to scroll", RULE, "❯", RULE].map(String::from).to_vec();
        assert!(!n.scan(&back, Some(4), true, true));
        assert!(n.scrolled);
        assert_eq!(n.here, Some(1));
        assert_eq!(n.prev(), Some(0));
        assert_eq!(n.next(), Some(Ok(2)));
        // a jump to alpha is on its way: the next ⌥⌘↓ steps from alpha
        n.pending = Some((0, Instant::now()));
        assert_eq!(n.next(), Some(Ok(1)));
        assert_eq!(n.prev(), None);
        n.pending = Some((2, Instant::now()));
        assert_eq!(n.next(), Some(Err(())), "past the last: live");
    }

    #[test]
    fn an_unknown_prompt_row_asks_for_the_list_once() {
        let mut n = nav(&["alpha one two"]);
        let s: Vec<String> = ["❯ alpha one two", "", "❯ a brand new prompt", "", "❯ /cost", RULE, "❯", RULE].map(String::from).to_vec();
        assert!(n.scan(&s, Some(6), true, true));
        assert!(!n.scan(&s, Some(6), true, true), "asked already");
    }
}
