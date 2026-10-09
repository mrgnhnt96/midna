//! Main windows (⌘⇧N opens more). Every terminal shows in exactly one of them, the window
//! that owns it; a pop-out still counts as part of the window it came from. A terminal
//! nobody owns yet (just opened by an agent, a trigger or the CLI, or its window closed)
//! goes to the home window: the main window focused last. The home window also posts
//! notifications and takes `window.command`s and update commands, so a second window
//! doesn't double them. Sidebar rows dragged onto another window move there; dropped
//! outside every window they get a new one.
use crate::app::{MainWindow, Overlay, Screen};
use crate::backend::Backend;
use gpui_kit::prelude::*;
use gpui_kit::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

#[derive(Clone)]
struct Main {
    id: EntityId,
    entity: WeakEntity<MainWindow>,
    handle: AnyWindowHandle,
    ns: Option<objc2::rc::Retained<objc2_app_kit::NSWindow>>,
}

#[derive(Default)]
pub struct Registry {
    /// Main windows, most recently focused first.
    mains: Vec<Main>,
    owner: HashMap<String, EntityId>,
    /// The window a dragged sidebar row is over (it highlights its sidebar).
    pub drop_hint: Option<EntityId>,
    /// Windows the human is closing (left out of the saved layout). A window closed by
    /// quitting isn't here, so the next launch reopens it.
    leaving: Vec<EntityId>,
    save_pending: bool,
}

impl Registry {
    fn home(&self) -> Option<EntityId> {
        self.mains.first().map(|m| m.id)
    }

    /// The window `session` shows in.
    pub fn place(&self, session: &str) -> Option<EntityId> {
        self.owner.get(session).copied().filter(|id| self.mains.iter().any(|m| m.id == *id)).or(self.home())
    }

    pub fn shows(&self, session: &str, me: EntityId) -> bool {
        self.place(session) == Some(me)
    }

    pub fn is_home(&self, me: EntityId) -> bool {
        self.home().is_none_or(|h| h == me)
    }

    /// Whether `session` already has a window (else the home window takes it).
    pub fn owned(&self, session: &str) -> bool {
        self.owner.get(session).is_some_and(|id| self.mains.iter().any(|m| m.id == *id))
    }

    /// Another main window is open (closing this one isn't quitting).
    pub fn has_other(&self, me: EntityId) -> bool {
        self.mains.iter().any(|m| m.id != me && !self.leaving.contains(&m.id))
    }

    pub fn claim(&mut self, session: &str, me: EntityId) {
        self.owner.insert(session.to_string(), me);
    }

    fn main(&self, id: EntityId) -> Option<Main> {
        self.mains.iter().find(|m| m.id == id).cloned()
    }
}

/// Shared by every main window (each keeps a clone, so `&self` code can ask where a terminal shows).
pub type Shared = Rc<RefCell<Registry>>;

#[derive(Default)]
struct Windows(Shared);
impl Global for Windows {}

fn shared(cx: &mut App) -> Shared {
    cx.default_global::<Windows>().0.clone()
}

/// A new main window: it becomes the home window.
pub fn register(entity: WeakEntity<MainWindow>, id: EntityId, window: &Window, cx: &mut App) -> Shared {
    let reg = shared(cx);
    let (handle, ns) = (window.window_handle(), crate::ui::twilight::ns_window(window));
    reg.borrow_mut().mains.insert(0, Main { id, entity, handle, ns });
    save_soon(cx);
    reg
}

/// The main windows' AppKit windows, most recently focused first (the badge docks only in these).
pub fn ns_windows(cx: &mut App) -> Vec<objc2::rc::Retained<objc2_app_kit::NSWindow>> {
    shared(cx).borrow().mains.iter().filter_map(|m| m.ns.clone()).collect()
}

/// `id` was focused: it's the home window now.
pub fn focused(id: EntityId, cx: &mut App) {
    let reg = shared(cx);
    let mut r = reg.borrow_mut();
    if let Some(i) = r.mains.iter().position(|m| m.id == id) {
        let m = r.mains.remove(i);
        r.mains.insert(0, m);
    }
}

