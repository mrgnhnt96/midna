//! The floating badge (design D3, "springs out beside", on the floating-notifications canvas):
//! a 44px square in a corner of the screen that counts what needs you, its border split by the
//! colors of what's waiting. When a notification comes in, a one-line capsule springs out
//! beside it, the badge pulses in the kind's color and the number slides (up: the new one rises
//! from below; down: it drops in from above). Kinds that stay (`notify.stay.<kind>` 0, and
//! every needs-you item) are counted until they're handled; the others pass, their capsule's
//! tinted fill clearing from left to right over the kind's duration. A kind with an image
//! (`notify.image[.<kind>]`): the badge wears it in its diamond's place while the capsule shows,
//! and the list's rows and cards show it as a thumbnail.
//!
//! Clicking the badge opens the list: what's waiting as capsules, newest first, scrolling (the
//! row at the middle is the clearest; the others fade with their distance from it). With the
//! pointer on a capsule, an × grows in at its start (off the list) and an arrow at its end (to
//! the terminal), and they linger a beat after it leaves. Clicking a capsule grows it into its
//! card (Approve / Deny, or Dismiss, and Terminal), one at a time, and glides the list to put
//! it in the middle; the card's top row collapses it. Clear takes everything off. Hovering a
//! notification's capsule beside the badge holds it and shows Approve / Deny. Right-click: Hide badge,
//! Notification settings…, Open midna. Drag it anywhere; let go and it slings to the nearest
//! corner of that screen on a spring, carrying your throw (`notify.badge.corner`, `inset_x` /
//! `inset_y` from its edges); `notify.badge.snap` `free`: it stays where it lands.
//! `notify.badge`: `background` (hidden only while another midna build is the app in front),
//! `always`, or `off` (in-app cards show instead, `ui/toast.rs`).
//!
//! Within midna (this build in front), the badge springs from its screen corner onto the front
//! window's top right and follows it there (laid out as top right, whatever its corner; it can't
//! be dragged), a band of the need color sweeping across it when something comes in and every
//! `REMIND` while anything waits. Leaving midna, it springs back to its corner. ⌘J goes to the
//! newest capsule's terminal while one shows (`newest`, `go_newest`). While the screen is
//! shared (`notify.badge.sharing`), it hides, or shows only its number, or carries on.
//! A `notify.send` notification (`Sent`) replaces the one with its id, may open a URL instead of
//! a terminal and have buttons on its card, and reports what you did with it (`notify.respond`).
//!
//! The window is a borderless non-activating panel (GPUI `PopUp`: every Space, over full-screen
//! apps) that never takes focus, so clicking it leaves the app you're in in front. It's a fixed
//! transparent rectangle centered on the badge; a timer lets clicks through everywhere but the
//! badge, the capsule and the stack (`zones`), shows and hides it, drags it by hand and slings
//! it. The home main window feeds it (`sync`, `push`); while it takes notifications, they
//! aren't macOS banners.
use crate::backend::Backend;
use crate::icons::Icon;
use crate::model::*;
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use midna_proto::notify::Posted;
use serde_json::{Value, json};
use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Room for the stack below (or above) the badge and a capsule beside it, from the screen's edge.
const W: f32 = 440.;
const H: f32 = 600.;
/// The badge's inset from the screen's edges.
const PAD: f32 = 14.;
const SIZE: f32 = 44.;
/// The window is centered on the badge, with that room on every side, so the badge sits at the
/// same place in it whatever the corner (a corner change never redraws it somewhere else). The
/// half that's past the screen's edge is empty and lets clicks through.
const MX: f32 = W - PAD - SIZE;
const MY: f32 = H - PAD - SIZE;
const WIN_W: f32 = MX * 2. + SIZE;
const WIN_H: f32 = MY * 2. + SIZE;
/// The list (clicking the badge): its scrolling part, the room above its first row, a capsule,
/// the gap between rows and an open card's width.
const LIST_W: f32 = 400.;
/// Room past the rows on the list's open side, so a card's shadow isn't cut off.
const LIST_BLEED: f32 = 40.;
/// The list closes itself after this long with nothing happening (the pointer resting on it
/// counts).
const LIST_IDLE: Duration = Duration::from_secs(5);
/// Resting the pointer on the badge this long opens the list; a click up to `HOVER_GRACE` after
/// it opened that way leaves it open (it was meant to open it), the next one closes it.
const HOVER_OPEN: Duration = Duration::from_millis(600);
const HOVER_GRACE: Duration = Duration::from_millis(800);
const LIST_H: f32 = 440.;
const LIST_TOP: f32 = 10.;
const ROW: f32 = 36.;
/// How strong a scrolling list's farthest rows show: their fill and their words.
const ROW_FILL: f32 = 0.88;
const ROW_WORDS: f32 = 0.4;
const GAP: f32 = 8.;
const CARD_W: f32 = 380.;
/// A capsule's × and arrow: they grow in, and stay this long after the pointer leaves (a quick
/// move off and back doesn't make the capsule jump).
const HOT_LINGER: Duration = Duration::from_millis(350);
const GROW: Duration = Duration::from_millis(240);
const SHRINK: Duration = Duration::from_millis(180);
/// A capsule growing into its card (its contents fade in after a beat), and back.
const CARD_IN: Duration = Duration::from_millis(380);
const CARD_FADE_AFTER: Duration = Duration::from_millis(120);
const CARD_FADE: Duration = Duration::from_millis(220);
const CARD_OUT: Duration = Duration::from_millis(200);
/// Opening the list: its rows drop out of the badge one after another, a little bounce each;
/// closing lifts them back up, the bottom one first (rows past the twelfth go with the twelfth).
const CASCADE_IN: Duration = Duration::from_millis(320);
const CASCADE_IN_STEP: Duration = Duration::from_millis(45);
const CASCADE_OUT: Duration = Duration::from_millis(180);
const CASCADE_OUT_STEP: Duration = Duration::from_millis(30);
const CASCADE_ROWS: usize = 12;
pub(crate) const CASCADE_DROP: f32 = 18.;

/// How far the `k`th of `n` rows is through the cascade (the list's, a sidebar project's):
/// opening, 0 not out yet to 1 in place (a little past it as it bounces), each row a beat after
/// the one above; closing, 1 to 0, the bottom one first. Rows past the twelfth move with it.
pub(crate) fn cascade_at(k: usize, n: usize, elapsed: Duration, closing: bool) -> f32 {
    let n = n.clamp(1, CASCADE_ROWS);
    let k = k.min(n - 1);
    if closing {
        let x = elapsed.saturating_sub(CASCADE_OUT_STEP * (n - 1 - k) as u32).as_secs_f32() / CASCADE_OUT.as_secs_f32();
        1. - ease_in(x.min(1.))
    } else {
        let x = elapsed.saturating_sub(CASCADE_IN_STEP * k as u32).as_secs_f32() / CASCADE_IN.as_secs_f32();
        back_out(x.min(1.))
    }
}

/// How long the cascade of `n` rows takes.
pub(crate) fn cascade_len(n: usize, closing: bool) -> Duration {
    let steps = n.clamp(1, CASCADE_ROWS) as u32 - 1;
    if closing { CASCADE_OUT_STEP * steps + CASCADE_OUT } else { CASCADE_IN_STEP * steps + CASCADE_IN }
}
/// Opening a card glides the list so the card sits in the middle.
const GLIDE: Duration = Duration::from_millis(500);
/// How long a capsule of a kind that stays shows (the item stays counted after).
const LINE_SHOW: Duration = Duration::from_secs(6);
/// After the pointer leaves a capsule, it stays this much longer.
const LINE_LINGER: Duration = Duration::from_secs(2);
const LINE_IN: Duration = Duration::from_millis(420);
const LINE_OUT: Duration = Duration::from_millis(220);
const PULSE: Duration = Duration::from_millis(650);
const ROLL: Duration = Duration::from_millis(420);
/// How long a needs-you item counted from its notification outlives a list without it.
const PENDING: Duration = Duration::from_secs(3);
/// Within midna: the badge's distance from the front window's right and top edges.
const DOCK: (f64, f64) = (12., 56.);
/// Within midna: a band of the need color sweeps across the badge when something comes in
/// (`SHIMMER_ALPHA`) and every `REMIND` while anything waits (`REMIND_ALPHA`), as the sidebar's
/// needs-you rows do (`need_anim`).
const SHIMMER: Duration = Duration::from_millis(900);
const SHIMMER_ALPHA: f32 = 0.28;
const REMIND_ALPHA: f32 = 0.16;
const REMIND: Duration = Duration::from_secs(60);
/// The badge wears a capsule's image: it grows from the diamond's size over `WEAR` and shrinks
/// back as the capsule leaves (`LINE_OUT`).
const WEAR: Duration = Duration::from_millis(420);
const WEAR_FROM: f32 = 12.;
/// A press that moves the window less than this is a click, not a drag.
const DRAG_SLOP: f64 = 3.;
/// How often to ask macOS whether the screen is being shared.
const WATCH_EVERY: Duration = Duration::from_millis(500);
/// The spring a dropped badge slings to its corner on: stiffness and damping (a little
/// overshoot, settled in about half a second).
const SPRING_K: f64 = 260.;
const SPRING_C: f64 = 24.;
/// The fastest throw it carries into the spring (points a second).
const THROW_MAX: f64 = 5000.;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Corner {
    TopRight,
    TopLeft,
    BottomRight,
    BottomLeft,
}

impl Corner {
    fn parse(v: &str) -> Corner {
        match v {
            "top_left" => Corner::TopLeft,
            "bottom_right" => Corner::BottomRight,
            "bottom_left" => Corner::BottomLeft,
            _ => Corner::TopRight,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Corner::TopRight => "top_right",
            Corner::TopLeft => "top_left",
            Corner::BottomRight => "bottom_right",
            Corner::BottomLeft => "bottom_left",
        }
    }

    fn right(self) -> bool {
        matches!(self, Corner::TopRight | Corner::BottomRight)
    }

    fn top(self) -> bool {
        matches!(self, Corner::TopRight | Corner::TopLeft)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Background,
    Always,
    Off,
}

/// `notify.badge.sharing`: what the badge does while the screen is shared.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sharing {
    Hide,
    Count,
    Show,
}

/// Something is watching the screen: a share (Zoom, Meet, Teams, …), a recording or Screen
/// Sharing. macOS has no public call for it; this is the WindowServer's own (private, looked up
/// at run time, so a macOS without it just never hides). `MIDNA_DEBUG_SHARING=1` fakes one.
fn screen_watched() -> bool {
    static WATCHER: std::sync::OnceLock<Option<unsafe extern "C" fn() -> bool>> = std::sync::OnceLock::new();
    if crate::dev::var("MIDNA_DEBUG_SHARING").is_ok_and(|v| v == "1") {
        return true;
    }
    let f = WATCHER.get_or_init(|| {
        let sym = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"CGSIsScreenWatcherPresent".as_ptr()) };
        (!sym.is_null()).then(|| unsafe { std::mem::transmute::<*mut libc::c_void, unsafe extern "C" fn() -> bool>(sym) })
    });
    f.is_some_and(|f| unsafe { f() })
}

/// A midna is the app in front: this one, or another build of it (Midna and Midna Dev both
/// running; either one's badge stays out of the way while you're in a midna).
/// This one counts only for one of its own windows: a press on the badge can make midna the
/// active app without it being in front of you.
fn midna_in_front(badge: &objc2_app_kit::NSWindow) -> bool {
    if crate::sounds::app_active() {
        let Some(mtm) = objc2::MainThreadMarker::new() else { return false };
        let key = objc2_app_kit::NSApplication::sharedApplication(mtm).keyWindow();
        return key.is_some_and(|k| !std::ptr::eq(&*k, badge));
    }
    let Some(app) = objc2_app_kit::NSWorkspace::sharedWorkspace().frontmostApplication() else { return false };
    app.bundleIdentifier().is_some_and(|id| id.to_string().starts_with("com.mrgnhnt.midna"))
}

/// This midna's own window is in front (`Some(true)`), or another app is (`Some(false)`);
/// `None` when the badge itself is midna's key window (a click on it), which says neither.
fn here(badge: &objc2_app_kit::NSWindow) -> Option<bool> {
    if !crate::sounds::app_active() {
        return Some(false);
    }
    let mtm = objc2::MainThreadMarker::new()?;
    match objc2_app_kit::NSApplication::sharedApplication(mtm).keyWindow() {
        Some(k) if std::ptr::eq(&*k, badge) => None,
        Some(_) => Some(true),
        None => Some(false),
    }
}

/// Where the window goes for the badge docked in midna's front window (`DOCK` from its top
/// right): its Cocoa origin.
fn dock_origin(badge: &objc2_app_kit::NSWindow) -> Option<(f64, f64)> {
    let mtm = objc2::MainThreadMarker::new()?;
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    let w = app.keyWindow().filter(|k| !std::ptr::eq(&**k, badge)).or_else(|| app.mainWindow().filter(|k| !std::ptr::eq(&**k, badge)))?;
    let f = w.frame();
    let size = SIZE as f64;
    let (bx, by) = (f.origin.x + f.size.width - DOCK.0 - size, f.origin.y + f.size.height - DOCK.1 - size);
    Some((bx - MX as f64, by - MY as f64))
}

