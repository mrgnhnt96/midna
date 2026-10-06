//! midna's opening, on every launch: the window opens see-through, with no shadow and no traffic
//! lights, and Twilight Tiles brings it in over the desktop. Then the window turns opaque and its
//! chrome comes back.
//!
//! - Setup open: the setup screen plays the design's opening itself (`setup_screen.rs`).
//! - Otherwise, over the main window: the real app draws once, masked (a Core Animation mask on
//!   its layer) to the cells the opening has reached, and a click-through window on top draws the
//!   tiles: each one runs the design's ripple to its orange diamond, where midna shows under it,
//!   then keeps turning and shrinks away. The teal wave crosses on top.
//! - The window gets its shadow, corners and traffic lights back (opaque, unmasked) the moment
//!   every cell shows it, while the tiles are still moving over the edges, so the change hides in
//!   the motion; the tiles then finish on top. Doing it at the very end showed as a pause, then a
//!   one-frame pop of the shadow and corners.
//! - Reduce motion (macOS Accessibility): still teal squares light up from the centre, each cell
//!   showing midna once lit, then all fade out together. 1.5s; no scaling, turning or wave.
//!
//! Dev: `MIDNA_INTRO=0` skips it, `MIDNA_INTRO_SPEED=0.25` slows it, `MIDNA_REDUCE_MOTION=1|0`
//! overrides the system setting, `MIDNA_DEBUG_SCREEN=twilight` replays it.
use crate::app::MainWindow;
use crate::icons::Icon;
use crate::theme::Theme;
use midna_proto::themes::ThemeDef;
use crate::ui::setup_screen::{self as ss, CELL, EXPOSE_AT, Model, OPEN_ORIGIN, PER_CELL_MS, RIPPLE_MS, rgb};
use gpui_kit::prelude::*;
use gpui_kit::*;
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Off,
    /// The opening is playing.
    Intro,
}

/// Reduce motion: the whole opening, the last cell lit by, and the shared fade.
const REDUCED_MS: f32 = 1500.;
const REDUCED_LIT_BY: f32 = 925.;
const REDUCED_RISE: f32 = 400.;
const REDUCED_FADE_AT: f32 = 950.;
const REDUCED_FADE: f32 = 550.;
const REDUCED_PER_CELL: f32 = 30.;
const REDUCED_ALPHA: f32 = 0.85;

/// Only the first main window of a launch plays it.
pub fn wanted() -> bool {
    static PLAYED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let want = std::env::var("MIDNA_INTRO").map(|v| v != "0").unwrap_or(true) && std::env::var("MIDNA_SNAPSHOT").is_err();
    want && !PLAYED.swap(true, std::sync::atomic::Ordering::SeqCst)
}

/// macOS Accessibility ▸ Display ▸ Reduce motion (`MIDNA_REDUCE_MOTION=1|0` overrides).
pub fn reduce_motion() -> bool {
    if let Ok(v) = std::env::var("MIDNA_REDUCE_MOTION") {
        return v != "0";
    }
    use objc2::runtime::{AnyObject, Bool};
    use objc2::{class, msg_send};
    unsafe {
        let ws: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        if ws.is_null() {
            return false;
        }
        let on: Bool = msg_send![ws, accessibilityDisplayShouldReduceMotion];
        on.as_bool()
    }
}

/// Playback rate of the opening (dev: `MIDNA_INTRO_SPEED`).
pub fn speed() -> f32 {
    std::env::var("MIDNA_INTRO_SPEED").ok().and_then(|v| v.parse::<f32>().ok()).filter(|v| *v > 0.).unwrap_or(1.)
}

/// The opening is playing over the setup screen (which draws it).
pub fn over_setup(m: &MainWindow) -> bool {
    m.twilight_phase == Phase::Intro && m.twilight_setup
}

/// How long this opening runs, in design time (ms at 1×).
pub fn design_ms(m: &MainWindow) -> f32 {
    m.twilight_total
}

/// How long this opening runs, in real time.
pub fn length(m: &MainWindow) -> Duration {
    Duration::from_secs_f32(m.twilight_total / 1000. / speed())
}

/// One play of the opening, shared by the main window and the tile window. Time starts at the
/// main window's first drawn frame (it is created well before it shows).
#[derive(Default)]
pub struct Clock {
    started: Cell<Option<Instant>>,
    /// The main window is still masked to the revealed cells.
    masked: Cell<bool>,
}

