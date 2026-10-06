//! Setup's "Twilight Tiles" screen: the whole window while setup is open. A port of the design
//! canvas's `Main.dc.html` (Twilight Tiles) and its timing study (`Timing.dc.html`): the same
//! 1440×900 composition, keyframes, delays and easing, scaled uniformly to the window.
//!
//! On launch (`twilight.rs` makes the window see-through, without chrome) the screen plays the
//! design's opening over the desktop:
//! - the tile grid is there from the first frame; a ripple runs out from cell (15, 9): each tile
//!   shrinks to a teal dot (16%), becomes an orange diamond (38%) and settles (100%);
//! - each cell shows the window only from its tile's orange moment on (the timing study's
//!   "window at the orange diamond"); until then it shows the desktop;
//! - the see-through teal wave sweeps diagonally from the top left;
//! - the card comes in (460ms wait, then 900ms: fade, rise 20px, open from the top).
//!
//! With Reduce motion on, the opening is `opening_reduced` instead (`twilight.rs` has the timing):
//! still teal squares light up from the centre, each cell showing the screen once lit, then all
//! fade together. When setup isn't open, the opening plays over the main window instead
//! (`twilight.rs`), with the tile drawing below.
//!
//! After that it is a normal screen: clicks ripple from the design's fixed origins, and the card
//! replays its entrance on every step change. The steps are `onboarding.rs`: what is done is
//! read from the real state, and the buttons do the real thing.
use crate::app::MainWindow;
use crate::icons::Icon;
use crate::theme::{Theme, ThemeMode};
use crate::ui::onboarding::{self as ob, STEPS, Step};
use gpui_kit::prelude::*;
use gpui_kit::*;
use std::cell::Cell;
use std::time::Duration;

/// The design's canvas; everything is laid out in these units and scaled to the window.
const DESIGN_W: f32 = 1440.;
const DESIGN_H: f32 = 900.;
pub(crate) const CELL: f32 = 48.;
pub(crate) const RIPPLE_MS: f32 = 1450.;
pub(crate) const PER_CELL_MS: f32 = 44.;
/// The opening ripple's origin (design grid cell).
pub(crate) const OPEN_ORIGIN: (f32, f32) = (15., 9.);
/// The opening, from first frame to the card fully in and the grid settled.
pub const INTRO_MS: f32 = 2600.;
/// From this point of its ripple a cell shows the window instead of the desktop.
pub(crate) const EXPOSE_AT: f32 = 0.38;
/// The card's entrance: wait, then open.
const CARD_WAIT_MS: f32 = 460.;
const CARD_OPEN_MS: f32 = 900.;
const CHAKRA: &str = "Chakra Petch";
const PLEX: &str = "IBM Plex Sans";
const MONO: &str = crate::theme::MONO_FONT;

thread_local! {
    /// The card's natural content height (design units), measured as it lays out; the entrance
    /// opens the card to it.
    static CARD_H: Cell<f32> = const { Cell::new(362.) };
}

// ------------------------------------------------------------------ palette

#[derive(Clone, Copy)]
pub(crate) struct Pal {
    bg: [f32; 3],
    pub(crate) tile: [f32; 3],
    pub(crate) glow: [f32; 3],
    pub(crate) ember: [f32; 3],
    panel: [f32; 3],
    line: [f32; 3],
    fg: [f32; 3],
    dim: [f32; 3],
    halo: Hsla,
    wash: Hsla,
}

fn hex(v: u32) -> [f32; 3] {
    [((v >> 16) & 0xff) as f32, ((v >> 8) & 0xff) as f32, (v & 0xff) as f32]
}

pub(crate) fn rgb(c: [f32; 3], a: f32) -> Hsla {
    Rgba { r: c[0] / 255., g: c[1] / 255., b: c[2] / 255., a }.into()
}

/// Twilight Tiles' two palettes (twilight / light world).
fn palette(mode: ThemeMode) -> Pal {
    match mode {
        ThemeMode::Dark => Pal {
            bg: hex(0x06080C),
            tile: hex(0x0D1016),
            glow: hex(0x3FE0C0),
            ember: hex(0xF29A38),
            panel: hex(0x0A0D12),
            line: hex(0x1E2A2B),
            fg: hex(0xE8ECEF),
            dim: hex(0x9AA6AE),
            halo: rgb(hex(0x3FE0C0), 0.06),
            wash: rgb(hex(0x3FE0C0), 0.14),
        },
        ThemeMode::Light => Pal {
            bg: hex(0xEAE3D2),
            tile: hex(0xE0D5BD),
            glow: hex(0x1F8A76),
            ember: hex(0xB5561A),
            panel: hex(0xF7F2E6),
            line: hex(0xCBBD9C),
            fg: hex(0x1D1A14),
            dim: hex(0x574F41),
            halo: rgb(hex(0x1F8A76), 0.08),
            wash: rgb(hex(0x1F8A76), 0.12),
        },
    }
}

// ------------------------------------------------------------------ motion (CSS, exactly)

