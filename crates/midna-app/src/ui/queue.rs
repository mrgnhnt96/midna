//! Queued messages (Queue-E on https://claude.ai/artifact/QWeZfgmJnhhRHEpmYTpG5e): a pill at
//! the bottom right of each terminal pane, just above the agent's input box (drawn by
//! `terminal/queue_pill.rs`), and this panel, which opens upward from it (⌘U or a click). The
//! daemon types the messages (`queue.*`); here they are added, edited, reordered, sent now and
//! removed. When one goes in, the pill turns green with a short glow ("Sent · N left").
//!
//! The terminals' queues live in the `QueueStore` global, synced from `session.list`, so every
//! pane (main, split, pop-out) draws its own pill. The panel is its own view (`QueueView`), so
//! any window can host one: the main window and each pop-out do. It sits at the window level,
//! outside the terminal's element tree, so typing in its field never reaches the terminal.
use crate::app::MainWindow;
use crate::backend::Backend;
use crate::icons::Icon;
use crate::model::{AgentKind, Session};
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use midna_proto::{ActorKind, QueueState, QueuedMessage, SendWhen};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

actions!(
    midna,
    [
        /// ⌘U: open or close the queued messages of the focused terminal.
        ToggleQueue,
    ]
);

/// How long the glow runs, and how long the pill keeps saying "Sent".
pub const GLOW: Duration = Duration::from_millis(1600);
pub const FLASH_HOLD: Duration = Duration::from_millis(2600);

/// The "send when" choices the panel cycles through (⇥). Times and other terminals are for
/// agents and the CLI (`midna queue add --at / --after`).
const WHENS: [(&str, SendWhen); 4] = [
    ("When idle", SendWhen::Idle),
    ("After 5 min idle", SendWhen::IdleFor { minutes: 5 }),
    ("After 15 min idle", SendWhen::IdleFor { minutes: 15 }),
    ("After 30 min idle", SendWhen::IdleFor { minutes: 30 }),
];

/// A message just went in: the pill glows and says "Sent" for a moment. Only a sent message
/// does this; a waiting queue is shown by the pill itself, and a failed one by its red state.
#[derive(Clone, Debug)]
pub struct Flash {
    pub at: Instant,
    pub left: usize,
    pub seq: u64,
}

#[derive(Default)]
pub struct QueueStore {
    /// Each terminal's (paused, messages), as last fetched.
    pub queues: HashMap<String, (bool, Vec<QueuedMessage>)>,
    /// Terminal names, for "next after api".
    pub names: HashMap<String, String>,
    /// Each agent terminal's agent ("Claude", "Codex"), for "next when Claude is idle".
    pub agents: HashMap<String, &'static str>,
    pub flash: HashMap<String, Flash>,
    /// The terminal whose panel is open.
    pub open: Option<String>,
    /// A pill was clicked: the terminal the next ToggleQueue is for.
    pub target: RefCell<Option<String>>,
    /// Each pill's top-right corner in window coordinates, from its last paint.
    pub anchors: RefCell<HashMap<String, Point<Pixels>>>,
    seq: u64,
}

impl Global for QueueStore {}

impl QueueStore {
    pub fn flash(&self, sid: &str) -> Option<&Flash> {
        self.flash.get(sid).filter(|f| f.at.elapsed() < FLASH_HOLD)
    }
}

/// macOS "Reduce motion": the glow becomes a steady ring.
pub fn reduce_motion() -> bool {
    static R: OnceLock<bool> = OnceLock::new();
    *R.get_or_init(|| {
        std::process::Command::new("/usr/bin/defaults")
            .args(["read", "com.apple.universalaccess", "reduceMotion"])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "1")
    })
}

/// Set up the store: once, before any terminal view exists.
pub fn init(cx: &mut App) {
    cx.set_global(QueueStore::default());
    // Read once off the UI thread, so the first glow doesn't wait for it.
    std::thread::spawn(reduce_motion);
}