/// A main window closed: its terminals go to the home window.
pub fn closed(id: EntityId, cx: &mut App) {
    let reg = shared(cx);
    let home = {
        let mut r = reg.borrow_mut();
        r.mains.retain(|m| m.id != id);
        r.owner.retain(|_, o| *o != id);
        r.home().and_then(|h| r.main(h))
    };
    if let Some(h) = home {
        let _ = h.entity.update(cx, |m, cx| m.request_refresh(crate::app::refresh::ALL, cx));
    }
}

/// The human is closing main window `id`: drop it from the saved layout.
pub fn leaving(id: EntityId, cx: &mut App) {
    shared(cx).borrow_mut().leaving.push(id);
    save_soon(cx);
}

/// One main window in `app-state.json` → `windows` (most recently used first).
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Saved {
    /// x, y, width, height in global (screen) coordinates.
    bounds: [f32; 4],
    terminals: Vec<String>,
    #[serde(default)]
    selected: Option<String>,
}

/// Save the windows, where they are and their terminals, a moment from now (window moves
/// fire this many times a second).
pub fn save_soon(cx: &mut App) {
    let reg = shared(cx);
    if std::mem::replace(&mut reg.borrow_mut().save_pending, true) {
        return;
    }
    cx.spawn(async move |cx| {
        cx.background_executor().timer(std::time::Duration::from_millis(400)).await;
        let _ = cx.update(save_layout);
    })
    .detach();
}

fn save_layout(cx: &mut App) {
    let reg = shared(cx);
    let (mains, leaving) = {
        let mut r = reg.borrow_mut();
        r.save_pending = false;
        (r.mains.clone(), r.leaving.clone())
    };
    let Some(first) = mains.first().and_then(|m| m.entity.upgrade()) else { return };
    let backend = first.read(cx).backend.clone();
    let all: Vec<String> = first.read(cx).sessions.iter().map(|s| s.id.clone()).collect();
    let mut out = vec![];
    for m in mains.iter().filter(|m| !leaving.contains(&m.id)) {
        let Some(e) = m.entity.upgrade() else { continue };
        let Ok(b) = m.handle.update(cx, |_, w, _| w.bounds()) else { continue };
        let r = reg.borrow();
        let terminals = all.iter().filter(|s| r.shows(s, m.id)).cloned().collect();
        out.push(Saved { bounds: [b.origin.x.into(), b.origin.y.into(), b.size.width.into(), b.size.height.into()], terminals, selected: e.read(cx).selected.clone() });
    }
    crate::ui::statusbar::update_state(&backend, "windows", serde_json::to_value(out).unwrap_or_default());
}

/// Launch: reopen the saved windows with their terminals (or one window at `fallback`).
/// Returns the window in front (the one used last).
pub fn restore(backend: Arc<dyn Backend>, fallback: Bounds<Pixels>, cx: &mut App) -> Option<WindowHandle<MainWindow>> {
    let saved: Vec<Saved> = crate::ui::statusbar::load_state(&backend, "windows");
    if saved.is_empty() {
        return open(backend, Some(fallback), cx);
    }
    let displays: Vec<Bounds<Pixels>> = cx.displays().iter().map(|d| d.bounds()).collect();
    let mut front = None;
    // Oldest first, so the one used last opens last: in front, and the home window.
    for w in saved.iter().rev() {
        let [x, y, width, height] = w.bounds;
        let b = Bounds::new(point(px(x), px(y)), size(px(width.max(720.)), px(height.max(420.))));
        // A display that's gone (unplugged monitor): put the window back on a visible one.
        let on_screen = displays.iter().any(|d| d.intersects(&b));
        let Some(h) = open(backend.clone(), Some(if on_screen { b } else { Bounds::centered(None, b.size, cx) }), cx) else { continue };
        if let Some(id) = id_of(h, cx) {
            let reg = shared(cx);
            for t in &w.terminals {
                reg.borrow_mut().claim(t, id);
            }
        }
        if let Some(sel) = w.selected.clone() {
            let _ = h.update(cx, |m, _, _| m.selected = Some(sel));
        }
        front = Some(h);
    }
    front
}

/// "this window" / "other window" when another main window has a terminal with the same
/// name as `s` (⌘K, needs-you cards), so the two can be told apart.
pub fn name_note(m: &MainWindow, s: &crate::model::Session) -> Option<&'static str> {
    let here = m.shows(&s.id);
    let clash = m.sessions.iter().any(|o| o.id != s.id && o.name == s.name && m.shows(&o.id) != here);
    clash.then_some(if here { "this window" } else { "other window" })
}