/// Whether a click on the badge may make midna the active app. A style mask set after the
/// window's made doesn't reach the WindowServer's "never activate" flag; this (private, so only
/// when it's there) does. While it's on, nothing can bring midna forward (itself, an app
/// switcher, the Dock), so it's on only while the pointer is on the badge (`Badge::tick`), and
/// opening a terminal lifts it for a moment (`Badge::bring_forward`).
fn prevent_activation(ns: &objc2_app_kit::NSWindow, on: bool) {
    use objc2::{msg_send, sel};
    unsafe {
        let responds: bool = msg_send![ns, respondsToSelector: sel!(_setPreventsActivation:)];
        if responds {
            let _: () = msg_send![ns, _setPreventsActivation: on];
        }
    }
}

/// A `notify.send` notification's id, URL and buttons.
#[derive(Clone)]
struct Sent {
    id: String,
    open: Option<String>,
    actions: Vec<String>,
}

impl Sent {
    fn of(p: &Posted) -> Option<Sent> {
        Some(Sent { id: p.id.clone()?, open: p.open.clone(), actions: p.actions.clone() })
    }
}

/// Something waiting on you: a needs-you item, or a notification of a kind that stays.
#[derive(Clone)]
struct Waiting {
    /// The needs-you id, or `n<seq>` for a notification.
    id: String,
    sent: Option<Sent>,
    need: Option<String>,
    session: Option<String>,
    category: String,
    /// The terminal (or project) it's about.
    name: String,
    label: String,
    color: String,
    title: String,
    detail: Option<String>,
    command: Option<String>,
    approval: bool,
    /// A question's options (label, description), numbered as in the terminal, and whether
    /// any number of them can be picked.
    options: Vec<(String, String)>,
    multi: bool,
    /// Its kind's image (`notify.image[.<kind>]`): the list row's thumbnail and the card's.
    image: Option<PathBuf>,
    /// When it came in (RFC 3339): the list shows the newest first.
    at: String,
}

/// A capsule beside the badge.
struct Line {
    serial: u64,
    /// Lines it replaced while they still showed (its "+N").
    burst: usize,
    /// The `Waiting` it shows, when it's one (it stays counted after the capsule goes).
    waiting: Option<String>,
    session: Option<String>,
    need: Option<String>,
    sent: Option<Sent>,
    category: String,
    name: String,
    text: String,
    color: String,
    /// Its kind's image: the badge wears it while the capsule shows.
    image: Option<PathBuf>,
    /// Passing kinds: how long it shows, its fill clearing over it. None: a kind that stays.
    stay: Option<Duration>,
    until: Instant,
    leaving: Option<Instant>,
}

/// The layout boxes that take clicks (window coordinates), measured as they paint.
#[derive(Clone, Default)]
struct Zones {
    badge: Rc<Cell<Option<Bounds<Pixels>>>>,
    line: Rc<Cell<Option<Bounds<Pixels>>>>,
    panel: Rc<Cell<Option<Bounds<Pixels>>>>,
}

impl Zones {
    fn clear(&self) {
        self.badge.set(None);
        self.line.set(None);
        self.panel.set(None);
    }

    fn hit(&self, p: Point<Pixels>) -> Option<&'static str> {
        [("badge", &self.badge), ("line", &self.line), ("panel", &self.panel)].into_iter().find(|(_, z)| z.get().is_some_and(|b| b.contains(&p))).map(|(n, _)| n)
    }
}

/// A press on the badge (Cocoa screen coordinates).
#[derive(Clone, Copy)]
struct Press {
    /// The pointer and the window's origin when it went down.
    mouse: (f64, f64),
    origin: (f64, f64),
    /// It moved past `DRAG_SLOP`: a drag, not a click.
    moved: bool,
    /// The pointer's last place and time, and its velocity (smoothed), for the throw.
    last: ((f64, f64), Instant),
    vel: (f64, f64),
}

/// The window on a spring to `to` (its origin).
#[derive(Clone, Copy)]
struct Slide {
    pos: (f64, f64),
    vel: (f64, f64),
    to: (f64, f64),
    at: Instant,
}

pub struct Badge {
    backend: Arc<dyn Backend>,
    mode: Mode,
    corner: Corner,
    /// `notify.badge.snap` is `free`: a drop stays where it lands.
    free: bool,
    /// `notify.badge.inset_x` / `inset_y`: the badge's distance from its corner's edges.
    inset: (f64, f64),
    sharing: Sharing,
    /// The screen is being shared (`screen_watched`), as of `watched_at`.
    shared: bool,
    watched_at: Option<Instant>,
    /// `notify.color.<kind>` for every kind (a needs-you item's color comes from its kind's).
    colors: HashMap<String, String>,
    needs: Vec<Waiting>,
    /// Notifications of kinds that stay, not tied to a needs-you item, until dismissed.
    held: Vec<Waiting>,
    /// Needs-you items counted from their notification, before a refresh had them.
    pending: Vec<(String, Instant)>,
    lines: VecDeque<Line>,
    serial: u64,
    /// The count shown, the one it rolled from and the roll's id.
    count: usize,
    from: usize,
    roll: u64,
    /// When the count and the pulse last changed: only a fresh one animates (the badge showing
    /// again, say coming back from another midna build, doesn't replay them).
    roll_at: Instant,
    pulse_at: Instant,
    /// The last pulse: its id and color.
    pulse: (u64, String),
    /// The list is open (clicking the badge), since when, and closing since when (it's shown
    /// until its rows have lifted away).
    list: bool,
    list_since: Instant,
    list_closing: Option<Instant>,
    /// The last thing you did with the list (pointer on it or the badge, a scroll, a click).
    list_touched: Instant,
    /// Needs-you items you cleared from the list (× or Clear): they stay in midna, not here.
    cleared: Vec<String>,
    /// The open card and since when; the ones collapsing back into their capsules (since when,
    /// and the height each had: each finishes, however quickly you open the next).
    expanded: Option<(String, Instant)>,
    collapsing: Vec<(String, Instant, f32)>,
    /// How far the list is scrolled (px), and a glide in progress (from, to, since).
    scroll: f32,
    glide: Option<(f32, f32, Instant)>,
    /// The capsule showing its × and arrow (since when), when the pointer left it, and the one
    /// whose × and arrow are shrinking away.
    hot: Option<(String, Instant)>,
    hot_left: Option<Instant>,
    cool: Option<(String, Instant)>,
    /// The badge pill's width, measured as it paints (it grows with the count; the capsule
    /// beside it springs out past it).
    badge_w: Rc<Cell<f32>>,
    /// The open card's contents, measured as they paint (its height to grow to).
    card_box: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// Answers midnad turned down, by needs-you id: shown on the item's card (it comes back).
    failed: HashMap<String, String>,
    /// Open the stack at this item once it's back (its answer failed).
    reopen: Option<String>,
    menu: bool,
    /// The pointer is over a zone.
    hover: Option<&'static str>,
    /// Since when the pointer's been on the badge, while resting there may still open the list
    /// (a click or opening it ends that until it comes back).
    hover_since: Option<Instant>,
    /// The list opened by resting on the badge, and when.
    hover_opened: Option<Instant>,
    /// A press on the badge: the badge follows the pointer (it's dragged by hand, not by
    /// AppKit's window drag, which can bring midna to the front).
    pressing: Option<Press>,
    /// The window slinging to its corner.
    slide: Option<Slide>,
    /// Docked in midna's front window (this build is in front).
    within: bool,
    /// `needs_you.replace`: a terminal's new blocked or note item replaces its older ones.
    replace: bool,
    /// The last sweep across the badge: its id, strength and when.
    shimmer: Option<(u64, f32, Instant)>,
    shimmers: u64,
    shown: bool,
    zones: Zones,
    /// The window, for `bring_forward` (it's made with the badge, on the main thread).
    ns: Option<objc2::rc::Retained<objc2_app_kit::NSWindow>>,
    /// When to stop letting the badge activate midna again, after `bring_forward`.
    reprevent: Option<Instant>,
    /// A click on the badge can't activate midna (`prevent_activation`): the pointer's on it.
    preventing: bool,
    /// `MIDNA_DEBUG_BADGE`: open the list (`list`), the list with its first card (`card`) or
    /// the menu (`menu`) once there's something.
    debug: Option<String>,
    _ticker: Task<()>,
}

struct BadgeWindow(Option<WindowHandle<Badge>>);
impl Global for BadgeWindow {}

fn handle(cx: &App) -> Option<WindowHandle<Badge>> {
    cx.try_global::<BadgeWindow>().and_then(|b| b.0)
}

/// Whether `window` is the badge (it doesn't keep midna running on its own).
pub fn is_badge(window: AnyWindowHandle, cx: &App) -> bool {
    handle(cx).is_some_and(|h| h.window_id() == window.window_id())
}

/// Open the badge's window (hidden until there's something to show).
pub fn init(backend: Arc<dyn Backend>, cx: &mut App) {
    if handle(cx).is_some() {
        return;
    }
    let display = cx.primary_display();
    let opts = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(point(px(0.), px(0.)), size(px(WIN_W), px(WIN_H))))),
        titlebar: None,
        kind: WindowKind::PopUp,
        focus: false,
        show: false,
        is_movable: true,
        display_id: display.map(|d| d.id()),
        window_background: WindowBackgroundAppearance::Transparent,
        // It never takes focus; unfocused windows are otherwise drawn at a reduced rate.
        inactive_frame_interval: None,
        ..Default::default()
    };
    let h = cx.open_window(opts, |window, cx| {
        if let Some(ns) = super::twilight::ns_window(window) {
            // GPUI makes it a titled panel; macOS draws a titled window's rim (a light line
            // along its top) even when it's transparent, and treats its top as a titlebar.
            ns.setStyleMask(objc2_app_kit::NSWindowStyleMask::Borderless | objc2_app_kit::NSWindowStyleMask::NonactivatingPanel);
            ns.setHasShadow(false);
            ns.setIgnoresMouseEvents(true);
            place(&ns, Corner::TopRight);
        }
        cx.new(|cx| Badge::new(backend, window, cx))
    });
    cx.set_global(BadgeWindow(h.ok()));
}

/// The badge takes notifications: it's on (in midna too, docked in its window).
pub fn takes(cx: &App) -> bool {
    let Some(h) = handle(cx) else { return false };
    h.read(cx).is_ok_and(|b| b.mode != Mode::Off)
}

/// You opened terminal `session` (`notify.badge.clear_on_open`): its held notifications and its
/// capsule go (needs-you items stay until they're handled).
pub fn seen(session: &str, cx: &mut App) {
    let Some(h) = handle(cx) else { return };
    let _ = h.update(cx, |b, _, cx| {
        b.held.retain(|w| w.session.as_deref() != Some(session));
        for l in b.lines.iter_mut().filter(|l| l.session.as_deref() == Some(session) && l.need.is_none()) {
            l.leaving.get_or_insert(Instant::now());
        }
        b.recount(cx);
    });
}

/// Docked in midna's window with a capsule showing beside it: ⌘J goes to it (`go_newest`).
pub fn newest(cx: &App) -> bool {
    let Some(h) = handle(cx) else { return false };
    h.read(cx).is_ok_and(|b| b.shown && b.within && b.lines.front().is_some_and(|l| l.leaving.is_none() && (l.session.is_some() || l.need.is_some())))
}

/// Go to the newest capsule's terminal (or its needs-you card), as clicking it does.
pub fn go_newest(cx: &mut App) {
    let Some(h) = handle(cx) else { return };
    let _ = h.update(cx, |b, _, cx| {
        let Some(l) = b.lines.front().filter(|l| l.leaving.is_none()) else { return };
        let held = l.need.is_none().then(|| l.waiting.clone()).flatten();
        let (session, need, sent) = (l.session.clone(), l.need.clone(), l.sent.clone());
        b.open(session, need, held, sent, cx);
    });
}

/// A notification the home window got while the badge takes them (`app.rs` `on_notification`).
pub fn push(m: &crate::app::MainWindow, seq: u64, session: Option<String>, p: Posted, cx: &mut App) {
    let Some(h) = handle(cx) else { return };
    let name = super::notifications::source(m, session.as_deref(), None);
    let need = p.needs_you_id.clone().and_then(|id| m.needs.iter().find(|n| n.id == id).cloned());
    let _ = h.update(cx, |b, _, cx| b.add(seq, session, p, name, need, cx));
}

/// `notify.withdraw`: take notification `id` off the badge.
pub fn withdraw(id: &str, cx: &mut App) {
    let Some(h) = handle(cx) else { return };
    let _ = h.update(cx, |b, _, cx| b.withdraw(id, cx));
}

