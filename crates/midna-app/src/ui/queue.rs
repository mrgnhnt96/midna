//! Queued messages (Queue-E on https://claude.ai/artifact/QWeZfgmJnhhRHEpmYTpG5e): a pill at
//! the bottom right of each terminal pane, just above the agent's input box (drawn by
//! `terminal/queue_pill.rs`), and this panel, which opens upward from it (⌘U or a click). The
//! daemon types the messages (`queue.*`); here they are added, edited, reordered, sent now and
//! removed. When one goes in, the pill turns green with a short glow ("Sent · N left").
//!
//! The terminals' queues live in the `QueueStore` global, synced from `session.list`, so every
//! pane (main, split, pop-out) draws its own pill.
use crate::app::{MainWindow, Menu};
use crate::icons::Icon;
use crate::model::{AgentKind, Session};
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use midna_proto::{ActorKind, QueueState, QueuedMessage, SendWhen};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::OnceLock;
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

/// A message just went in (or failed): the pill says so for a moment.
#[derive(Clone, Debug)]
pub struct Flash {
    pub ok: bool,
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

pub struct QueuePanel {
    pub input: Entity<super::text_input::TextField>,
    pub focus: FocusHandle,
    /// The terminal the panel is for.
    pub session: Option<String>,
    sel: Option<usize>,
    /// The message being edited (its text is in the field).
    editing: Option<String>,
    when: usize,
    scroll: ScrollHandle,
}

impl QueuePanel {
    pub fn new(cx: &mut App) -> QueuePanel {
        cx.set_global(QueueStore::default());
        // Read once off the UI thread, so the first glow doesn't wait for it.
        std::thread::spawn(reduce_motion);
        let input = cx.new(|cx| super::text_input::TextField::new(cx, false, "Queue a message…"));
        let focus = input.read(cx).focus.clone();
        QueuePanel { input, focus, session: None, sel: None, editing: None, when: 0, scroll: ScrollHandle::new() }
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

/// `session.queue`: a message went in or failed. Flash that terminal's pill.
pub fn on_event(m: &mut MainWindow, e: &crate::model::Event, cx: &mut Context<MainWindow>) {
    let Some(sid) = e.session_id.clone() else { return };
    let ok = match e.data.get("action").and_then(Value::as_str) {
        Some("sent") => true,
        Some("failed") => false,
        _ => return,
    };
    let left = e.data.get("left").and_then(Value::as_u64).unwrap_or(0) as usize;
    let seq = cx.update_global::<QueueStore, _>(|st, _| {
        st.seq += 1;
        let seq = st.seq;
        st.flash.insert(sid.clone(), Flash { ok, at: Instant::now(), left, seq });
        seq
    });
    if !ok && m.menu != Menu::Queue {
        let keys = m.key_label("keys.queue");
        m.toast(format!("A queued message couldn't be sent. {keys} to retry or remove it."), cx);
    }
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

// ------------------------------------------------------------------ opening

fn items(m: &MainWindow, cx: &App) -> Vec<QueuedMessage> {
    m.queue.session.as_ref().and_then(|s| cx.global::<QueueStore>().queues.get(s)).map(|(_, q)| q.clone()).unwrap_or_default()
}

fn paused(m: &MainWindow, cx: &App) -> bool {
    m.queue.session.as_ref().and_then(|s| cx.global::<QueueStore>().queues.get(s)).is_some_and(|(p, _)| *p)
}

/// The terminal ⌘U is about: a clicked pill, else the focused split pane, else the selected one.
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

pub fn toggle(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    let sid = target(m, window, cx);
    if m.menu == Menu::Queue && (sid.is_none() || sid == m.queue.session) {
        close(m, window, cx);
        return;
    }
    let Some(sid) = sid else { return };
    m.menu = Menu::Queue;
    m.queue.session = Some(sid.clone());
    m.queue.sel = None;
    m.queue.editing = None;
    m.queue.when = 0;
    m.queue.input.update(cx, |f, cx| f.clear(cx));
    m.queue.scroll.scroll_to_item(0);
    cx.update_global::<QueueStore, _>(|st, _| st.open = Some(sid));
    m.queue.focus.focus(window, cx);
    cx.notify();
}

pub fn close(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    if m.menu == Menu::Queue {
        m.menu = Menu::None;
    }
    closed(cx);
    m.focus_terminal(window, cx);
    cx.notify();
}

/// The panel went away (any way it closes): the pill stops looking open.
pub fn closed(cx: &mut App) {
    if cx.global::<QueueStore>().open.is_some() {
        cx.update_global::<QueueStore, _>(|st, _| st.open = None);
    }
}

// ------------------------------------------------------------------ doing

fn call(m: &mut MainWindow, method: &'static str, mut p: Value, cx: &mut Context<MainWindow>) {
    let Some(sid) = m.queue.session.clone() else { return };
    p["session"] = json!(sid);
    m.rpc(method, p, cx, |_, _, _, _| {});
}

fn submit(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    let text = m.queue.input.read(cx).text().trim().to_string();
    if text.is_empty() {
        return;
    }
    let when = serde_json::to_value(&WHENS[m.queue.when].1).unwrap_or(Value::Null);
    match m.queue.editing.take() {
        Some(id) => call(m, "queue.update", json!({ "id": id, "text": text, "when": when, "retry": true }), cx),
        None => call(m, "queue.add", json!({ "text": text, "when": when }), cx),
    }
    m.queue.input.update(cx, |f, cx| f.clear(cx));
    m.queue.when = 0;
}

fn edit(m: &mut MainWindow, q: &QueuedMessage, window: &mut Window, cx: &mut Context<MainWindow>) {
    m.queue.editing = Some(q.id.clone());
    m.queue.when = WHENS.iter().position(|(_, w)| *w == q.when).unwrap_or(0);
    m.queue.input.update(cx, |f, cx| f.set_text(&q.text, cx));
    m.queue.focus.focus(window, cx);
    cx.notify();
}

fn cancel_edit(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    m.queue.editing = None;
    m.queue.when = 0;
    m.queue.input.update(cx, |f, cx| f.clear(cx));
}

fn on_key(m: &mut MainWindow, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<MainWindow>) {
    let ks = &ev.keystroke;
    let md = &ks.modifiers;
    let list = items(m, cx);
    let n = list.len();
    let sel = m.queue.sel.filter(|&i| i < n);
    match ks.key.as_str() {
        "up" | "down" if md.alt && !md.platform => {
            let Some(i) = sel else { return };
            let to = if ks.key == "up" { i.saturating_sub(1) } else { (i + 1).min(n - 1) };
            if to != i {
                m.queue.sel = Some(to);
                call(m, "queue.move", json!({ "id": list[i].id, "to": to }), cx);
            }
        }
        "up" | "down" if !md.platform => {
            if n > 0 {
                m.queue.sel = Some(match (sel, ks.key.as_str()) {
                    (None, "up") => n - 1,
                    (None, _) => 0,
                    (Some(i), "up") => (i + n - 1) % n,
                    (Some(i), _) => (i + 1) % n,
                });
                m.queue.scroll.scroll_to_item(m.queue.sel.unwrap_or(0));
            }
        }
        "enter" if md.shift && !md.platform => {
            let Some(i) = sel else { return };
            call(m, "queue.send_now", json!({ "id": list[i].id }), cx);
        }
        "enter" if !md.platform && !md.alt => {
            if !m.queue.input.read(cx).text().trim().is_empty() {
                submit(m, cx);
            } else if let Some(i) = sel {
                edit(m, &list[i].clone(), window, cx);
            } else {
                return;
            }
        }
        "backspace" if md.alt && !md.platform && m.queue.input.read(cx).text().is_empty() => {
            let Some(i) = sel else { return };
            if m.queue.editing.as_deref() == Some(list[i].id.as_str()) {
                cancel_edit(m, cx);
            }
            call(m, "queue.remove", json!({ "id": list[i].id }), cx);
            m.queue.sel = if n > 1 { Some(i.min(n - 2)) } else { None };
        }
        "tab" => m.queue.when = (m.queue.when + if md.shift { WHENS.len() - 1 } else { 1 }) % WHENS.len(),
        "escape" if m.queue.editing.is_some() => cancel_edit(m, cx),
        "escape" => close(m, window, cx),
        // Everything else is the field's: editing keys, and typing through the IME.
        _ => return,
    }
    cx.stop_propagation();
    cx.notify();
}

/// Dev only (`MIDNA_DEBUG_SCREEN`): `queue` opens the selected terminal's panel; `queue-sent`
/// and `queue-failed` flash its pill. `MIDNA_DEBUG_QUEUE_GLOW=0.3` holds the glow at that point.
pub fn debug(m: &mut MainWindow, what: &str, window: &mut Window, cx: &mut Context<MainWindow>) {
    let Some(sid) = m.selected.clone() else { return };
    match what {
        "queue" => toggle(m, window, cx),
        _ => {
            let left = cx.global::<QueueStore>().queues.get(&sid).map(|(_, q)| q.len()).unwrap_or(0);
            let ok = what == "queue-sent";
            cx.update_global::<QueueStore, _>(|st, _| {
                st.seq += 1;
                st.flash.insert(sid, Flash { ok, at: Instant::now() + Duration::from_secs(3600), left, seq: st.seq });
            });
        }
    }
}

// ------------------------------------------------------------------ rendering

/// The panel, anchored above the pill of the terminal it is for (window level, so typing in
/// its field never reaches the terminal).
pub fn panel(m: &MainWindow, t: &Theme, window: &Window, cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    if m.menu != Menu::Queue {
        return None;
    }
    let sid = m.queue.session.clone()?;
    let store = cx.global::<QueueStore>();
    let vp = window.viewport_size();
    let anchor = store.anchors.borrow().get(&sid).copied().unwrap_or(point(vp.width - px(22.), vp.height - px(120.)));
    let names = store.names.clone();
    let name = names.get(&sid).cloned().unwrap_or_else(|| sid.clone());
    let list = items(m, cx);
    let is_paused = paused(m, cx);
    let agent = m.sessions.iter().find(|s| s.id == sid).and_then(|s| s.agent).map(|a| match a {
        AgentKind::Codex => "Codex",
        _ => "Claude",
    });
    let sel = m.queue.sel.filter(|&i| i < list.len());

    let header = div()
        .flex()
        .items_center()
        .gap(px(8.))
        .px(px(14.))
        .pt(px(10.))
        .pb(px(6.))
        .child(Icon::Queue.el(13., t.accent))
        .child(super::caps_label(t, &format!("Queued for {name}")))
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
                    .on_click(cx.listener(move |m, _, _, cx| call(m, "queue.pause", json!({ "paused": !is_paused }), cx)))
                    .child(if is_paused { Icon::Play.el(11., t.accent) } else { Icon::Pause.el(11., t.dim) })
                    .child(if is_paused { "Resume" } else { "Pause" }),
            )
        });

