//! Prompt fast travel in an agent terminal: a bar floating on the top row (Live ↓ while scrolled
//! back, then a pill with the prompt you're reading; click it for the searchable list in the
//! command bar), shown while the agent's view is scrolled back, or always with
//! `terminal.prompt_bar` = always (the default); a rail of ticks on the right edge (one per
//! prompt; hover for the text, click to jump); and `keys.prev_prompt` / `keys.next_prompt`.
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
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// `terminal.prompt_bar` = always.
#[derive(Default)]
struct AlwaysBar(bool);

impl Global for AlwaysBar {}

/// Called by the main window whenever settings change.
pub fn share(always: bool, cx: &mut App) {
    cx.set_global(AlwaysBar(always));
}

/// Height of the bar.
pub(super) const BAR_H: f32 = PAD_Y + LINE_H;

/// Live ↓: its width (the gap after it included), and how long it takes to slide in or out.
const LIVE_W: f32 = 70.;
const LIVE_MS: f32 = 180.;

/// How long a frame without Claude's scrolled-back hint is taken for one it is still painting
/// (scrolling redraws the screen) rather than the view reaching the live end.
const HINT_HOLD: Duration = Duration::from_millis(250);

/// The prompt pill's width, eased to its new content's (another prompt's title) over `LIVE_MS`.
/// `to` = 0 until first measured.
#[derive(Default)]
struct PillWidth {
    from: f32,
    to: f32,
    at: Option<Instant>,
}

impl PillWidth {
    fn now(&self) -> Option<f32> {
        let e = self.at.map_or(1., |at| 1. - (1. - (at.elapsed().as_secs_f32() * 1000. / LIVE_MS).min(1.)).powi(3));
        (self.to > 0.).then(|| self.from + (self.to - self.from) * e)
    }

    fn moving(&self) -> bool {
        self.at.is_some_and(|at| at.elapsed().as_secs_f32() * 1000. < LIVE_MS)
    }

    /// Its content measured `w` wide (at prepaint): ease there from where it is.
    fn measured(&mut self, w: f32, animate: bool) -> bool {
        if (w - self.to).abs() < 0.5 {
            return false;
        }
        self.from = if animate { self.now().unwrap_or(w) } else { w };
        self.to = w;
        self.at = animate.then(Instant::now);
        animate
    }
}

#[derive(Default)]
pub struct PromptNav {
    pub prompts: Vec<PromptMark>,
    /// The prompt the top of the view belongs to (index into `prompts`).
    here: Option<usize>,
    before_first: bool,
    /// The oldest prompt whose row is on screen.
    top_shown: Option<usize>,
    /// The prompt Claude pins on row 0 while scrolled back, as it reads: the bar's title when
    /// the list doesn't have it (e.g. a conversation resumed here, its prompts typed elsewhere).
    pinned: Option<String>,
    /// Live ↓ is in the bar, and since when it has been sliding in or out (`LIVE_MS`).
    live: bool,
    live_at: Option<Instant>,
    /// The pointer is on the bar, so its → (to the prompt it names) is out, and since when it
    /// has been sliding in or out (`LIVE_MS`).
    go: bool,
    go_at: Option<Instant>,
    pill: Rc<RefCell<PillWidth>>,
    /// The agent's view (or the scrollback) is scrolled back from the live end.
    scrolled: bool,
    /// When the agent last drew its scrolled-back hint (`HINT_HOLD`).
    hint_at: Option<Instant>,
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
        if alt && sc.scrolled {
            self.hint_at = Some(Instant::now());
        } else if alt && self.scrolled && self.holding() {
            // Half-painted: keep the last frame's reading, or Live ↓ flickers while scrolling.
            return false;
        }
        let hint = self.here.and_then(|h| vis.iter().position(|&i| i == h));
        let a = scr::assign(&sc.rows, &texts, hint);
        self.scrolled = if alt { sc.scrolled } else { !at_bottom };
        self.top_shown = a.iter().flatten().next().and_then(|&i| vis.get(i).copied());
        self.pinned = sc.rows.first().filter(|(r, _)| *r == 0 && self.scrolled).map(|(_, t)| scr::key(t));
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

