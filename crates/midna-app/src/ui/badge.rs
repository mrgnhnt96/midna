//! The floating badge (design D3, "springs out beside", on the floating-notifications canvas):
//! a 44px square in a corner of the screen that counts what needs you, its border split by the
//! colors of what's waiting. When a notification comes in, a one-line capsule springs out
//! beside it, the badge pulses in the kind's color and the number slides (up: the new one rises
//! from below; down: it drops in from above). Kinds that stay (`notify.stay.<kind>` 0, and
//! every needs-you item) are counted until they're handled; the others pass, their capsule's
//! tinted fill clearing from left to right over the kind's duration.
//!
//! Clicking the badge opens the card stack (1 of N): Approve / Deny an approval, Open the
//! terminal. Hovering a capsule holds it and shows the same buttons. Right-click: Hide badge,
//! Notification settings…, Open midna. Drag it anywhere; it snaps to the nearest corner of that
//! screen (`notify.badge.corner`). `notify.badge`: `background` (only while midna isn't the
//! app in front; in front, the in-app cards show instead), `always`, or `off`.
//!
//! The window is a non-activating panel (GPUI `PopUp`: every Space, over full-screen apps)
//! that never takes focus, so clicking it leaves the app you're in in front. It's a fixed
//! transparent square anchored at its corner; a timer lets clicks through everywhere but the
//! badge, the capsule and the stack (`zones`), shows and hides it, and tells a drag from a
//! click. The home main window feeds it (`sync`, `push`); while it takes notifications, they
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
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The window: room for the stack below (or above) the badge and a capsule beside it.
const W: f32 = 440.;
const H: f32 = 400.;
/// The badge's inset from the window's (and so the screen's) edges.
const PAD: f32 = 14.;
const SIZE: f32 = 44.;
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
/// A press that moves the window less than this is a click, not a drag.
const DRAG_SLOP: f64 = 3.;

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

/// Something waiting on you: a needs-you item, or a notification of a kind that stays.
#[derive(Clone)]
struct Waiting {
    /// The needs-you id, or `n<seq>` for a notification.
    id: String,
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
    category: String,
    name: String,
    text: String,
    color: String,
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

pub struct Badge {
    backend: Arc<dyn Backend>,
    mode: Mode,
    corner: Corner,
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
    /// The last pulse: its id and color.
    pulse: (u64, String),
    /// The open stack, at this item.
    panel: Option<usize>,
    menu: bool,
    /// The pointer is over a zone.
    hover: Option<&'static str>,
    /// A press on the badge: the window's origin then (Cocoa), to tell a drag from a click.
    pressing: Option<(f64, f64)>,
    shown: bool,
    zones: Zones,
    /// `MIDNA_DEBUG_BADGE`: open the stack (`panel`) or the menu (`menu`) once there's something.
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
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(point(px(0.), px(0.)), size(px(W), px(H))))),
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
            ns.setHasShadow(false);
            ns.setIgnoresMouseEvents(true);
            place(&ns, Corner::TopRight, false);
        }
        cx.new(|cx| Badge::new(backend, window, cx))
    });
    cx.set_global(BadgeWindow(h.ok()));
}

/// The badge takes notifications now: on, and midna isn't in front (or `always`).
pub fn takes(cx: &App) -> bool {
    let Some(h) = handle(cx) else { return false };
    h.read(cx).is_ok_and(|b| match b.mode {
        Mode::Off => false,
        Mode::Always => true,
        Mode::Background => !crate::sounds::app_active(),
    })
}