/// The home window's needs-you items and settings changed.
pub fn sync(m: &crate::app::MainWindow, cx: &mut App) {
    let Some(h) = handle(cx) else { return };
    let setting = |k: &str| m.settings.get(k).and_then(Value::as_str).map(str::to_string);
    let mode = match setting("notify.badge").as_deref() {
        Some("off") => Mode::Off,
        Some("always") => Mode::Always,
        _ => Mode::Background,
    };
    let corner = Corner::parse(&setting("notify.badge.corner").unwrap_or_default());
    let free = setting("notify.badge.snap").as_deref() == Some("free");
    let inset_of = |k: &str| m.settings.get(k).and_then(Value::as_f64).unwrap_or(PAD as f64).max(0.);
    let inset = (inset_of("notify.badge.inset_x"), inset_of("notify.badge.inset_y"));
    let sharing = match setting("notify.badge.sharing").as_deref() {
        Some("count") => Sharing::Count,
        Some("show") => Sharing::Show,
        _ => Sharing::Hide,
    };
    let flag = |k: &str| m.settings.get(k).and_then(Value::as_bool).unwrap_or(true);
    let replace = flag("needs_you.replace");
    // `needs_you.clear_failed_on_run`: a terminal working again since a failure clears it.
    let working: HashMap<String, String> = if flag("needs_you.clear_failed_on_run") {
        m.sessions.iter().filter(|s| s.status.state == StatusState::Working).filter_map(|s| Some((s.id.clone(), s.status.since.clone()?))).collect()
    } else {
        HashMap::new()
    };
    let colors: HashMap<String, String> =
        m.settings.iter().filter_map(|(k, v)| Some((k.strip_prefix("notify.color.")?.to_string(), v.as_str()?.to_string()))).collect();
    let home = midna_proto::paths::midna_home();
    let image = |kind: &str| midna_proto::notify::kind_image(&home, kind, |k| setting(k).unwrap_or_default());
    let needs: Vec<Waiting> = m
        .needs
        .iter()
        .map(|n| {
            let category = category_of(n.kind).to_string();
            let image = image(&category);
            let command = n.approval.as_ref().map(|a| a.action.value.clone()).filter(|v| !v.is_empty());
            let title = match n.question.as_ref().filter(|q| !q.text.trim().is_empty()) {
                Some(q) if !q.header.trim().is_empty() => q.header.trim().to_string(),
                Some(_) => "Asked a question".to_string(),
                None => capitalize(n.title.trim()),
            };
            let detail = n.question.as_ref().map(|q| q.text.trim().to_string()).or_else(|| Some(n.detail.trim().to_string())).filter(|d| !d.is_empty() && Some(d) != command.as_ref());
            Waiting {
                id: n.id.clone(),
                sent: None,
                need: Some(n.id.clone()),
                session: n.session_id.clone(),
                name: super::notifications::source(m, n.session_id.as_deref(), n.project_id.as_deref()),
                label: midna_proto::notify::category(&category).map(|c| c.label.to_string()).unwrap_or_default(),
                color: colors.get(&category).cloned().unwrap_or_else(|| default_color(&category).into()),
                category,
                title,
                detail,
                command,
                approval: n.is_approval(),
                options: options_of(n),
                multi: n.question.as_ref().is_some_and(|q| q.multi_select),
                image,
                at: n.created_at.clone(),
            }
        })
        .collect();
    let _ = h.update(cx, |b, window, cx| {
        // A drop saves these too; it's already there (or on its way) then.
        let moved = (b.corner != corner || b.inset != inset) && b.pressing.is_none() && b.slide.is_none();
        b.free = free;
        b.inset = inset;
        b.replace = replace;
        b.held.retain(|w| !(w.category == "failed" && w.session.as_ref().and_then(|s| working.get(s)).is_some_and(|since| *since > w.at)));
        b.mode = mode;
        b.sharing = sharing;
        b.colors = colors;
        // A list fetched just before an item came in doesn't drop it (it'd count down, then up).
        let fresh: Vec<Waiting> = b.needs.iter().filter(|w| b.pending.iter().any(|(id, at)| *id == w.id && at.elapsed() < PENDING) && !needs.iter().any(|n| n.id == w.id)).cloned().collect();
        b.pending.retain(|(_, at)| at.elapsed() < PENDING);
        b.needs = needs;
        b.needs.extend(fresh);
        b.recount(cx);
        if moved {
            b.snap(corner, (0., 0.), window, cx);
        }
        cx.notify();
    });
}

/// A question's options (label, description).
fn options_of(n: &NeedsYou) -> Vec<(String, String)> {
    n.question.as_ref().map(|q| q.options.iter().map(|o| (o.label.clone(), o.description.trim().to_string())).collect()).unwrap_or_default()
}

/// A needs-you item's notification kind (as midnad picks it).
fn category_of(k: NeedsYouKind) -> &'static str {
    match k {
        NeedsYouKind::Approval | NeedsYouKind::PermissionPrompt => "approval",
        NeedsYouKind::Blocked | NeedsYouKind::Note => "attention",
        NeedsYouKind::Failed => "failed",
        _ => "requests",
    }
}

/// A kind's color before the first settings refresh (the catalog's defaults).
fn default_color(category: &str) -> &'static str {
    match category {
        "approval" | "attention" => "need",
        "failed" => "err",
        "turn_done" | "background" | "pr_checks" => "ok",
        "from_trigger" | "triggers" | "restarted" => "work",
        "exited" => "dim",
        _ => "accent",
    }
}

