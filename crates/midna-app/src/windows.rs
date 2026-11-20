//! Main windows (⌘⇧N opens more). Each shows one Mac's terminals: this Mac's, or another of
//! my Macs' over a drive link (the Window menu opens it). Every terminal shows in exactly one
//! window of its Mac, the window that owns it; a pop-out still counts as part of the window it
//! came from. A terminal nobody owns yet (just opened by an agent, a trigger or the CLI, or its
//! window closed) goes to its Mac's home window: that Mac's main window focused last. The home
//! window also takes its daemon's `window.command`s, and this Mac's posts notifications and
//! takes update commands, so a second window doesn't double them. Closing this Mac's last window
//! while another Mac's is open only hides it (see [`hide`]): those duties keep a window, and it
//! comes back as it was. Sidebar rows dragged onto
//! another window of the same Mac move there; dropped outside every window they get a new one.
//! A terminal never moves to another Mac's window.
use crate::app::{MainWindow, Overlay, Screen};
use crate::backend::{Backend, Mac};
use gpui_kit::prelude::*;
use gpui_kit::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

/// Which Mac a window shows: another Mac's fingerprint, or None for this one.
pub type MacId = Option<String>;

#[derive(Clone)]
struct Main {
    id: EntityId,
    entity: WeakEntity<MainWindow>,
    handle: AnyWindowHandle,
    ns: Option<objc2::rc::Retained<objc2_app_kit::NSWindow>>,
}

/// Which Mac each main window shows, most recently focused first, and the window each
/// terminal shows in. Plain data: the windows themselves are `Registry::mains`.
#[derive(Default, Debug)]
pub struct Places {
    wins: Vec<(EntityId, MacId)>,
    /// The window each terminal shows in, by its Mac and its id.
    owner: HashMap<(MacId, String), EntityId>,
    /// This Mac's last window, closed while another Mac's was open: hidden, not gone, so this
    /// Mac's banners, sounds, badge and update commands still have a window. It shows again
    /// the next time it's brought forward (focused).
    hidden: Option<EntityId>,
}

impl Places {
    /// A new window for `mac`: it becomes that Mac's home window.
    fn opened(&mut self, id: EntityId, mac: MacId) {
        self.wins.insert(0, (id, mac));
    }

    fn focused(&mut self, id: EntityId) {
        if self.hidden == Some(id) {
            self.hidden = None;
        }
        if let Some(i) = self.wins.iter().position(|w| w.0 == id) {
            let w = self.wins.remove(i);
            self.wins.insert(0, w);
        }
    }

    /// Window `id` closed: its terminals go to its Mac's home window, returned.
    fn closed(&mut self, id: EntityId) -> Option<EntityId> {
        let mac = self.mac_of(id);
        if self.hidden == Some(id) {
            self.hidden = None;
        }
        self.wins.retain(|w| w.0 != id);
        self.owner.retain(|_, o| *o != id);
        mac.and_then(|mac| self.home(&mac))
    }

    fn open(&self, id: EntityId) -> bool {
        self.wins.iter().any(|w| w.0 == id)
    }

    /// `mac`'s home window: its main window focused last.
    fn home(&self, mac: &MacId) -> Option<EntityId> {
        self.wins.iter().find(|w| &w.1 == mac).map(|w| w.0)
    }

    fn mac_of(&self, me: EntityId) -> Option<MacId> {
        self.wins.iter().find(|w| w.0 == me).map(|w| w.1.clone())
    }

    /// The window `session` (one of `mac`'s terminals) shows in.
    pub fn place(&self, mac: &MacId, session: &str) -> Option<EntityId> {
        self.owner.get(&(mac.clone(), session.to_string())).copied().filter(|id| self.open(*id)).or(self.home(mac))
    }

    pub fn shows(&self, session: &str, me: EntityId) -> bool {
        self.mac_of(me).is_some_and(|mac| self.place(&mac, session) == Some(me))
    }

    /// `me` is its Mac's home window.
    pub fn is_home(&self, me: EntityId) -> bool {
        match self.mac_of(me) {
            Some(mac) => self.home(&mac) == Some(me),
            None => true,
        }
    }

    /// Whether `session`, one of `me`'s Mac's terminals, already has a window (else that
    /// Mac's home window takes it).
    pub fn owned(&self, session: &str, me: EntityId) -> bool {
        let Some(mac) = self.mac_of(me) else { return false };
        self.owner.get(&(mac, session.to_string())).is_some_and(|id| self.open(*id))
    }

    /// `session`, one of `me`'s Mac's terminals, shows in `me` now.
    pub fn claim(&mut self, session: &str, me: EntityId) {
        if let Some(mac) = self.mac_of(me) {
            self.owner.insert((mac, session.to_string()), me);
        }
    }