    let mut rows = div().id("queue-list").flex().flex_col().max_h(px(300.)).overflow_y_scroll().track_scroll(&m.queue.scroll).pb(px(4.));
    for (i, q) in list.iter().enumerate() {
        rows = rows.child(row(m, t, q, i, sel == Some(i), &names, cx));
    }
    if list.is_empty() {
        let who = agent.unwrap_or("the terminal");
        rows = rows.child(div().px(px(14.)).pb(px(10.)).text_color(t.dim).child(format!("Nothing queued. What you add goes in once {who} is ready, in order.")));
    }
    if is_paused && !list.is_empty() {
        rows = rows.child(div().px(px(14.)).pt(px(4.)).pb(px(6.)).text_size(px(11.5)).text_color(t.accent).child("Paused: nothing is sent until you resume."));
    }

    let editing = m.queue.editing.is_some();
    let when_label = WHENS[m.queue.when].0;
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
                        .child(m.queue.input.clone()),
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
                        .on_click(cx.listener(|m, _, _, cx| {
                            m.queue.when = (m.queue.when + 1) % WHENS.len();
                            cx.notify();
                        }))
                        .child(when_label),
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
                        .on_click(cx.listener(|m, _, _, cx| {
                            submit(m, cx);
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
        .child(key(&m.key_label("keys.queue"), ""));

    Some(
        deferred(
            anchored().position(point(anchor.x, anchor.y - px(8.))).anchor(Anchor::BottomRight).snap_to_window_with_margin(px(8.)).child(
                div()
                    .id("queue-panel")
                    .key_context("MidnaOverlay")
                    .track_focus(&m.queue.focus)
                    .on_key_down(cx.listener(on_key))
                    .occlude()
                    .w(px(440.))
                    .flex()
                    .flex_col()
                    .rounded(px(12.))
                    .border_1()
                    .border_color(t.line)
                    .bg(t.raised)
                    .text_color(t.fg)
                    .text_size(px(13.))
                    .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.4), offset: point(px(0.), px(18.)), blur_radius: px(48.), spread_radius: px(0.), inset: false }])
                    .child(header)
                    .child(rows)
                    .child(compose)
                    .child(footer),
            ),
        )
        .with_priority(1)
        .into_any_element(),
    )
}