/// What the panel tells its window: it closed (refocus the terminal), or a call failed.
pub enum QueueEvent {
    Closed,
    Error(String),
}

/// The panel. Each window that shows terminals hosts one.
pub struct QueueView {
    backend: Arc<dyn Backend>,
    input: Entity<super::text_input::TextField>,
    focus: FocusHandle,
    /// The terminal the panel is open for (None = closed).
    session: Option<String>,
    sel: Option<usize>,
    /// The message being edited (its text is in the field).
    editing: Option<String>,
    when: usize,
    scroll: ScrollHandle,
}

impl EventEmitter<QueueEvent> for QueueView {}

impl QueueView {
    pub fn new(backend: Arc<dyn Backend>, cx: &mut Context<Self>) -> QueueView {
        let input = cx.new(|cx| super::text_input::TextField::new(cx, false, "Queue a message…"));
        let focus = input.read(cx).focus.clone();
        QueueView { backend, input, focus, session: None, sel: None, editing: None, when: 0, scroll: ScrollHandle::new() }
    }

    pub fn is_open(&self) -> bool {
        self.session.is_some()
    }

    pub fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    /// Open for `sid`, or close if it is already open for it.
    pub fn toggle(&mut self, sid: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.session.as_deref() == Some(sid.as_str()) {
            self.close(cx);
            return;
        }
        self.session = Some(sid.clone());
        self.sel = None;
        self.editing = None;
        self.when = 0;
        self.input.update(cx, |f, cx| f.clear(cx));
        self.scroll.scroll_to_item(0);
        cx.update_global::<QueueStore, _>(|st, _| st.open = Some(sid));
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// Closed by the panel itself (esc, ⌘U, a click outside): the host refocuses its terminal.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.hide(cx) {
            cx.emit(QueueEvent::Closed);
        }
    }

    /// Closed because the host moved on (another screen or overlay): no event.
    pub fn hide(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(sid) = self.session.take() else { return false };
        self.editing = None;
        if cx.global::<QueueStore>().open.as_deref() == Some(sid.as_str()) {
            cx.update_global::<QueueStore, _>(|st, _| st.open = None);
        }
        cx.notify();
        true
    }
}

// ------------------------------------------------------------------ store

/// Copy the sessions' queues into the store (after every `session.list`).
pub fn sync(sessions: &[Session], cx: &mut App) {
    let queues: HashMap<String, (bool, Vec<QueuedMessage>)> =
        sessions.iter().filter(|s| !s.queue.is_empty() || s.queue_paused).map(|s| (s.id.clone(), (s.queue_paused, s.queue.clone()))).collect();
    let names: HashMap<String, String> = sessions.iter().map(|s| (s.id.clone(), s.name.clone())).collect();
    let agents: HashMap<String, &'static str> = sessions
        .iter()
        .filter_map(|s| Some((s.id.clone(), if s.agent? == AgentKind::Codex { "Codex" } else { "Claude" })))
        .collect();
    let st = cx.global::<QueueStore>();
    if st.queues == queues && st.names == names && st.agents == agents {
        return;
    }
    cx.update_global::<QueueStore, _>(|st, _| {
        st.queues = queues;
        st.names = names;
        st.agents = agents;
    });
}

/// `session.queue`: a message went in (flash that terminal's pill) or failed (a toast; the
/// pill turns red by itself and stays so until the message is retried or removed).
pub fn on_event(m: &mut MainWindow, e: &crate::model::Event, cx: &mut Context<MainWindow>) {
    let Some(sid) = e.session_id.clone() else { return };
    match e.data.get("action").and_then(Value::as_str) {
        Some("sent") => {}
        Some("failed") => {
            if !m.queue.read(cx).is_open() {
                let keys = m.key_label("keys.queue");
                m.toast(format!("A queued message couldn't be sent. {keys} to retry or remove it."), cx);
            }
            return;
        }
        _ => return,
    }
    let left = e.data.get("left").and_then(Value::as_u64).unwrap_or(0) as usize;
    let seq = cx.update_global::<QueueStore, _>(|st, _| {
        st.seq += 1;
        let seq = st.seq;
        st.flash.insert(sid.clone(), Flash { at: Instant::now(), left, seq });
        seq
    });
    // Back to the plain pill (or gone) once the flash is over.
    cx.spawn(async move |_, cx| {
        cx.background_executor().timer(FLASH_HOLD + Duration::from_millis(50)).await;
        cx.update(|cx| {
            cx.update_global::<QueueStore, _>(|st, _| {
                if st.flash.get(&sid).is_some_and(|f| f.seq == seq) {
                    st.flash.remove(&sid);
                }
            })
        });
    })
    .detach();
}