/// CSS `cubic-bezier(x1, y1, x2, y2)` at `x` (Newton, then bisection), as the browser does it.
pub(crate) fn bezier(x1: f32, y1: f32, x2: f32, y2: f32, x: f32) -> f32 {
    let (cx, cy) = (3. * x1, 3. * y1);
    let (bx, by) = (3. * (x2 - x1) - cx, 3. * (y2 - y1) - cy);
    let (ax, ay) = (1. - cx - bx, 1. - cy - by);
    let sx = |t: f32| ((ax * t + bx) * t + cx) * t;
    let sy = |t: f32| ((ay * t + by) * t + cy) * t;
    let mut t = x;
    for _ in 0..8 {
        let err = sx(t) - x;
        if err.abs() < 1e-5 {
            return sy(t);
        }
        let d = (3. * ax * t + 2. * bx) * t + cx;
        if d.abs() < 1e-6 {
            break;
        }
        t -= err / d;
    }
    let (mut lo, mut hi) = (0., 1.);
    t = x;
    for _ in 0..30 {
        let v = sx(t);
        if (v - x).abs() < 1e-5 {
            break;
        }
        if v < x {
            lo = t;
        } else {
            hi = t;
        }
        t = (lo + hi) / 2.;
    }
    sy(t)
}

/// The keyframe segment `p` is in, and the eased progress through it (CSS applies the timing
/// function per segment).
pub(crate) fn segment(stops: &[f32], p: f32, ease: impl Fn(f32) -> f32) -> (usize, f32) {
    let mut i = 0;
    while i < stops.len() - 2 && p > stops[i + 1] {
        i += 1;
    }
    (i, ease(((p - stops[i]) / (stops[i + 1] - stops[i])).clamp(0., 1.)))
}

/// `twRipA`: (scale, rotation°, colour) of a tile `p` through its ripple.
fn ripple_at(p: f32, pal: &Pal) -> (f32, f32, [f32; 3]) {
    const STOPS: [f32; 4] = [0., 0.16, 0.38, 1.];
    let scale = [1., 0.3, 0.62, 1.];
    let rot = [0., 45., 45., 0.];
    let col = [pal.tile, pal.glow, pal.ember, pal.tile];
    let (i, k) = segment(&STOPS, p, |u| bezier(0.2, 0.7, 0.2, 1., u));
    let mix = |a: f32, b: f32| a + (b - a) * k;
    (mix(scale[i], scale[i + 1]), mix(rot[i], rot[i + 1]), [0, 1, 2].map(|j| mix(col[i][j], col[i + 1][j]).round()))
}

/// `rvA`: (opacity, scale, rotation°) of a wave square `p` through its 520ms.
fn wave_at(p: f32) -> (f32, f32, f32) {
    const STOPS: [f32; 3] = [0., 0.32, 1.];
    let op = [0., 1., 0.];
    let scale = [0.7, 0.97, 0.6];
    let rot = [0., 0., 10.];
    let (i, k) = segment(&STOPS, p, |u| bezier(0.3, 0.7, 0.3, 1., u));
    let mix = |a: f32, b: f32| a + (b - a) * k;
    (mix(op[i], op[i + 1]), mix(scale[i], scale[i + 1]), mix(rot[i], rot[i + 1]))
}

/// `twInA` with CSS's default `ease`: 0 → 1 over 900ms after a 460ms wait.
fn card_in(ms: f32) -> f32 {
    bezier(0.25, 0.1, 0.25, 1., ((ms - CARD_WAIT_MS) / CARD_OPEN_MS).clamp(0., 1.))
}

// ------------------------------------------------------------------ model

#[derive(Clone, Copy, PartialEq)]
enum Stage {
    Intro,
    At(Step),
    Done,
}