impl Clock {
    /// Design ms since the first frame (None: not drawn yet).
    fn ms(&self) -> Option<f32> {
        self.started.get().map(|t| t.elapsed().as_secs_f32() * 1000. * speed())
    }
}

/// The farthest cell from the opening's origin, in cells.
fn farthest(cols: usize, rows: usize) -> f32 {
    let (ox, oy) = OPEN_ORIGIN;
    [(0., 0.), (cols as f32 - 1., 0.), (0., rows as f32 - 1.), (cols as f32 - 1., rows as f32 - 1.)].iter().map(|(x, y)| (x - ox).hypot(y - oy)).fold(0., f32::max)
}

fn dist(c: usize, r: usize) -> f32 {
    (c as f32 - OPEN_ORIGIN.0).hypot(r as f32 - OPEN_ORIGIN.1)
}

/// Reduce motion at `ms`: for each cell, (lit: showing the window, the teal square's alpha).
/// The squares light from the centre, the last by `REDUCED_LIT_BY` (faster in a big window).
pub fn reduced(cols: usize, rows: usize, ms: f32) -> impl Fn(usize, usize) -> (bool, f32) {
    let per = REDUCED_PER_CELL.min((REDUCED_LIT_BY - REDUCED_RISE) / farthest(cols, rows).max(1.));
    let ease = |u: f32| ss::bezier(0.25, 0.1, 0.25, 1., u.clamp(0., 1.));
    let out = 1. - ease((ms - REDUCED_FADE_AT) / REDUCED_FADE);
    move |c, r| {
        let rise = (ms - dist(c, r) * per) / REDUCED_RISE;
        (rise >= 1., if rise > 0. { REDUCED_ALPHA * ease(rise) * out } else { 0. })
    }
}

/// Per row, the run of columns where `shown` (the opening's fronts are discs, so one run a row).
pub fn runs(cols: usize, rows: usize, shown: impl Fn(usize, usize) -> bool) -> Vec<(usize, usize, usize)> {
    (0..rows)
        .filter_map(|r| {
            let first = (0..cols).find(|&c| shown(c, r))?;
            let last = (0..cols).rev().find(|&c| shown(c, r))?;
            Some((r, first, last))
        })
        .collect()
}

/// Reduce motion's still teal squares.
pub fn squares(md: &Model, cols: usize, rows: usize, lit: &impl Fn(usize, usize) -> (bool, f32)) -> Div {
    let mut el = div().absolute().inset_0();
    for r in 0..rows {
        for c in 0..cols {
            let a = lit(c, r).1;
            if a > 0.002 {
                let (x, y) = (c as f32 * CELL, r as f32 * CELL);
                el = el.child(div().absolute().left(md.u(x + 1.)).top(md.u(y + 1.)).size(md.u(CELL - 2.)).rounded(md.u(2.)).bg(rgb(md.pal.glow, a)));
            }
        }
    }
    el
}

/// Over the main window, a tile `p` through its ripple: the design's keyframes to the orange
/// diamond, then it keeps turning and shrinks away (scale, rotation°, colour, alpha).
fn leaving_at(p: f32, md: &Model) -> (f32, f32, [f32; 3], f32) {
    const STOPS: [f32; 4] = [0., 0.16, EXPOSE_AT, 1.];
    let scale = [1., 0.3, 0.62, 0.];
    let rot = [0., 45., 45., 90.];
    let col = [md.pal.tile, md.pal.glow, md.pal.ember, md.pal.ember];
    let alpha = [1., 1., 1., 0.];
    let (i, k) = ss::segment(&STOPS, p, |u| ss::bezier(0.2, 0.7, 0.2, 1., u));
    let mix = |a: f32, b: f32| a + (b - a) * k;
    (mix(scale[i], scale[i + 1]), mix(rot[i], rot[i + 1]), [0, 1, 2].map(|j| mix(col[i][j], col[i + 1][j]).round()), mix(alpha[i], alpha[i + 1]))
}