    /// Whether windows `a` and `b` show the same Mac (a terminal may move between them).
    pub fn same_mac(&self, a: EntityId, b: EntityId) -> bool {
        matches!((self.mac_of(a), self.mac_of(b)), (Some(x), Some(y)) if x == y)
    }

    /// Another Mac's window is open.
    fn any_remote(&self) -> bool {
        self.wins.iter().any(|w| w.1.is_some())
    }

    /// The windows on screen (all but a hidden one), most recently focused first.
    fn shown(&self) -> impl Iterator<Item = &(EntityId, MacId)> {
        self.wins.iter().filter(|w| Some(w.0) != self.hidden)
    }

    pub fn is_hidden(&self, id: EntityId) -> bool {
        self.hidden == Some(id)
    }

    /// Closing `me` hides it instead: it's this Mac's last window on screen and another Mac's
    /// window stays open (`leaving`: windows already on their way out).
    fn hides_on_close(&self, me: EntityId, leaving: &[EntityId]) -> bool {
        let stays = |w: &&(EntityId, MacId)| w.0 != me && !leaving.contains(&w.0);
        self.hidden.is_none()
            && self.mac_of(me) == Some(None)
            && !self.shown().filter(stays).any(|w| w.1.is_none())
            && self.shown().filter(stays).any(|w| w.1.is_some())
    }

    /// `id` is hidden (see `hidden`): last in line, so the window you see is the one used last.
    fn hide(&mut self, id: EntityId) {
        self.hidden = Some(id);
        if let Some(i) = self.wins.iter().position(|w| w.0 == id) {
            let w = self.wins.remove(i);
            self.wins.push(w);
        }
    }

    /// The hidden window, once no window is on screen (it closes then: nothing's left to quit for).
    fn hidden_alone(&self) -> Option<EntityId> {
        self.hidden.filter(|_| self.shown().next().is_none())
    }
}

/// The name a window of `mac` (None: this Mac) shows in its sidebar's plate, its status bar and
/// its title: another Mac's always; this Mac's (`local`) only while another Mac's window is
/// open. None: the window looks as it did before there were other Macs' windows.
fn shown_name(mac: Option<&Mac>, any_remote: bool, local: Option<&str>) -> Option<String> {
    match mac {
        Some(m) => Some(m.name.clone()),
        None => local.filter(|_| any_remote).map(str::to_string),
    }
}

#[derive(Default)]
pub struct Registry {
    places: Places,
    /// The main windows, by id (`places` has their order).
    mains: HashMap<EntityId, Main>,
    /// The window a dragged sidebar row is over (it highlights its sidebar).
    pub drop_hint: Option<EntityId>,
    /// Windows the human is closing (left out of the saved layout). A window closed by
    /// quitting isn't here, so the next launch reopens it.
    leaving: Vec<EntityId>,
    save_pending: bool,
    /// This Mac's name (`peers.list`'s `me`), once its daemon said it.
    local_name: Option<String>,
    /// The name this Mac's windows show now (see `shown_name`).
    local_shown: Option<String>,
}

impl std::ops::Deref for Registry {
    type Target = Places;
    fn deref(&self) -> &Places {
        &self.places
    }
}

impl Registry {
    /// Another main window is open (closing this one isn't quitting).
    pub fn has_other(&self, me: EntityId) -> bool {
        self.places.shown().any(|w| w.0 != me && !self.leaving.contains(&w.0))
    }

    /// Closing `me` hides it instead (see `Places::hidden`).
    pub fn hides_on_close(&self, me: EntityId) -> bool {
        self.places.hides_on_close(me, &self.leaving)
    }

    pub fn claim(&mut self, session: &str, me: EntityId) {
        self.places.claim(session, me)
    }

    /// The name a window of `mac` shows (see `shown_name`).
    pub fn shown_name(&self, mac: Option<&Mac>) -> Option<String> {
        shown_name(mac, self.places.any_remote(), self.local_name.as_deref())
    }

    fn main(&self, id: EntityId) -> Option<Main> {
        self.mains.get(&id).cloned()
    }