/// Bring `session` forward wherever it lives: its pop-out, or its main window (selected there).
pub fn reveal(session: String, cx: &mut App) {
    reveal_need(session, None, cx)
}

/// `reveal`, for a clicked notification: about a needs-you item (`need`), open the stack on
/// that card, or its terminal when the card can't show the whole question (`needs_you::too_long`).
pub fn reveal_need(session: String, need: Option<String>, cx: &mut App) {
    reveal_on(session, need, true, cx)
}

/// `reveal`, for a notification's "Terminal" button: its terminal, never the stack. Its
/// needs-you card (`need`) shows only when the terminal is gone.
pub fn reveal_terminal(session: String, need: Option<String>, cx: &mut App) {
    reveal_on(session, need, false, cx)
}

/// `reveal_need`; `card`: show the needs-you card over a terminal that's still there.
fn reveal_on(session: String, need: Option<String>, card: bool, cx: &mut App) {
    if crate::ui::popout::activate(&session, cx) {
        return;
    }
    let target = {
        let reg = shared(cx);
        let r = reg.borrow();
        r.place(&session).and_then(|id| r.main(id))
    };
    let Some(t) = target else { return };
    let _ = t.handle.update(cx, |_, window, cx| {
        window.activate_window();
        let _ = t.entity.update(cx, |m, cx| {
            if !m.sessions.iter().any(|s| s.id == session) {
                // No terminal (none behind it, or it's closed): its needs-you card, if still open.
                if let Some(id) = need.filter(|id| m.needs.iter().any(|n| n.id == *id)) {
                    crate::ui::needs_you::show(m, id, window, cx);
                }
                return;
            }
            if m.screen != Screen::Terminal {
                m.set_screen(Screen::Terminal, window, cx);
            }
            m.select(session, window, cx);
            match need.filter(|_| card).and_then(|id| m.needs.iter().find(|n| n.id == id).cloned()) {
                Some(n) if !crate::ui::needs_you::too_long(&n) => crate::ui::needs_you::show(m, n.id, window, cx),
                // A stack left open would hide the terminal you asked for.
                _ if m.overlay == Overlay::NeedsYou => m.set_overlay(Overlay::None, window, cx),
                _ => {}
            }
        });
    });
}

/// Run `f` on the main window you're using (the active one, else the home window).
pub fn with_active(cx: &mut App, f: impl FnOnce(&mut MainWindow, &mut Window, &mut Context<MainWindow>)) {
    let active = cx.active_window();
    let target = {
        let reg = shared(cx);
        let r = reg.borrow();
        r.mains.iter().find(|m| Some(m.handle) == active).or(r.mains.first()).cloned()
    };
    let Some(t) = target else { return };
    let _ = t.handle.update(cx, |_, window, cx| {
        let _ = t.entity.update(cx, |m, cx| f(m, window, cx));
    });
}

/// Whether `session` is on screen in the window you're using (notifications skip it).
/// Called from main window `me` (its own state passed in, since it's being updated).
pub fn on_screen(session: &str, window: &Window, me: EntityId, my_selected: Option<&str>, cx: &App) -> bool {
    if crate::ui::popout::is_active(session, cx) {
        return true;
    }
    let Some(reg) = cx.try_global::<Windows>().map(|w| w.0.clone()) else { return false };
    let r = reg.borrow();
    match r.place(session) {
        Some(id) if id == me => window.is_window_active() && my_selected == Some(session),
        Some(id) => r.main(id).is_some_and(|m| cx.active_window() == Some(m.handle) && m.entity.upgrade().is_some_and(|e| e.read(cx).selected.as_deref() == Some(session))),
        None => false,
    }
}

/// A daemon `window.command` for main window `id` (the one showing its target).
pub fn command(id: EntityId, v: serde_json::Value, cx: &mut App) {
    let reg = shared(cx);
    let Some(t) = reg.borrow().main(id) else { return };
    let _ = t.handle.update(cx, |_, window, cx| {
        let _ = t.entity.update(cx, |m, cx| m.on_window_command(v, window, cx));
    });
}