/// What the overlay window draws at `ms`.
fn tiles(md: &Model, ms: f32, reduced_motion: bool) -> Div {
    let (cols, rows) = md.cells(CELL, CELL);
    if reduced_motion {
        return squares(md, cols, rows, &reduced(cols, rows, ms));
    }
    let mut el = div().absolute().inset_0();
    for r in 0..rows {
        for c in 0..cols {
            let p = (ms - dist(c, r) * PER_CELL_MS) / RIPPLE_MS;
            if p <= 0. || p >= 1. {
                continue;
            }
            let (sc, rot, col, a) = leaving_at(p, md);
            let size = (CELL - 2.) * sc;
            if size <= 0.05 || a <= 0.002 {
                continue;
            }
            let (x, y) = (c as f32 * CELL + (CELL - size) / 2., r as f32 * CELL + (CELL - size) / 2.);
            el = el.child(div().absolute().left(md.u(x)).top(md.u(y)).child(Icon::Tile.el(size * md.s, rgb(col, a)).with_transformation(Transformation::rotate(radians(rot.to_radians())))));
        }
    }
    el.child(ss::wave(md, ms))
}

/// The cells showing the main window at `ms`, as runs per row.
fn revealed(cols: usize, rows: usize, ms: f32, reduced_motion: bool) -> Vec<(usize, usize, usize)> {
    if reduced_motion {
        let lit = reduced(cols, rows, ms);
        return runs(cols, rows, |c, r| lit(c, r).0);
    }
    runs(cols, rows, |c, r| (ms - dist(c, r) * PER_CELL_MS) / RIPPLE_MS >= EXPOSE_AT)
}

/// The click-through window over the main window that draws the tiles. It also keeps the main
/// window's mask in step, so the app doesn't redraw every frame of the opening.
struct Overlay {
    clock: Rc<Clock>,
    reduced: bool,
    /// The theme the tiles wear (`setup_screen::world_palette`).
    def: ThemeDef,
    main: AnyWindowHandle,
    /// The main window's NSView (only touched while `main` is open).
    main_view: usize,
    /// The mask last set: only rebuilt when the revealed cells change.
    last_runs: Vec<(usize, usize, usize)>,
}

impl Render for Overlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.request_animation_frame();
        let size = window.viewport_size();
        let md = Model::bare(&self.def, f32::from(size.width), f32::from(size.height));
        let Some(ms) = self.clock.ms() else { return div().size_full() };
        if self.clock.masked.get() && cx.windows().contains(&self.main) {
            let (cols, rows) = md.cells(CELL, CELL);
            let runs = revealed(cols, rows, ms, self.reduced);
            if runs != self.last_runs {
                set_mask(self.main_view as *mut objc2::runtime::AnyObject, f64::from(size.height), &runs, CELL * md.s);
                self.last_runs = runs;
            }
        }
        div().size_full().child(tiles(&md, ms, self.reduced))
    }
}

/// Start the opening. Call from `MainWindow::new` (or to replay).
pub fn start(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    m.twilight_seq += 1;
    m.twilight_phase = Phase::Intro;
    m.twilight_reduced = reduce_motion();
    m.twilight_setup = ss::active(m);
    m.twilight_clock = Rc::new(Clock::default());
    let size = window.viewport_size();
    let md = Model::bare(&cx.global::<Theme>().def, f32::from(size.width), f32::from(size.height));
    let (cols, rows) = md.cells(CELL, CELL);
    m.twilight_total = if m.twilight_reduced {
        REDUCED_MS
    } else if m.twilight_setup {
        ss::INTRO_MS
    } else {
        // The last tile gone, or the wave across, whichever is later.
        let (wc, wr) = md.cells(96., 100.);
        (farthest(cols, rows) * PER_CELL_MS + RIPPLE_MS).max(120. + (wc + wr) as f32 * 52. + 520.)
    };
    // Every cell shows the window from here.
    m.twilight_revealed = if m.twilight_reduced { REDUCED_LIT_BY } else { farthest(cols, rows) * PER_CELL_MS + EXPOSE_AT * RIPPLE_MS };
    window.set_background_appearance(WindowBackgroundAppearance::Transparent);
    chrome(window, false);
    m.twilight_lights = false;
    m.twilight_masked = true;
    m.twilight_clock.masked.set(true);
    if !m.twilight_setup {
        if let Some(view) = ns_view_ptr(window) {
            set_mask(view, f64::from(f32::from(size.height)), &[], 0.);
        }
        m.twilight_overlay = open_overlay(m, window, cx);
    }
    cx.notify();
}