#[allow(clippy::too_many_arguments)]
fn row(m: &MainWindow, t: &Theme, q: &QueuedMessage, i: usize, selected: bool, names: &HashMap<String, String>, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let failed = q.state == QueueState::Failed;
    let sending = q.state == QueueState::Sending;
    let editing = m.queue.editing.as_deref() == Some(q.id.as_str());
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
        .on_mouse_move(cx.listener(move |m, _, _, cx| {
            if m.queue.sel != Some(i) {
                m.queue.sel = Some(i);
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
                    .child(btn(format!("queue-now-{}", q.id), Icon::SendNow, "Send now", "⇧↩").on_click(cx.listener(move |m, _, _, cx| {
                        call(m, "queue.send_now", json!({ "id": q2 }), cx);
                    })))
                    .child(btn(format!("queue-edit-{}", q.id), Icon::Pencil, "Edit", "↩").on_click(cx.listener(move |m, _, window, cx| edit(m, &q1, window, cx))))
                    .child(btn(format!("queue-rm-{}", q.id), Icon::Cross, "Remove", "⌥⌫").on_click(cx.listener(move |m, _, _, cx| {
                        if m.queue.editing.as_deref() == Some(q3.as_str()) {
                            cancel_edit(m, cx);
                        }
                        call(m, "queue.remove", json!({ "id": q3 }), cx);
                    }))),
            )
        })
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