fn icon_of(category: &str) -> Icon {
    match category {
        "approval" => Icon::Shell,
        "attention" => Icon::Bell,
        "failed" => Icon::Cross,
        "requests" => Icon::Rules,
        "turn_done" | "background" => Icon::Check,
        "pr_checks" => Icon::Pr,
        "exited" => Icon::Screen,
        "agent" => Icon::Orbit,
        "from_trigger" | "triggers" => Icon::Triggers,
        "restarted" => Icon::Restart,
        _ => Icon::Bell,
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// Ease out with a little overshoot (`cubic-bezier(.34, 1.56, .64, 1)`-like).
/// A notification's image, cropped to a rounded square.
pub fn thumb(path: PathBuf, size: f32, radius: f32) -> Img {
    img(path).flex_none().size(px(size)).rounded(px(radius)).object_fit(ObjectFit::Cover)
}

fn back_out(x: f32) -> f32 {
    let (c1, c3) = (1.70158, 2.70158);
    1. + c3 * (x - 1.).powi(3) + c1 * (x - 1.).powi(2)
}

fn ease_in(x: f32) -> f32 {
    x * x * x
}

// ------------------------------------------------------------------ the native window

/// The badge's center on screen (Cocoa, y up), where the window is now.
fn badge_center(ns: &objc2_app_kit::NSWindow) -> (f64, f64) {
    let f = ns.frame();
    (f.origin.x + (MX + SIZE / 2.) as f64, f.origin.y + (MY + SIZE / 2.) as f64)
}

/// The screen the badge is on (the window's mostly off it at a corner), else the main one.
fn screen_of(ns: &objc2_app_kit::NSWindow) -> Option<objc2::rc::Retained<objc2_app_kit::NSScreen>> {
    let mtm = objc2::MainThreadMarker::new()?;
    let (x, y) = badge_center(ns);
    let screens = objc2_app_kit::NSScreen::screens(mtm);
    let on = screens.iter().find(|s| {
        let f = s.frame();
        x >= f.origin.x && x < f.origin.x + f.size.width && y >= f.origin.y && y < f.origin.y + f.size.height
    });
    on.or_else(|| screens.firstObject())
}

/// Where the window goes for the badge `inset` from `corner` of its screen's visible frame
/// (clear of the menu bar and the Dock): its Cocoa origin. An inset too big for the screen
/// keeps the badge on it.
fn origin_at(ns: &objc2_app_kit::NSWindow, corner: Corner, inset: (f64, f64)) -> Option<(f64, f64)> {
    let vf = screen_of(ns)?.visibleFrame();
    let size = SIZE as f64;
    let ix = inset.0.clamp(0., (vf.size.width - size).max(0.));
    let iy = inset.1.clamp(0., (vf.size.height - size).max(0.));
    let bx = if corner.right() { vf.origin.x + vf.size.width - ix - size } else { vf.origin.x + ix };
    let by = if corner.top() { vf.origin.y + vf.size.height - iy - size } else { vf.origin.y + iy };
    Some((bx - MX as f64, by - MY as f64))
}

/// The badge's distance from `corner`'s edges of its screen's visible frame, where it is now
/// (a free drop), kept on the screen.
fn inset_from(ns: &objc2_app_kit::NSWindow, corner: Corner) -> Option<(f64, f64)> {
    let vf = screen_of(ns)?.visibleFrame();
    let size = SIZE as f64;
    let (cx, cy) = badge_center(ns);
    let (bx, by) = (cx - size / 2., cy - size / 2.);
    let ix = if corner.right() { vf.origin.x + vf.size.width - size - bx } else { bx - vf.origin.x };
    let iy = if corner.top() { vf.origin.y + vf.size.height - size - by } else { by - vf.origin.y };
    Some((ix.clamp(0., (vf.size.width - size).max(0.)).round(), iy.clamp(0., (vf.size.height - size).max(0.)).round()))
}

fn place(ns: &objc2_app_kit::NSWindow, corner: Corner) {
    if let Some((x, y)) = origin_at(ns, corner, (PAD as f64, PAD as f64)) {
        ns.setFrameOrigin(objc2_foundation::NSPoint::new(x, y));
    }
}

/// The corner of its screen the badge is nearest, where the window is now (plus `ahead`: where
/// a throw carries it).
fn nearest(ns: &objc2_app_kit::NSWindow, corner: Corner, ahead: (f64, f64)) -> Corner {
    let Some(screen) = screen_of(ns) else { return corner };
    let vf = screen.visibleFrame();
    let (bx, by) = badge_center(ns);
    let (bx, by) = (bx + ahead.0, by + ahead.1);
    let right = bx > vf.origin.x + vf.size.width / 2.;
    let top = by > vf.origin.y + vf.size.height / 2.;
    match (top, right) {
        (true, true) => Corner::TopRight,
        (true, false) => Corner::TopLeft,
        (false, true) => Corner::BottomRight,
        (false, false) => Corner::BottomLeft,
    }
}

impl Badge {
    fn new(backend: Arc<dyn Backend>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let ticker = cx.spawn_in(window, async move |this, cx| {
            loop {
                // Docked, it follows its window closely as that moves.
                let every = this
                    .update(cx, |b, _| match () {
                        _ if b.slide.is_some() || b.pressing.is_some() => 8,
                        _ if b.shown && b.within => 16,
                        _ if b.shown => 33,
                        _ => 250,
                    })
                    .unwrap_or(250);
                cx.background_executor().timer(Duration::from_millis(every)).await;
                if this.update_in(cx, |b, window, cx| b.tick(window, cx)).is_err() {
                    break;
                }
            }
        });
        cx.observe_global::<Theme>(|_, cx| cx.notify()).detach();
        // Dev (screenshots): `MIDNA_DEBUG_BADGE=list|card|menu` opens the list (with its first
        // card) or the menu.
        let debug = crate::dev::var("MIDNA_DEBUG_BADGE").ok();
        Badge {
            debug,
            backend,
            mode: Mode::Background,
            corner: Corner::TopRight,
            free: false,
            inset: (PAD as f64, PAD as f64),
            sharing: Sharing::Hide,
            shared: false,
            watched_at: None,
            colors: HashMap::new(),
            needs: vec![],
            held: vec![],
            pending: vec![],
            lines: VecDeque::new(),
            serial: 0,
            count: 0,
            from: 0,
            roll: 0,
            roll_at: Instant::now(),
            pulse_at: Instant::now(),
            pulse: (0, String::new()),
            list: false,
            list_since: Instant::now(),
            list_closing: None,
            list_touched: Instant::now(),
            cleared: vec![],
            expanded: None,
            collapsing: vec![],
            scroll: 0.,
            glide: None,
            hot: None,
            hot_left: None,
            cool: None,
            card_box: Rc::new(Cell::new(None)),
            badge_w: Rc::new(Cell::new(SIZE)),
            failed: HashMap::new(),
            reopen: None,
            menu: false,
            hover: None,
            hover_since: None,
            hover_opened: None,
            pressing: None,
            slide: None,
            within: false,
            replace: true,
            shimmer: None,
            shimmers: 0,
            shown: false,
            zones: Zones::default(),
            ns: super::twilight::ns_window(window),
            reprevent: None,
            preventing: false,
            _ticker: ticker,
        }
    }

    /// Where it lays out from: its corner, or top right while docked in midna's window.
    fn side(&self) -> Corner {
        if self.within { Corner::TopRight } else { self.corner }
    }

    /// Sweep a band across the badge (within midna).
    fn shine(&mut self, alpha: f32) {
        self.shimmers += 1;
        self.shimmer = Some((self.shimmers, alpha, Instant::now()));
    }

    /// The screen is shared and the badge mustn't say what came in.
    fn quiet(&self) -> bool {
        self.shared && self.sharing != Sharing::Show
    }

    fn waiting(&self) -> Vec<&Waiting> {
        self.needs.iter().chain(self.held.iter()).collect()
    }

    /// What the list shows (and the badge counts): what's waiting, less what you cleared from
    /// it, newest first.
    fn rows(&self) -> Vec<&Waiting> {
        let mut rows: Vec<&Waiting> = self.waiting().into_iter().filter(|w| !self.cleared.contains(&w.id)).collect();
        rows.sort_by(|a, b| b.at.cmp(&a.at));
        rows
    }

    fn open_list(&mut self) {
        self.list = true;
        self.list_since = Instant::now();
        self.list_touched = self.list_since;
        self.list_closing = None;
        self.scroll = 0.;
        self.glide = None;
        self.expanded = None;
        self.collapsing.clear();
    }

    fn close_list(&mut self) {
        self.list = false;
        self.hover_opened = None;
        self.list_closing = None;
        self.expanded = None;
        self.collapsing.clear();
        self.glide = None;
        self.hot = None;
        self.hot_left = None;
        self.cool = None;
    }

    /// Take something off the list (its ×): a held notification is dismissed; a needs-you item
    /// stays in midna, just not here.
    fn forget(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(sent) = self.held.iter().find(|w| w.id == id).and_then(|w| w.sent.clone()) {
            self.respond(&sent, midna_proto::notify::ResponseKind::Dismissed, None);
        }
        if self.held.iter().any(|w| w.id == id) {
            self.dismiss(id, cx);
        } else {
            self.cleared.push(id.to_string());
            self.recount(cx);
        }
    }

    /// Clear: everything off the list (and so the badge, until something new comes in).
    fn clear_all(&mut self, cx: &mut Context<Self>) {
        self.held.clear();
        self.cleared = self.needs.iter().map(|w| w.id.clone()).collect();
        let now = Instant::now();
        for l in self.lines.iter_mut() {
            l.leaving.get_or_insert(now);
        }
        self.close_list();
        self.recount(cx);
    }

    /// Open a row's card (the open one collapses as it grows; one at a time) and glide the list
    /// so the card sits in the middle, or collapse it if it's the open one.
    fn toggle_card(&mut self, id: &str) {
        let now = Instant::now();
        let height = self.card_height();
        if let Some((e, _)) = self.expanded.take() {
            self.collapsing.retain(|(c, _, _)| *c != e);
            self.collapsing.push((e.clone(), now, height));
            if e == id {
                return;
            }
        }
        // Reopened while it was still collapsing: it grows again.
        self.collapsing.retain(|(c, _, _)| c != id);
        // The new card's height is measured as it paints.
        self.card_box.set(None);
        let rows = self.rows();
        let Some(i) = rows.iter().position(|w| w.id == id) else { return };
        let top = i as f32 * (ROW + GAP);
        let card = self.card_height();
        let span = LIST_TOP + rows.len() as f32 * (ROW + GAP) - GAP + card - ROW;
        // A list that fits (card and all) doesn't move.
        // Otherwise it glides toward the middle, as far as the list goes.
        let to = if span <= LIST_H { 0. } else { (LIST_TOP + top + card / 2. - LIST_H / 2.).clamp(0., span + LIST_TOP - LIST_H) };
        let from = self.scroll_now(now);
        self.expanded = Some((id.to_string(), now));
        if super::queue::reduce_motion() {
            self.scroll = to;
            self.glide = None;
        } else {
            self.scroll = to;
            self.glide = Some((from, to, now));
        }
    }

    /// The pointer moved onto (or off) a capsule: its × and arrow grow in at once, and stay a
    /// beat after it leaves.
    fn hover_row(&mut self, id: &str, on: bool, cx: &mut Context<Self>) {
        let now = Instant::now();
        if on {
            if !self.hot.as_ref().is_some_and(|(h, _)| h == id) {
                self.cool = self.hot.take().map(|(h, _)| (h, now));
                if self.cool.as_ref().is_some_and(|(c, _)| c == id) {
                    self.cool = None;
                }
                self.hot = Some((id.to_string(), now));
            }
            self.hot_left = None;
        } else if self.hot.as_ref().is_some_and(|(h, _)| h == id) {
            self.hot_left = Some(now);
        }
        cx.notify();
    }

    /// How far a capsule's × and arrow are out: 0 hidden, 1 out (a little past it while they
    /// spring in).
    fn grow(&self, id: &str, now: Instant) -> f32 {
        let reduce = super::queue::reduce_motion();
        if let Some((h, at)) = &self.hot
            && h == id
        {
            return if reduce { 1. } else { back_out((now.duration_since(*at).as_secs_f32() / GROW.as_secs_f32()).min(1.)) };
        }
        if let Some((c, at)) = &self.cool
            && c == id
            && !reduce
        {
            return 1. - ease_in((now.duration_since(*at).as_secs_f32() / SHRINK.as_secs_f32()).min(1.));
        }
        0.
    }

    /// An open (or collapsing) card's progress: 0 its capsule, 1 the card (a little past it as
    /// it springs open). None: a capsule.
    fn card_k(&self, id: &str, now: Instant) -> Option<f32> {
        let reduce = super::queue::reduce_motion();
        if let Some((e, at)) = &self.expanded
            && e == id
        {
            return Some(if reduce { 1. } else { back_out((now.duration_since(*at).as_secs_f32() / CARD_IN.as_secs_f32()).min(1.)) });
        }
        if let Some((_, at, _)) = self.collapsing.iter().find(|(c, _, _)| c == id) {
            return Some(if reduce { 0. } else { 1. - ease_in((now.duration_since(*at).as_secs_f32() / CARD_OUT.as_secs_f32()).min(1.)) });
        }
        None
    }

    /// A card's full height: a collapsing one's as it was, the open one's measured.
    fn card_height_of(&self, id: &str) -> f32 {
        self.collapsing.iter().find(|(c, _, _)| c == id).map_or_else(|| self.card_height(), |(_, _, h)| *h)
    }

    /// The open card's height, measured (a guess until it has painted once).
    fn card_height(&self) -> f32 {
        // Its top row and contents (measured together), the padding below and the border.
        self.card_box.get().map_or(230., |b| f32::from(b.size.height) + 16.)
    }

    /// Each row's top in the list, and its height (an open card's grows from a capsule's).
    fn layout(&self, rows: &[&Waiting], now: Instant) -> (Vec<f32>, Vec<f32>) {
        let mut tops = vec![];
        let mut heights = vec![];
        let mut y = 0.;
        for w in rows {
            let h = self.card_k(&w.id, now).map_or(ROW, |k| ROW + (self.card_height_of(&w.id) - ROW) * k.min(1.));
            tops.push(y);
            heights.push(h);
            y += h + GAP;
        }
        (tops, heights)
    }

    /// Where the list is scrolled now (partway through a glide).
    fn scroll_now(&self, now: Instant) -> f32 {
        match self.glide {
            Some((from, to, at)) => {
                let x = (now.duration_since(at).as_secs_f32() / GLIDE.as_secs_f32()).min(1.);
                from + (to - from) * (1. - (1. - x).powi(4))
            }
            None => self.scroll,
        }
    }

    /// How far the list scrolls: from its first row at the top to its last at the bottom (no
    /// room above the first or below the last).
    fn scroll_range(&self, tops: &[f32], heights: &[f32]) -> (f32, f32) {
        let end = tops.last().zip(heights.last()).map_or(0., |(t, h)| LIST_TOP + t + h + LIST_TOP);
        (0., (end - LIST_H).max(0.))
    }

    fn wheel(&mut self, dy: f32, cx: &mut Context<Self>) {
        let now = Instant::now();
        self.list_touched = now;
        let s = self.scroll_now(now);
        let rows = self.rows();
        let (tops, heights) = self.layout(&rows, now);
        let (lo, hi) = self.scroll_range(&tops, &heights);
        // A list that fits doesn't scroll (and settles back if a card had moved it).
        let fits = tops.last().zip(heights.last()).is_none_or(|(t, h)| LIST_TOP + t + h <= LIST_H);
        self.glide = None;
        if fits {
            self.scroll = 0.;
            cx.notify();
            return;
        }
        self.scroll = (s - dy).clamp(lo.min(s), hi.max(s));
        cx.notify();
    }

    /// How far the list's `k`th row on screen is in: 0 not yet out of the badge (or lifted away),
    /// 1 in place (a little past it as it bounces).
    fn cascade(&self, k: usize, now: Instant) -> f32 {
        if super::queue::reduce_motion() {
            return if self.list_closing.is_some() { 0. } else { 1. };
        }
        match self.list_closing {
            Some(at) => cascade_at(k, CASCADE_ROWS, now.duration_since(at), true),
            None => cascade_at(k, CASCADE_ROWS, now.duration_since(self.list_since), false),
        }
    }

    /// Something in the list is moving (draw the next frame too).
    fn list_moving(&self, now: Instant) -> bool {
        self.list
            && (self.glide.is_some()
                || self.list_closing.is_some()
                || now.duration_since(self.list_since) < CASCADE_IN_STEP * CASCADE_ROWS as u32 + CASCADE_IN
                || !self.collapsing.is_empty()
                || self.cool.is_some()
                || self.expanded.as_ref().is_some_and(|(_, t)| now.duration_since(*t) < CARD_IN + CARD_FADE_AFTER + CARD_FADE)
                || self.hot.as_ref().is_some_and(|(_, t)| now.duration_since(*t) < GROW))
    }

    /// The count is what's waiting; a change rolls the number.
    fn recount(&mut self, cx: &mut Context<Self>) {
        let held_needs: Vec<String> = self.needs.iter().map(|n| n.id.clone()).collect();
        // A notification's line for a needs-you item that's been answered goes at once.
        for l in self.lines.iter_mut() {
            if l.need.as_ref().is_some_and(|id| !held_needs.contains(id)) && l.leaving.is_none() {
                l.leaving = Some(Instant::now());
            }
        }
        self.cleared.retain(|id| self.needs.iter().any(|w| w.id == *id));
        let n = self.rows().len();
        if n > 0 && let Some(d) = self.debug.take() {
            self.list = d == "list" || d == "card";
            if d == "card" {
                self.expanded = self.rows().first().map(|w| (w.id.clone(), Instant::now()));
            }
            self.menu = d == "menu";
        }
        if n != self.count {
            self.from = self.count;
            self.count = n;
            self.roll += 1;
            self.roll_at = Instant::now();
        }
        self.failed.retain(|id, _| self.needs.iter().any(|w| w.id == *id));
        if let Some(id) = self.reopen.clone()
            && self.rows().iter().any(|w| w.id == id)
        {
            self.reopen = None;
            self.open_list();
            self.toggle_card(&id);
        }
        if n == 0 {
            self.close_list();
        } else if self.expanded.as_ref().is_some_and(|(id, _)| !self.rows().iter().any(|w| w.id == *id)) {
            self.expanded = None;
        }
        cx.notify();
    }

    fn add(&mut self, seq: u64, session: Option<String>, p: Posted, name: String, need: Option<NeedsYou>, cx: &mut Context<Self>) {
        let stay = crate::ui::toast::stay_of(&p);
        let color = p.color.clone().unwrap_or_else(|| default_color(&p.category).into());
        let mut waiting = None;
        if let Some(id) = p.needs_you_id.clone() {
            if !self.needs.iter().any(|w| w.id == id) {
                // It replaces the terminal's older ones (`needs_you.replace`): they go now too, so
                // the count doesn't go up and back down when the refresh lands.
                if self.replace && p.category == "attention" && session.is_some() {
                    self.needs.retain(|w| !(w.category == "attention" && w.session == session));
                }
                // The notification can beat the needs-you refresh: count it now (the next sync
                // fills it in, or drops it if it's already answered).
                let first = p.body.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_string();
                self.pending.push((id.clone(), Instant::now()));
                self.needs.push(Waiting {
                    id: id.clone(),
                    sent: None,
                    need: Some(id.clone()),
                    session: session.clone(),
                    category: p.category.clone(),
                    name: name.clone(),
                    label: midna_proto::notify::category(&p.category).map(|c| c.label.to_string()).unwrap_or_default(),
                    color: color.clone(),
                    title: need.as_ref().map(|n| capitalize(n.title.trim())).unwrap_or(first),
                    detail: need.as_ref().map(|n| n.detail.trim().to_string()).filter(|d| !d.is_empty()),
                    command: need.as_ref().and_then(|n| n.approval.as_ref()).map(|a| a.action.value.clone()).filter(|v| !v.is_empty()),
                    approval: need.as_ref().map_or(p.category == "approval", |n| n.is_approval()),
                    options: need.as_ref().map(options_of).unwrap_or_default(),
                    multi: need.as_ref().and_then(|n| n.question.as_ref()).is_some_and(|q| q.multi_select),
                    image: p.image.clone().map(PathBuf::from),
                    at: midna_proto::time::now_rfc3339(),
                });
            }
            waiting = Some(id);
        } else if stay.is_none() {
            // A kind that stays: counted until you dismiss it (one per terminal and kind, or
            // per `notify.send` id).
            match p.id.as_deref() {
                Some(sent) => self.held.retain(|w| w.sent.as_ref().is_none_or(|s| s.id != sent)),
                None => self.held.retain(|w| !(w.session == session && w.category == p.category)),
            }
            let mut lines = p.body.lines().map(str::trim).filter(|l| !l.is_empty());
            let id = format!("n{seq}");
            self.held.push(Waiting {
                id: id.clone(),
                sent: Sent::of(&p),
                need: None,
                session: session.clone(),
                category: p.category.clone(),
                name: name.clone(),
                label: p.label.clone().or_else(|| midna_proto::notify::category(&p.category).map(|c| c.label.to_string())).unwrap_or_default(),
                color: color.clone(),
                title: lines.next().unwrap_or("").to_string(),
                detail: Some(lines.collect::<Vec<_>>().join("\n")).filter(|d| !d.is_empty()),
                command: None,
                approval: false,
                options: vec![],
                multi: false,
                image: p.image.clone().map(PathBuf::from),
                at: midna_proto::time::now_rfc3339(),
            });
            waiting = Some(id);
        }
        self.serial += 1;
        self.pulse = (self.serial, color.clone());
        self.pulse_at = Instant::now();
        if self.within {
            self.shine(SHIMMER_ALPHA);
        }
        // Shared screen: it's counted, but no line says what it is.
        if self.quiet() {
            self.recount(cx);
            return;
        }
        let text = p.body.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or(&p.title).to_string();
        // The newest shows at once; the ones it cut short are its "+N" (what they're about is
        // still counted, or in the stack).
        let burst = self.lines.front().filter(|l| l.leaving.is_none()).map_or(0, |l| l.burst + 1);
        self.lines.clear();
        let now = Instant::now();
        self.lines.push_back(Line {
            burst,
            serial: self.serial,
            waiting,
            session,
            need: p.needs_you_id.clone(),
            sent: Sent::of(&p),
            category: p.category.clone(),
            name,
            text,
            color: color.clone(),
            image: p.image.clone().map(PathBuf::from),
            stay,
            until: now + stay.unwrap_or(LINE_SHOW),
            leaving: None,
        });
        self.recount(cx);
    }

    /// Take away `notify.send` notification `sent` (`notify.withdraw`).
    fn withdraw(&mut self, sent: &str, cx: &mut Context<Self>) {
        let is = |s: &Option<Sent>| s.as_ref().is_some_and(|s| s.id == sent);
        self.held.retain(|w| !is(&w.sent));
        let now = Instant::now();
        for l in self.lines.iter_mut().filter(|l| is(&l.sent)) {
            l.leaving.get_or_insert(now);
        }
        self.recount(cx);
    }

    /// Report what you did with a `notify.send` notification.
    fn respond(&self, sent: &Sent, kind: midna_proto::notify::ResponseKind, action: Option<String>) {
        crate::notify::respond(&self.backend, &sent.id, midna_proto::notify::Response { kind, action });
    }

    /// One of a `notify.send` notification's buttons: report it, and it's handled.
    fn pick(&mut self, id: &str, sent: &Sent, action: String, cx: &mut Context<Self>) {
        self.respond(sent, midna_proto::notify::ResponseKind::Action, Some(action));
        self.dismiss(id, cx);
    }

    /// Drop a held notification (the stack's Dismiss).
    fn dismiss(&mut self, id: &str, cx: &mut Context<Self>) {
        self.held.retain(|w| w.id != id);
        for l in self.lines.iter_mut().filter(|l| l.waiting.as_deref() == Some(id)) {
            l.leaving.get_or_insert(Instant::now());
        }
        self.recount(cx);
    }

    /// Answer an approval. It goes at once; if midnad turns the answer down (say, what it'd run
    /// changed since you were asked), it comes back with the reason on its card, the stack open
    /// on it (the home window's toast for it isn't where you're looking).
    fn approve(&mut self, need: String, deny: bool, cx: &mut Context<Self>) {
        self.needs.retain(|w| w.id != need);
        self.failed.remove(&need);
        self.recount(cx);
        crate::sounds::play(if deny { "denied" } else { "approved" });
        let res = if deny { Resolution::Deny } else { Resolution::Approve { scope: ApprovalScope::Once } };
        let params = json!({ "id": need, "resolution": res });
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { backend.call("needs_you.resolve", params) }).await;
            let _ = this.update(cx, |b, cx| {
                if let Err(e) = r {
                    b.failed.insert(need.clone(), format!("{e:#}"));
                    b.reopen = Some(need);
                    cx.notify();
                }
            });
            // Either way, the home window refetches (and syncs the badge).
            let _ = cx.update(|cx| crate::windows::with_active(cx, |m, _, cx| m.request_refresh(crate::app::refresh::NEEDS | crate::app::refresh::SESSIONS | crate::app::refresh::RULES, cx)));
        })
        .detach();
    }

    /// Bring midna forward on the item's terminal (as a clicked notification does), or open a
    /// `notify.send` notification's URL.
    fn open(&mut self, session: Option<String>, need: Option<String>, held: Option<String>, sent: Option<Sent>, cx: &mut Context<Self>) {
        self.close_list();
        self.menu = false;
        // What you clicked is seen: its capsule doesn't come back when the badge does.
        self.lines.clear();
        if let Some(id) = held {
            self.dismiss(&id, cx);
        }
        if let Some(sent) = &sent {
            self.respond(sent, midna_proto::notify::ResponseKind::Clicked, None);
            if let Some(url) = &sent.open {
                crate::notify::open_url(url);
                return cx.notify();
            }
        }
        self.bring_forward(cx);
        match session {
            Some(s) => crate::windows::reveal_need(s, need, cx),
            None => crate::windows::with_active(cx, |m, window, cx| {
                window.activate_window();
                if let Some(id) = need.filter(|id| m.needs.iter().any(|n| n.id == *id)) {
                    crate::ui::needs_you::show(m, id, window, cx);
                }
            }),
        }
        cx.notify();
    }

    /// Make midna the app in front (opening a terminal, Settings): the badge's "never activate"
    /// is lifted while it asks, or macOS turns the request down.
    fn bring_forward(&mut self, cx: &mut Context<Self>) {
        if let Some(ns) = &self.ns {
            prevent_activation(ns, false);
            self.preventing = false;
            self.reprevent = Some(Instant::now() + Duration::from_secs(1));
        }
        cx.activate(true);
    }

    fn hide_badge(&mut self, cx: &mut Context<Self>) {
        self.menu = false;
        self.close_list();
        self.mode = Mode::Off;
        let backend = self.backend.clone();
        cx.background_executor().spawn(async move { backend.call("settings.set", json!({ "key": "notify.badge", "value": "off" })) }).detach();
        cx.notify();
    }

    /// Save where a drop put it: its corner, and in free mode its distances from it.
    fn save(&mut self, corner: Corner, inset: Option<(f64, f64)>, cx: &mut Context<Self>) {
        let mut sets = vec![];
        if corner != self.corner {
            sets.push(json!({ "key": "notify.badge.corner", "value": corner.key() }));
        }
        if let Some((x, y)) = inset.filter(|i| *i != self.inset) {
            sets.push(json!({ "key": "notify.badge.inset_x", "value": x as i64 }));
            sets.push(json!({ "key": "notify.badge.inset_y", "value": y as i64 }));
        }
        let backend = self.backend.clone();
        cx.background_executor()
            .spawn(async move {
                for s in sets {
                    let _ = backend.call("settings.set", s);
                }
            })
            .detach();
    }

    /// Sling the badge to `corner`, carrying `vel` (the throw, points a second) into a spring.
    fn snap(&mut self, corner: Corner, vel: (f64, f64), window: &mut Window, cx: &mut Context<Self>) {
        self.corner = corner;
        cx.notify();
        // Docked in midna's window: it goes there when midna's left.
        if self.within {
            return;
        }
        let Some(ns) = super::twilight::ns_window(window) else { return };
        let Some(to) = origin_at(&ns, corner, self.inset) else { return };
        if super::queue::reduce_motion() {
            self.slide = None;
            ns.setFrameOrigin(objc2_foundation::NSPoint::new(to.0, to.1));
            return;
        }
        let f = ns.frame();
        let clamp = |v: f64| v.clamp(-THROW_MAX, THROW_MAX);
        self.slide = Some(Slide { pos: (f.origin.x, f.origin.y), vel: (clamp(vel.0), clamp(vel.1)), to, at: Instant::now() });
    }

    /// Show or hide the window, let clicks through outside the zones, age the capsules, and
    /// tell a drag from a click.
    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ns) = super::twilight::ns_window(window) else { return };
        let now = Instant::now();
        let mut changed = false;

        if self.reprevent.is_some_and(|t| now >= t) {
            self.reprevent = None;
            crate::lifecycle::log(&format!("badge brought midna forward: {}", if midna_in_front(&ns) { "yes" } else { "no (macOS kept the other app in front)" }));
        }

        // The badge follows the pointer while it's pressed (and moved past the slop).
        // Docked in midna's window, it stays put: a press is a click.
        let within = self.within;
        if let Some(p) = self.pressing.as_mut() {
            let m = objc2_app_kit::NSEvent::mouseLocation();
            let (dx, dy) = (m.x - p.mouse.0, m.y - p.mouse.1);
            let was = p.moved;
            p.moved |= !within && (dx.abs() > DRAG_SLOP || dy.abs() > DRAG_SLOP);
            // A drag starting: an open list's rows lift away (tick closes it after).
            if p.moved && !was && self.list && self.list_closing.is_none() {
                self.list_closing = Some(now);
                changed = true;
            }
            if p.moved {
                ns.setFrameOrigin(objc2_foundation::NSPoint::new(p.origin.0 + dx, p.origin.1 + dy));
            }
            let dt = now.duration_since(p.last.1).as_secs_f64();
            if dt > 0.001 {
                let v = ((m.x - p.last.0.0) / dt, (m.y - p.last.0.1) / dt);
                p.vel = (p.vel.0 * 0.4 + v.0 * 0.6, p.vel.1 * 0.4 + v.1 * 0.6);
                p.last = ((m.x, m.y), now);
            }
        }

        // Midna came to the front or went: spring onto its window's top right, or back to the
        // corner (straight there while it's hidden). A press on the badge says neither.
        if let Some(within) = here(&ns)
            && within != self.within
            && self.pressing.is_none()
        {
            self.within = within;
            // Leaving midna with the list open: it lifts away.
            if !within && self.list && self.list_closing.is_none() {
                self.list_closing = Some(now);
            }
            let to = if within { dock_origin(&ns) } else { origin_at(&ns, self.corner, self.inset) };
            if let Some(to) = to {
                if self.shown && !super::queue::reduce_motion() {
                    let f = ns.frame();
                    self.slide = Some(Slide { pos: (f.origin.x, f.origin.y), vel: (0., 0.), to, at: now });
                } else {
                    self.slide = None;
                    ns.setFrameOrigin(objc2_foundation::NSPoint::new(to.0, to.1));
                }
            }
            changed = true;
        }
        // Docked: it follows its window (and the window it's springing to).
        if self.within
            && self.pressing.is_none()
            && let Some(to) = dock_origin(&ns)
        {
            match self.slide.as_mut() {
                Some(sl) => sl.to = to,
                None => {
                    let f = ns.frame();
                    if (f.origin.x - to.0).abs() > 0.25 || (f.origin.y - to.1).abs() > 0.25 {
                        ns.setFrameOrigin(objc2_foundation::NSPoint::new(to.0, to.1));
                    }
                }
            }
        }
        // Docked, with something waiting: a softer sweep now and then (not under the pointer).
        if self.within && self.count > 0 && !self.list && self.hover != Some("badge") && self.shimmer.is_none_or(|(_, _, at)| now.duration_since(at) >= REMIND) {
            self.shine(REMIND_ALPHA);
            changed = true;
        }

        // The sling: a damped spring to the corner, stepped in small slices.
        if let Some(sl) = self.slide.as_mut() {
            let mut left = now.duration_since(sl.at).as_secs_f64().min(0.1);
            sl.at = now;
            while left > 0. {
                let dt = left.min(0.002);
                left -= dt;
                for (x, v, to) in [(&mut sl.pos.0, &mut sl.vel.0, sl.to.0), (&mut sl.pos.1, &mut sl.vel.1, sl.to.1)] {
                    *v += (-SPRING_K * (*x - to) - SPRING_C * *v) * dt;
                    *x += *v * dt;
                }
            }
            let settled = (sl.pos.0 - sl.to.0).hypot(sl.pos.1 - sl.to.1) < 0.5 && sl.vel.0.hypot(sl.vel.1) < 15.;
            let at = if settled { sl.to } else { sl.pos };
            ns.setFrameOrigin(objc2_foundation::NSPoint::new(at.0, at.1));
            if settled {
                self.slide = None;
            }
        }

        // Capsules: the front one shows until its time (held while the pointer's on it).
        if let Some(front) = self.lines.front_mut() {
            if self.hover == Some("line") {
                front.until = front.until.max(now + LINE_LINGER);
            } else if front.leaving.is_none() && now >= front.until {
                front.leaving = Some(now);
                changed = true;
            }
            if front.leaving.is_some_and(|t| now.duration_since(t) >= LINE_OUT) {
                self.lines.pop_front();
                if let Some(next) = self.lines.front_mut() {
                    next.until = now + next.stay.unwrap_or(LINE_SHOW);
                }
                changed = true;
            }
        }

        if self.watched_at.is_none_or(|t| now.duration_since(t) >= WATCH_EVERY) {
            self.watched_at = Some(now);
            let shared = self.sharing != Sharing::Show && screen_watched();
            if shared != self.shared {
                self.shared = shared;
                changed = true;
            }
        }
        // Sharing started: what's out goes now, before anyone reads it.
        if self.quiet() && (!self.lines.is_empty() || self.list) {
            self.lines.clear();
            self.close_list();
            changed = true;
        }

        // In another midna build, that one's shows instead (an open stack or menu closes); a
        // press or a sling finishes first.
        let busy = self.pressing.is_some() || self.slide.is_some();
        // `MIDNA_DEBUG_BADGE` (dev screenshots): shown whatever's in front.
        let enabled = match self.mode {
            _ if crate::dev::var("MIDNA_DEBUG_BADGE").is_ok() => true,
            Mode::Off => false,
            Mode::Always => true,
            Mode::Background => busy || self.within || !midna_in_front(&ns),
        } && !(self.shared && self.sharing == Sharing::Hide);
        if !enabled && (self.list || self.menu) {
            self.close_list();
            self.menu = false;
            changed = true;
        }
        let want = enabled && (self.count > 0 || !self.lines.is_empty() || self.list || self.menu);
        if want != self.shown {
            self.shown = want;
            crate::lifecycle::log(&format!(
                "badge {} (mode {}, count {}, app active {}, shared {})",
                if want { "shown" } else { "hidden" },
                match self.mode { Mode::Off => "off", Mode::Always => "always", Mode::Background => "background" },
                self.count,
                crate::sounds::app_active(),
                self.shared,
            ));
            if want {
                ns.orderFrontRegardless();
            } else {
                ns.orderOut(None);
                self.hover = None;
                self.pressing = None;
            }
            changed = true;
        }

        if self.shown {
            // Where the pointer is, in the window's coordinates.
            let mouse = objc2_app_kit::NSEvent::mouseLocation();
            let f = ns.frame();
            let p = point(px((mouse.x - f.origin.x) as f32), px((f.origin.y + f.size.height - mouse.y) as f32));
            let hover = self.zones.hit(p);
            if hover != self.hover {
                if hover == Some("badge") {
                    self.hover_since = Some(now);
                } else if self.hover == Some("badge") {
                    self.hover_since = None;
                }
                self.hover = hover;
                changed = true;
            }
            // Resting on the badge opens the list.
            if self.hover_since.is_some_and(|t| now.duration_since(t) >= HOVER_OPEN) && self.pressing.is_none() {
                self.hover_since = None;
                if !self.list && !self.menu && self.count > 0 && !self.quiet() {
                    self.open_list();
                    self.hover_opened = Some(now);
                    changed = true;
                }
            }
            ns.setIgnoresMouseEvents(hover.is_none() && self.pressing.is_none());

            // A press on the badge ended: a drag slings to the nearest corner, a click opens the stack.
            if let Some(p) = self.pressing
                && objc2_app_kit::NSEvent::pressedMouseButtons() & 1 == 0
            {
                self.pressing = None;
                // A click or a drag ends the wait to open the list by resting there.
                self.hover_since = None;
                if p.moved {
                    // A pause before letting go is a drop, not a throw.
                    let vel = if now.duration_since(p.last.1) > Duration::from_millis(80) { (0., 0.) } else { p.vel };
                    if self.free {
                        // It stays where it lands (nudged back on if it's past the edge): saved
                        // as the nearest corner and its distances from it.
                        let corner = nearest(&ns, self.corner, (0., 0.));
                        let inset = inset_from(&ns, corner).unwrap_or(self.inset);
                        self.save(corner, Some(inset), cx);
                        self.inset = inset;
                        self.snap(corner, (0., 0.), window, cx);
                    } else {
                        // The corner it's thrown toward: where it'd coast to in a fifth of a second.
                        let corner = nearest(&ns, self.corner, (vel.0 * 0.2, vel.1 * 0.2));
                        self.save(corner, None, cx);
                        self.snap(corner, vel, window, cx);
                    }
                } else if self.menu {
                    self.menu = false;
                } else {
                    if self.hover_opened.take().is_some_and(|t| now.duration_since(t) < HOVER_GRACE) {
                        // Clicked as resting there opened it: it stays open.
                    } else if self.list && self.list_closing.is_none() {
                        // Its rows lift away first (tick closes it after).
                        self.list_closing = Some(now);
                    } else if self.count > 0 && !self.quiet() {
                        self.open_list();
                    }
                }
                changed = true;
            }
        }
        // Only a click on the badge is kept from activating midna: left on, the flag turns away
        // anything else bringing it forward (an app switcher's flashes in and back out).
        let prevent = self.shown && (self.hover.is_some() || self.pressing.is_some()) && self.reprevent.is_none();
        if prevent != self.preventing {
            self.preventing = prevent;
            prevent_activation(&ns, prevent);
        }
        // The list: × and arrow linger, then shrink; finished animations end.
        if self.hot.is_some() && self.hot_left.is_none() && self.hover != Some("panel") {
            // The pointer left the window's list without a hover-out reaching us.
            self.hot_left = Some(now);
        }
        if self.hot_left.is_some_and(|t| now.duration_since(t) >= HOT_LINGER) {
            self.hot_left = None;
            self.cool = self.hot.take().map(|(id, _)| (id, now));
            changed = true;
        }
        if self.cool.as_ref().is_some_and(|(_, t)| now.duration_since(*t) >= SHRINK) {
            self.cool = None;
            changed = true;
        }
        // Left alone, the list closes itself (the same cascade as clicking the badge).
        if self.list && self.hover.is_some() {
            self.list_touched = now;
        }
        if self.list && self.list_closing.is_none() && now.duration_since(self.list_touched) >= LIST_IDLE {
            self.list_closing = Some(now);
            changed = true;
        }
        if self.list_closing.is_some_and(|t| now.duration_since(t) >= CASCADE_OUT_STEP * CASCADE_ROWS as u32 + CASCADE_OUT) {
            self.close_list();
            changed = true;
        }
        let before = self.collapsing.len();
        self.collapsing.retain(|(_, t, _)| now.duration_since(*t) < CARD_OUT);
        changed |= self.collapsing.len() != before;
        if let Some((_, to, t)) = self.glide
            && now.duration_since(t) >= GLIDE
        {
            self.glide = None;
            self.scroll = to;
        }
        if changed {
            cx.notify();
        }
    }
}