/// "next when Claude is idle", "next after 5 min idle", "next at 18:00", "next after api".
pub fn next_phrase(when: &SendWhen, agent: Option<&str>, names: &HashMap<String, String>) -> String {
    match when {
        SendWhen::Idle => match agent {
            Some(a) => format!("next when {a} is idle"),
            None => "next when it's idle".into(),
        },
        SendWhen::IdleFor { minutes } => format!("next after {minutes} min idle"),
        SendWhen::At { at } => format!("next at {}", hhmm(at)),
        SendWhen::After { session } => format!("next after {}", names.get(session).map(String::as_str).unwrap_or(session)),
    }
}

/// The `when` of a message in the list: "when idle", "after 5 min idle", "at 18:00", "after api".
fn when_label(when: &SendWhen, names: &HashMap<String, String>) -> String {
    next_phrase(when, None, names).trim_start_matches("next ").replace("when it's idle", "when idle")
}

fn hhmm(at: &str) -> String {
    let c = super::screen_kit::clock_or_day(at);
    if c.len() == 8 && c.as_bytes()[2] == b':' { c[..5].to_string() } else { c }
}

fn by_label(m: &QueuedMessage) -> &'static str {
    match m.by.kind {
        ActorKind::Human => "you",
        ActorKind::Agent => "an agent",
        ActorKind::Trigger => "a trigger",
        ActorKind::System => "midna",
    }
}

// ------------------------------------------------------------------ hosting

/// The terminal ⌘U is about in the main window: a clicked pill, else the focused split pane,
/// else the selected one.
fn target(m: &MainWindow, window: &Window, cx: &App) -> Option<String> {
    if let Some(sid) = cx.global::<QueueStore>().target.borrow_mut().take() {
        return Some(sid);
    }
    if let Some(sp) = &m.split
        && sp.view.read(cx).focus_handle().contains_focused(window, cx)
    {
        return Some(sp.session_id(cx));
    }
    m.selected.clone()
}

/// The main window's panel: toast its errors, refocus the terminal when it closes.
pub fn new_for_main(backend: Arc<dyn Backend>, window: &Window, cx: &mut Context<MainWindow>) -> Entity<QueueView> {
    let view = cx.new(|cx| QueueView::new(backend, cx));
    cx.subscribe_in(&view, window, |m: &mut MainWindow, _, ev: &QueueEvent, window, cx| match ev {
        QueueEvent::Closed => m.focus_terminal(window, cx),
        QueueEvent::Error(e) => m.toast(e.clone(), cx),
    })
    .detach();
    view
}

pub fn toggle(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    let open = m.queue.read(cx).session().map(str::to_string);
    let sid = match target(m, window, cx) {
        Some(sid) => sid,
        None if open.is_some() => return m.queue.update(cx, |v, cx| v.close(cx)),
        None => return,
    };
    m.menu = crate::app::Menu::None;
    m.queue.update(cx, |v, cx| v.toggle(sid, window, cx));
    cx.notify();
}

/// The main window moved on (another screen, an overlay, Dismiss).
pub fn hide(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    m.queue.update(cx, |v, cx| {
        v.hide(cx);
    });
}

// ------------------------------------------------------------------ doing