/// The main window's first drawn frame of an opening: start the clock, and the timers that
/// restore the window and end the opening. Call from every main-window render.
pub fn first_frame(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    if m.twilight_phase != Phase::Intro || m.twilight_clock.started.get().is_some() {
        return;
    }
    m.twilight_clock.started.set(Some(Instant::now()));
    let seq = m.twilight_seq;
    let (revealed_at, length) = (Duration::from_secs_f32(m.twilight_revealed / 1000. / speed()), length(m));
    cx.spawn_in(window, async move |this, cx| {
        cx.background_executor().timer(revealed_at).await;
        let _ = this.update_in(cx, |m, window, cx| {
            if m.twilight_seq == seq {
                restore(m, window, cx.global::<Theme>().bg);
            }
        });
        cx.background_executor().timer(length.saturating_sub(revealed_at)).await;
        let _ = this.update_in(cx, |m, window, cx| {
            if m.twilight_seq == seq {
                finish(m, window, cx);
            }
        });
    })
    .detach();
}

/// The setup screen has no window controls: hide the traffic lights while it shows (and while
/// the opening plays). Call each main-window frame.
pub fn sync_lights(m: &mut MainWindow, window: &Window) {
    let show = !m.twilight_masked && !ss::active(m);
    if show != m.twilight_lights {
        m.twilight_lights = show;
        traffic_lights(window, show);
    }
}

/// The whole window shows: back to an ordinary window (opaque, shadow, traffic lights).
fn restore(m: &mut MainWindow, window: &Window, bg: Hsla) {
    if !m.twilight_masked {
        return;
    }
    m.twilight_masked = false;
    m.twilight_clock.masked.set(false);
    clear_mask(window);
    window.set_background_appearance(WindowBackgroundAppearance::Opaque);
    native_bg(window, bg);
    chrome(window, true);
    m.twilight_lights = true;
    sync_lights(m, window);
}

fn finish(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    restore(m, window, cx.global::<Theme>().bg);
    m.twilight_phase = Phase::Off;
    // The opening already brought the card in.
    m.onboarding.card_seq_opened = m.onboarding.card_seq;
    if let Some(h) = m.twilight_overlay.take() {
        let _ = h.update(cx, |_, w, _| w.remove_window());
    }
    cx.notify();
}

/// Play it again (dev).
pub fn replay(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    if let Some(h) = m.twilight_overlay.take() {
        let _ = h.update(cx, |_, w, _| w.remove_window());
    }
    start(m, window, cx);
}

fn open_overlay(m: &MainWindow, window: &Window, cx: &mut Context<MainWindow>) -> Option<AnyWindowHandle> {
    let opts = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(window.bounds())),
        titlebar: None,
        kind: WindowKind::PopUp,
        focus: false,
        show: true,
        is_movable: false,
        display_id: window.display(cx).map(|d| d.id()),
        window_background: WindowBackgroundAppearance::Transparent,
        // It never takes focus; unfocused windows are otherwise drawn at a reduced rate.
        inactive_frame_interval: None,
        ..Default::default()
    };
    let (clock, reduced, def, main) = (m.twilight_clock.clone(), m.twilight_reduced, cx.global::<Theme>().def.clone(), window.window_handle());
    let main_view = ns_view_ptr(window)? as usize;
    cx.open_window(opts, |window, cx| {
        click_through(window);
        cx.new(|_| Overlay { clock, reduced, def, main, main_view, last_runs: vec![] })
    })
    .ok()
    .map(|h| h.into())
}

/// The NSWindow's own background: what shows before (or around) GPUI's first paint.
pub fn native_bg(window: &Window, c: Hsla) {
    let Some(w) = ns_window(window) else { return };
    let c = Rgba::from(c);
    let color = objc2_app_kit::NSColor::colorWithSRGBRed_green_blue_alpha(c.r as f64, c.g as f64, c.b as f64, 1.);
    w.setBackgroundColor(Some(&color));
}

pub(crate) fn ns_window(window: &Window) -> Option<objc2::rc::Retained<objc2_app_kit::NSWindow>> {
    use objc2_app_kit::NSView;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return None };
    let view: &NSView = unsafe { h.ns_view.cast::<NSView>().as_ref() };
    view.window()
}

fn ns_view_ptr(window: &Window) -> Option<*mut objc2::runtime::AnyObject> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return None };
    Some(h.ns_view.as_ptr().cast())
}

/// Show or hide the native window's shadow and traffic lights.
fn chrome(window: &Window, visible: bool) {
    let Some(ns) = ns_window(window) else { return };
    ns.setHasShadow(visible);
    traffic_lights(window, visible);
}

fn traffic_lights(window: &Window, visible: bool) {
    use objc2_app_kit::NSWindowButton;
    let Some(ns) = ns_window(window) else { return };
    for b in [NSWindowButton::CloseButton, NSWindowButton::MiniaturizeButton, NSWindowButton::ZoomButton] {
        if let Some(button) = ns.standardWindowButton(b) {
            button.setHidden(!visible);
        }
    }
}