// ------------------------------------------------------------------ drawing

/// Record an element's painted bounds in `cell` (its zone).
fn measure(cell: Rc<Cell<Option<Bounds<Pixels>>>>) -> impl IntoElement {
    canvas(move |b, _, _| cell.set(Some(b)), |_, _, _, _| {}).absolute().top_0().left_0().size_full()
}

/// A question's options, numbered as in the terminal (where you pick one).
fn options(t: &Theme, w: &Waiting) -> Div {
    let mut col = div().flex().flex_col().gap(px(5.));
    if w.multi {
        col = col.child(div().text_size(px(11.5)).text_color(t.dim).child("Pick any number of these in the terminal"));
    }
    for (i, (label, description)) in w.options.iter().enumerate() {
        col = col.child(
            div()
                .flex()
                .gap(px(8.))
                .px(px(10.))
                .py(px(6.))
                .rounded(px(8.))
                .border_1()
                .border_color(t.line)
                .bg(t.panel)
                .child(div().flex_none().w(px(14.)).font_family(t.mono_font.clone()).text_size(px(11.5)).text_color(t.dim).child((i + 1).to_string()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().text_size(px(13.)).font_weight(FontWeight::BOLD).child(label.clone()))
                        .when(!description.is_empty(), |d| d.child(div().text_size(px(12.)).line_height(px(17.)).text_color(t.dim).child(description.clone()))),
                ),
        );
    }
    col
}