impl QueueView {
    fn items(&self, cx: &App) -> Vec<QueuedMessage> {
        self.session.as_ref().and_then(|s| cx.global::<QueueStore>().queues.get(s)).map(|(_, q)| q.clone()).unwrap_or_default()
    }

    fn paused(&self, cx: &App) -> bool {
        self.session.as_ref().and_then(|s| cx.global::<QueueStore>().queues.get(s)).is_some_and(|(p, _)| *p)
    }

    fn call(&mut self, method: &'static str, mut p: Value, cx: &mut Context<Self>) {
        let Some(sid) = self.session.clone() else { return };
        p["session"] = json!(sid);
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let res = cx.background_executor().spawn(async move { backend.call(method, p) }).await;
            if let Err(e) = res {
                let _ = this.update(cx, |_, cx| cx.emit(QueueEvent::Error(format!("{method} failed: {e:#}"))));
            }
        })
        .detach();
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let text = self.input.read(cx).text().trim().to_string();
        if text.is_empty() {
            return;
        }
        let when = serde_json::to_value(&WHENS[self.when].1).unwrap_or(Value::Null);
        match self.editing.take() {
            Some(id) => self.call("queue.update", json!({ "id": id, "text": text, "when": when, "retry": true }), cx),
            None => self.call("queue.add", json!({ "text": text, "when": when }), cx),
        }
        self.input.update(cx, |f, cx| f.clear(cx));
        self.when = 0;
    }

    fn edit(&mut self, q: &QueuedMessage, window: &mut Window, cx: &mut Context<Self>) {
        self.editing = Some(q.id.clone());
        self.when = WHENS.iter().position(|(_, w)| *w == q.when).unwrap_or(0);
        self.input.update(cx, |f, cx| f.set_text(&q.text, cx));
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn cancel_edit(&mut self, cx: &mut Context<Self>) {
        self.editing = None;
        self.when = 0;
        self.input.update(cx, |f, cx| f.clear(cx));
    }

    fn remove(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.editing.as_deref() == Some(id) {
            self.cancel_edit(cx);
        }
        self.call("queue.remove", json!({ "id": id }), cx);
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let md = &ks.modifiers;
        let list = self.items(cx);
        let n = list.len();
        let sel = self.sel.filter(|&i| i < n);
        match ks.key.as_str() {
            "up" | "down" if md.alt && !md.platform => {
                let Some(i) = sel else { return };
                let to = if ks.key == "up" { i.saturating_sub(1) } else { (i + 1).min(n - 1) };
                if to != i {
                    self.sel = Some(to);
                    self.call("queue.move", json!({ "id": list[i].id, "to": to }), cx);
                }
            }
            "up" | "down" if !md.platform => {
                if n > 0 {
                    self.sel = Some(match (sel, ks.key.as_str()) {
                        (None, "up") => n - 1,
                        (None, _) => 0,
                        (Some(i), "up") => (i + n - 1) % n,
                        (Some(i), _) => (i + 1) % n,
                    });
                    self.scroll.scroll_to_item(self.sel.unwrap_or(0));
                }
            }
            "enter" if md.shift && !md.platform => {
                let Some(i) = sel else { return };
                self.call("queue.send_now", json!({ "id": list[i].id }), cx);
            }
            "enter" if !md.platform && !md.alt => {
                if !self.input.read(cx).text().trim().is_empty() {
                    self.submit(cx);
                } else if let Some(i) = sel {
                    self.edit(&list[i].clone(), window, cx);
                } else {
                    return;
                }
            }
            "backspace" if md.alt && !md.platform && self.input.read(cx).text().is_empty() => {
                let Some(i) = sel else { return };
                self.remove(&list[i].id, cx);
                self.sel = if n > 1 { Some(i.min(n - 2)) } else { None };
            }
            "tab" => self.when = (self.when + if md.shift { WHENS.len() - 1 } else { 1 }) % WHENS.len(),
            "escape" if self.editing.is_some() => self.cancel_edit(cx),
            "escape" => self.close(cx),
            // Everything else is the field's: editing keys, and typing through the IME.
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }
}

/// Dev only (`MIDNA_DEBUG_SCREEN`): `queue` opens the selected terminal's panel; `queue-sent`
/// flashes its pill. `MIDNA_DEBUG_QUEUE_GLOW=0.3` holds the glow at that point.
pub fn debug(m: &mut MainWindow, what: &str, window: &mut Window, cx: &mut Context<MainWindow>) {
    let Some(sid) = m.selected.clone() else { return };
    match what {
        "queue" => toggle(m, window, cx),
        _ => {
            let left = cx.global::<QueueStore>().queues.get(&sid).map(|(_, q)| q.len()).unwrap_or(0);
            cx.update_global::<QueueStore, _>(|st, _| {
                st.seq += 1;
                st.flash.insert(sid, Flash { at: Instant::now() + Duration::from_secs(3600), left, seq: st.seq });
            });
        }
    }
}

// ------------------------------------------------------------------ rendering

impl Render for QueueView {
    /// Anchored above the pill of the terminal it is for, with a click-away layer under it.
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(sid) = self.session.clone() else { return div().into_any_element() };
        let t = cx.global::<Theme>().clone();
        let store = cx.global::<QueueStore>();
        let vp = window.viewport_size();
        let anchor = store.anchors.borrow().get(&sid).copied().unwrap_or(point(vp.width - px(22.), vp.height - px(120.)));
        let names = store.names.clone();
        let agent = store.agents.get(&sid).copied();
        let name = names.get(&sid).cloned().unwrap_or_else(|| sid.clone());
        let list = self.items(cx);
        let is_paused = self.paused(cx);
        let sel = self.sel.filter(|&i| i < list.len());

        let header = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(14.))
            .pt(px(10.))
            .pb(px(6.))
            .child(Icon::Queue.el(13., t.accent))
            .child(super::caps_label(&t, &format!("Queued for {name}")))
            .child(div().text_size(px(11.5)).text_color(t.dim).child(if list.is_empty() { String::new() } else { list.len().to_string() }))
            .child(div().flex_1())
            .when(!list.is_empty() || is_paused, |d| {
                d.child(
                    div()
                        .id("queue-pause")
                        .flex()
                        .items_center()
                        .gap(px(5.))
                        .h(px(24.))
                        .px(px(8.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(t.line)
                        .text_size(px(11.5))
                        .text_color(if is_paused { t.accent } else { t.dim })
                        .cursor_pointer()
                        .hover(|st| st.bg(t.panel))
                        .on_click(cx.listener(move |v, _, _, cx| v.call("queue.pause", json!({ "paused": !is_paused }), cx)))
                        .child(if is_paused { Icon::Play.el(11., t.accent) } else { Icon::Pause.el(11., t.dim) })
                        .child(if is_paused { "Resume" } else { "Pause" }),
                )
            });

        // The list gets what's left above the pill after the header, field and footer (~140px),
        // so the panel stays above its pill in a small pop-out window.
        let room = (f32::from(anchor.y) - 16. - 140.).clamp(64., 300.);
        let mut rows = div().id("queue-list").flex().flex_col().max_h(px(room)).overflow_y_scroll().track_scroll(&self.scroll).pb(px(4.));
        for (i, q) in list.iter().enumerate() {
            rows = rows.child(self.row(&t, q, i, sel == Some(i), &names, cx));
        }
        if list.is_empty() {
            let who = agent.unwrap_or("the terminal");
            rows = rows.child(div().px(px(14.)).pb(px(10.)).text_color(t.dim).child(format!("Nothing queued. What you add goes in once {who} is ready, in order.")));
        }
        if is_paused && !list.is_empty() {
            rows = rows.child(div().px(px(14.)).pt(px(4.)).pb(px(6.)).text_size(px(11.5)).text_color(t.accent).child("Paused: nothing is sent until you resume."));
        }

        let editing = self.editing.is_some();
        let compose = div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .px(px(10.))
            .py(px(10.))
            .border_t_1()
            .border_color(t.line)
            .when(editing, |d| d.child(div().px(px(2.)).text_size(px(11.5)).text_color(t.accent).child("Editing · ↩ to save, esc to cancel")))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .h(px(32.))
                            .px(px(10.))
                            .rounded(px(7.))
                            .border_1()
                            .border_color(t.accent)
                            .bg(t.panel)
                            .overflow_hidden()
                            .child(self.input.clone()),
                    )
                    .child(
                        div()
                            .id("queue-when")
                            .flex()
                            .flex_none()
                            .items_center()
                            .h(px(32.))
                            .px(px(10.))
                            .rounded(px(7.))
                            .border_1()
                            .border_color(t.line)
                            .text_size(px(12.))
                            .cursor_pointer()
                            .hover(|st| st.bg(t.panel))
                            .tooltip(super::header::tip_fixed("When it goes in", "⇥"))
                            .on_click(cx.listener(|v, _, _, cx| {
                                v.when = (v.when + 1) % WHENS.len();
                                cx.notify();
                            }))
                            .child(WHENS[self.when].0),
                    )
                    .child(
                        div()
                            .id("queue-submit")
                            .flex()
                            .flex_none()
                            .items_center()
                            .h(px(32.))
                            .px(px(12.))
                            .rounded(px(7.))
                            .bg(t.accent)
                            .text_color(t.accent_fg)
                            .font_weight(FontWeight::BOLD)
                            .cursor_pointer()
                            .on_click(cx.listener(|v, _, _, cx| {
                                v.submit(cx);
                                cx.notify();
                            }))
                            .child(if editing { "Save" } else { "Queue" }),
                    ),
            );

        let key = |k: &str, what: &str| div().flex().gap(px(5.)).child(div().font_family(t.mono_font.clone()).child(k.to_string())).child(what.to_string());
        let footer = div()
            .flex()
            .flex_wrap()
            .gap_x(px(14.))
            .gap_y(px(2.))
            .px(px(14.))
            .py(px(8.))
            .border_t_1()
            .border_color(t.line)
            .text_size(px(11.5))
            .text_color(t.dim)
            .child(key("↑↓ ↩", "edit"))
            .child(key("⌥↑↓", "reorder"))
            .child(key("⇧↩", "send now"))
            .child(key("⌥⌫", "remove"))
            .child(div().flex_1())
            .child(key(&crate::actions::label(cx, "keys.queue"), ""));

        div()
            // A click anywhere else closes it (the pill's own click toggles it).
            .child(
                deferred(
                    anchored().position(point(px(0.), px(0.))).child(div().id("queue-dismiss").w(vp.width).h(vp.height).on_mouse_down(MouseButton::Left, cx.listener(|v, _, _, cx| v.close(cx)))),
                )
                .with_priority(0),
            )
            .child(
                deferred(
                    anchored().position(point(anchor.x, anchor.y - px(8.))).anchor(Anchor::BottomRight).snap_to_window_with_margin(px(8.)).child(
                        div()
                            .id("queue-panel")
                            .key_context("MidnaOverlay")
                            .track_focus(&self.focus)
                            .on_key_down(cx.listener(Self::on_key))
                            .occlude()
                            .w(px(440.))
                            .flex()
                            .flex_col()
                            .rounded(px(12.))
                            .border_1()
                            .border_color(t.line)
                            .bg(t.raised)
                            .text_color(t.fg)
                            .font_family(t.ui_font.clone())
                            .text_size(px(13.))
                            .line_height(px(13. * 1.45))
                            .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.4), offset: point(px(0.), px(18.)), blur_radius: px(48.), spread_radius: px(0.), inset: false }])
                            .child(header)
                            .child(rows)
                            .child(compose)
                            .child(footer),
                    ),
                )
                .with_priority(1),
            )
            .into_any_element()
    }
}