#[derive(Clone, Copy)]
enum Action {
    Primary,
    Later,
    Jump(Step),
    Theme(&'static str),
    Replay,
}

struct Copy {
    name: &'static str,
    kicker: &'static str,
    title: &'static str,
    body: &'static str,
    human: bool,
    ask: Option<&'static str>,
    optional: bool,
}

fn copy(s: Step) -> Copy {
    match s {
        Step::Daemon => Copy {
            name: "Background daemon",
            kicker: "JUMP 01 / 05 · macOS",
            title: "Keep shells alive when midna quits",
            body: "Install midnad and your terminals survive quitting midna and updating it. macOS then asks you to allow it under Login Items.",
            human: true,
            ask: None,
            optional: false,
        },
        Step::Notifications => Copy {
            name: "Notifications",
            kicker: "JUMP 02 / 05 · macOS",
            title: "Get a ping when a terminal needs you",
            body: "Approvals, questions and failures. Clicking one jumps straight to that terminal.",
            human: true,
            ask: None,
            optional: true,
        },
        Step::Project => Copy {
            name: "First project",
            kicker: "JUMP 03 / 05",
            title: "Add your first project",
            body: "A project is a folder. It gets ⌘1, a shell, and git status in the header. Dropping a folder anywhere works too.",
            human: false,
            ask: Some("add ~/Development/my-app as a project"),
            optional: false,
        },
        Step::Webhooks => Copy {
            name: "Webhooks",
            kicker: "JUMP 04 / 05",
            title: "Start agents from GitHub and Bitbucket",
            body: "A pull request or a comment can start an agent here. Tailscale Funnel gives midnad a public URL for free, with nothing to host.",
            human: false,
            ask: Some("start Claude on every PR opened in my repo"),
            optional: true,
        },
        Step::Theme => Copy {
            name: "Theme",
            kicker: "JUMP 05 / 05",
            title: "Twilight, or the light world?",
            body: "It applies right away. You can switch whenever you like.",
            human: false,
            ask: None,
            optional: true,
        },
    }
}

fn stage(m: &MainWindow) -> Stage {
    if !m.onboarding.intro_seen {
        return Stage::Intro;
    }
    match ob::current(m) {
        Some(s) => Stage::At(s),
        None => Stage::Done,
    }
}

/// The design's ripple origin for each control (design grid cells).
fn origin(a: Action) -> (f32, f32) {
    match a {
        Action::Primary => (4., 13.),
        Action::Later => (7., 13.),
        Action::Jump(s) => {
            let i = STEPS.iter().position(|x| *x == s).unwrap_or(0) as f32;
            (21., 4. + (i * 1.25).round())
        }
        Action::Theme(_) => (6., 10.),
        Action::Replay => OPEN_ORIGIN,
    }
}

#[derive(Clone)]
struct RailRow {
    step: Step,
    num: String,
    name: &'static str,
    tag: &'static str,
    result: &'static str,
    done: bool,
    later: bool,
    here: bool,
}

/// Everything the screen shows, owned, so the opening can draw it inside an animation.
#[derive(Clone)]
pub(crate) struct Model {
    pub(crate) pal: Pal,
    pub(crate) s: f32,
    w: f32,
    h: f32,
    stage: Stage,
    code: String,
    kicker: String,
    title: Vec<String>,
    title_size: f32,
    title_lh: f32,
    body: String,
    body_size: f32,
    body_top: f32,
    body_max: f32,
    status: Option<(String, bool)>,
    human: bool,
    ask: Option<String>,
    theme: Option<String>,
    primary: String,
    buttons_top: f32,
    can_later: bool,
    rail: Vec<RailRow>,
    done_n: usize,
}

fn model(m: &MainWindow, t: &Theme, window: &Window) -> Model {
    let size = window.viewport_size();
    let (w, h) = (f32::from(size.width), f32::from(size.height));
    let st = stage(m);
    let cur = match st {
        Stage::At(s) => Some(s),
        _ => None,
    };
    let rail = STEPS
        .iter()
        .enumerate()
        .map(|(i, &s)| {
            let (done, later, here) = (ob::done(m, s), ob::later(m, s), cur == Some(s));
            let tag = if done {
                "DONE"
            } else if later && !here {
                "LATER"
            } else if here {
                "HERE"
            } else if copy(s).optional {
                "OPTIONAL"
            } else {
                ""
            };
            let result = if done {
                "DONE"
            } else if later {
                "LATER"
            } else {
                "SKIPPED"
            };
            RailRow { step: s, num: format!("0{}", i + 1), name: copy(s).name, tag, result, done, later, here }
        })
        .collect();
    let mut md = Model {
        pal: palette(t.mode),
        s: (w / DESIGN_W).min(h / DESIGN_H),
        w,
        h,
        stage: st,
        code: String::new(),
        kicker: String::new(),
        title: vec![],
        title_size: 48.,
        title_lh: 1.05,
        body: String::new(),
        body_size: 18.,
        body_top: 22.,
        body_max: 540.,
        status: None,
        human: false,
        ask: None,
        theme: None,
        primary: String::new(),
        buttons_top: 38.,
        can_later: false,
        rail,
        done_n: STEPS.iter().filter(|s| ob::done(m, **s)).count(),
    };
    match st {
        Stage::Intro => {
            md.code = "ARRIVAL".into();
            md.kicker = "TWILIGHT IS FALLING".into();
            md.title = vec!["Your terminals,".into(), "wherever you go.".into()];
            md.title_size = 64.;
            md.title_lh = 1.;
            md.body = "Five quick jumps and midna can carry your shells and agents across quits, updates and webhooks. Every jump can wait.".into();
            md.body_size = 19.;
            md.body_top = 24.;
            md.body_max = 520.;
            md.primary = "Begin ↩".into();
            md.buttons_top = 40.;
        }
        Stage::At(s) => {
            let c = copy(s);
            let v = ob::view(m, s);
            let done = ob::done(m, s);
            md.code = format!("JUMP 0{} · {}", STEPS.iter().position(|x| *x == s).unwrap_or(0) + 1, c.name.to_uppercase());
            md.kicker = c.kicker.into();
            md.title = vec![c.title.into()];
            md.body = c.body.into();
            md.status = v.status;
            md.human = c.human && !done;
            md.ask = c.ask.filter(|_| !done).map(|a| format!("or {} → “{a}”", m.key_label("keys.command_bar")));
            md.theme = (s == Step::Theme).then(|| m.setting_str("theme").unwrap_or_else(|| "system".into()));
            md.primary = v.action.map(str::to_string).unwrap_or_else(|| if s == Step::Theme { "Finish setup".into() } else { "Next".into() });
            md.can_later = s != Step::Theme && !done;
        }
        Stage::Done => {
            md.code = "ALL JUMPS MADE".into();
            md.kicker = "ALL SET".into();
            md.title = vec!["You made it through".into(), "the twilight.".into()];
            md.title_size = 56.;
            md.title_lh = 1.02;
            md.body = "Anything you put off waits in Settings. Agents can do the rest when you ask.".into();
            md.body_size = 16.;
            md.body_top = 24.;
            md.primary = "Open midna ↩".into();
            md.buttons_top = 32.;
        }
    }
    md
}

pub fn visible(m: &MainWindow) -> bool {
    crate::ui::twilight::over_setup(m) || active(m)
}

/// Setup is open and showing (the opening then plays over this screen, not the main window).
pub fn active(m: &MainWindow) -> bool {
    ob::active(m) && !m.onboarding.card_hidden
}

// ------------------------------------------------------------------ actions

fn ripple(m: &mut MainWindow, from: (f32, f32), cx: &mut Context<MainWindow>) {
    let seq = m.onboarding.ripple.map(|r| r.0 + 1).unwrap_or(1);
    m.onboarding.ripple = Some((seq, from.0, from.1));
    cx.notify();
}

fn run(m: &mut MainWindow, a: Action, window: &mut Window, cx: &mut Context<MainWindow>) {
    match a {
        Action::Primary => match stage(m) {
            Stage::Intro => {
                m.onboarding.intro_seen = true;
                m.onboarding.card_seq += 1;
            }
            Stage::Done => {
                ob::finish(m, cx);
                return;
            }
            Stage::At(s) => {
                if ob::view(m, s).action.is_some() {
                    ob::act(m, s, window, cx);
                } else {
                    if s == Step::Theme {
                        m.onboarding.theme_done = true;
                    }
                    ob::next(m, s, cx);
                }
            }
        },
        Action::Later => {
            if let Stage::At(s) = stage(m) {
                ob::put_off(m, s, cx);
            }
        }
        Action::Jump(s) => ob::pick(m, s, cx),
        Action::Theme(key) => ob::set_theme(m, key, cx),
        Action::Replay => m.onboarding.card_seq += 1,
    }
    ripple(m, origin(a), cx);
}

/// Live: clickable. Inside the opening (`cx` = None): just drawn.
fn clickable(el: Div, id: impl Into<ElementId>, a: Action, cx: Option<&mut Context<MainWindow>>) -> AnyElement {
    match cx {
        Some(cx) => el.id(id).cursor_pointer().on_click(cx.listener(move |m, _, w, cx| run(m, a, w, cx))).into_any_element(),
        None => el.into_any_element(),
    }
}

// ------------------------------------------------------------------ drawing

impl Model {
    /// Just the geometry and palette: for drawing tiles over the main window (`twilight.rs`).
    pub(crate) fn bare(mode: ThemeMode, w: f32, h: f32) -> Model {
        Model {
            pal: palette(mode),
            s: (w / DESIGN_W).min(h / DESIGN_H),
            w,
            h,
            stage: Stage::Done,
            code: String::new(),
            kicker: String::new(),
            title: vec![],
            title_size: 48.,
            title_lh: 1.05,
            body: String::new(),
            body_size: 18.,
            body_top: 22.,
            body_max: 540.,
            status: None,
            human: false,
            ask: None,
            theme: None,
            primary: String::new(),
            buttons_top: 38.,
            can_later: false,
            rail: vec![],
            done_n: 0,
        }
    }