fn button(t: &Theme, id: impl Into<ElementId>, label: &str, primary: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .h(px(26.))
        .px(px(10.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(13.))
        .text_size(px(12.))
        .cursor_pointer()
        .map(|d| {
            if primary {
                d.bg(t.accent).text_color(t.accent_fg).font_weight(FontWeight::BOLD)
            } else {
                d.border_1().border_color(t.line).text_color(t.fg).hover(|s| s.bg(t.panel))
            }
        })
        .child(label.to_string())
}

impl Badge {
    fn color(&self, t: &Theme, c: &str) -> Hsla {
        super::notifications::color_of(t, c).unwrap_or(t.accent)
    }

    /// The badge, a glass pill (design "Twilight tiles"): a glowing accent diamond, the count
    /// (or a check while a passing line shows), and a small diamond per kind that's waiting in
    /// its color (most first, up to three); a teal shine along its top edge. All theme colors.
    fn badge(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let reduce = super::queue::reduce_motion();
        let passing = self.lines.front().filter(|l| l.leaving.is_none() && l.waiting.is_none());
        let mut tally: Vec<(String, usize)> = vec![];
        for w in self.rows() {
            match tally.iter_mut().find(|(c, _)| *c == w.color) {
                Some((_, n)) => *n += 1,
                None => tally.push((w.color.clone(), 1)),
            }
        }
        tally.sort_by(|a, b| b.1.cmp(&a.1));
        let diamond = |size: f32, color: Hsla| Icon::Tile.el(size, color).with_transformation(Transformation::rotate(radians(std::f32::consts::FRAC_PI_4)));

        // The number rolls: up, the new one rises from below; down, it drops in from above.
        let (from, to) = (self.from, self.count);
        let label = |n: usize| if n > 99 { "99+".to_string() } else { n.to_string() };
        let digits = |n: usize| div().h(px(20.)).flex().items_center().justify_center().child(label(n));
        let up = to > from;
        let wide = label(from).len().max(label(to).len()) as f32;
        let column = div().absolute().left_0().right_0().flex().flex_col().map(|d| if up { d.child(digits(from)).child(digits(to)) } else { d.child(digits(to)).child(digits(from)) });
        let number = div()
            .relative()
            .flex_none()
            .w(px(10. * wide + 2.))
            .h(px(20.))
            .overflow_hidden()
            .text_size(px(16.))
            .font_weight(FontWeight::BOLD)
            .text_color(t.fg)
            .child(if reduce || from == to || self.roll_at.elapsed() >= ROLL {
                div().absolute().top_0().left_0().right_0().child(digits(to)).into_any_element()
            } else {
                column
                    .with_animation(SharedString::from(format!("roll-{}", self.roll)), Animation::new(ROLL).with_easing(back_out), move |el, d| {
                        el.top(px(if up { -20. * d } else { -20. + 20. * d }))
                    })
                    .into_any_element()
            });

        // The lead diamond: the accent, glowing (a passing line's color while one shows).
        let lead = passing.map_or(t.accent, |l| self.color(t, &l.color));
        let mark = div()
            .relative()
            .flex_none()
            .size(px(12.))
            .flex()
            .items_center()
            .justify_center()
            .child(div().absolute().size(px(6.)).rounded_full().shadow(vec![BoxShadow { color: lead.opacity(0.7), offset: point(px(0.), px(0.)), blur_radius: px(9.), spread_radius: px(1.), inset: false }]))
            .child(diamond(9., lead));
        let kinds = div().flex().items_center().gap(px(4.)).children(tally.iter().take(3).map(|(c, _)| diamond(6., self.color(t, c))));
        // While a capsule with an image shows, the badge wears it in the diamond's place.
        let worn = self.lines.front().filter(|_| !self.quiet()).and_then(|l| Some(self.worn(l.serial, l.leaving.is_some(), l.image.clone()?, self.color(t, &l.color), reduce)));
        // The shine: the theme's cyan (Twilight's teal), fading out toward both ends.
        let glow = t.ansi[6];
        let shine = div()
            .absolute()
            .top_0()
            .left(px(14.))
            .right(px(14.))
            .h(px(1.))
            .flex()
            .child(div().flex_1().h_full().bg(linear_gradient(90., linear_color_stop(glow.opacity(0.), 0.), linear_color_stop(glow.opacity(0.7), 1.))))
            .child(div().flex_1().h_full().bg(linear_gradient(90., linear_color_stop(glow.opacity(0.7), 0.), linear_color_stop(glow.opacity(0.), 1.))));
        let width = self.badge_w.clone();
        let inner = div()
            .relative()
            .h_full()
            .pl(px(13.))
            .pr(px(14.))
            .flex()
            .items_center()
            .gap(px(9.))
            .child(canvas(move |b, _, _| width.set(f32::from(b.size.width)), |_, _, _, _| {}).absolute().top_0().left_0().size_full())
            .map(|d| match worn {
                Some(w) => d.child(w),
                None => d.child(mark),
            })
            .map(|d| match passing {
                Some(l) if self.count == 0 => d.child(Icon::Check.el(16., self.color(t, &l.color))),
                _ => d.child(number),
            })
            .when(!tally.is_empty(), |d| d.child(kinds));

        let pulse = self.color(t, &self.pulse.1);
        let lift = self.hover == Some("badge");
        let el = div()
            .id("badge")
            .relative()
            .h(px(SIZE))
            .min_w(px(SIZE))
            .rounded(px(SIZE / 2.))
            .bg(linear_gradient(180., linear_color_stop(t.raised, 0.), linear_color_stop(t.panel, 1.)))
            .border_1()
            .border_color(if lift { t.dim.opacity(0.5) } else { t.line })
            .cursor_pointer()
            .shadow(vec![BoxShadow { color: hsla(0., 0., 0., if lift { 0.5 } else { 0.4 }), offset: point(px(0.), px(10.)), blur_radius: px(26.), spread_radius: px(0.), inset: false }])
            .child(shine)
            .children(self.shimmer.filter(|(_, _, at)| self.within && !reduce && at.elapsed() < SHIMMER).map(|(n, alpha, _)| self.sweep(t, n, alpha)))
            .child(inner)
            .child(measure(self.zones.badge.clone()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|b, _, window, cx| {
                    b.menu = false;
                    if let Some(ns) = super::twilight::ns_window(window) {
                        // Caught mid-sling: it's held where it is.
                        b.slide = None;
                        let (f, m) = (ns.frame(), objc2_app_kit::NSEvent::mouseLocation());
                        let mouse = (m.x, m.y);
                        b.pressing = Some(Press { mouse, origin: (f.origin.x, f.origin.y), moved: false, last: (mouse, Instant::now()), vel: (0., 0.) });
                    }
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|b, _, _, cx| {
                    b.menu = !b.menu;
                    b.close_list();
                    cx.notify();
                }),
            );
        if reduce || self.pulse.0 == 0 || self.pulse_at.elapsed() >= PULSE {
            return el.into_any_element();
        }
        el.with_animation(SharedString::from(format!("pulse-{}", self.pulse.0)), Animation::new(PULSE), move |el, d| {
            // Up fast, then fade: a ring of the kind's color around the badge.
            let k = if d < 0.25 { d / 0.25 } else { 1. - (d - 0.25) / 0.75 };
            el.shadow(vec![
                BoxShadow { color: pulse.opacity(0.28 * k), offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(7. * k), inset: false },
                BoxShadow { color: hsla(0., 0., 0., 0.4), offset: point(px(0.), px(10.)), blur_radius: px(26.), spread_radius: px(0.), inset: false },
            ])
        })
        .into_any_element()
    }

    /// The image the badge wears while its capsule shows, ringed in the kind's color: it grows
    /// out of the diamond's place when the capsule springs out, and shrinks back as it leaves.
    fn worn(&self, serial: u64, leaving: bool, path: PathBuf, color: Hsla, reduce: bool) -> AnyElement {
        let full = SIZE - 10.;
        // The badge's start padding, taken back as it grows, so it sits evenly in the round end.
        let at = move |k: f32| (WEAR_FROM + (full - WEAR_FROM) * k, -8. * k.max(0.));
        let el = |k: f32| {
            let (size, ml) = at(k);
            div()
                .flex_none()
                .size(px(size))
                .ml(px(ml))
                .rounded_full()
                .border_2()
                .border_color(color)
                .overflow_hidden()
                .child(img(path.clone()).size_full().rounded_full().object_fit(ObjectFit::Cover))
        };
        if reduce {
            return el(1.).into_any_element();
        }
        if leaving {
            let base = el(1.);
            return base
                .with_animation(SharedString::from(format!("unwear-{serial}")), Animation::new(LINE_OUT).with_easing(ease_in), move |d, p| {
                    let (size, ml) = at(1. - p);
                    d.size(px(size)).ml(px(ml)).opacity(1. - p)
                })
                .into_any_element();
        }
        el(0.)
            .with_animation(SharedString::from(format!("wear-{serial}")), Animation::new(WEAR).with_easing(back_out), move |d, p| {
                let (size, ml) = at(p);
                d.size(px(size)).ml(px(ml))
            })
            .into_any_element()
    }

    /// The band sweeping across the badge (within midna), under its contents. Clipping is only
    /// ever square, so it isn't clipped: it's a pill of the badge's height with its own round
    /// ends, kept inside the badge's, and fades in as it sets off and out as it arrives.
    fn sweep(&self, t: &Theme, n: u64, alpha: f32) -> AnyElement {
        let h = SIZE - 2.;
        let w = self.badge_w.get().max(h);
        let band_w = (w * super::need_anim::BAND).max(h);
        let travel = (w - band_w).max(0.);
        let c = t.need;
        let half = |from: f32, to: f32| linear_gradient(90., linear_color_stop(c.opacity(from), 0.), linear_color_stop(c.opacity(to), 1.));
        let band = move |a: f32| {
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .w(px(band_w))
                .flex()
                .child(div().flex_1().h_full().rounded_l(px(h / 2.)).bg(half(0., a)))
                .child(div().flex_1().h_full().rounded_r(px(h / 2.)).bg(half(a, 0.)))
        };
        band(alpha)
            .with_animation(SharedString::from(format!("shimmer-{n}")), Animation::new(SHIMMER), move |el, d| {
                let k = (d * 4.).min(1.) * ((1. - d) * 4.).min(1.);
                el.left(px(travel * d)).opacity(k)
            })
            .into_any_element()
    }

    /// The capsule beside the badge: springs out, holds, slides back in.
    fn line(&self, t: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let l = self.lines.front()?;
        let reduce = super::queue::reduce_motion();
        let color = self.color(t, &l.color);
        let right = self.side().right();
        let extra = l.burst;
        let hover = self.hover == Some("line");
        let need = l.need.clone().filter(|id| self.needs.iter().any(|w| w.id == *id && w.approval));
        let (session, need_id, waiting, sent) = (l.session.clone(), l.need.clone(), l.waiting.clone(), l.sent.clone());
        let held = waiting.filter(|w| self.held.iter().any(|h| h.id == *w));
        let mut row = div()
            .id(SharedString::from(format!("line-{}", l.serial)))
            .relative()
            .h(px(36.))
            // No wider than the room the window leaves past a widened badge, or its start is cut off.
            .max_w(px((W - SIZE - PAD * 2. - 8.).min(MX + SIZE - self.badge_w.get().max(SIZE) - 8. - 4.)))
            .pl(px(14.))
            .pr(px(if need.is_some() && hover { 6. } else { 14. }))
            .flex()
            .items_center()
            .gap(px(8.))
            .rounded(px(18.))
            .overflow_hidden()
            .bg(t.panel)
            .border_1()
            .border_color(if hover { color.opacity(0.6) } else { t.line })
            .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.4), offset: point(px(0.), px(10.)), blur_radius: px(26.), spread_radius: px(0.), inset: false }])
            .text_size(px(13.))
            .whitespace_nowrap()
            .cursor_pointer()
            .on_click(cx.listener(move |b, _, _, cx| b.open(session.clone(), need_id.clone(), held.clone(), sent.clone(), cx)));
        // A passing kind's tint clears from left to right over its time. Not while it slides
        // away (its animation would start over, full width), and rounded like the capsule (GPUI
        // doesn't clip a child to its parent's corners).
        if let Some(stay) = l.stay.filter(|_| !hover && !reduce && l.leaving.is_none()) {
            let tint = div().absolute().top_0().bottom_0().left_0().right_0().rounded(px(18.)).bg(color.opacity(0.16));
            row = row.child(tint.with_animation(SharedString::from(format!("fill-{}", l.serial)), Animation::new(stay), |el, d| el.left(relative(d))));
        }
        row = row
            .child(measure(self.zones.line.clone()))
            .child(icon_of(&l.category).el(14., color))
            .child(div().flex_none().max_w(px(180.)).truncate().font_weight(FontWeight::BOLD).text_color(t.fg).child(l.name.clone()))
            .child(div().min_w_0().truncate().text_color(t.dim).child(l.text.clone()))
            .when(extra > 0, |d| {
                d.child(div().flex_none().px(px(7.)).rounded(px(9.)).bg(t.raised).text_size(px(11.5)).font_weight(FontWeight::BOLD).text_color(t.fg).child(format!("+{extra}")))
            });
        if let (Some(id), true) = (need, hover) {
            let (a, d) = (id.clone(), id);
            row = row
                .child(button(t, "line-approve", "Approve", true).on_click(cx.listener(move |b, _, _, cx| {
                    cx.stop_propagation();
                    b.approve(a.clone(), false, cx);
                })))
                .child(button(t, "line-deny", "Deny", false).on_click(cx.listener(move |b, _, _, cx| {
                    cx.stop_propagation();
                    b.approve(d.clone(), true, cx);
                })));
        }
        if reduce {
            return Some(row.into_any_element());
        }
        // Out from behind the badge with a little overshoot; back the way it came.
        let dir = if right { 1. } else { -1. };
        Some(match l.leaving {
            Some(_) => row
                .with_animation(SharedString::from(format!("line-out-{}", l.serial)), Animation::new(LINE_OUT).with_easing(ease_in), move |el, d| {
                    el.opacity(1. - d).left(px(dir * 20. * d))
                })
                .into_any_element(),
            None => row
                .with_animation(SharedString::from(format!("line-in-{}", l.serial)), Animation::new(LINE_IN).with_easing(back_out), move |el, d| {
                    el.opacity((d * 2.).min(1.)).left(px(dir * 28. * (1. - d)))
                })
                .into_any_element(),
        })
    }

    /// The list: what's waiting as capsules, newest first, scrolling; the row at the middle is
    /// the clearest, the others fade with their distance from it (and at the edges). Clear sits
    /// on the badge's side of it.
    fn list(&self, t: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.list {
            return None;
        }
        let rows = self.rows();
        if rows.is_empty() {
            return None;
        }
        let now = Instant::now();
        let right = self.side().right();
        let (tops, heights) = self.layout(&rows, now);
        let s = self.scroll_now(now);
        let mid = s + LIST_H / 2. - LIST_TOP;
        let tall = tops.last().zip(heights.last()).is_some_and(|(t, h)| LIST_TOP + t + h > LIST_H);
        // Rows fade out at an edge only while there's more past it.
        let (lo, hi) = self.scroll_range(&tops, &heights);
        let (more_above, more_below) = (s > lo + 0.5, s < hi - 0.5);
        let mut col = div()
            .absolute()
            .left_0()
            .right_0()
            .top(px(LIST_TOP - s))
            .flex()
            .flex_col()
            .gap(px(GAP))
            .map(|d| if right { d.items_end().pr(px(10.)) } else { d.items_start().pl(px(10.)) });
        // Rows on screen, top to bottom, for the cascade (closing: the bottom one first).
        let on_screen = |i: usize| ((LIST_TOP + tops[i] - s) / (ROW + GAP)).round().max(0.) as usize;
        let last = (0..rows.len()).filter(|&i| LIST_TOP + tops[i] - s < LIST_H).map(on_screen).max().unwrap_or(0);
        let step = |i: usize| if self.list_closing.is_some() { CASCADE_ROWS - 1 - (last - on_screen(i).min(last)).min(CASCADE_ROWS - 1) } else { on_screen(i) };
        for (i, w) in rows.iter().enumerate() {
            let v = self.cascade(step(i), now);
            let drop = |el: AnyElement| div().relative().top(px((1. - v) * -CASCADE_DROP)).opacity((v * 1.4).clamp(0., 1.)).child(el).into_any_element();
            let el = match self.card_k(&w.id, now) {
                Some(k) => drop(self.card(t, w, k, now, cx)),
                None => {
                    // When it scrolls, clearest at the middle line and softer away from it: the
                    // words fade, the capsule's fill only a little (what's behind the window
                    // never shows through enough to muddle them). Rows fade out whole only at
                    // the list's edges. The one under the pointer comes up to full, as its ×
                    // and arrow grow in.
                    let k = if tall { ((tops[i] + ROW / 2. - mid).abs() / (ROW + GAP)).min(3.) / 3. } else { 0. };
                    let y = LIST_TOP + tops[i] + heights[i] / 2. - s;
                    let above = if more_above { (y / 26.).clamp(0., 1.) } else { 1. };
                    let below = if more_below { ((LIST_H - y) / 52.).clamp(0., 1.) } else { 1. };
                    let edge = above * below;
                    let lift = self.grow(&w.id, now).clamp(0., 1.);
                    let fade = |floor: f32| 1. - (1. - floor) * k * (1. - lift);
                    let capsule = self.capsule(t, w, now, fade(ROW_FILL), fade(ROW_WORDS), cx);
                    drop(div().flex().opacity(edge + (1. - edge) * lift).child(capsule).into_any_element())
                }
            };
            col = col.child(el);
        }
        // Where you are in it (when it runs past the list).
        let thumb = (tall && hi - lo > 1.).then(|| {
            let track = LIST_H - 54.;
            let span = hi - lo + LIST_H;
            let size = (track * LIST_H / span).max(18.);
            let at = ((s - lo) / (hi - lo)).clamp(0., 1.) * (track - size);
            div()
                .absolute()
                .top(px(14.))
                .h(px(track))
                .w(px(3.))
                .rounded(px(2.))
                .bg(t.panel)
                .map(|d| if right { d.right_0() } else { d.left_0() })
                .child(div().absolute().left_0().right_0().top(px(at)).h(px(size)).rounded(px(2.)).bg(t.dim.opacity(0.6)))
        });
        let viewport = div()
            .id("badge-list")
            .relative()
            .w(px(LIST_W + LIST_BLEED))
            .h(px(LIST_H))
            .overflow_hidden()
            .on_scroll_wheel(cx.listener(|b, e: &ScrollWheelEvent, _, cx| b.wheel(f32::from(e.delta.pixel_delta(px(18.)).y), cx)))
            .child(col)
            .children(thumb);
        let clear = div()
            .id("list-clear")
            .px(px(10.))
            .py(px(3.))
            .rounded(px(11.))
            .bg(t.raised)
            .border_1()
            .border_color(t.dim.opacity(0.3))
            // A faint dark ring keeps it apart from a busy window behind.
            .shadow(vec![
                BoxShadow { color: t.bg.opacity(0.35), offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(4.), inset: false },
                BoxShadow { color: hsla(0., 0., 0., 0.5), offset: point(px(0.), px(4.)), blur_radius: px(14.), spread_radius: px(0.), inset: false },
            ])
            .text_size(px(12.))
            .text_color(t.fg.opacity(0.85))
            .cursor_pointer()
            .hover(|s| s.bg(t.line).text_color(t.fg))
            .on_click(cx.listener(|b, _, _, cx| b.clear_all(cx)))
            .child("Clear");
        let header = div().relative().opacity(self.cascade(if self.list_closing.is_some() { CASCADE_ROWS - 1 } else { 0 }, now).clamp(0., 1.)).w(px(LIST_W)).h(px(26.)).px(px(6.)).flex().items_center().map(|d| if right { d.justify_end() } else { d.justify_start() }).child(clear);
        let top = self.side().top();
        Some(
            div()
                .relative()
                .flex()
                .map(|d| if top { d.flex_col() } else { d.flex_col_reverse() })
                .map(|d| if right { d.items_end() } else { d.items_start() })
                .gap(px(4.))
                .child(measure(self.zones.panel.clone()))
                .child(header)
                .child(viewport)
                .into_any_element(),
        )
    }

    /// A row as a capsule: dot, terminal, what it's about. With the pointer on it, an × grows
    /// in at its start (off the list) and an arrow at its end (to the terminal); a click
    /// anywhere else opens its card. `fill`, `words`: how strong its fill (and edge and shadow)
    /// and its dot and words show.
    fn capsule(&self, t: &Theme, w: &Waiting, now: Instant, fill: f32, words: f32, cx: &mut Context<Self>) -> Stateful<Div> {
        let k = self.grow(&w.id, now).max(0.);
        let out = k > 0.01;
        let color = self.color(t, &w.color);
        let hot = self.hot.as_ref().is_some_and(|(h, _)| *h == w.id);
        let id = w.id.clone();
        let mut row = div()
            .id(SharedString::from(format!("row-{}", w.id)))
            .h(px(ROW))
            .max_w(px(LIST_W - 10.))
            .flex()
            .items_center()
            .rounded(px(18.))
            .bg(t.panel.opacity(fill))
            .border_1()
            .border_color(if hot { t.dim.opacity(0.55) } else { t.line.opacity(fill) })
            .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.4 * (1. - (1. - fill) * 5.)), offset: point(px(0.), px(10.)), blur_radius: px(26.), spread_radius: px(0.), inset: false }])
            .overflow_hidden()
            .cursor_pointer()
            .text_size(px(13.))
            .whitespace_nowrap()
            .on_hover(cx.listener({
                let id = id.clone();
                move |b, on: &bool, _, cx| b.hover_row(&id, *on, cx)
            }))
            .on_click(cx.listener({
                let id = id.clone();
                move |b, _, _, cx| {
                    b.toggle_card(&id);
                    cx.notify();
                }
            }));
        if out {
            let id = id.clone();
            row = row.child(
                div()
                    .id(SharedString::from(format!("row-x-{}", w.id)))
                    .flex_none()
                    .h(px(24.))
                    .w(px(24. * k))
                    .ml(px(6. * k))
                    .rounded(px(12.))
                    .bg(t.raised)
                    .opacity(k.min(1.))
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .justify_center()
                    .hover(|s| s.bg(t.line))
                    .child(Icon::Cross.el(10., t.dim))
                    .on_click(cx.listener(move |b, _, _, cx| {
                        cx.stop_propagation();
                        b.forget(&id, cx);
                    })),
            );
        }
        row = row.child(
            div()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(8.))
                .pl(px(14. - 6. * k.min(1.)))
                .pr(px(if w.image.is_some() { 8. } else { 14. - 6. * k.min(1.) }))
                .opacity(words)
                .child(div().size(px(8.)).flex_none().rounded_full().bg(color))
                .child(div().flex_none().max_w(px(130.)).truncate().font_weight(FontWeight::BOLD).text_color(t.fg).child(w.name.clone()))
                .child(div().min_w_0().truncate().text_color(t.dim).child(w.title.clone())),
        );
        if out {
            let (session, need, sent) = (w.session.clone(), w.need.clone(), w.sent.clone());
            let held = w.need.is_none().then(|| w.id.clone());
            row = row.child(
                div()
                    .id(SharedString::from(format!("row-go-{}", w.id)))
                    .flex_none()
                    .h(px(28.))
                    .w(px(28. * k))
                    .mr(px(4. * k))
                    .rounded(px(14.))
                    .bg(t.raised)
                    .opacity(k.min(1.))
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .justify_center()
                    .hover(|s| s.bg(t.line))
                    .child(Icon::Arrow.el(12., t.fg))
                    .on_click(cx.listener(move |b, _, _, cx| {
                        cx.stop_propagation();
                        b.open(session.clone(), need.clone(), held.clone(), sent.clone(), cx);
                    })),
            );
        }
        if let Some(path) = w.image.clone() {
            row = row.child(div().flex_none().size(px(26.)).mr(px(4.)).opacity(words).child(thumb(path, 26., 7.)));
        }
        row
    }

    /// A row's card, `k` of the way grown out of its capsule: what it's about in full and its
    /// buttons. Its top row (dot, terminal, kind, ⌃) collapses it.
    fn card(&self, t: &Theme, w: &Waiting, k: f32, now: Instant, cx: &mut Context<Self>) -> AnyElement {
        let color = self.color(t, &w.color);
        let fade = match &self.expanded {
            Some((e, at)) if *e == w.id => {
                if super::queue::reduce_motion() {
                    1.
                } else {
                    (now.duration_since(*at).saturating_sub(CARD_FADE_AFTER).as_secs_f32() / CARD_FADE.as_secs_f32()).min(1.)
                }
            }
            // Collapsing: its contents fade out over the first half, ahead of the card.
            _ => self.collapsing.iter().find(|(c, _, _)| *c == w.id).map_or(0., |(_, at, _)| 1. - (now.duration_since(*at).as_secs_f32() / (CARD_OUT.as_secs_f32() / 2.)).min(1.)),
        };
        let id = w.id.clone();
        let header = div()
            .id(SharedString::from(format!("card-top-{}", w.id)))
            .flex()
            .items_center()
            .gap(px(8.))
            .pt(px(14.))
            .pb(px(2.))
            .cursor_pointer()
            .on_click(cx.listener(move |b, _, _, cx| {
                b.toggle_card(&id);
                cx.notify();
            }))
            .child(div().size(px(8.)).flex_none().rounded_full().bg(color))
            .child(div().max_w(px(170.)).truncate().font_weight(FontWeight::BOLD).text_size(px(14.)).child(w.name.clone()))
            .when(!w.label.is_empty(), |d| d.child(div().flex_none().text_size(px(12.)).text_color(t.dim).child(w.label.clone())))
            .child(div().flex_1())
            .child(Icon::Chevron.el(12., t.dim).with_transformation(Transformation::rotate(radians(std::f32::consts::PI))));
        let mut buttons = div().flex().gap(px(8.)).pt(px(4.));
        if let Some(need) = w.need.clone().filter(|_| w.approval) {
            let (a, d) = (need.clone(), need);
            buttons = buttons
                .child(button(t, "card-approve", "Approve", true).flex_1().on_click(cx.listener(move |b, _, _, cx| b.approve(a.clone(), false, cx))))
                .child(button(t, "card-deny", "Deny", false).on_click(cx.listener(move |b, _, _, cx| b.approve(d.clone(), true, cx))));
        } else if let Some(sent) = w.sent.clone().filter(|s| !s.actions.is_empty()) {
            // A `notify.send` notification's own buttons.
            for (i, label) in sent.actions.iter().enumerate() {
                let (id, sent, label) = (w.id.clone(), sent.clone(), label.clone());
                let first = i == 0;
                buttons = buttons.child(button(t, ("card-action", i), &label, first).on_click(cx.listener(move |b, _, _, cx| b.pick(&id, &sent, label.clone(), cx))));
            }
            buttons = buttons.child(div().flex_1());
        } else {
            let id = w.id.clone();
            buttons = buttons.child(button(t, "card-dismiss", "Dismiss", false).on_click(cx.listener(move |b, _, _, cx| b.forget(&id, cx)))).child(div().flex_1());
        }
        let (session, need, sent) = (w.session.clone(), w.need.clone(), w.sent.clone());
        let held = w.need.is_none().then(|| w.id.clone());
        let opens = sent.as_ref().is_some_and(|s| s.open.is_some());
        // Somewhere to go: its URL, its terminal, or its needs-you card.
        if opens || session.is_some() || need.is_some() {
            buttons = buttons.child(
                button(t, "card-open", if opens { "Open" } else { "Terminal" }, false)
                    .gap(px(6.))
                    .child(Icon::Arrow.el(12., t.fg))
                    .on_click(cx.listener(move |b, _, _, cx| b.open(session.clone(), need.clone(), held.clone(), sent.clone(), cx))),
            );
        }
        let content = div()
            .relative()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(9.))
            .opacity(fade)
            .child(match w.image.clone() {
                // Its image beside the title, the card's detail under the title beside it too.
                Some(path) => div()
                    .flex()
                    .items_start()
                    .gap(px(12.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(div().text_size(px(14.)).font_weight(FontWeight::BOLD).line_height(px(19.)).max_h(px(19. * 3.)).overflow_hidden().child(w.title.clone()))
                            .children(w.detail.clone().filter(|_| w.command.is_none()).map(|d| div().max_h(px(19. * 5.)).overflow_hidden().text_size(px(13.)).line_height(px(19.)).text_color(t.dim).child(d))),
                    )
                    .child(thumb(path, 56., 10.)),
                None => div().text_size(px(14.)).font_weight(FontWeight::BOLD).line_height(px(19.)).max_h(px(19. * 3.)).overflow_hidden().child(w.title.clone()),
            })
            .children(self.failed.get(&w.id).map(|e| {
                div().px(px(10.)).py(px(7.)).rounded(px(7.)).bg(t.err.opacity(0.14)).text_size(px(12.5)).line_height(px(18.)).text_color(t.err).child(format!("Didn't go through: {e}"))
            }))
            .children(w.command.clone().map(|c| div().px(px(10.)).py(px(8.)).rounded(px(7.)).bg(t.term).font_family(t.mono_font.clone()).text_size(px(12.)).truncate().child(c)))
            .children(w.detail.clone().filter(|_| w.image.is_none() || w.command.is_some()).map(|d| div().max_h(px(19. * 5.)).overflow_hidden().text_size(px(13.)).line_height(px(19.)).text_color(t.dim).child(d)))
            .when(!w.options.is_empty(), |d| d.child(options(t, w)))
            .child(buttons);
        div()
            .id(SharedString::from(format!("card-{}", w.id)))
            .flex_none()
            .w(px(300. + (CARD_W - 300.) * k))
            .max_h(px(ROW + (self.card_height_of(&w.id) - ROW) * k.clamp(0., 1.)))
            .overflow_hidden()
            .px(px(14.))
            .pb(px(14.))
            .rounded(px(18.))
            .bg(t.raised)
            .border_1()
            .border_color(t.line)
            .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.4), offset: point(px(0.), px(8.)), blur_radius: px(22.), spread_radius: px(0.), inset: false }])
            .text_color(t.fg)
            .child(
                div()
                    .relative()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap(px(9.))
                    .when(self.expanded.as_ref().is_some_and(|(e, _)| *e == w.id), |d| d.child(measure(self.card_box.clone())))
                    .child(header)
                    .child(content),
            )
            .into_any_element()
    }

    fn menu(&self, t: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.menu {
            return None;
        }
        let item = |id: &'static str, label: &'static str| div().id(id).px(px(10.)).py(px(6.)).rounded(px(6.)).cursor_pointer().hover(|s| s.bg(t.accent_soft)).child(label);
        Some(
            super::sidebar::menu_box(t)
                .id("badge-menu")
                .relative()
                .text_size(px(13.))
                .child(measure(self.zones.panel.clone()))
                .child(item("menu-open", "Open midna").on_click(cx.listener(|b, _, _, cx| b.open(None, None, None, None, cx))))
                .child(item("menu-settings", "Notification settings…").on_click(cx.listener(|b, _, _, cx| {
                    b.menu = false;
                    b.bring_forward(cx);
                    crate::settings_window::open(b.backend.clone(), cx);
                    cx.notify();
                })))
                .child(div().h(px(1.)).my(px(4.)).bg(t.line))
                .child(item("menu-hide", "Hide badge").on_click(cx.listener(|b, _, _, cx| b.hide_badge(cx))))
                .child(div().px(px(10.)).pt(px(2.)).pb(px(4.)).text_size(px(11.5)).text_color(t.dim).child("Bring it back in Settings ▸ Notifications"))
                .into_any_element(),
        )
    }
}