/// A notification the home window got while the badge takes them (`app.rs` `on_notification`).
pub fn push(m: &crate::app::MainWindow, seq: u64, session: Option<String>, p: Posted, cx: &mut App) {
    let Some(h) = handle(cx) else { return };
    let name = super::notifications::source(m, session.as_deref(), None);
    let need = p.needs_you_id.clone().and_then(|id| m.needs.iter().find(|n| n.id == id).cloned());
    let _ = h.update(cx, |b, _, cx| b.add(seq, session, p, name, need, cx));
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
    let colors: HashMap<String, String> =
        m.settings.iter().filter_map(|(k, v)| Some((k.strip_prefix("notify.color.")?.to_string(), v.as_str()?.to_string()))).collect();
    let needs: Vec<Waiting> = m
        .needs
        .iter()
        .map(|n| {
            let category = category_of(n.kind).to_string();
            let command = n.approval.as_ref().map(|a| a.action.value.clone()).filter(|v| !v.is_empty());
            let title = match n.question.as_ref().filter(|q| !q.text.trim().is_empty()) {
                Some(q) if !q.header.trim().is_empty() => q.header.trim().to_string(),
                Some(_) => "Asked a question".to_string(),
                None => capitalize(n.title.trim()),
            };
            let detail = n.question.as_ref().map(|q| q.text.trim().to_string()).or_else(|| Some(n.detail.trim().to_string())).filter(|d| !d.is_empty() && Some(d) != command.as_ref());
            Waiting {
                id: n.id.clone(),
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
            }
        })
        .collect();
    let _ = h.update(cx, |b, window, cx| {
        let moved = b.corner != corner;
        b.mode = mode;
        b.corner = corner;
        b.colors = colors;
        // A list fetched just before an item came in doesn't drop it (it'd count down, then up).
        let fresh: Vec<Waiting> = b.needs.iter().filter(|w| b.pending.iter().any(|(id, at)| *id == w.id && at.elapsed() < PENDING) && !needs.iter().any(|n| n.id == w.id)).cloned().collect();
        b.pending.retain(|(_, at)| at.elapsed() < PENDING);
        b.needs = needs;
        b.needs.extend(fresh);
        b.recount(cx);
        if moved && let Some(ns) = super::twilight::ns_window(window) {
            place(&ns, corner, true);
        }
        cx.notify();
    });
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
fn back_out(x: f32) -> f32 {
    let (c1, c3) = (1.70158, 2.70158);
    1. + c3 * (x - 1.).powi(3) + c1 * (x - 1.).powi(2)
}

fn ease_in(x: f32) -> f32 {
    x * x * x
}

// ------------------------------------------------------------------ the native window

/// Put the window at `corner` of the screen it's on (the main screen at first), inside its
/// visible frame (clear of the menu bar and the Dock).
fn place(ns: &objc2_app_kit::NSWindow, corner: Corner, animate: bool) {
    use objc2_foundation::{NSPoint, NSRect};
    let Some(mtm) = objc2::MainThreadMarker::new() else { return };
    let Some(screen) = ns.screen().or_else(|| objc2_app_kit::NSScreen::screens(mtm).firstObject()) else { return };
    let vf = screen.visibleFrame();
    let f = ns.frame();
    let x = if corner.right() { vf.origin.x + vf.size.width - f.size.width } else { vf.origin.x };
    let y = if corner.top() { vf.origin.y + vf.size.height - f.size.height } else { vf.origin.y };
    ns.setFrame_display_animate(NSRect::new(NSPoint::new(x, y), f.size), true, animate);
}

/// The corner of its screen the badge is nearest, where the window is now.
fn nearest(ns: &objc2_app_kit::NSWindow, corner: Corner) -> Corner {
    let Some(screen) = ns.screen() else { return corner };
    let (vf, f) = (screen.visibleFrame(), ns.frame());
    // The badge's center, in screen coordinates (y up).
    let bx = if corner.right() { f.origin.x + f.size.width - (PAD + SIZE / 2.) as f64 } else { f.origin.x + (PAD + SIZE / 2.) as f64 };
    let by = if corner.top() { f.origin.y + f.size.height - (PAD + SIZE / 2.) as f64 } else { f.origin.y + (PAD + SIZE / 2.) as f64 };
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
                let fast = this.update(cx, |b, _| b.shown).unwrap_or(false);
                cx.background_executor().timer(Duration::from_millis(if fast { 33 } else { 250 })).await;
                if this.update_in(cx, |b, window, cx| b.tick(window, cx)).is_err() {
                    break;
                }
            }
        });
        cx.observe_global::<Theme>(|_, cx| cx.notify()).detach();
        // Dev (screenshots): `MIDNA_DEBUG_BADGE=panel|menu` opens the stack or the menu.
        let debug = crate::dev::var("MIDNA_DEBUG_BADGE").ok();
        Badge {
            debug,
            backend,
            mode: Mode::Background,
            corner: Corner::TopRight,
            colors: HashMap::new(),
            needs: vec![],
            held: vec![],
            pending: vec![],
            lines: VecDeque::new(),
            serial: 0,
            count: 0,
            from: 0,
            roll: 0,
            pulse: (0, String::new()),
            panel: None,
            menu: false,
            hover: None,
            pressing: None,
            shown: false,
            zones: Zones::default(),
            _ticker: ticker,
        }
    }

    fn waiting(&self) -> Vec<&Waiting> {
        self.needs.iter().chain(self.held.iter()).collect()
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
        let n = self.needs.len() + self.held.len();
        if n > 0 && let Some(d) = self.debug.take() {
            self.panel = (d == "panel").then_some(0);
            self.menu = d == "menu";
        }
        if n != self.count {
            self.from = self.count;
            self.count = n;
            self.roll += 1;
        }
        if let Some(i) = self.panel {
            match n {
                0 => self.panel = None,
                n if i >= n => self.panel = Some(n - 1),
                _ => {}
            }
        }
        cx.notify();
    }

    fn add(&mut self, seq: u64, session: Option<String>, p: Posted, name: String, need: Option<NeedsYou>, cx: &mut Context<Self>) {
        let stay = crate::ui::toast::stay_of(&p);
        let color = p.color.clone().unwrap_or_else(|| default_color(&p.category).into());
        let mut waiting = None;
        if let Some(id) = p.needs_you_id.clone() {
            if !self.needs.iter().any(|w| w.id == id) {
                // The notification can beat the needs-you refresh: count it now (the next sync
                // fills it in, or drops it if it's already answered).
                let first = p.body.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_string();
                self.pending.push((id.clone(), Instant::now()));
                self.needs.push(Waiting {
                    id: id.clone(),
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
                });
            }
            waiting = Some(id);
        } else if stay.is_none() {
            // A kind that stays: counted until you dismiss it (one per terminal and kind).
            self.held.retain(|w| !(w.session == session && w.category == p.category));
            let mut lines = p.body.lines().map(str::trim).filter(|l| !l.is_empty());
            let id = format!("n{seq}");
            self.held.push(Waiting {
                id: id.clone(),
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
            });
            waiting = Some(id);
        }
        let text = p.body.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or(&p.title).to_string();
        self.serial += 1;
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
            category: p.category.clone(),
            name,
            text,
            color: color.clone(),
            stay,
            until: now + stay.unwrap_or(LINE_SHOW),
            leaving: None,
        });
        self.pulse = (self.serial, color);
        self.recount(cx);
    }

    /// Drop a held notification (the stack's Dismiss).
    fn dismiss(&mut self, id: &str, cx: &mut Context<Self>) {
        self.held.retain(|w| w.id != id);
        for l in self.lines.iter_mut().filter(|l| l.waiting.as_deref() == Some(id)) {
            l.leaving.get_or_insert(Instant::now());
        }
        self.recount(cx);
    }

    fn approve(&mut self, need: String, deny: bool, cx: &mut Context<Self>) {
        self.needs.retain(|w| w.id != need);
        self.recount(cx);
        crate::windows::with_active(cx, |m, _, cx| m.resolve(need, if deny { Resolution::Deny } else { Resolution::Approve { scope: ApprovalScope::Once } }, cx));
    }

    /// Bring midna forward on the item's terminal (as a clicked notification does).
    fn open(&mut self, session: Option<String>, need: Option<String>, held: Option<String>, cx: &mut Context<Self>) {
        self.panel = None;
        self.menu = false;
        if let Some(id) = held {
            self.dismiss(&id, cx);
        }
        cx.activate(true);
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

    fn hide_badge(&mut self, cx: &mut Context<Self>) {
        self.menu = false;
        self.panel = None;
        self.mode = Mode::Off;
        let backend = self.backend.clone();
        cx.background_executor().spawn(async move { backend.call("settings.set", json!({ "key": "notify.badge", "value": "off" })) }).detach();
        cx.notify();
    }

    fn save_corner(&mut self, corner: Corner, cx: &mut Context<Self>) {
        self.corner = corner;
        let backend = self.backend.clone();
        cx.background_executor().spawn(async move { backend.call("settings.set", json!({ "key": "notify.badge.corner", "value": corner.key() })) }).detach();
    }

    /// Show or hide the window, let clicks through outside the zones, age the capsules, and
    /// tell a drag from a click.
    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ns) = super::twilight::ns_window(window) else { return };
        let now = Instant::now();
        let mut changed = false;

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

        let enabled = match self.mode {
            Mode::Off => false,
            Mode::Always => true,
            Mode::Background => !crate::sounds::app_active(),
        };
        if !enabled && (self.panel.is_some() || self.menu) {
            self.panel = None;
            self.menu = false;
            changed = true;
        }
        let want = enabled && (self.count > 0 || !self.lines.is_empty() || self.panel.is_some() || self.menu);
        if want != self.shown {
            self.shown = want;
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
                self.hover = hover;
                changed = true;
            }
            ns.setIgnoresMouseEvents(hover.is_none() && self.pressing.is_none());

            // A press on the badge ended: a drag snaps to the nearest corner, a click opens the stack.
            if let Some((x0, y0)) = self.pressing
                && objc2_app_kit::NSEvent::pressedMouseButtons() & 1 == 0
            {
                self.pressing = None;
                let moved = (f.origin.x - x0).abs() > DRAG_SLOP || (f.origin.y - y0).abs() > DRAG_SLOP;
                if moved {
                    let corner = nearest(&ns, self.corner);
                    place(&ns, corner, true);
                    if corner != self.corner {
                        self.save_corner(corner, cx);
                    }
                } else if self.menu {
                    self.menu = false;
                } else {
                    self.panel = match self.panel {
                        Some(_) => None,
                        None if self.count > 0 => Some(0),
                        None => None,
                    };
                }
                changed = true;
            }
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

    /// The badge: the count (or a check while a passing line shows), its border split by the
    /// colors of what's waiting.
    fn badge(&self, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let reduce = super::queue::reduce_motion();
        let passing = self.lines.front().filter(|l| l.leaving.is_none() && l.waiting.is_none());
        // The border: up to two colors of what's waiting (most first), else the line's.
        let mut tally: Vec<(String, usize)> = vec![];
        for w in self.waiting() {
            match tally.iter_mut().find(|(c, _)| *c == w.color) {
                Some((_, n)) => *n += 1,
                None => tally.push((w.color.clone(), 1)),
            }
        }
        tally.sort_by(|a, b| b.1.cmp(&a.1));
        let ring: Vec<Hsla> = match (tally.as_slice(), passing) {
            ([], Some(l)) => vec![self.color(t, &l.color)],
            ([], None) => vec![t.line],
            (tally, _) => tally.iter().take(2).map(|(c, _)| self.color(t, c)).collect(),
        };
        let border = match ring.as_slice() {
            [a, b, ..] => div().bg(linear_gradient(135., linear_color_stop(*b, 0.25), linear_color_stop(*a, 0.6))),
            [a] => div().bg(*a),
            [] => div().bg(t.line),
        };

        // The number rolls: up, the new one rises from below; down, it drops in from above.
        let (from, to) = (self.from, self.count);
        let digits = |n: usize| div().h(px(20.)).flex().items_center().justify_center().child(if n > 99 { "99+".to_string() } else { n.to_string() });
        let up = to > from;
        let column = div().absolute().left_0().right_0().flex().flex_col().map(|d| if up { d.child(digits(from)).child(digits(to)) } else { d.child(digits(to)).child(digits(from)) });
        let number = div()
            .relative()
            .w(px(30.))
            .h(px(20.))
            .overflow_hidden()
            .text_size(px(if to > 9 { 13. } else { 15. }))
            .font_weight(FontWeight::BOLD)
            .text_color(t.fg)
            .child(if reduce || from == to {
                div().absolute().top_0().left_0().right_0().child(digits(to)).into_any_element()
            } else {
                column
                    .with_animation(SharedString::from(format!("roll-{}", self.roll)), Animation::new(ROLL).with_easing(back_out), move |el, d| {
                        el.top(px(if up { -20. * d } else { -20. + 20. * d }))
                    })
                    .into_any_element()
            });

        let inner = div()
            .absolute()
            .top(px(2.))
            .left(px(2.))
            .size(px(SIZE - 4.))
            .rounded(px(12.))
            .bg(t.panel)
            .flex()
            .items_center()
            .justify_center()
            .map(|d| match passing {
                Some(l) if self.count == 0 => d.child(Icon::Check.el(18., self.color(t, &l.color))),
                Some(l) => d.child(Icon::Check.el(18., self.color(t, &l.color))).child(
                    div()
                        .absolute()
                        .right(px(-6.))
                        .bottom(px(-6.))
                        .min_w(px(18.))
                        .h(px(18.))
                        .px(px(5.))
                        .rounded(px(9.))
                        .bg(ring.first().copied().unwrap_or(t.need))
                        .text_color(t.badge_fg)
                        .text_size(px(11.))
                        .font_weight(FontWeight::BOLD)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(self.count.to_string()),
                ),
                None => d.child(number),
            });

        let pulse = self.color(t, &self.pulse.1);
        let lift = self.hover == Some("badge");
        let el = div()
            .id("badge")
            .relative()
            .size(px(SIZE))
            .rounded(px(14.))
            .cursor_pointer()
            .shadow(vec![BoxShadow { color: hsla(0., 0., 0., if lift { 0.5 } else { 0.4 }), offset: point(px(0.), px(10.)), blur_radius: px(26.), spread_radius: px(0.), inset: false }])
            .child(border.absolute().inset_0().rounded(px(14.)))
            .child(inner)
            .child(measure(self.zones.badge.clone()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|b, _, window, cx| {
                    b.menu = false;
                    if let Some(ns) = super::twilight::ns_window(window) {
                        let f = ns.frame();
                        b.pressing = Some((f.origin.x, f.origin.y));
                    }
                    window.start_window_move();
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|b, _, _, cx| {
                    b.menu = !b.menu;
                    b.panel = None;
                    cx.notify();
                }),
            );
        if reduce || self.pulse.0 == 0 {
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

    /// The capsule beside the badge: springs out, holds, slides back in.
    fn line(&self, t: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let l = self.lines.front()?;
        let reduce = super::queue::reduce_motion();
        let color = self.color(t, &l.color);
        let right = self.corner.right();
        let extra = l.burst;
        let hover = self.hover == Some("line");
        let need = l.need.clone().filter(|id| self.needs.iter().any(|w| w.id == *id && w.approval));
        let (session, need_id, waiting) = (l.session.clone(), l.need.clone(), l.waiting.clone());
        let held = waiting.filter(|w| self.held.iter().any(|h| h.id == *w));
        let mut row = div()
            .id(SharedString::from(format!("line-{}", l.serial)))
            .relative()
            .h(px(36.))
            .max_w(px(W - SIZE - PAD * 2. - 8.))
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
            .on_click(cx.listener(move |b, _, _, cx| b.open(session.clone(), need_id.clone(), held.clone(), cx)));
        // A passing kind's tint clears from left to right over its time.
        if let Some(stay) = l.stay.filter(|_| !hover && !reduce) {
            let tint = div().absolute().top_0().bottom_0().left_0().right_0().bg(color.opacity(0.16));
            row = row.child(tint.with_animation(SharedString::from(format!("fill-{}", l.serial)), Animation::new(stay), |el, d| el.left(relative(d))));
        }
        row = row
            .child(measure(self.zones.line.clone()))
            .child(icon_of(&l.category).el(14., color))
            .child(div().flex_none().font_weight(FontWeight::BOLD).text_color(t.fg).child(l.name.clone()))
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

    /// The stack: one waiting item at a time with its buttons, 1 of N.
    fn panel(&self, t: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let i = self.panel?;
        let all = self.waiting();
        let w = (*all.get(i)?).clone();
        let n = all.len();
        let color = self.color(t, &w.color);
        let nav = |id: &'static str, label: &'static str, to: usize, on: bool| {
            div()
                .id(id)
                .size(px(22.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .text_color(if on { t.fg } else { t.line })
                .when(on, |d| d.cursor_pointer().hover(|s| s.bg(t.raised)))
                .on_click(cx.listener(move |b, _, _, cx| {
                    if on {
                        b.panel = Some(to);
                        cx.notify();
                    }
                }))
                .child(label)
        };
        let mut buttons = div().flex().gap(px(8.)).pt(px(4.));
        if let Some(id) = w.need.clone().filter(|_| w.approval) {
            let (a, d) = (id.clone(), id);
            buttons = buttons
                .child(button(t, "panel-approve", "Approve", true).flex_1().on_click(cx.listener(move |b, _, _, cx| b.approve(a.clone(), false, cx))))
                .child(button(t, "panel-deny", "Deny", false).on_click(cx.listener(move |b, _, _, cx| b.approve(d.clone(), true, cx))));
        }
        let held = w.need.is_none().then(|| w.id.clone());
        let (s, need) = (w.session.clone(), w.need.clone());
        let h = held.clone();
        buttons = buttons.child(button(t, "panel-open", "Open in midna", w.need.is_none() || !w.approval).when(!w.approval, |d| d.flex_1()).on_click(cx.listener(move |b, _, _, cx| {
            b.open(s.clone(), need.clone(), h.clone(), cx)
        })));
        if let Some(id) = held {
            buttons = buttons.child(button(t, "panel-dismiss", "Dismiss", false).on_click(cx.listener(move |b, _, _, cx| b.dismiss(&id, cx))));
        }
        let card = div()
            .id("panel")
            .relative()
            .w(px(360.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .p(px(14.))
            .rounded(px(14.))
            .bg(t.raised)
            .border_1()
            .border_color(t.line)
            .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.45), offset: point(px(0.), px(18.)), blur_radius: px(48.), spread_radius: px(0.), inset: false }])
            .text_color(t.fg)
            .child(measure(self.zones.panel.clone()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().size(px(8.)).rounded_full().bg(color))
                    .child(div().font_weight(FontWeight::BOLD).truncate().child(w.name.clone()))
                    .when(!w.label.is_empty(), |d| d.child(div().text_size(px(12.)).text_color(t.dim).child(w.label.clone())))
                    .child(div().flex_1())
                    .child(div().text_size(px(12.)).text_color(t.dim).child(format!("{} of {n}", i + 1)))
                    .child(nav("panel-prev", "‹", i.saturating_sub(1), i > 0))
                    .child(nav("panel-next", "›", i + 1, i + 1 < n)),
            )
            .child(div().text_size(px(14.)).font_weight(FontWeight::BOLD).child(w.title.clone()))
            .children(w.command.clone().map(|c| div().px(px(10.)).py(px(8.)).rounded(px(7.)).bg(t.term).font_family(t.mono_font.clone()).text_size(px(12.)).truncate().child(c)))
            .children(w.detail.clone().map(|d| div().max_h(px(120.)).overflow_hidden().text_size(px(13.)).line_height(px(19.)).text_color(t.dim).child(d)))
            .child(buttons);
        Some(card.into_any_element())
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
                .child(item("menu-open", "Open midna").on_click(cx.listener(|b, _, _, cx| b.open(None, None, None, cx))))
                .child(item("menu-settings", "Notification settings…").on_click(cx.listener(|b, _, _, cx| {
                    b.menu = false;
                    cx.activate(true);
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<Theme>().clone();
        self.zones.clear();
        let (top, right) = (self.corner.top(), self.corner.right());
        let edge = |d: Div, inset: f32| {
            let d = if top { d.top(px(inset)) } else { d.bottom(px(inset)) };
            if right { d.right(px(PAD)) } else { d.left(px(PAD)) }
        };
        let mut root = div().size_full().relative().font_family(t.ui_font.clone()).text_color(t.fg);
        if !self.shown {
            return root;
        }
        root = root.child(edge(div().absolute(), PAD).child(self.badge(&t, cx)));
        if let Some(line) = self.line(&t, cx) {
            let side = PAD + SIZE + 8.;
            let at = div().absolute().flex().map(|d| if top { d.top(px(PAD + 4.)) } else { d.bottom(px(PAD + 4.)) });
            let at = if right { at.right(px(side)).justify_end() } else { at.left(px(side)) };
            root = root.child(at.child(line));
        }
        let below = PAD + SIZE + 10.;
        if let Some(menu) = self.menu(&t, cx) {
            root = root.child(edge(div().absolute(), below).child(menu));
        } else if let Some(panel) = self.panel(&t, cx) {
            root = root.child(edge(div().absolute(), below).child(panel));
        }
        root
    }
}