    /// Design units to window pixels.
    pub(crate) fn u(&self, v: f32) -> Pixels {
        px(v * self.s)
    }

    /// How many `cw`×`ch` (design units) cells cover the window.
    pub(crate) fn cells(&self, cw: f32, ch: f32) -> (usize, usize) {
        ((self.w / (cw * self.s)).ceil() as usize, (self.h / (ch * self.s)).ceil() as usize)
    }

    /// The card's full height (design units): content, padding 52/48, border.
    fn card_h(&self) -> f32 {
        CARD_H.with(Cell::get) + 52. + 48. + 2.
    }
}

/// Text with CSS letter spacing (GPUI has none): one element per character.
fn tracked(md: &Model, text: &str, family: &'static str, size: f32, weight: FontWeight, em: f32, color: Hsla) -> Div {
    let gap = size * em;
    div().flex().flex_none().font_family(family).text_size(md.u(size)).font_weight(weight).text_color(color).children(text.chars().map(|ch| {
        if ch == ' ' { div().w(md.u(size * 0.6 + gap)) } else { div().mr(md.u(gap)).child(ch.to_string()) }
    }))
}

fn diamond(md: &Model, size: f32, icon: Icon, color: Hsla) -> Svg {
    icon.el(size * md.s, color).with_transformation(Transformation::rotate(radians(std::f32::consts::FRAC_PI_4)))
}

/// One tile: at rest, or (scale, rotation, colour) mid-ripple. Its runes turn with it.
fn tile(md: &Model, mut el: Div, c: usize, r: usize, v: Option<(f32, f32, [f32; 3])>) -> Div {
    let (x, y) = (c as f32 * CELL, r as f32 * CELL);
    let (sc, rot, col) = v.unwrap_or((1., 0., md.pal.tile));
    let size = (CELL - 2.) * sc;
    if size <= 0.05 {
        return el;
    }
    let place = |e: Svg| div().absolute().left(md.u(x + (CELL - size) / 2.)).top(md.u(y + (CELL - size) / 2.)).child(e.with_transformation(Transformation::rotate(radians(rot.to_radians()))));
    el = match v {
        None => el.child(div().absolute().left(md.u(x + 1.)).top(md.u(y + 1.)).size(md.u(CELL - 2.)).rounded(md.u(2.)).bg(rgb(col, 1.))),
        Some(_) => el.child(place(Icon::Tile.el(size * md.s, rgb(col, 1.)))),
    };
    let rune = match (c * 73 + r * 151 + c * r) % 31 {
        0 => Some((Icon::Rune1, md.pal.ember, 0.34)),
        9 => Some((Icon::Rune2, md.pal.glow, 0.3)),
        17 => Some((Icon::Rune3, md.pal.glow, 0.22)),
        _ => None,
    };
    if let Some((icon, tint, alpha)) = rune {
        el = el.child(place(icon.el(size * md.s, rgb(tint, alpha))));
    }
    el
}

/// The tile grid, `ms` into a ripple from `(ox, oy)` (None: at rest). With `unreached_bare`,
/// cells the ripple hasn't reached draw nothing (the opening: the desktop shows there).
fn grid(md: &Model, ripple: Option<(f32, f32, f32)>, unreached_bare: bool) -> Div {
    let (cols, rows) = md.cells(CELL, CELL);
    let mut el = div().absolute().inset_0();
    for r in 0..rows {
        for c in 0..cols {
            let p = ripple.map(|(ms, ox, oy)| (ms - (c as f32 - ox).hypot(r as f32 - oy) * PER_CELL_MS) / RIPPLE_MS);
            if unreached_bare && p.is_some_and(|p| p <= 0.) {
                continue;
            }
            let v = p.and_then(|p| (p > 0. && p < 1.).then(|| ripple_at(p, &md.pal)));
            el = tile(md, el, c, r, v);
        }
    }
    el
}

/// The see-through teal wave at `ms`: 96×100 cells, 120ms + 52ms per diagonal, 520ms each.
pub(crate) fn wave(md: &Model, ms: f32) -> Div {
    let (cols, rows) = md.cells(96., 100.);
    let mut el = div().absolute().inset_0();
    let line = rgb(md.pal.glow, 1.);
    for r in 0..rows {
        for c in 0..cols {
            let p = (ms - 120. - (c + r) as f32 * 52.) / 520.;
            if p <= 0. || p >= 1. {
                continue;
            }
            let (op, sc, rot) = wave_at(p);
            let (bw, bh) = (90. * sc, 94. * sc);
            let (x, y) = (c as f32 * 96. + 3. + (90. - bw) / 2., r as f32 * 100. + 3. + (94. - bh) / 2.);
            let t = Transformation::rotate(radians(rot.to_radians()));
            el = el.child(
                div()
                    .absolute()
                    .left(md.u(x))
                    .top(md.u(y))
                    .child(svg().path(Icon::WaveFill.path()).w(md.u(bw)).h(md.u(bh)).text_color(Hsla { a: md.pal.wash.a * op, ..md.pal.wash }).with_transformation(t))
                    .child(div().absolute().left_0().top_0().child(svg().path(Icon::WaveLine.path()).w(md.u(bw)).h(md.u(bh)).text_color(Hsla { a: op, ..line }).with_transformation(t))),
            );
        }
    }
    el
}

fn button(md: &Model, label: &str, primary: bool) -> Div {
    let (glow, ember) = (rgb(md.pal.glow, 1.), rgb(md.pal.ember, 1.));
    let d = div().h(md.u(48.)).px(md.u(22.)).flex().items_center().rounded(md.u(3.)).border_1().font_family(PLEX).text_size(md.u(15.)).font_weight(FontWeight::SEMIBOLD).child(label.to_string());
    if primary {
        d.bg(glow).border_color(glow).text_color(rgb(hex(0x04130F), 1.)).hover(move |s| s.bg(ember).border_color(ember))
    } else {
        d.border_color(rgb(md.pal.line, 1.)).text_color(rgb(md.pal.fg, 1.)).hover(move |s| s.border_color(glow))
    }
}

/// The card's content column.
fn card_body(md: &Model, mut cx: Option<&mut Context<MainWindow>>) -> Div {
    let (fg, dim, ember, glow, line) = (rgb(md.pal.fg, 1.), rgb(md.pal.dim, 1.), rgb(md.pal.ember, 1.), rgb(md.pal.glow, 1.), rgb(md.pal.line, 1.));
    let mut col = div()
        .relative()
        .flex()
        .flex_col()
        .flex_none()
        .child(tracked(md, &md.kicker, MONO, 13., FontWeight::SEMIBOLD, 0.24, ember))
        .child(div().mt(md.u(18.)).font_family(CHAKRA).font_weight(FontWeight::BOLD).text_size(md.u(md.title_size)).line_height(md.u(md.title_size * md.title_lh)).text_color(fg).children(md.title.iter().map(|l| div().child(l.clone()))));
    if md.stage == Stage::Done {
        col = col.child(div().mt(md.u(30.)).flex().gap(md.u(10.)).children(md.rail.iter().map(|r| {
            let color = if r.done { glow } else { ember };
            div().flex_1().px(md.u(12.)).py(md.u(14.)).border_1().border_color(line).rounded(md.u(3.)).child(tracked(md, r.result, MONO, 11., FontWeight::SEMIBOLD, 0.16, color)).child(div().mt(md.u(8.)).font_family(PLEX).text_size(md.u(14.)).font_weight(FontWeight::SEMIBOLD).text_color(fg).child(r.name))
        })));
    }
    col = col.child(div().mt(md.u(md.body_top)).max_w(md.u(md.body_max)).font_family(PLEX).text_size(md.u(md.body_size)).line_height(md.u(md.body_size * 1.55)).text_color(dim).child(md.body.clone()));
    if let Some(cur) = &md.theme {
        let chips: Vec<AnyElement> = [("dark", "Twilight"), ("light", "Light world"), ("system", "Match macOS")]
            .into_iter()
            .map(|(key, label)| {
                let on = cur == key;
                let chip = div()
                    .h(md.u(44.))
                    .px(md.u(18.))
                    .flex()
                    .items_center()
                    .rounded(md.u(22.))
                    .border_1()
                    .border_color(if on { glow } else { line })
                    .when(on, |d| d.bg(md.pal.halo))
                    .font_family(PLEX)
                    .text_size(md.u(14.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(fg)
                    .child(label);
                clickable(chip, SharedString::from(format!("setup-theme-{key}")), Action::Theme(key), cx.as_deref_mut())
            })
            .collect();
        col = col.child(div().mt(md.u(28.)).flex().gap(md.u(10.)).children(chips));
    }
    if let Some((text, ok)) = &md.status {
        let color = if *ok { glow } else { ember };
        col = col.child(div().mt(md.u(26.)).flex().items_center().gap(md.u(10.)).font_family(PLEX).text_size(md.u(14.)).text_color(color).child(diamond(md, 8., Icon::Tile, color)).child(text.clone()));
    }
    if md.human {
        col = col.child(div().mt(md.u(26.)).flex().items_center().gap(md.u(10.)).font_family(PLEX).text_size(md.u(14.)).text_color(dim).child(diamond(md, 8., Icon::Tile, ember)).child("Only you can do this one: macOS asks a person, not an agent."));
    }
    if let Some(ask) = &md.ask {
        col = col.child(div().mt(md.u(26.)).font_family(MONO).text_size(md.u(14.)).text_color(dim).child(ask.clone()));
    }
    let mut row = div().mt(md.u(md.buttons_top)).flex().gap(md.u(12.)).child(clickable(button(md, &md.primary, true), "setup-primary", Action::Primary, cx.as_deref_mut()));
    if md.can_later {
        row = row.child(clickable(button(md, "Later", false), "setup-later", Action::Later, cx));
    }
    // Measure the natural height; the entrance opens the card to it.
    let s = md.s;
    col.child(row).child(canvas(move |b, _, _| CARD_H.with(|h| h.set(f32::from(b.size.height) / s)), |_, _, _, _| {}).absolute().inset_0())
}

/// The card (and its halo ring) at entrance progress `e`: fade in, rise 20px, open from the top.
fn card_style<E: Styled>(md: &Model, el: E, e: f32) -> E {
    el.top(md.u(176. + 20. * (1. - e))).max_h(md.u(md.card_h() * e)).opacity(e)
}

fn card(md: &Model, cx: Option<&mut Context<MainWindow>>) -> Div {
    div()
        .absolute()
        .left(md.u(144.))
        .w(md.u(672.))
        .overflow_hidden()
        .pt(md.u(52.))
        .px(md.u(52.))
        .pb(md.u(48.))
        .bg(rgb(md.pal.panel, 1.))
        .border_1()
        .border_color(rgb(md.pal.line, 1.))
        .rounded(md.u(4.))
        .child(card_body(md, cx))
}

/// `box-shadow: 0 0 0 10px` around the card, clipped with it.
fn halo(md: &Model) -> Div {
    div().absolute().left(md.u(144.)).w(md.u(672.)).h(md.u(md.card_h())).rounded(md.u(4.)).shadow(vec![BoxShadow { color: md.pal.halo, offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: md.u(10.), inset: false }])
}

fn rail(md: &Model, mut cx: Option<&mut Context<MainWindow>>) -> Div {
    let (dim, ember, glow, fg) = (rgb(md.pal.dim, 1.), rgb(md.pal.ember, 1.), rgb(md.pal.glow, 1.), rgb(md.pal.fg, 1.));
    let mut col = div().absolute().left(md.u(912.)).top(md.u(176.)).w(md.u(384.)).flex().flex_col().gap(md.u(6.)).child(div().px(md.u(16.)).pb(md.u(12.)).child(tracked(md, &format!("JUMPS · {} OF 5", md.done_n), MONO, 12., FontWeight::SEMIBOLD, 0.22, dim)));
    for (i, r) in md.rail.iter().enumerate() {
        let pip: AnyElement = if r.done {
            diamond(md, 14., Icon::Tile, glow).into_any_element()
        } else if r.here {
            // Filled and outlined in ember, blinking (steps(2), 1.6s).
            let d = div().child(diamond(md, 14., Icon::Tile, ember)).child(div().absolute().left_0().top_0().child(diamond(md, 14., Icon::TileOutline, ember)));
            if cx.is_some() {
                d.with_animation(SharedString::from(format!("setup-blink-{i}")), Animation::new(Duration::from_millis(1600)).repeat(), |el, t| el.opacity(if t < 0.5 { 1. } else { 0.25 })).into_any_element()
            } else {
                d.into_any_element()
            }
        } else if r.later {
            diamond(md, 14., Icon::TileDashed, ember).into_any_element()
        } else {
            diamond(md, 14., Icon::TileOutline, dim).into_any_element()
        };
        let row = div()
            .h(md.u(56.))
            .px(md.u(16.))
            .flex()
            .items_center()
            .gap(md.u(16.))
            .rounded(md.u(3.))
            .border_1()
            .border_color(if r.here { rgb(md.pal.line, 1.) } else { Hsla::transparent_black() })
            .when(r.here, |d| d.bg(rgb(md.pal.panel, 1.)))
            .child(div().relative().size(md.u(14.)).flex_none().flex().items_center().justify_center().child(pip))
            .child(div().font_family(MONO).text_size(md.u(13.)).font_weight(FontWeight::SEMIBOLD).text_color(dim).child(r.num.clone()))
            .child(div().flex_1().font_family(PLEX).text_size(md.u(16.)).font_weight(FontWeight::SEMIBOLD).text_color(if r.done { dim } else { fg }).child(r.name))
            .child(tracked(md, r.tag, MONO, 11., FontWeight::SEMIBOLD, 0.14, if r.done { glow } else { ember }));
        col = col.child(clickable(row, SharedString::from(format!("setup-jump-{i}")), Action::Jump(r.step), cx.as_deref_mut()));
    }
    col
}

/// Top bar, card, rail and footer: the window's content above the tiles. The card is at
/// entrance progress `card_e`, or (live, `animate`) runs its entrance as an animation.
fn content(md: &Model, card_e: f32, animate: Option<u64>, mut cx: Option<&mut Context<MainWindow>>) -> Div {
    let (fg, dim, glow) = (rgb(md.pal.fg, 1.), rgb(md.pal.dim, 1.), rgb(md.pal.glow, 1.));
    let (halo_el, card_el): (AnyElement, AnyElement) = match animate {
        None => (card_style(md, halo(md), card_e).into_any_element(), card_style(md, card(md, cx.as_deref_mut()), card_e).into_any_element()),
        Some(seq) => {
            let (m1, m2) = (md.clone(), md.clone());
            let anim = || Animation::new(Duration::from_millis((CARD_WAIT_MS + CARD_OPEN_MS) as u64));
            (
                halo(md).with_animation(SharedString::from(format!("setup-halo-{seq}")), anim(), move |el, d| card_style(&m1, el, card_in(d * (CARD_WAIT_MS + CARD_OPEN_MS)))).into_any_element(),
                card(md, cx.as_deref_mut()).with_animation(SharedString::from(format!("setup-card-{seq}")), anim(), move |el, d| card_style(&m2, el, card_in(d * (CARD_WAIT_MS + CARD_OPEN_MS)))).into_any_element(),
            )
        }
    };
    div()
        .absolute()
        .left_0()
        .top_0()
        .w(px(md.w))
        .h(px(md.h))
        .child(
            div()
                .absolute()
                .top(md.u(28.))
                .left(md.u(40.))
                .w(md.u(DESIGN_W - 80.))
                .flex()
                .items_center()
                .justify_between()
                .child(div().flex().items_center().gap(md.u(14.)).child(diamond(md, 16., Icon::TileOutline, glow)).child(tracked(md, "MIDNA", CHAKRA, 20., FontWeight::BOLD, 0.32, fg)))
                .child(tracked(md, &md.code, MONO, 12., FontWeight::SEMIBOLD, 0.22, dim)),
        )
        .child(halo_el)
        .child(card_el)
        .child(rail(md, cx.as_deref_mut()))
        .child(
            div()
                .absolute()
                .left(md.u(40.))
                .top(md.u(DESIGN_H - 28. - 15.))
                .w(md.u(DESIGN_W - 80.))
                .flex()
                .justify_between()
                .child(tracked(md, "↩ CONTINUE · ESC LATER · ⌘K ASK AN AGENT", MONO, 12., FontWeight::NORMAL, 0.12, dim))
                .child(clickable(div().child(tracked(md, "REPLAY INTRO", MONO, 12., FontWeight::NORMAL, 0.12, dim)), "setup-replay", Action::Replay, cx)),
        )
}

/// The opening at `ms`: desktop where the ripple hasn't reached its orange yet, the window
/// (background under the tiles, content above them) where it has.
fn opening(md: &Model, ms: f32) -> Div {
    let (cols, rows) = md.cells(CELL, CELL);
    let cell = CELL * md.s;
    let (ox, oy) = OPEN_ORIGIN;
    // Which cells show the window: per row, a run of columns (the ripple front is a disc).
    let runs: Vec<(usize, usize, usize)> = (0..rows)
        .filter_map(|r| {
            let shown = |c: &usize| (ms - (*c as f32 - ox).hypot(r as f32 - oy) * PER_CELL_MS) / RIPPLE_MS >= EXPOSE_AT;
            let first = (0..cols).find(shown)?;
            let last = (0..cols).rev().find(shown)?;
            Some((r, first, last))
        })
        .collect();
    let bg = rgb(md.pal.bg, 1.);
    let mut el = div().absolute().inset_0();
    for &(r, c0, c1) in &runs {
        // 1px of overlap with the next row: no hairline seam between strips.
        el = el.child(div().absolute().left(px(c0 as f32 * cell)).top(px(r as f32 * cell)).w(px((c1 - c0 + 1) as f32 * cell)).h(px(cell + 1.)).bg(bg));
    }
    el = el.child(grid(md, Some((ms, ox, oy)), true));
    let e = card_in(ms);
    for &(r, c0, c1) in &runs {
        let (x, y) = (c0 as f32 * cell, r as f32 * cell);
        el = el.child(div().absolute().left(px(x)).top(px(y)).w(px((c1 - c0 + 1) as f32 * cell)).h(px(cell + 1.)).overflow_hidden().child(div().absolute().left(px(-x)).top(px(-y)).child(content(md, e, None, None))));
    }
    el.child(wave(md, ms))
}

/// The Reduce motion opening at `ms`: cells show the screen (at rest, card in) once their still
/// teal square is lit; the squares fade together at the end.
fn opening_reduced(md: &Model, ms: f32) -> Div {
    let (cols, rows) = md.cells(CELL, CELL);
    let cell = CELL * md.s;
    let lit = crate::ui::twilight::reduced(cols, rows, ms);
    let mut el = div().absolute().inset_0();
    for (r, c0, c1) in crate::ui::twilight::runs(cols, rows, |c, r| lit(c, r).0) {
        let (x, y) = (c0 as f32 * cell, r as f32 * cell);
        let screen = div().absolute().left(px(-x)).top(px(-y)).w(px(md.w)).h(px(md.h)).bg(rgb(md.pal.bg, 1.)).child(grid(md, None, false)).child(content(md, 1., None, None));
        el = el.child(div().absolute().left(px(x)).top(px(y)).w(px((c1 - c0 + 1) as f32 * cell)).h(px(cell + 1.)).overflow_hidden().child(screen));
    }
    el.child(crate::ui::twilight::squares(md, cols, rows, &lit))
}

/// The opening (while `twilight` plays it) or the live screen. `None` when setup isn't showing.
pub fn render(m: &MainWindow, t: &Theme, window: &Window, cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    if !visible(m) {
        return None;
    }
    let md = model(m, t, window);
    if m.twilight_phase == crate::ui::twilight::Phase::Intro {
        let reduced = m.twilight_reduced;
        let total = crate::ui::twilight::design_ms(m);
        let el = div()
            .id("setup-screen")
            .absolute()
            .inset_0()
            .occlude()
            .with_animation(SharedString::from(format!("setup-opening-{}", m.twilight_seq)), Animation::new(crate::ui::twilight::length(m)), move |el, d| {
                el.child(if reduced { opening_reduced(&md, d * total) } else { opening(&md, d * total) })
            });
        return Some(el.into_any_element());
    }
    let ripple_el: AnyElement = match m.onboarding.ripple {
        None => grid(&md, None, false).into_any_element(),
        Some((seq, ox, oy)) => {
            let (cols, rows) = md.cells(CELL, CELL);
            let far = [(0., 0.), (cols as f32, 0.), (0., rows as f32), (cols as f32, rows as f32)].iter().map(|(x, y)| (x - ox).hypot(y - oy)).fold(0., f32::max);
            let total = far * PER_CELL_MS + RIPPLE_MS;
            let md2 = md.clone();
            div().absolute().inset_0().with_animation(SharedString::from(format!("setup-ripple-{seq}")), Animation::new(Duration::from_millis(total as u64)), move |el, d| el.child(grid(&md2, Some((d * total, ox, oy)), false))).into_any_element()
        }
    };
    // The card replays its entrance on step changes; the opening already played the first one.
    let seq = m.onboarding.card_seq;
    let animate = (seq != m.onboarding.card_seq_opened).then_some(seq);
    let el = div().id("setup-screen").absolute().inset_0().occlude().bg(rgb(md.pal.bg, 1.)).child(ripple_el).child(content(&md, 1., animate, Some(cx)));
    Some(el.into_any_element())
}

#[cfg(test)]
mod tests {
    use super::{EXPOSE_AT, card_in, palette, ripple_at, wave_at};
    use crate::theme::ThemeMode;

    #[test]
    fn ripple_keyframes_match_the_design() {
        let p = palette(ThemeMode::Dark);
        assert_eq!(ripple_at(0.16, &p), (0.3, 45., p.glow));
        assert_eq!(ripple_at(EXPOSE_AT, &p), (0.62, 45., p.ember));
        let (s, r, c) = ripple_at(0.9999, &p);
        assert!((s - 1.).abs() < 0.01 && r.abs() < 0.5 && c == p.tile, "{s} {r} {c:?}");
    }

    #[test]
    fn wave_and_card_timing_match_the_design() {
        assert_eq!(wave_at(0.32), (1., 0.97, 0.));
        assert!(card_in(460.) < 1e-4);
        assert!((card_in(1360.) - 1.).abs() < 1e-4);
    }
}