/// Where a drag released at `screen` (global coordinates) lands: another main window, or
/// nowhere (None: outside every window). `Some(me)` = still over the source window.
pub fn window_at(screen: Point<Pixels>, me: EntityId, my_bounds: Bounds<Pixels>, cx: &mut App) -> Option<EntityId> {
    let reg = shared(cx);
    let mains = reg.borrow().mains.clone();
    // Front to back, so overlapping windows resolve to the one you see.
    let stack = cx.window_stack().unwrap_or_else(|| mains.iter().map(|m| m.handle).collect());
    for h in stack {
        if let Some(m) = mains.iter().find(|m| m.handle == h) {
            let bounds = if m.id == me { Some(my_bounds) } else { h.update(cx, |_, w, _| w.bounds()).ok() };
            if bounds.is_some_and(|b| b.contains(&screen)) {
                return Some(m.id);
            }
        } else if h.update(cx, |_, w, _| w.bounds()).is_ok_and(|b| b.contains(&screen)) {
            // a pop-out or Settings in front: not a target
            return Some(me);
        }
    }
    None
}

/// Show which window a drag is over (`None`: no highlight).
pub fn set_drop_hint(id: Option<EntityId>, cx: &mut App) {
    let reg = shared(cx);
    let (old, mains) = {
        let mut r = reg.borrow_mut();
        let old = std::mem::replace(&mut r.drop_hint, id);
        (old, r.mains.clone())
    };
    if old == id {
        return;
    }
    for m in mains.iter().filter(|m| Some(m.id) == old || Some(m.id) == id) {
        let _ = m.entity.update(cx, |_, cx| cx.notify());
    }
}

/// Hand `sessions` to `target` (they've already left the source window) and show the first.
pub fn give(sessions: Vec<String>, target: EntityId, cx: &mut App) {
    let reg = shared(cx);
    let Some(t) = reg.borrow().main(target) else { return };
    for s in &sessions {
        reg.borrow_mut().claim(s, target);
    }
    save_soon(cx);
    let Some(first) = sessions.into_iter().next() else { return };
    let _ = t.handle.update(cx, |_, window, cx| {
        window.activate_window();
        let _ = t.entity.update(cx, |m, cx| {
            if m.screen != Screen::Terminal {
                m.set_screen(Screen::Terminal, window, cx);
            }
            m.select(first, window, cx);
        });
    });
}

/// A main window at `bounds` (None: centered, or cascaded from the active window).
pub fn open(backend: Arc<dyn Backend>, bounds: Option<Bounds<Pixels>>, cx: &mut App) -> Option<WindowHandle<MainWindow>> {
    let size_env = |k: &str, d: f32| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
    let sz = size(px(size_env("MIDNA_W", 1280.)), px(size_env("MIDNA_H", 800.)));
    let cascade = cx.active_window().and_then(|w| w.update(cx, |_, window, _| window.bounds()).ok()).map(|b| Bounds::new(b.origin + point(px(28.), px(28.)), b.size));
    let bounds = bounds.or(cascade).unwrap_or_else(|| Bounds::centered(None, sz, cx));
    let opts = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions { title: Some("midna".into()), appears_transparent: true, traffic_light_position: Some(point(px(16.), px(16.))) }),
        window_min_size: Some(size(px(720.), px(420.))),
        app_id: Some("com.mrgnhnt.midna".into()),
        // Keep rendering daemon updates at full rate while the window is in the background.
        inactive_frame_interval: None,
        // Screenshot runs float the window so it is never occluded (occluded windows stop drawing).
        kind: if crate::dev::var("MIDNA_DEBUG_ONTOP").is_ok() { WindowKind::PopUp } else { WindowKind::Normal },
        focus: std::env::var("MIDNA_NO_ACTIVATE").is_err(),
        // See-through until MainWindow::new decides (the launch entrance shows the desktop first).
        // (Offscreen snapshots need an opaque window.)
        window_background: if std::env::var("MIDNA_SNAPSHOT").is_ok() { WindowBackgroundAppearance::Opaque } else { WindowBackgroundAppearance::Transparent },
        ..Default::default()
    };
    cx.open_window(opts, |window, cx| cx.new(|cx| MainWindow::new(backend, window, cx))).ok()
}

/// The entity id of a main window's root.
pub fn id_of(handle: WindowHandle<MainWindow>, cx: &mut App) -> Option<EntityId> {
    handle.update(cx, |_, _, cx| cx.entity_id()).ok()
}