/// The overlay never takes clicks, focus or a shadow.
fn click_through(window: &Window) {
    let Some(ns) = ns_window(window) else { return };
    ns.setIgnoresMouseEvents(true);
    ns.setHasShadow(false);
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}
unsafe impl objc2::Encode for CGPoint {
    const ENCODING: objc2::Encoding = objc2::Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
}
unsafe impl objc2::Encode for CGSize {
    const ENCODING: objc2::Encoding = objc2::Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
}
unsafe impl objc2::Encode for CGRect {
    const ENCODING: objc2::Encoding = objc2::Encoding::Struct("CGRect", &[CGPoint::ENCODING, CGSize::ENCODING]);
}
/// `CGPathRef`.
#[repr(transparent)]
#[derive(Clone, Copy)]
struct CGPath(*const std::ffi::c_void);
unsafe impl objc2::Encode for CGPath {
    const ENCODING: objc2::Encoding = objc2::Encoding::Pointer(&objc2::Encoding::Struct("CGPath", &[]));
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPathCreateMutable() -> *mut std::ffi::c_void;
    fn CGPathAddRect(path: *mut std::ffi::c_void, m: *const std::ffi::c_void, rect: CGRect);
    fn CGPathRelease(path: *const std::ffi::c_void);
}

/// Mask `view`'s layer (`h` points tall) to `runs` (row, first column, last column) of
/// `cell`-point cells. Empty: nothing shows.
fn set_mask(view: *mut objc2::runtime::AnyObject, h: f64, runs: &[(usize, usize, usize)], cell: f32) {
    use objc2::runtime::{AnyObject, Bool};
    use objc2::{class, msg_send};
    unsafe {
        let layer: *mut AnyObject = msg_send![view, layer];
        if layer.is_null() {
            return;
        }
        let _: () = msg_send![class!(CATransaction), begin];
        let _: () = msg_send![class!(CATransaction), setDisableActions: Bool::YES];
        let mut mask: *mut AnyObject = msg_send![layer, mask];
        if mask.is_null() {
            mask = msg_send![class!(CAShapeLayer), layer];
            let _: () = msg_send![layer, setMask: mask];
        }
        let bounds: CGRect = msg_send![layer, bounds];
        let _: () = msg_send![mask, setFrame: bounds];
        let path = CGPathCreateMutable();
        let cell = f64::from(cell);
        for &(r, c0, c1) in runs {
            // The layer's origin is bottom left; 1px of overlap down, so rows don't seam.
            let y = h - (r as f64 + 1.) * cell - 1.;
            CGPathAddRect(path, std::ptr::null(), CGRect { origin: CGPoint { x: c0 as f64 * cell, y }, size: CGSize { width: (c1 - c0 + 1) as f64 * cell, height: cell + 1. } });
        }
        let _: () = msg_send![mask, setPath: CGPath(path)];
        CGPathRelease(path);
        let _: () = msg_send![class!(CATransaction), commit];
    }
}

fn clear_mask(window: &Window) {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    let Some(view) = ns_view_ptr(window) else { return };
    unsafe {
        let layer: *mut AnyObject = msg_send![view, layer];
        if layer.is_null() {
            return;
        }
        let _: () = msg_send![class!(CATransaction), begin];
        let _: () = msg_send![class!(CATransaction), setDisableActions: objc2::runtime::Bool::YES];
        let _: () = msg_send![layer, setMask: std::ptr::null_mut::<AnyObject>()];
        let _: () = msg_send![class!(CATransaction), commit];
    }
}

#[cfg(test)]
mod tests {
    use super::{REDUCED_LIT_BY, reduced, runs};

    #[test]
    fn reduced_lights_every_cell_before_the_fade_and_clears_after() {
        for (cols, rows) in [(30, 19), (60, 40)] {
            let lit = reduced(cols, rows, REDUCED_LIT_BY + 1.);
            assert!((0..rows).all(|r| (0..cols).all(|c| lit(c, r).0)));
            let end = reduced(cols, rows, 1500.);
            assert!((0..rows).all(|r| (0..cols).all(|c| end(c, r).1 < 0.001)));
        }
    }

    #[test]
    fn runs_are_one_span_per_row() {
        let r = runs(5, 3, |c, r| r == 1 && (1..=3).contains(&c));
        assert_eq!(r, vec![(1, 1, 3)]);
    }
}