    /// The main windows, most recently focused first, with the Mac each shows.
    fn ordered(&self) -> Vec<(Main, MacId)> {
        self.places.wins.iter().filter_map(|(id, mac)| Some((self.mains.get(id)?.clone(), mac.clone()))).collect()
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

/// A new main window for `mac`: it becomes that Mac's home window.
pub fn register(entity: WeakEntity<MainWindow>, id: EntityId, mac: MacId, window: &Window, cx: &mut App) -> Shared {
    let reg = shared(cx);
    let (handle, ns) = (window.window_handle(), crate::ui::twilight::ns_window(window));
    let mut r = reg.borrow_mut();
    // This Mac has a window on screen again: the hidden one goes, its terminals to this one.
    let hidden = r.places.hidden.filter(|_| mac.is_none()).and_then(|h| r.main(h));
    r.places.opened(id, mac);
    r.mains.insert(id, Main { id, entity, handle, ns });
    drop(r);
    if let Some(h) = hidden {
        cx.defer(move |cx| remove(&h, cx));
    }
    save_soon(cx);
    cx.defer(sync_names);
    cx.defer(crate::macs::set_menus);
    reg
}

/// Another Mac's window opened or closed, or this Mac's name changed: this Mac's windows show
/// its name, or stop showing it, in their plate, status bar and title.
fn sync_names(cx: &mut App) {
    let reg = shared(cx);
    let name = {
        let mut r = reg.borrow_mut();
        let now = r.shown_name(None);
        if r.local_shown == now {
            return;
        }
        r.local_shown = now.clone();
        now
    };
    let mains = reg.borrow().ordered();
    let title = title(name.as_deref());
    for (m, _) in mains.into_iter().filter(|(_, mac)| mac.is_none()) {
        let _ = m.handle.update(cx, |_, window, _| window.set_window_title(&title));
        let _ = m.entity.update(cx, |_, cx| cx.notify());
    }
}

/// This Mac's name, from its daemon (`peers.list`).
pub(crate) fn set_local_name(name: String, cx: &mut App) {
    let reg = shared(cx);
    let name = Some(name).filter(|n| !n.is_empty());
    if std::mem::replace(&mut reg.borrow_mut().local_name, name.clone()) != name {
        sync_names(cx);
    }
}

/// The main windows' AppKit windows on screen, most recently focused first (the badge docks
/// only in these).
pub fn ns_windows(cx: &mut App) -> Vec<objc2::rc::Retained<objc2_app_kit::NSWindow>> {
    let reg = shared(cx);
    let r = reg.borrow();
    r.ordered().into_iter().filter(|(m, _)| !r.is_hidden(m.id)).filter_map(|(m, _)| m.ns).collect()
}

/// `id` was focused: it's its Mac's home window now.
pub fn focused(id: EntityId, cx: &mut App) {
    let reg = shared(cx);
    reg.borrow_mut().places.focused(id);
    // The Window menu checks the front window's Mac.
    cx.defer(crate::macs::set_menus);
}

/// A main window closed: its terminals go to its Mac's home window. The last one on screen
/// takes a hidden one with it (midna quits).
pub fn closed(id: EntityId, cx: &mut App) {
    let reg = shared(cx);
    let (home, alone) = {
        let mut r = reg.borrow_mut();
        r.mains.remove(&id);
        let home = r.places.closed(id).and_then(|h| r.main(h));
        (home, r.places.hidden_alone().and_then(|h| r.main(h)))
    };
    if let Some(h) = alone {
        cx.defer(move |cx| remove(&h, cx));
    } else if let Some(h) = home {
        let _ = h.entity.update(cx, |m, cx| m.request_refresh(crate::app::refresh::ALL, cx));
    }
    cx.defer(sync_names);
    cx.defer(crate::macs::set_menus);
}

/// Close this Mac's last window on screen by hiding it (`Registry::hides_on_close`): it keeps
/// its terminals and this Mac's duties, and the window you see now is another Mac's.
pub fn hide(id: EntityId, window: &Window, cx: &mut App) {
    let reg = shared(cx);
    let next = {
        let mut r = reg.borrow_mut();
        r.places.hide(id);
        r.places.shown().next().and_then(|w| r.main(w.0))
    };
    if let Some(ns) = crate::ui::twilight::ns_window(window) {
        ns.orderOut(None);
    }
    if let Some(n) = next {
        cx.defer(move |cx| {
            let _ = n.handle.update(cx, |_, window, _| window.activate_window());
        });
    }
    save_soon(cx);
    cx.defer(crate::macs::set_menus);
}

/// Close main window `m` for good.
fn remove(m: &Main, cx: &mut App) {
    let _ = m.handle.update(cx, |_, window, _| window.remove_window());
}

/// The human is closing main window `id`: drop it from the saved layout.
pub fn leaving(id: EntityId, cx: &mut App) {
    shared(cx).borrow_mut().leaving.push(id);
    save_soon(cx);
}

/// One main window in `app-state.json` → `windows` (most recently used first).
#[derive(serde::Serialize, serde::Deserialize, Default, Debug, PartialEq)]
struct Saved {
    /// Another of my Macs' window: its fingerprint and name (absent: this Mac's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mac: Option<SavedMac>,
    /// x, y, width, height in global (screen) coordinates.
    bounds: [f32; 4],
    terminals: Vec<String>,
    #[serde(default)]
    selected: Option<String>,
    /// Each terminal's split (`ui/split.rs`), by the terminal it belongs to.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    splits: HashMap<String, crate::ui::split::SplitOf>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq, Clone)]
struct SavedMac {
    peer: String,
    name: String,
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
        (r.ordered().into_iter().map(|(m, _)| m).collect::<Vec<_>>(), r.leaving.clone())
    };
    let Some(first) = mains.first().and_then(|m| m.entity.upgrade()) else { return };
    // app-state.json is this Mac's, whichever Mac's window is in front.
    let backend = crate::backend::local(&first.read(cx).backend);
    let mut out = vec![];
    for m in mains.iter().filter(|m| !leaving.contains(&m.id)) {
        let Some(e) = m.entity.upgrade() else { continue };
        let Ok(b) = m.handle.update(cx, |_, w, _| w.bounds()) else { continue };
        let r = reg.borrow();
        let e = e.read(cx);
        let terminals = e.sessions.iter().filter(|s| r.shows(&s.id, m.id)).map(|s| s.id.clone()).collect();
        let mac = e.backend.mac().map(|x| SavedMac { peer: x.peer.clone(), name: x.name.clone() });
        let bounds = [b.origin.x.into(), b.origin.y.into(), b.size.width.into(), b.size.height.into()];
        out.push(Saved { mac, bounds, terminals, selected: e.selected.clone(), splits: e.splits.clone() });
    }
    crate::ui::statusbar::update_state(&backend, "windows", serde_json::to_value(out).unwrap_or_default());
}

/// Launch: reopen the saved windows with their terminals (or one window at `fallback`),
/// another Mac's over its drive link again. Returns the window in front (the one used last).
pub fn restore(backend: Arc<dyn Backend>, fallback: Bounds<Pixels>, cx: &mut App) -> Option<WindowHandle<MainWindow>> {
    let mut saved: Vec<Saved> = crate::ui::statusbar::load_state(&backend, "windows");
    // This Mac always gets a window: its terminals, notifications and updates live there.
    if !saved.iter().any(|w| w.mac.is_none()) {
        saved.push(Saved { bounds: [fallback.origin.x.into(), fallback.origin.y.into(), fallback.size.width.into(), fallback.size.height.into()], ..Default::default() });
    }
    let displays: Vec<Bounds<Pixels>> = cx.displays().iter().map(|d| d.bounds()).collect();
    let mut front = None;
    // Oldest first, so the one used last opens last: in front, and the home window.
    for w in saved.iter().rev() {
        let [x, y, width, height] = w.bounds;
        let b = Bounds::new(point(px(x), px(y)), size(px(width.max(720.)), px(height.max(420.))));
        // A display that's gone (unplugged monitor): put the window back on a visible one.
        let on_screen = displays.iter().any(|d| d.intersects(&b));
        let b = if on_screen { b } else { Bounds::centered(None, b.size, cx) };
        let backend = match &w.mac {
            Some(m) => crate::backend::for_mac(&backend, Mac { peer: m.peer.clone(), name: m.name.clone() }),
            None => backend.clone(),
        };
        let Some(h) = open(backend, Some(b), cx) else { continue };
        if let Some(id) = id_of(h, cx) {
            let reg = shared(cx);
            for t in &w.terminals {
                reg.borrow_mut().claim(t, id);
            }
        }
        let (sel, splits) = (w.selected.clone(), w.splits.clone());
        let _ = h.update(cx, |m, _, _| {
            m.selected = sel.or(m.selected.take());
            m.splits = splits;
        });
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

/// Bring `session`, one of `mac`'s terminals, forward wherever it lives: its pop-out, or its
/// main window (selected there).
pub fn reveal(mac: MacId, session: String, cx: &mut App) {
    reveal_on(mac, session, None, true, cx)
}

/// Run `f` on `mac`'s home window (its main window focused last), brought to the front.
/// Nothing when that Mac has no main window open.
pub fn on_home(mac: &MacId, cx: &mut App, f: impl FnOnce(&mut MainWindow, &mut Window, &mut Context<MainWindow>) + 'static) {
    let target = {
        let reg = shared(cx);
        let r = reg.borrow();
        r.home(mac).and_then(|id| r.main(id))
    };
    let Some(t) = target else { return };
    let _ = t.handle.update(cx, |_, window, cx| {
        window.activate_window();
        let _ = t.entity.update(cx, |m, cx| f(m, window, cx));
    });
}

/// What waits on this Mac, as its home window last listed it (empty with no window onto it).
/// Not from inside that window's own update: it reads the window.
pub fn local_needs(cx: &App) -> Vec<crate::model::NeedsYou> {
    let Some(reg) = cx.try_global::<Windows>().map(|w| w.0.clone()) else { return vec![] };
    let target = {
        let r = reg.borrow();
        r.home(&None).and_then(|id| r.main(id))
    };
    target.and_then(|t| t.entity.upgrade()).map(|e| e.read(cx).needs.clone()).unwrap_or_default()
}

/// `reveal` of one of this Mac's terminals, for a clicked notification: about a needs-you
/// item (`need`), open the stack on that card, or its terminal when the card can't show the
/// whole question (`needs_you::too_long`).
pub fn reveal_need(session: String, need: Option<String>, cx: &mut App) {
    reveal_on(None, session, need, true, cx)
}

/// `reveal` of one of this Mac's terminals, for a notification's "Terminal" button: its
/// terminal, never the stack. Its needs-you card (`need`) shows only when the terminal is gone.
pub fn reveal_terminal(session: String, need: Option<String>, cx: &mut App) {
    reveal_on(None, session, need, false, cx)
}

/// Another of my Macs' needs-you item, from this Mac's pill ("Open on <Mac>"): that Mac's window
/// comes forward on its terminal with its card showing. With no window onto that Mac, one opens
/// and connects first (its connecting card), and goes there once it has its terminals.
pub fn reveal_on_mac(local: &Arc<dyn Backend>, mac: Mac, session: Option<String>, need: Option<String>, cx: &mut App) {
    let id: MacId = Some(mac.peer.clone());
    let Some(w) = open_mac(local, Some(mac), cx) else { return };
    let listed = w.read_with(cx, |m, _| m.listed).unwrap_or(false);
    if listed {
        reveal_there(id, session, need, cx);
    } else {
        let _ = w.update(cx, |m, _| m.reveal_waiting = Some((session, need)));
    }
}

/// `mac`'s needs-you item `need`: its terminal with its card over it, or without a terminal, its
/// card in that Mac's home window.
pub fn reveal_there(mac: MacId, session: Option<String>, need: Option<String>, cx: &mut App) {
    if let Some(session) = session {
        return reveal_on(mac, session, need, true, cx);
    }
    let target = {
        let reg = shared(cx);
        let r = reg.borrow();
        r.home(&mac).and_then(|id| r.main(id))
    };
    let Some(t) = target else { return };
    let _ = t.handle.update(cx, |_, window, cx| {
        window.activate_window();
        let _ = t.entity.update(cx, |m, cx| {
            if let Some(id) = need.filter(|id| m.needs.iter().any(|n| n.id == *id)) {
                crate::ui::needs_you::show(m, id, window, cx);
            }
        });
    });
}

/// `reveal_need`; `card`: show the needs-you card over a terminal that's still there.
fn reveal_on(mac: MacId, session: String, need: Option<String>, card: bool, cx: &mut App) {
    if crate::ui::popout::activate(&session, cx) {
        return;
    }
    let target = {
        let reg = shared(cx);
        let r = reg.borrow();
        r.place(&mac, &session).and_then(|id| r.main(id))
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

/// Run `f` on the main window you're using (the active one, else the one used last).
pub fn with_active(cx: &mut App, f: impl FnOnce(&mut MainWindow, &mut Window, &mut Context<MainWindow>)) {
    with_active_where(cx, |_| true, f)
}

/// `with_active`, among this Mac's windows: for what works on this Mac's files and daemon
/// (a folder opened in Midna, this Mac's notifications and hooks).
pub fn with_active_local(cx: &mut App, f: impl FnOnce(&mut MainWindow, &mut Window, &mut Context<MainWindow>)) {
    with_active_where(cx, |m| m.is_none(), f)
}

fn with_active_where(cx: &mut App, mac: impl Fn(&MacId) -> bool, f: impl FnOnce(&mut MainWindow, &mut Window, &mut Context<MainWindow>)) {
    let active = cx.active_window();
    let target = {
        let reg = shared(cx);
        let r = reg.borrow();
        let mine: Vec<Main> = r.ordered().into_iter().filter(|(_, m)| mac(m)).map(|(m, _)| m).collect();
        mine.iter().find(|m| Some(m.handle) == active).or(mine.first()).cloned()
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
    let Some(mac) = r.mac_of(me) else { return false };
    match r.place(&mac, session) {
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
    let mains: Vec<Main> = reg.borrow().ordered().into_iter().map(|(m, _)| m).collect();
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
        (old, r.mains.values().cloned().collect::<Vec<_>>())
    };
    if old == id {
        return;
    }
    for m in mains.iter().filter(|m| Some(m.id) == old || Some(m.id) == id) {
        let _ = m.entity.update(cx, |_, cx| cx.notify());
    }
}

/// Hand `sessions` to `target` (they've already left the source window `from`) and show the
/// first. Refused (false) when `target` shows another Mac: a terminal stays on its Mac.
pub fn give(sessions: Vec<String>, from: EntityId, target: EntityId, cx: &mut App) -> bool {
    let reg = shared(cx);
    if !reg.borrow().same_mac(from, target) {
        return false;
    }
    let Some(t) = reg.borrow().main(target) else { return false };
    for s in &sessions {
        reg.borrow_mut().claim(s, target);
    }
    save_soon(cx);
    let Some(first) = sessions.into_iter().next() else { return true };
    let _ = t.handle.update(cx, |_, window, cx| {
        window.activate_window();
        let _ = t.entity.update(cx, |m, cx| {
            if m.screen != Screen::Terminal {
                m.set_screen(Screen::Terminal, window, cx);
            }
            m.select(first, window, cx);
        });
    });
    true
}

/// Whether windows `a` and `b` show the same Mac (a row may be dragged between them).
pub fn same_mac(a: EntityId, b: EntityId, cx: &mut App) -> bool {
    shared(cx).borrow().same_mac(a, b)
}

/// Open `mac`'s window (None: this Mac's), or bring its window used last forward, and return
/// it. `local` is this Mac's backend, which opens the drive link the first time.
pub fn open_mac(local: &Arc<dyn Backend>, mac: Option<Mac>, cx: &mut App) -> Option<WeakEntity<MainWindow>> {
    let target = {
        let reg = shared(cx);
        let r = reg.borrow();
        r.home(&mac.as_ref().map(|m| m.peer.clone())).and_then(|id| r.main(id))
    };
    match target {
        Some(t) => {
            let _ = t.handle.update(cx, |_, window, _| window.activate_window());
            Some(t.entity)
        }
        None => {
            let b = match mac {
                Some(mac) => crate::backend::for_mac(local, mac),
                None => local.clone(),
            };
            open(b, None, cx)?.update(cx, |_, _, cx| cx.entity().downgrade()).ok()
        }
    }
}

/// My paired Macs, by name (`macs` adds this one and how each stands). Whether one lets this
/// Mac drive it is that Mac's setting; once a link to it is refused, `drive.list` says so.
pub fn my_macs(l: &midna_proto::nearby::PeersListResult) -> Vec<Mac> {
    let mut macs: Vec<Mac> = l.paired.iter().filter(|p| p.tier == midna_proto::nearby::Tier::MyDevice).map(|p| Mac { peer: p.fingerprint.clone(), name: p.name.clone() }).collect();
    macs.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.peer.cmp(&b.peer)));
    macs
}

/// The Macs with a main window on screen, and the one the window focused last shows.
pub fn mac_windows(cx: &mut App) -> (Vec<MacId>, Option<MacId>) {
    let reg = shared(cx);
    let r = reg.borrow();
    (r.places.shown().map(|w| w.1.clone()).collect(), r.places.shown().next().map(|w| w.1.clone()))
}

/// Redraw every main window (the Macs' states changed).
pub fn redraw_all(cx: &mut App) {
    let mains = shared(cx).borrow().ordered();
    for (m, _) in mains {
        let _ = m.entity.update(cx, |_, cx| cx.notify());
    }
}

/// Every other Mac's window works out its theme again (this Mac's picks for them changed).
/// Never call it from inside a main window's update: it updates each one.
pub fn reapply_mac_themes(cx: &mut App) {
    let remote: Vec<Main> = {
        let reg = shared(cx);
        let r = reg.borrow();
        r.ordered().into_iter().filter(|(_, mac)| mac.is_some()).map(|(m, _)| m).collect()
    };
    for m in remote {
        let _ = m.handle.update(cx, |_, window, cx| {
            let _ = m.entity.update(cx, |w, cx| w.apply_theme(window, cx));
        });
    }
}

/// The title macOS shows for a window (Mission Control, ⌘`, the Window menu), from the name
/// it shows (`shown_name`): another Mac's window is named for it, and this Mac's beside one.
pub fn title(name: Option<&str>) -> SharedString {
    match name {
        Some(n) => format!("{n} — Midna").into(),
        None => "midna".into(),
    }
}

/// A main window at `bounds` (None: centered, or cascaded from the active window).
pub fn open(backend: Arc<dyn Backend>, bounds: Option<Bounds<Pixels>>, cx: &mut App) -> Option<WindowHandle<MainWindow>> {
    let size_env = |k: &str, d: f32| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
    let sz = size(px(size_env("MIDNA_W", 1280.)), px(size_env("MIDNA_H", 800.)));
    let cascade = cx.active_window().and_then(|w| w.update(cx, |_, window, _| window.bounds()).ok()).map(|b| Bounds::new(b.origin + point(px(28.), px(28.)), b.size));
    let bounds = bounds.or(cascade).unwrap_or_else(|| Bounds::centered(None, sz, cx));
    let name = shared(cx).borrow().shown_name(backend.mac());
    let opts = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions { title: Some(title(name.as_deref())), appears_transparent: true, traffic_light_position: Some(point(px(16.), px(16.))) }),
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
    let opts = crate::hidden::window_options(opts, cx);
    cx.open_window(opts, |window, cx| {
        crate::hidden::attach(window, cx);
        cx.new(|cx| MainWindow::new(backend, window, cx))
    })
    .ok()
}

/// The entity id of a main window's root.
pub fn id_of(handle: WindowHandle<MainWindow>, cx: &mut App) -> Option<EntityId> {
    handle.update(cx, |_, _, cx| cx.entity_id()).ok()
}

#[cfg(test)]
mod tests {
    use super::{MacId, Places, Saved, SavedMac, my_macs, shown_name, title};
    use crate::backend::{Backend, Mac};

    const MINI: &str = "ffff";

    fn mini() -> MacId {
        Some(MINI.to_string())
    }

    /// Windows 1 and 2 show this Mac, 3 and 4 the Mac mini; 4 and then 2 were focused last.
    fn four() -> Places {
        let mut p = Places::default();
        p.opened(1u64.into(), None);
        p.opened(3u64.into(), mini());
        p.opened(2u64.into(), None);
        p.opened(4u64.into(), mini());
        p.focused(2u64.into());
        p
    }

    #[test]
    fn each_mac_has_its_own_home_window() {
        let p = four();
        assert!(p.is_home(2u64.into()) && p.is_home(4u64.into()));
        assert!(!p.is_home(1u64.into()) && !p.is_home(3u64.into()));
        // A terminal nobody has yet shows in its own Mac's home window.
        assert_eq!(p.place(&None, "a1"), Some(2u64.into()));
        assert_eq!(p.place(&mini(), "b1"), Some(4u64.into()));
        assert!(p.shows("b1", 4u64.into()) && !p.shows("b1", 3u64.into()));
    }

    #[test]
    fn a_terminal_is_owned_per_mac() {
        let mut p = four();
        p.claim("s1", 3u64.into());
        assert_eq!(p.place(&mini(), "s1"), Some(3u64.into()));
        assert!(p.owned("s1", 4u64.into()), "another window of the Mac mini sees it's taken");
        // The same id on this Mac is another terminal: nobody has it yet.
        assert!(!p.owned("s1", 1u64.into()));
        assert_eq!(p.place(&None, "s1"), Some(2u64.into()));
        assert!(p.shows("s1", 3u64.into()) && !p.shows("s1", 4u64.into()));
        assert!(p.shows("s1", 2u64.into()), "this Mac's s1 is shown by this Mac's home window");
    }

    #[test]
    fn terminals_move_only_between_one_macs_windows() {
        let p = four();
        assert!(p.same_mac(1u64.into(), 2u64.into()));
        assert!(p.same_mac(3u64.into(), 4u64.into()));
        assert!(!p.same_mac(2u64.into(), 4u64.into()), "a terminal never moves to another Mac");
        assert!(!p.same_mac(2u64.into(), 9u64.into()), "nor to a window that's gone");
    }

    #[test]
    fn closing_a_window_hands_its_terminals_to_its_own_macs_home() {
        let mut p = four();
        p.claim("s1", 4u64.into());
        assert_eq!(p.closed(4u64.into()), Some(3u64.into()));
        assert!(!p.owned("s1", 3u64.into()));
        assert_eq!(p.place(&mini(), "s1"), Some(3u64.into()));
        assert_eq!(p.closed(3u64.into()), None, "the Mac mini's last window");
        assert_eq!(p.place(&mini(), "s1"), None);
        assert_eq!(p.place(&None, "a1"), Some(2u64.into()));
    }

    #[test]
    fn this_macs_last_window_hides_beside_another_macs() {
        let mut p = four();
        // Another of this Mac's windows is still open: 2 just closes.
        assert!(!p.hides_on_close(2u64.into(), &[]));
        assert!(!p.hides_on_close(3u64.into(), &[]), "another Mac's window never hides");
        // 1 is on its way out, so 2 is this Mac's last window: it hides.
        assert!(p.hides_on_close(2u64.into(), &[1u64.into()]));
        p.closed(1u64.into());
        p.claim("a1", 2u64.into());
        p.hide(2u64.into());
        assert!(p.is_hidden(2u64.into()));
        assert!(!p.hides_on_close(2u64.into(), &[]), "only one hidden window");
        // It's still this Mac's home: its banners, sounds and new terminals, and a1 stays put.
        assert!(p.is_home(2u64.into()));
        assert_eq!(p.place(&None, "a1"), Some(2u64.into()));
        assert_eq!(p.place(&None, "new"), Some(2u64.into()));
        // The Mac mini's window is the one you see, and the one used last.
        assert_eq!(p.shown().map(|w| w.0).collect::<Vec<_>>(), [4u64.into(), 3u64.into()]);
        assert_eq!(p.hidden_alone(), None);
        // Brought forward: on screen again.
        p.focused(2u64.into());
        assert!(!p.is_hidden(2u64.into()));
        assert_eq!(p.shown().next().map(|w| w.0), Some(2u64.into()));
    }

    #[test]
    fn a_hidden_window_goes_with_the_last_window_on_screen() {
        let mut p = Places::default();
        p.opened(1u64.into(), None);
        p.opened(2u64.into(), mini());
        assert!(!p.hides_on_close(2u64.into(), &[]), "another Mac's last window just closes");
        assert!(p.hides_on_close(1u64.into(), &[]));
        assert!(!p.hides_on_close(1u64.into(), &[2u64.into()]), "nothing stays on screen: midna quits");
        p.hide(1u64.into());
        p.closed(2u64.into());
        assert_eq!(p.hidden_alone(), Some(1u64.into()), "nothing on screen: it closes too");
        p.closed(1u64.into());
        assert!(!p.is_hidden(1u64.into()) && p.wins.is_empty());
    }

    #[test]
    fn the_saved_layout_remembers_each_windows_mac() {
        let w = vec![
            Saved { mac: Some(SavedMac { peer: MINI.into(), name: "Morgan's Mac mini".into() }), bounds: [10., 20., 900., 600.], terminals: vec!["b1".into()], selected: Some("b1".into()), ..Default::default() },
            Saved { bounds: [0., 0., 1280., 800.], terminals: vec!["a1".into()], ..Default::default() },
        ];
        let v = serde_json::to_value(&w).unwrap();
        assert_eq!(v[0]["mac"], serde_json::json!({ "peer": MINI, "name": "Morgan's Mac mini" }));
        assert!(v[1].get("mac").is_none(), "this Mac's windows save as before");
        assert_eq!(serde_json::from_value::<Vec<Saved>>(v).unwrap(), w);
        // A layout saved before windows had a Mac is this Mac's.
        let old: Vec<Saved> = serde_json::from_value(serde_json::json!([{ "bounds": [0., 0., 800., 600.], "terminals": ["a1"] }])).unwrap();
        assert_eq!(old[0].mac, None);
    }

    #[test]
    fn the_window_menu_lists_my_macs_not_contacts() {
        let b = crate::backend::fake::FakeBackend::new();
        let list = serde_json::from_value(b.call("peers.list", serde_json::json!({})).unwrap()).unwrap();
        let names: Vec<String> = my_macs(&list).into_iter().map(|m| m.name).collect();
        assert_eq!(names, ["Morgan's Mac mini", "Office iMac", "Studio"]);
    }

    #[test]
    fn another_macs_window_is_named_for_it() {
        let m = Mac { peer: MINI.into(), name: "Morgan's Mac mini".into() };
        let name = shown_name(Some(&m), true, Some("Work MacBook Pro"));
        assert_eq!(title(name.as_deref()), "Morgan's Mac mini — Midna");
        assert_eq!(title(None), "midna");
    }

    #[test]
    fn this_mac_names_itself_only_beside_another_macs_window() {
        let m = Mac { peer: MINI.into(), name: "Morgan's Mac mini".into() };
        assert_eq!(shown_name(None, false, Some("Work MacBook Pro")), None, "no other Mac's window: as before");
        assert_eq!(shown_name(None, true, Some("Work MacBook Pro")).as_deref(), Some("Work MacBook Pro"));
        assert_eq!(shown_name(None, true, None), None, "this Mac's name isn't known yet");
        assert_eq!(shown_name(Some(&m), false, None).as_deref(), Some("Morgan's Mac mini"), "another Mac's window always says whose it is");
        let mut p = four();
        assert!(p.any_remote());
        p.closed(3u64.into());
        p.closed(4u64.into());
        assert!(!p.any_remote(), "the Mac mini's last window closed");
    }
}