impl QueueView {
    #[allow(clippy::too_many_arguments)]
    fn row(&self, t: &Theme, q: &QueuedMessage, i: usize, selected: bool, names: &HashMap<String, String>, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let failed = q.state == QueueState::Failed;
        let sending = q.state == QueueState::Sending;
        let editing = self.editing.as_deref() == Some(q.id.as_str());
        let sub = if failed {
            format!("Couldn't send: {}", q.error.clone().unwrap_or_default())
        } else if sending {
            "going in now…".to_string()
        } else if i == 0 && !q.waiting_for.is_empty() {
            format!("{} · waiting for {}", when_label(&q.when, names), q.waiting_for.join(", "))
        } else {
            format!("{} · {}", when_label(&q.when, names), by_label(q))
        };
        let btn = |id: String, icon: Icon, tip: &'static str, keys: &'static str| {
            div()
                .id(SharedString::from(id))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(26.))
                .rounded(px(6.))
                .cursor_pointer()
                .hover(|st| st.bg(t.panel))
                .tooltip(super::header::tip_fixed(tip, keys))
                .child(icon.el(13., t.dim))
        };
        let (q1, q2, q3) = (q.clone(), q.id.clone(), q.id.clone());
        div()
            .id(SharedString::from(format!("queue-{}", q.id)))
            .flex()
            .items_start()
            .gap(px(10.))
            .mx(px(6.))
            .pl(px(8.))
            .pr(px(4.))
            .py(px(7.))
            .rounded(px(7.))
            .when(selected || editing, |d| d.bg(t.accent_soft))
            .on_mouse_move(cx.listener(move |v, _, _, cx| {
                if v.sel != Some(i) {
                    v.sel = Some(i);
                    cx.notify();
                }
            }))
            .child(div().flex_none().w(px(16.)).pt(px(1.)).font_family(t.mono_font.clone()).text_size(px(11.5)).text_color(t.dim).child((i + 1).to_string()))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.))
                    .child(div().min_w_0().line_clamp(2).child(q.text.clone()))
                    .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_size(px(11.5)).text_color(if failed { t.err } else { t.dim }).child(sub)),
            )
            .when(!sending, |d| {
                d.child(
                    div()
                        .flex()
                        .flex_none()
                        .gap(px(2.))
                        .opacity(if selected { 1. } else { 0.55 })
                        .child(btn(format!("queue-now-{}", q.id), Icon::SendNow, "Send now", "⇧↩").on_click(cx.listener(move |v, _, _, cx| {
                            v.call("queue.send_now", json!({ "id": q2 }), cx);
                        })))
                        .child(btn(format!("queue-edit-{}", q.id), Icon::Pencil, "Edit", "↩").on_click(cx.listener(move |v, _, window, cx| v.edit(&q1, window, cx))))
                        .child(btn(format!("queue-rm-{}", q.id), Icon::Cross, "Remove", "⌥⌫").on_click(cx.listener(move |v, _, _, cx| v.remove(&q3, cx)))),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's glob re-export would shadow `#[test]`.
    use super::{next_phrase, when_label};
    use midna_proto::SendWhen;
    use std::collections::HashMap;

    #[test]
    fn phrases_say_when_the_next_one_goes() {
        let names = HashMap::from([("a1".to_string(), "api".to_string())]);
        assert_eq!(next_phrase(&SendWhen::Idle, Some("Claude"), &names), "next when Claude is idle");
        assert_eq!(next_phrase(&SendWhen::Idle, None, &names), "next when it's idle");
        assert_eq!(next_phrase(&SendWhen::IdleFor { minutes: 5 }, None, &names), "next after 5 min idle");
        assert_eq!(next_phrase(&SendWhen::After { session: "a1".into() }, None, &names), "next after api");
        assert_eq!(when_label(&SendWhen::Idle, &names), "when idle");
        assert_eq!(when_label(&SendWhen::After { session: "zz".into() }, &names), "after zz");
    }
}