    /// Scrolled back, the hint missing from the screen for less than `HINT_HOLD`.
    fn holding(&self) -> bool {
        self.hint_at.is_some_and(|at| at.elapsed() < HINT_HOLD)
    }

    /// The prompt ⌥⌘↑ goes to: from the live end, the last one not already on screen.
    fn prev(&self) -> Option<usize> {
        let vis = self.on_screen();
        let from = self.pending.map(|p| p.0).or(self.here.filter(|_| self.scrolled));
        match from {
            Some(i) => vis.iter().rev().find(|&&j| j < i).copied(),
            None if self.before_first => None,
            None => match self.top_shown {
                Some(top) => vis.iter().rev().find(|&&j| j < top).copied(),
                None => vis.last().copied(),
            },
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
    /// The bar shows at the live end too (`terminal.prompt_bar` = always, agents only).
    pub(super) fn bar_row(&self, cx: &App) -> bool {
        self.is_agent() && cx.try_global::<AlwaysBar>().is_some_and(|a| a.0)
    }

    /// The grid's top row is Claude's pinned copy of the prompt: the grid slides up a row so the
    /// next line sits on the bar's row instead.
    pub(super) fn hides_top_row(&self) -> bool {
        self.is_agent() && self.nav.pinned.is_some()
    }

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
        let live = self.nav.scrolled || self.nav.pending.is_some();
        if live != self.nav.live {
            self.nav.live = live;
            gpui_kit::base::apply_system_reduce_motion(cx);
            self.nav.live_at = (!cx.reduce_motion()).then(Instant::now);
        }
        // While holding, look again: the agent may not paint another frame once it is live.
        let moving = |at: Option<Instant>| at.is_some_and(|at| at.elapsed().as_secs_f32() * 1000. < LIVE_MS);
        let anim = moving(self.nav.live_at) || moving(self.nav.go_at) || self.nav.pill.borrow().moving();
        if anim || (self.nav.scrolled && self.ext.alt_screen && self.nav.holding()) {
            window.request_animation_frame();
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
        let row = self.bar_row(cx);
        let mut out = vec![];
        // With no prompts listed, the bar shows only to cover Claude's own pinned copy of the
        // prompt on row 0 (scrolled back); otherwise there's nothing for it to name.
        if !self.is_agent() || (nav.prompts.is_empty() && (nav.pinned.is_none() || !(row || nav.scrolled))) {
            return out;
        }
        // The bar floats on the grid's top row. Claude's pinned copy of the prompt there (which the
        // bar already names) slides out above (`hides_top_row`): cover what's left in the padding.
        if self.hides_top_row() {
            out.push(div().absolute().top_0().left_0().right_0().h(px(PAD_Y)).bg(theme.term).into_any_element());
        }
        if !nav.prompts.is_empty() {
            out.push(self.render_rail(theme, cx));
        }
        // The bar's pill only names a prompt (never "Before your first prompt"): without one, the
        // bar is just Live ↓ while scrolled back.
        if nav.scrolled || (row && self.shown_prompt().is_some()) {
            out.push(self.render_bar(theme, cx));
        }
        out
    }

    /// The prompt the bar names: the one being jumped to, or the one the view is in (an index
    /// into the list), at the live end the last one sent; failing that, Claude's pinned copy of it.
    fn shown_prompt(&self) -> Option<(Option<usize>, &str)> {
        let nav = &self.nav;
        let live = || nav.on_screen().last().copied().filter(|_| !nav.scrolled);
        match nav.pending.map(|p| p.0).or(nav.here).or_else(live).and_then(|i| nav.prompts.get(i).map(|p| (i, p))) {
            Some((i, p)) => Some((Some(i), p.text.as_str())),
            None if nav.before_first => None,
            None => nav.pinned.as_deref().map(|t| (None, t)),
        }
    }

    fn render_bar(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let nav = &self.nav;
        let vis = nav.on_screen();
        let shown = self.shown_prompt();
        let target = shown.and_then(|s| s.0);
        let (num, title, pos) = match shown {
            Some((Some(i), text)) => {
                let pos = vis.iter().position(|&j| j == i).map(|k| format!("{}/{}", k + 1, vis.len())).unwrap_or_default();
                (format!("#{}", nav.prompts[i].n), scr::key(text), pos)
            }
            Some((None, text)) => (String::new(), text.to_string(), String::new()),
            None => (String::new(), String::new(), String::new()),
        };
        let eased = |at: Option<Instant>| at.map_or(1., |at| 1. - (1. - (at.elapsed().as_secs_f32() * 1000. / LIVE_MS).min(1.)).powi(3));
        // Live ↓ slides in from the left edge, pushing the pill over, and back out at the live end.
        let e = eased(nav.live_at);
        let live = if nav.live { e } else { 1. - e };
        // → grows out of the pill's right while the pointer is on the bar.
        let e = eased(nav.go_at);
        let go = if target.is_none() { 0. } else if nav.go { e } else { 1. - e };
        let stop = |d: Stateful<Div>| d.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        // Live ↓ leads while scrolled back, then the prompt pill (a click opens the list); the
        // rest of the row stays the terminal's.
        // Only as wide as its buttons, centred on the top row: the rest of the row stays the
        // terminal's, text and clicks.
        stop(div().id("prompt-bar"))
            .absolute()
            .top(px(PAD_Y + (LINE_H - 24.) / 2.))
            .left(px(0.))
            .h(px(24.))
            .flex()
            .items_center()
            .px(px(8.))
            .text_size(px(12.5))
            .font_family(t.ui_font.clone())
            .on_hover(cx.listener(|v, on: &bool, _, cx| {
                if v.nav.go != *on {
                    v.nav.go = *on;
                    gpui_kit::base::apply_system_reduce_motion(cx);
                    v.nav.go_at = (!cx.reduce_motion()).then(Instant::now);
                    cx.notify();
                }
            }))
            .when(live > 0., |d| {
                d.child(div().flex_none().w(px(LIVE_W * live)).h(px(24.)).overflow_hidden().opacity(live).child(
                    stop(div().id("prompt-live"))
                        .w(px(LIVE_W - 6.))
                        .h(px(24.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(6.))
                        .cursor_pointer()
                        .bg(t.accent)
                        .text_color(t.accent_fg)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(12.))
                        .hover(|s| s.opacity(0.85))
                        .child("Live ↓")
                        .on_click(cx.listener(|v, _, w, cx| v.jump_to(None, w, cx))),
                ))
            })
            .when(!title.is_empty(), |d| d.child(
                // The pill is as wide as its content (up to 520px), easing to a new prompt's width.
                stop(div().id("prompt-title"))
                    .flex_none()
                    .when_some(nav.pill.borrow().now(), |d, w| d.w(px(w + 2.)))
                    .h(px(24.))
                    .overflow_hidden()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(t.line)
                    .bg(t.raised)
                    .text_color(t.fg)
                    .cursor_pointer()
                    .hover(|s| s.border_color(t.dim))
                    .child(div().flex().h_full().on_children_prepainted({
                        let pill = nav.pill.clone();
                        move |b, window, cx| {
                            if let Some(b) = b.first()
                                && pill.borrow_mut().measured(f32::from(b.size.width), !cx.reduce_motion())
                            {
                                window.request_animation_frame();
                            }
                        }
                    }).child(
                        div()
                            .flex_none()
                            .max_w(px(518.))
                            .h_full()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .pl(px(10.))
                            .pr(px(8.))
                            .when(!num.is_empty(), |d| d.child(div().flex_none().font_family(t.mono_font.clone()).text_size(px(11.5)).text_color(t.accent).child(num)))
                            .child(div().min_w(px(0.)).overflow_hidden().whitespace_nowrap().text_ellipsis().child(title))
                            .when(!pos.is_empty(), |d| d.child(div().flex_none().text_size(px(11.5)).text_color(t.dim).child(pos)))
                            .child(crate::icons::Icon::Chevron.el(10., t.dim)),
                    ))
                    .on_click(|_, w, cx| w.dispatch_action(Box::new(OpenPrompts), cx)),
            ))
            .when_some(target.filter(|_| go > 0.), |d, i| {
                // Like a notification's →: takes you to the prompt the pill names.
                d.child(
                    stop(div().id("prompt-go"))
                        .flex_none()
                        .h(px(24.))
                        .w(px(24. * go))
                        .ml(px(6. * go))
                        .rounded(px(12.))
                        .bg(t.raised)
                        .border_1()
                        .border_color(t.line)
                        .opacity(go)
                        .overflow_hidden()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(|s| s.bg(t.line))
                        .child(crate::icons::Icon::Arrow.el(11., t.fg))
                        .on_click(cx.listener(move |v, _, w, cx| v.jump_to(Some(i), w, cx))),
                )
            })
            .into_any_element()
    }

    fn render_rail(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let nav = &self.nav;
        let n = nav.prompts.len();
        let current = nav.pending.map(|p| p.0).or(nav.here.filter(|_| nav.scrolled));
        let top = if nav.scrolled || self.bar_row(cx) { BAR_H } else { PAD_Y };
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
    use std::time::{Duration, Instant};

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
    fn from_live_steps_past_a_prompt_already_on_screen() {
        let mut n = nav(&["alpha one two", "bravo one two", "charlie one two"]);
        let live: Vec<String> = ["❯ charlie one two", "  1. Norway", RULE, "❯", RULE].map(String::from).to_vec();
        assert!(!n.scan(&live, Some(3), true, true));
        assert!(!n.scrolled);
        assert_eq!(n.prev(), Some(1), "charlie is in view: ‹ goes to bravo");
    }

    #[test]
    fn a_pinned_prompt_the_list_lacks_still_names_the_bar() {
        let mut n = nav(&[]);
        let back: Vec<String> = ["❯ [Image #3]", "  Ran 1 shell command", "  done  Jump to bottom (click) ↓", RULE, "❯", RULE].map(String::from).to_vec();
        n.scan(&back, Some(4), true, true);
        assert!(n.scrolled);
        assert_eq!(n.pinned.as_deref(), Some("[Image #3]"));
        let live: Vec<String> = ["❯ [Image #3]", "  Ran 1 shell command", RULE, "❯", RULE].map(String::from).to_vec();
        n.hint_at = Some(Instant::now() - Duration::from_secs(1));
        n.scan(&live, Some(3), true, true);
        assert_eq!(n.pinned, None, "only while scrolled back");
    }

    #[test]
    fn a_frame_missing_the_hint_while_scrolling_stays_scrolled() {
        let mut n = nav(&["alpha one two", "bravo one two"]);
        let back: Vec<String> = ["❯ bravo one two", "  12. Peach", "  13. Pear  Jump to bottom: fn+↓ to scroll", RULE, "❯", RULE].map(String::from).to_vec();
        n.scan(&back, Some(4), true, true);
        // mid-redraw: the hint's row not painted yet
        let torn: Vec<String> = ["❯ bravo one two", "  12. Peach", "", RULE, "❯", RULE].map(String::from).to_vec();
        n.scan(&torn, Some(4), true, true);
        assert!(n.scrolled, "Live ↓ stays");
        assert_eq!(n.pinned.as_deref(), Some("bravo one two"));
        // still no hint once the hold is over: live
        n.hint_at = Some(Instant::now() - Duration::from_secs(1));
        n.scan(&torn, Some(4), true, true);
        assert!(!n.scrolled);
    }

    #[test]
    fn an_unknown_prompt_row_asks_for_the_list_once() {
        let mut n = nav(&["alpha one two"]);
        let s: Vec<String> = ["❯ alpha one two", "", "❯ a brand new prompt", "", "❯ /cost", RULE, "❯", RULE].map(String::from).to_vec();
        assert!(n.scan(&s, Some(6), true, true));
        assert!(!n.scan(&s, Some(6), true, true), "asked already");
    }
}