impl Render for Badge {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<Theme>().clone();
        self.zones.clear();
        let (top, right) = (self.side().top(), self.side().right());
        // `inset` is from the screen's edge, as if the badge's corner were the window's.
        let edge = |d: Div, inset: f32| {
            let d = if top { d.top(px(inset - PAD + MY)) } else { d.bottom(px(inset - PAD + MY)) };
            if right { d.right(px(MX)) } else { d.left(px(MX)) }
        };
        let mut root = div().size_full().relative().font_family(t.ui_font.clone()).text_color(t.fg);
        if !self.shown {
            return root;
        }
        root = root.child(edge(div().absolute(), PAD).child(self.badge(&t, cx)));
        if self.list_moving(Instant::now()) {
            window.request_animation_frame();
        }
        if let Some(line) = self.line(&t, cx).filter(|_| !self.list) {
            let side = MX + self.badge_w.get().max(SIZE) + 8.;
            let at = div().absolute().flex().map(|d| if top { d.top(px(MY + 4.)) } else { d.bottom(px(MY + 4.)) });
            let at = if right { at.right(px(side)).justify_end() } else { at.left(px(side)) };
            root = root.child(at.child(line));
        }
        let below = PAD + SIZE + 10.;
        if let Some(menu) = self.menu(&t, cx) {
            root = root.child(edge(div().absolute(), below).child(menu));
        } else if let Some(list) = self.list(&t, cx) {
            root = root.child(edge(div().absolute(), below).child(list));
        }
        root
    }
}
