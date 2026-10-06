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
use midna_proto::themes::ThemeDef;
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
/// Concept E: the jumps across the top, one card centred under them (copy left, guide right).
const PIPS_X: f32 = 220.;
const PIPS_W: f32 = 1000.;
const PIPS_TOP: f32 = 104.;
const PIP_W: f32 = 150.;
const CARD_X: f32 = 190.;
const CARD_W: f32 = 1060.;
const CARD_TOP: f32 = 196.;
const CARD_MIN_H: f32 = 470.;
const GUIDE_W: f32 = 500.;
const CHAKRA: &str = "Chakra Petch";
const PLEX: &str = "IBM Plex Sans";
const MONO: &str = crate::theme::MONO_FONT;

thread_local! {
    /// The card's natural content height (design units), measured as it lays out; the entrance
    /// opens the card to it.
    static CARD_H: Cell<f32> = const { Cell::new(362.) };
}

/// Record the card content's height; when it changed, draw again so the card (sized from the
/// last measurement) fits it.
fn measure(h: f32, window: &mut Window) {
    if (CARD_H.with(|c| c.replace(h)) - h).abs() > 0.5 {
        window.refresh();
    }
}

// ------------------------------------------------------------------ palette

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Pal {
    bg: [f32; 3],
    pub(crate) tile: [f32; 3],
    pub(crate) glow: [f32; 3],
    pub(crate) ember: [f32; 3],
    panel: [f32; 3],
    /// The card's guide panel, and the sample notifications in it.
    guide: [f32; 3],
    toast: [f32; 3],
    line: [f32; 3],
    fg: [f32; 3],
    dim: [f32; 3],
    /// Text on the primary (glow) button.
    glow_fg: [f32; 3],
    /// The card's halo and the opening wave's fill: glow at these alphas.
    halo_a: f32,
    wash_a: f32,
}

impl Pal {
    fn halo(&self) -> Hsla {
        rgb(self.glow, self.halo_a)
    }

    fn wash(&self) -> Hsla {
        rgb(self.glow, self.wash_a)
    }
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
            guide: hex(0x080B10),
            toast: hex(0x282E38),
            line: hex(0x1E2A2B),
            fg: hex(0xE8ECEF),
            dim: hex(0x9AA6AE),
            glow_fg: hex(0x04130F),
            halo_a: 0.06,
            wash_a: 0.14,
        },
        ThemeMode::Light => Pal {
            bg: hex(0xEAE3D2),
            tile: hex(0xE0D5BD),
            glow: hex(0x1F8A76),
            ember: hex(0xB5561A),
            panel: hex(0xF7F2E6),
            guide: hex(0xEFE8D8),
            toast: hex(0xFFFDF8),
            line: hex(0xCBBD9C),
            fg: hex(0x1D1A14),
            dim: hex(0x574F41),
            glow_fg: hex(0x04130F),
            halo_a: 0.08,
            wash_a: 0.12,
        },
    }
}

/// What the opening and setup's other steps wear: Twilight Tiles' own palette for the stock
/// Twilight theme (exactly as before themes), else the theme (`theme_palette`), Daylight included.
pub(crate) fn world_palette(d: &ThemeDef) -> Pal {
    if d.is_builtin("twilight") { palette(ThemeMode::Dark) } else { theme_palette(d) }
}

/// The setup screen wearing a theme (the theme step, ThemeStep-B): tiles in the theme's panel
/// (raised in light themes), glow = accent, ember = need.
fn theme_palette(d: &ThemeDef) -> Pal {
    let c = |k: &str| d.get(k).unwrap_or([255, 0, 255]).map(f32::from);
    Pal {
        bg: c("bg"),
        tile: if d.dark { c("panel") } else { c("raised") },
        glow: c("accent"),
        ember: c("need"),
        panel: c("panel"),
        guide: c("panel"),
        toast: c("raised"),
        line: c("line"),
        fg: c("fg"),
        dim: c("dim"),
        glow_fg: if d.dark { c("bg") } else { [255.; 3] },
        halo_a: 0.08,
        wash_a: 0.14,
    }
}

fn mix3(a: [f32; 3], b: [f32; 3], k: f32) -> [f32; 3] {
    [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * k)
}

fn mix_pal(a: &Pal, b: &Pal, k: f32) -> Pal {
    let m = |x: [f32; 3], y: [f32; 3]| mix3(x, y, k);
    Pal {
        bg: m(a.bg, b.bg),
        tile: m(a.tile, b.tile),
        glow: m(a.glow, b.glow),
        ember: m(a.ember, b.ember),
        panel: m(a.panel, b.panel),
        guide: m(a.guide, b.guide),
        toast: m(a.toast, b.toast),
        line: m(a.line, b.line),
        fg: m(a.fg, b.fg),
        dim: m(a.dim, b.dim),
        glow_fg: m(a.glow_fg, b.glow_fg),
        halo_a: a.halo_a + (b.halo_a - a.halo_a) * k,
        wash_a: a.wash_a + (b.wash_a - a.wash_a) * k,
    }
}

fn mix_def(a: &ThemeDef, b: &ThemeDef, k: f32) -> ThemeDef {
    let m = |x: [u8; 3], y: [u8; 3]| mix3(x.map(f32::from), y.map(f32::from), k).map(|v| v.round() as u8);
    let mut out = b.clone();
    for i in 0..out.ui.len() {
        out.ui[i] = m(a.ui[i], b.ui[i]);
    }
    for i in 0..16 {
        out.ansi[i] = m(a.ansi[i], b.ansi[i]);
    }
    out
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
    /// The theme step: browse by ±1, pick dot `i` of the shown pool, One theme / Match macOS,
    /// which macOS slot is being picked, and Back.
    ThemeStep(i32),
    ThemeTo(usize),
    ThemeMatch(bool),
    ThemeSlot(bool),
    Back,
    Replay,
}

struct Copy {
    name: &'static str,
    kicker: &'static str,
    title: &'static str,
    body: &'static str,
    human: bool,
    ask: Option<&'static str>,
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
        },
        Step::Notifications => Copy {
            name: "Notifications",
            kicker: "JUMP 02 / 05 · macOS",
            title: "Get a ping when a terminal needs you",
            body: "Approvals, questions and failures. Clicking one jumps straight to that terminal.",
            human: true,
            ask: None,
        },
        Step::Project => Copy {
            name: "First project",
            kicker: "JUMP 03 / 05",
            title: "Add your first project",
            body: "A project is a folder. It gets ⌘1, a shell, and git status in the header. Dropping a folder anywhere works too.",
            human: false,
            ask: Some("add ~/Development/my-app as a project"),
        },
        Step::Webhooks => Copy {
            name: "Webhooks",
            kicker: "JUMP 04 / 05",
            title: "Start agents from GitHub and Bitbucket",
            body: "A pull request or a comment can start an agent here. Tailscale Funnel gives midnad a public URL for free, with nothing to host.",
            human: false,
            ask: Some("start Claude on every PR opened in my repo"),
        },
        Step::Theme => Copy {
            name: "Theme",
            kicker: "JUMP 05 / 05",
            title: "Pick your colors",
            body: "Browse with ← and →. What you see is what you get, terminal included.",
            human: false,
            ask: None,
        },
    }
}

fn stage(m: &MainWindow) -> Stage {
    // Dev: open a given screen (intro, daemon, notifications, project, webhooks, theme, done).
    if let Ok(v) = crate::dev::var("MIDNA_DEBUG_SETUP_STEP") {
        let step = [Step::Daemon, Step::Notifications, Step::Project, Step::Webhooks, Step::Theme].into_iter().find(|s| format!("{s:?}").eq_ignore_ascii_case(&v));
        return match (v.as_str(), step) {
            (_, Some(s)) => Stage::At(s),
            ("done", _) => Stage::Done,
            _ => Stage::Intro,
        };
    }
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
        Action::Primary => (5.5, 12.),
        Action::Later => (8.5, 12.),
        Action::Jump(s) => {
            let i = STEPS.iter().position(|x| *x == s).unwrap_or(0) as f32;
            ((PIPS_X + PIP_W / 2. + i * pip_step()) / CELL, 2.3)
        }
        Action::ThemeStep(_) | Action::ThemeTo(_) | Action::ThemeMatch(_) | Action::ThemeSlot(_) => SHIFT_ORIGIN,
        Action::Back => (8.5, 12.),
        Action::Replay => OPEN_ORIGIN,
    }
}

#[derive(Clone)]
struct RailRow {
    step: Step,
    name: &'static str,
    result: &'static str,
    done: bool,
    later: bool,
    here: bool,
}

/// The theme step's left column.
#[derive(Clone)]
struct ThemeUi {
    pick: ob::ThemePick,
    /// The themes being browsed (all, or one kind while matching macOS): id, name, bg, accent.
    pool: Vec<(String, String, [f32; 3], [f32; 3])>,
    /// "3 OF 8", "2 OF 5 DARK".
    pos: String,
    /// Names for the slot cards: the one theme, the dark one, the light one.
    one: String,
    dark: String,
    light: String,
}

fn system_dark(window: &Window) -> bool {
    matches!(window.appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark)
}

/// The themes the theme step browses: every theme, or only one kind while matching macOS.
fn theme_pool<'a>(all: &'a [ThemeDef], p: &ob::ThemePick) -> Vec<&'a ThemeDef> {
    all.iter().filter(|d| !p.sys || d.dark == p.dark_slot).collect()
}

/// Fill the theme step's controls; returns the theme being browsed (with `theme.colors`).
fn theme_ui(m: &MainWindow, window: &Window, md: &mut Model) -> ThemeDef {
    let pick = ob::theme_pick(m, system_dark(window));
    let all = crate::theme::cached_themes();
    let def = m.theme_def(pick.current(), if pick.sys { pick.dark_slot } else { system_dark(window) });
    let pool = theme_pool(&all, &pick);
    let at = pool.iter().position(|d| d.id == def.id).map(|i| (i + 1).to_string()).unwrap_or_else(|| "–".into());
    let slot = if !pick.sys { "" } else if pick.dark_slot { " DARK" } else { " LIGHT" };
    let name = |id: &str| all.iter().find(|d| d.id == id).map(|d| d.name.clone()).unwrap_or_else(|| id.to_string());
    let f = |c: Option<[u8; 3]>| c.unwrap_or([0; 3]).map(f32::from);
    md.pick = Some(ThemeUi {
        pos: format!("{at} OF {}{slot}", pool.len()),
        pool: pool.iter().map(|d| (d.id.clone(), d.name.clone(), f(d.get("bg")), f(d.get("accent")))).collect(),
        one: name(&pick.one),
        dark: name(&pick.dark),
        light: name(&pick.light),
        pick,
    });
    def
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
    /// The theme step's controls, and the theme its preview (and the whole screen) wears.
    pick: Option<ThemeUi>,
    preview: Option<ThemeDef>,
    primary: String,
    buttons_top: f32,
    can_later: bool,
    /// The second button: "Later", or "Add folder…" on a project step that is already done.
    later_label: &'static str,
    rail: Vec<RailRow>,
    /// The command bar's shortcut, as shown (⌘K).
    command_key: String,
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
        .map(|&s| {
            let (done, later, here) = (ob::done(m, s), ob::later(m, s), cur == Some(s));
            let result = if done {
                "DONE"
            } else if later {
                "LATER"
            } else {
                "SKIPPED"
            };
            RailRow { step: s, name: copy(s).name, result, done, later, here }
        })
        .collect();
    let mut md = Model {
        pal: world_palette(&t.def),
        s: (w / DESIGN_W).min(h / DESIGN_H),
        w,
        h,
        stage: st,
        code: String::new(),
        kicker: String::new(),
        title: vec![],
        title_size: 40.,
        title_lh: 1.06,
        body: String::new(),
        body_size: 17.,
        body_top: 20.,
        body_max: 440.,
        status: None,
        human: false,
        ask: None,
        pick: None,
        preview: None,
        primary: String::new(),
        buttons_top: 34.,
        can_later: false,
        later_label: "Later",
        rail,
        command_key: m.key_label("keys.command_bar"),
    };
    match st {
        Stage::Intro => {
            md.code = "ARRIVAL".into();
            md.kicker = "TWILIGHT IS FALLING".into();
            md.title = vec!["Your terminals,".into(), "wherever you go.".into()];
            md.title_size = 50.;
            md.title_lh = 1.04;
            md.body = "Five quick jumps and midna can carry your shells and agents across quits, updates and webhooks. Every jump can wait.".into();
            md.body_size = 18.;
            md.body_top = 20.;
            md.primary = "Begin ↩".into();
            md.buttons_top = 34.;
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
            if s == Step::Theme {
                let def = theme_ui(m, window, &mut md);
                md.pal = theme_palette(&def);
                md.preview = Some(def);
            }
            md.primary = v.action.map(str::to_string).unwrap_or_else(|| if s == Step::Theme { "Finish setup".into() } else { "Next".into() });
            md.can_later = s != Step::Theme && !done;
            if s == Step::Project && done {
                // Projects can exist before setup (an agent's `open --cwd`); still offer the picker.
                md.can_later = true;
                md.later_label = "Add folder…";
            }
        }
        Stage::Done => {
            md.code = "ALL JUMPS MADE".into();
            md.kicker = "ALL SET".into();
            md.title = vec!["midna is ready.".into()];
            md.title_size = 44.;
            md.title_lh = 1.04;
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
                        let p = ob::theme_pick(m, system_dark(window));
                        ob::save_theme(m, &p, cx);
                    }
                    ob::next(m, s, cx);
                }
            }
        },
        Action::Later => match stage(m) {
            Stage::At(Step::Project) if ob::done(m, Step::Project) => m.pick_project(cx),
            Stage::At(s) => ob::put_off(m, s, cx),
            _ => {}
        },
        Action::Jump(s) => ob::pick(m, s, cx),
        Action::ThemeStep(_) | Action::ThemeTo(_) | Action::ThemeMatch(_) | Action::ThemeSlot(_) => {
            // No click ripple: the tiles ripple into the new colors instead (`Shift`).
            browse(m, a, window);
            cx.notify();
            return;
        }
        Action::Back => {
            if let Stage::At(s) = stage(m)
                && let Some(i) = STEPS.iter().position(|x| *x == s).filter(|i| *i > 0)
            {
                ob::pick(m, STEPS[i - 1], cx);
            }
        }
        Action::Replay => m.onboarding.card_seq += 1,
    }
    ripple(m, origin(a), cx);
}

/// The theme step's controls change what is browsed; nothing is saved until "Finish setup".
fn browse(m: &mut MainWindow, a: Action, window: &Window) {
    let mut p = ob::theme_pick(m, system_dark(window));
    let all = crate::theme::cached_themes();
    match a {
        Action::ThemeMatch(on) if on != p.sys => {
            if on {
                // Keep what was picked: it fills the slot of its own kind.
                let one = p.one.clone();
                p.dark_slot = all.iter().find(|d| d.id == one).map_or(p.dark_slot, |d| d.dark);
                p.sys = true;
                p.set_current(one);
            } else {
                p.one = p.current().to_string();
                p.sys = false;
            }
        }
        Action::ThemeSlot(dark) if p.sys => p.dark_slot = dark,
        Action::ThemeTo(i) => {
            if let Some(d) = theme_pool(&all, &p).get(i) {
                p.set_current(d.id.clone());
            }
        }
        Action::ThemeStep(d) => {
            let pool = theme_pool(&all, &p);
            let n = pool.len() as i32;
            if n > 0 {
                let i = pool.iter().position(|t| t.id == p.current()).map_or(if d > 0 { -1 } else { 0 }, |i| i as i32);
                p.set_current(pool[(((i + d) % n + n) % n) as usize].id.clone());
            }
        }
        _ => {}
    }
    m.onboarding.theme_pick = Some(p);
}

/// ← and → browse themes while the theme step shows.
pub fn on_key(m: &mut MainWindow, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<MainWindow>) -> bool {
    let k = &ev.keystroke;
    if !active(m) || stage(m) != Stage::At(Step::Theme) || k.modifiers.modified() {
        return false;
    }
    let d = match k.key.as_str() {
        "left" => -1,
        "right" => 1,
        _ => return false,
    };
    run(m, Action::ThemeStep(d), window, cx);
    true
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
    pub(crate) fn bare(def: &ThemeDef, w: f32, h: f32) -> Model {
        Model {
            pal: world_palette(def),
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
            pick: None,
            preview: None,
            primary: String::new(),
            buttons_top: 38.,
            can_later: false,
        later_label: "Later",
            rail: vec![],
            command_key: "⌘K".into(),
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

    /// The card's full height (design units): content, padding (52/48; the theme card 44/44), border.
    fn card_h(&self) -> f32 {
        let pad = if self.preview.is_some() { 88. } else { 100. };
        (CARD_H.with(Cell::get) + pad + 2.).max(CARD_MIN_H)
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
fn tile(md: &Model, pal: &Pal, mut el: Div, c: usize, r: usize, v: Option<(f32, f32, [f32; 3])>) -> Div {
    let (x, y) = (c as f32 * CELL, r as f32 * CELL);
    let (sc, rot, col) = v.unwrap_or((1., 0., pal.tile));
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
        0 => Some((Icon::Rune1, pal.ember, 0.34)),
        9 => Some((Icon::Rune2, pal.glow, 0.3)),
        17 => Some((Icon::Rune3, pal.glow, 0.22)),
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
            el = tile(md, &md.pal, el, c, r, v);
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
                    .child(svg().path(Icon::WaveFill.path()).w(md.u(bw)).h(md.u(bh)).text_color(Hsla { a: md.pal.wash().a * op, ..md.pal.wash() }).with_transformation(t))
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
        d.bg(glow).border_color(glow).text_color(rgb(md.pal.glow_fg, 1.)).hover(move |s| s.bg(ember).border_color(ember))
    } else {
        d.border_color(rgb(md.pal.line, 1.)).text_color(rgb(md.pal.fg, 1.)).hover(move |s| s.border_color(glow))
    }
}

/// The card's content column.
fn card_body(md: &Model, mut cx: Option<&mut Context<MainWindow>>) -> Div {
    let (fg, dim, ember, glow) = (rgb(md.pal.fg, 1.), rgb(md.pal.dim, 1.), rgb(md.pal.ember, 1.), rgb(md.pal.glow, 1.));
    let mut col = div()
        .relative()
        .flex()
        .flex_col()
        .flex_none()
        .child(tracked(md, &md.kicker, MONO, 13., FontWeight::SEMIBOLD, 0.24, ember))
        .child(div().mt(md.u(18.)).font_family(CHAKRA).font_weight(FontWeight::BOLD).text_size(md.u(md.title_size)).line_height(md.u(md.title_size * md.title_lh)).text_color(fg).children(md.title.iter().map(|l| div().child(l.clone()))));
    col = col.child(div().mt(md.u(md.body_top)).max_w(md.u(md.body_max)).font_family(PLEX).text_size(md.u(md.body_size)).line_height(md.u(md.body_size * 1.55)).text_color(dim).child(md.body.clone()));
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
        row = row.child(clickable(button(md, md.later_label, false), "setup-later", Action::Later, cx));
    }
    // Measure the natural height; the entrance opens the card to it.
    let s = md.s;
    col.child(row).child(canvas(move |b, window, _| measure(f32::from(b.size.height) / s, window), |_, _, _, _| {}).absolute().inset_0())
}

/// The card (and its halo ring) at entrance progress `e`: fade in, rise 20px, open from the top.
fn card_style<E: Styled>(md: &Model, el: E, e: f32) -> E {
    el.top(md.u(CARD_TOP + 20. * (1. - e))).max_h(md.u(md.card_h() * e)).opacity(e)
}

fn card(md: &Model, mut cx: Option<&mut Context<MainWindow>>) -> Div {
    if md.preview.is_some() {
        return theme_card(md, cx);
    }
    div()
        .absolute()
        .left(md.u(CARD_X))
        .w(md.u(CARD_W))
        .overflow_hidden()
        .flex()
        .bg(rgb(md.pal.panel, 1.))
        .border_1()
        .border_color(rgb(md.pal.line, 1.))
        .rounded(md.u(4.))
        .child(div().flex_1().min_w_0().pt(md.u(52.)).px(md.u(52.)).pb(md.u(48.)).child(card_body(md, cx.as_deref_mut())))
        .child(guide_panel(md, cx))
}

// ------------------------------------------------------------------ the theme step (ThemeStep-B)

/// The theme step's card: browsing on the left, a big live preview of midna on the right.
fn theme_card(md: &Model, mut cx: Option<&mut Context<MainWindow>>) -> Div {
    div()
        .absolute()
        .left(md.u(CARD_X))
        .w(md.u(CARD_W))
        .overflow_hidden()
        .flex()
        // Not stretched: the left column measures its natural height (the entrance opens to it).
        .items_start()
        .gap(md.u(40.))
        .pt(md.u(44.))
        .pr(md.u(44.))
        .pb(md.u(44.))
        .pl(md.u(48.))
        .bg(rgb(md.pal.panel, 1.))
        .border_1()
        .border_color(rgb(md.pal.line, 1.))
        .rounded(md.u(4.))
        .child(theme_body(md, cx.as_deref_mut()))
        .children(md.preview.as_ref().map(|d| div().flex_1().min_w_0().self_center().child(preview(md, d))))
}

fn theme_body(md: &Model, mut cx: Option<&mut Context<MainWindow>>) -> Div {
    let Some(ui) = &md.pick else { return div() };
    let (fg, dim, ember, glow, line) = (rgb(md.pal.fg, 1.), rgb(md.pal.dim, 1.), rgb(md.pal.ember, 1.), rgb(md.pal.glow, 1.), rgb(md.pal.line, 1.));
    let square = |label: &'static str| {
        div().size(md.u(44.)).flex().items_center().justify_center().rounded(md.u(3.)).border_1().border_color(line).font_family(PLEX).text_size(md.u(18.)).text_color(fg).hover(move |s| s.border_color(glow)).child(label)
    };
    let browse = div()
        .mt(md.u(22.))
        .flex()
        .items_center()
        .gap(md.u(10.))
        .child(clickable(square("‹"), "setup-theme-prev", Action::ThemeStep(-1), cx.as_deref_mut()))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .items_center()
                .child(div().font_family(CHAKRA).font_weight(FontWeight::BOLD).text_size(md.u(22.)).text_color(fg).whitespace_nowrap().child(md.preview.as_ref().map(|d| d.name.clone()).unwrap_or_default()))
                .child(tracked(md, &ui.pos, MONO, 11., FontWeight::MEDIUM, 0.16, dim)),
        )
        .child(clickable(square("›"), "setup-theme-next", Action::ThemeStep(1), cx.as_deref_mut()));
    let cur = md.preview.as_ref().map(|d| d.id.clone()).unwrap_or_default();
    let mut dots = div().mt(md.u(14.)).flex().flex_wrap().justify_center().gap(md.u(8.));
    for (i, (id, _, bg, accent)) in ui.pool.iter().enumerate() {
        let on = *id == cur;
        let d = div()
            .size(md.u(32.))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .border(md.u(2.))
            .border_color(if on { glow } else { line })
            .bg(rgb(*bg, 1.))
            .when(on, |d| d.shadow(vec![BoxShadow { color: rgb(md.pal.glow, 0.18), offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: md.u(3.), inset: false }]))
            .child(div().size(md.u(12.)).rounded_full().bg(rgb(*accent, 1.)));
        dots = dots.child(clickable(d, SharedString::from(format!("setup-theme-dot-{i}")), Action::ThemeTo(i), cx.as_deref_mut()));
    }
    let ask = div()
        .mt(md.u(20.))
        .font_family(MONO)
        .text_size(md.u(13.5))
        .line_height(md.u(13.5 * 1.6))
        .text_color(dim)
        .child(div().child("Want your own? Ask an agent:"))
        .child(div().font_weight(FontWeight::MEDIUM).text_color(fg).child("“make me a theme like Nord with a pink accent”"));
    let buttons = div()
        .mt(md.u(26.))
        .flex()
        .gap(md.u(12.))
        .child(clickable(button(md, &md.primary, true), "setup-primary", Action::Primary, cx.as_deref_mut()))
        .child(clickable(button(md, "Back", false), "setup-back", Action::Back, cx.as_deref_mut()));
    let s = md.s;
    div()
        .relative()
        .w(md.u(360.))
        .flex_none()
        .min_h(md.u(420.))
        .flex()
        .flex_col()
        .child(tracked(md, &md.kicker, MONO, 13., FontWeight::SEMIBOLD, 0.24, ember))
        .child(div().mt(md.u(16.)).font_family(CHAKRA).font_weight(FontWeight::BOLD).text_size(md.u(40.)).line_height(md.u(40. * 1.06)).text_color(fg).children(md.title.iter().map(|l| div().child(l.clone()))))
        .child(div().mt(md.u(16.)).font_family(PLEX).text_size(md.u(16.)).line_height(md.u(16. * 1.55)).text_color(dim).child(md.body.clone()))
        .child(mode_control(md, ui, cx.as_deref_mut()))
        .child(browse)
        .child(dots)
        .child(ask)
        .child(div().flex_1())
        .child(buttons)
        .child(canvas(move |b, window, _| measure(f32::from(b.size.height) / s, window), |_, _, _, _| {}).absolute().inset_0())
}

/// Linked (one theme, day and night) or a theme per macOS mode (ThemeStep-B·2): a fixed 58px
/// row of ☾ DARK MODE card · chain button · ☀ LIGHT MODE card, and one hint line under it.
/// Linked, both cards show the one theme, both lit. Unlinked, a card picks which mode ← → and
/// the dots browse.
fn mode_control(md: &Model, ui: &ThemeUi, mut cx: Option<&mut Context<MainWindow>>) -> Div {
    let (fg, dim, glow, line) = (rgb(md.pal.fg, 1.), rgb(md.pal.dim, 1.), rgb(md.pal.glow, 1.), rgb(md.pal.line, 1.));
    let p = &ui.pick;
    let mut row = div().mt(md.u(22.)).h(md.u(58.)).flex().items_center().gap(md.u(6.));
    let card = |dark: bool| {
        let on = !p.sys || p.dark_slot == dark;
        let name = if !p.sys { &ui.one } else if dark { &ui.dark } else { &ui.light };
        let (icon, label) = if dark { (Icon::Moon, "DARK MODE") } else { (Icon::Sun, "LIGHT MODE") };
        div()
            .flex_1()
            .min_w_0()
            .h(md.u(58.))
            .flex()
            .flex_col()
            .justify_center()
            .gap(md.u(2.))
            .px(md.u(12.))
            .rounded(md.u(3.))
            .border_1()
            .border_color(if on { glow } else { line })
            .when(on, |d| d.bg(md.pal.halo()))
            .child(div().flex().items_center().gap(md.u(6.)).child(icon.el(13. * md.s, dim)).child(tracked(md, label, MONO, 10.5, FontWeight::SEMIBOLD, 0.16, dim)))
            .child(div().font_family(PLEX).text_size(md.u(14.)).font_weight(FontWeight::SEMIBOLD).text_color(fg).whitespace_nowrap().overflow_hidden().text_ellipsis().child(name.to_string()))
    };
    row = row.child(clickable(card(true), "setup-theme-slot-dark", Action::ThemeSlot(true), cx.as_deref_mut()));
    let chain = div()
        .flex_none()
        .size(md.u(36.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1()
        .when(!p.sys, |d| d.border_color(glow).bg(rgb(md.pal.glow, 0.18)))
        .when(p.sys, |d| d.border_dashed().border_color(line))
        .child(if p.sys { Icon::ChainBroken.el(16. * md.s, dim) } else { Icon::Chain.el(16. * md.s, fg) });
    row = row.child(clickable(chain, "setup-theme-link", Action::ThemeMatch(!p.sys), cx.as_deref_mut()));
    row = row.child(clickable(card(false), "setup-theme-slot-light", Action::ThemeSlot(false), cx.as_deref_mut()));
    let hint = if p.sys { "Unlinked: midna switches when macOS does. Tap the chain to use one." } else { "Linked: one theme, day and night. Tap the chain to follow macOS." };
    div().flex().flex_col().child(row).child(div().mt(md.u(8.)).font_family(PLEX).text_size(md.u(13.)).text_color(dim).child(hint))
}

/// midna in theme `d`: sidebar rows with status dots, a header with branch and diff stats, a
/// terminal in the theme's ANSI colors with its 16-color strip, and the status bar.
fn preview(md: &Model, d: &ThemeDef) -> Div {
    let c = |k: &str| crate::theme::rgb3(d.get(k).unwrap_or([255, 0, 255]), 1.);
    let a = |i: usize| crate::theme::rgb3(d.ansi[i], 1.);
    let (fg, dim, line, panel) = (c("fg"), c("dim"), c("line"), c("panel"));
    let dot = |state: &str| {
        let el = div().size(md.u(7.)).rounded_full();
        match state {
            "needs" => el.bg(c("need")),
            "working" => el.bg(c("work")),
            "done" => el.bg(c("ok")),
            "failed" => el.bg(c("err")),
            _ => el.border(md.u(1.5)).border_color(dim),
        }
    };
    let rows = [("api", "working"), ("migrate", "needs"), ("tests", "done"), ("build", "failed"), ("zsh", "idle")];
    let mut side = div()
        .w(md.u(150.))
        .flex_none()
        .flex()
        .flex_col()
        .bg(panel)
        .border_r_1()
        .border_color(line)
        .child(div().flex().gap(md.u(6.)).px(md.u(10.)).py(md.u(12.)).children([0xFF5F57, 0xFEBC2E, 0x28C840].map(|x| div().size(md.u(9.)).rounded_full().bg(rgb(hex(x), 1.)))))
        .child(div().mx(md.u(8.)).mb(md.u(6.)).px(md.u(8.)).py(md.u(5.)).border_1().border_color(c("need")).rounded(md.u(6.)).text_color(c("need")).font_weight(FontWeight::SEMIBOLD).text_size(md.u(11.)).child("● 2 need you"))
        .child(div().px(md.u(10.)).pt(md.u(6.)).pb(md.u(2.)).text_size(md.u(9.5)).font_weight(FontWeight::BOLD).text_color(dim).child("ZONAI"));
    for (i, (name, state)) in rows.into_iter().enumerate() {
        side = side.child(
            div()
                .flex()
                .items_center()
                .gap(md.u(7.))
                .px(md.u(10.))
                .py(md.u(5.))
                .when(i == 0, |r| r.bg(c("raised")).border_l(md.u(2.)).border_color(c("accent")))
                .child(dot(state))
                .child(name),
        );
    }
    let seg = |t: &str, color: Hsla, bold: bool| div().text_color(color).when(bold, |d| d.font_weight(FontWeight::BOLD)).child(t.to_string());
    let lines: Vec<Vec<Div>> = vec![
        vec![seg("~/zonai", a(4), true), seg(" on ", fg, false), seg("feat/auth", a(5), true), seg(" ❯ ", a(2), true), seg("ls", fg, false)],
        vec![seg("src/", a(4), true), seg("  build.sh", a(2), true), seg("  current", a(6), false), seg("  notes.md", a(8), false)],
        vec![seg("⏺ ", a(2), false), seg("Update(src/auth/session.rs)", fg, false)],
        vec![seg("  + pub fn refresh(&mut self)", a(2), false)],
        vec![seg("  - pub fn refresh(&self)", a(1), false)],
        vec![seg("  warning", a(3), true), seg(": unused variable `token`", fg, false)],
        vec![seg("  test auth::refresh ... ", fg, false), seg("ok", a(2), true)],
        vec![seg("  test auth::expiry ... ", fg, false), seg("FAILED", a(1), true)],
        vec![seg("✻ Wiring refresh… ", a(5), false), seg("(4m 12s)", a(8), false)],
    ];
    let term = div()
        .flex_1()
        .min_h_0()
        .overflow_hidden()
        .px(md.u(12.))
        .py(md.u(10.))
        .bg(c("term"))
        .font_family(MONO)
        .text_size(md.u(11.))
        .line_height(md.u(11. * 1.65))
        .text_color(fg)
        .children(lines.into_iter().map(|l| div().flex().whitespace_nowrap().children(l)))
        .child(div().mt(md.u(6.)).flex().children((0..16).map(|i| div().flex_1().h(md.u(10.)).bg(a(i)))));
    let header = div()
        .flex()
        .items_center()
        .gap(md.u(8.))
        .h(md.u(32.))
        .px(md.u(12.))
        .border_b_1()
        .border_color(line)
        .child(div().size(md.u(7.)).rounded_full().bg(c("work")))
        .child(div().font_weight(FontWeight::BOLD).child("api"))
        .child(div().text_color(dim).child("feat/auth"))
        .child(div().flex().font_family(MONO).text_size(md.u(11.)).child(seg("+48", c("ok"), false)).child(seg(" −12", c("err"), false)));
    let status = div()
        .flex()
        .items_center()
        .gap(md.u(12.))
        .h(md.u(22.))
        .px(md.u(10.))
        .bg(panel)
        .border_t_1()
        .border_color(line)
        .text_size(md.u(10.))
        .text_color(dim)
        .child(div().flex().child(seg("●", c("ok"), false)).child(" midnad · 7 terminals"))
        .child(div().flex_1())
        .child("⌘K commands");
    div()
        .w_full()
        .h(md.u(420.))
        .flex()
        .flex_col()
        .rounded(md.u(10.))
        .overflow_hidden()
        .border_1()
        .border_color(rgb(hex(0x2A3540), 1.))
        .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.6), offset: point(px(0.), md.u(30.)), blur_radius: md.u(80.), spread_radius: px(0.), inset: false }])
        .bg(c("bg"))
        .text_color(fg)
        .font_family(PLEX)
        .text_size(md.u(12.))
        .child(div().flex_1().min_h_0().flex().child(side).child(div().flex_1().min_w_0().flex().flex_col().child(header).child(term)))
        .child(status)
}

/// `box-shadow: 0 0 0 10px` around the card, clipped with it.
fn halo(md: &Model) -> Div {
    div().absolute().left(md.u(CARD_X)).w(md.u(CARD_W)).h(md.u(md.card_h())).rounded(md.u(4.)).shadow(vec![BoxShadow { color: md.pal.halo(), offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: md.u(10.), inset: false }])
}

/// Horizontal distance between jump diamonds (design units).
fn pip_step() -> f32 {
    (PIPS_W - PIP_W) / (STEPS.len() as f32 - 1.)
}

/// The jumps across the top: a diamond each (made, put off, here, to do), joined by a line.
fn pips(md: &Model, mut cx: Option<&mut Context<MainWindow>>) -> Div {
    let (dim, ember, glow, fg, line) = (rgb(md.pal.dim, 1.), rgb(md.pal.ember, 1.), rgb(md.pal.glow, 1.), rgb(md.pal.fg, 1.), rgb(md.pal.line, 1.));
    let step = pip_step();
    let mut el = div().absolute().left(md.u(PIPS_X)).top(md.u(PIPS_TOP)).w(md.u(PIPS_W)).h(md.u(40.));
    // The line runs between the first and last diamond, behind them.
    el = el.child(div().absolute().left(md.u(PIP_W / 2.)).top(md.u(8.)).w(md.u(PIPS_W - PIP_W)).h(md.u(1.)).bg(line));
    for (i, r) in md.rail.iter().enumerate() {
        let pip: AnyElement = if r.done {
            diamond(md, 16., Icon::Tile, glow).into_any_element()
        } else if r.here {
            // Filled and outlined in ember, blinking (steps(2), 1.6s).
            let d = div().child(diamond(md, 16., Icon::Tile, ember)).child(div().absolute().left_0().top_0().child(diamond(md, 16., Icon::TileOutline, ember)));
            if cx.is_some() {
                d.with_animation(SharedString::from(format!("setup-blink-{i}")), Animation::new(Duration::from_millis(1600)).repeat(), |el, t| el.opacity(if t < 0.5 { 1. } else { 0.25 })).into_any_element()
            } else {
                d.into_any_element()
            }
        } else if r.later {
            diamond(md, 16., Icon::TileDashed, ember).into_any_element()
        } else {
            diamond(md, 16., Icon::TileOutline, dim).into_any_element()
        };
        let col = div()
            .absolute()
            .left(md.u(i as f32 * step))
            .top_0()
            .w(md.u(PIP_W))
            .flex()
            .flex_col()
            .items_center()
            .gap(md.u(10.))
            // Hide the line under the diamond.
            .child(div().relative().size(md.u(18.)).flex().items_center().justify_center().bg(rgb(md.pal.bg, 1.)).child(pip))
            .child(tracked(md, &r.name.to_uppercase(), MONO, 11., FontWeight::SEMIBOLD, 0.14, if r.here { fg } else { dim }));
        el = el.child(clickable(col, SharedString::from(format!("setup-jump-{i}")), Action::Jump(r.step), cx.as_deref_mut()));
    }
    el
}

/// Where to finish a step later, and what to ask an agent (if one can do it).
fn finish_later(s: Step) -> (&'static str, Option<&'static str>) {
    match s {
        Step::Daemon | Step::Notifications => ("Settings ▸ Permissions", None),
        Step::Project => ("Settings ▸ Projects", copy(s).ask),
        Step::Webhooks => ("Settings ▸ Webhooks", copy(s).ask),
        Step::Theme => ("Settings ▸ Look", None),
    }
}

/// The card's right side: a drawing of what the step does (Done: what's left).
fn guide_panel(md: &Model, cx: Option<&mut Context<MainWindow>>) -> Div {
    let glow = md.pal.glow;
    div()
        .relative()
        .w(md.u(GUIDE_W))
        .flex_none()
        .min_h(md.u(CARD_MIN_H - 2.))
        .border_l_1()
        .border_color(rgb(md.pal.line, 1.))
        .bg(rgb(md.pal.guide, 1.))
        .flex()
        .items_center()
        .justify_center()
        .p(md.u(40.))
        // The panel's soft teal light.
        .child(div().absolute().left(md.u(GUIDE_W / 2. - 60.)).top(md.u(CARD_MIN_H / 2. - 60.)).size(md.u(120.)).rounded_full().shadow(vec![BoxShadow { color: rgb(glow, 0.07), offset: point(px(0.), px(0.)), blur_radius: md.u(160.), spread_radius: md.u(80.), inset: false }]))
        .child(guide(md, cx))
}

fn guide(md: &Model, _cx: Option<&mut Context<MainWindow>>) -> AnyElement {
    match md.stage {
        Stage::Intro => guide_arrival(md).into_any_element(),
        Stage::At(Step::Daemon) => guide_daemon(md).into_any_element(),
        Stage::At(Step::Notifications) => guide_notifications(md).into_any_element(),
        Stage::At(Step::Project) => guide_project(md).into_any_element(),
        Stage::At(Step::Webhooks) => guide_webhooks(md).into_any_element(),
        // The theme step draws its own card (`theme_card`).
        Stage::At(Step::Theme) => div().into_any_element(),
        Stage::Done => guide_done(md).into_any_element(),
    }
}

/// A bordered box in the guide's style.
fn gbox(md: &Model, lit: bool) -> Div {
    div().border_1().border_color(if lit { rgb(md.pal.glow, 0.5) } else { rgb(md.pal.line, 1.) }).rounded(md.u(4.)).bg(rgb(md.pal.panel, 1.))
}

fn gtext(md: &Model, text: impl Into<SharedString>, size: f32, weight: FontWeight, color: Hsla) -> Div {
    div().font_family(PLEX).text_size(md.u(size)).font_weight(weight).text_color(color).child(text.into())
}

fn gmono(md: &Model, text: impl Into<SharedString>, size: f32, color: Hsla) -> Div {
    div().font_family(MONO).text_size(md.u(size)).text_color(color).child(text.into())
}

/// Arrival: the five jumps on an arc, and how long they take.
fn guide_arrival(md: &Model) -> Div {
    let (glow, dim, fg, ember) = (rgb(md.pal.glow, 1.), rgb(md.pal.dim, 1.), rgb(md.pal.fg, 1.), rgb(md.pal.ember, 1.));
    let (w, h, k) = (400., 340., 0.82);
    let mut el = div().relative().w(md.u(w)).h(md.u(h));
    for i in 0..STEPS.len() {
        let a = std::f32::consts::PI * (0.15 + i as f32 * 0.175);
        let (x, y) = (w / 2. - a.cos() * 200. * k, 250. - a.sin() * 200. * k);
        el = el
            .child(div().absolute().left(md.u(x - 22.)).top(md.u(y - 22.)).child(diamond(md, 44., Icon::TileOutline, glow)))
            .child(div().absolute().left(md.u(x - 20.)).top(md.u(y + 32.)).w(md.u(40.)).flex().justify_center().child(gmono(md, format!("0{}", i + 1), 11., dim)));
    }
    el.child(div().absolute().left_0().top(md.u(214.)).w(md.u(w)).flex().flex_col().items_center().gap(md.u(4.)).child(tracked(md, "FIVE JUMPS", MONO, 12., FontWeight::SEMIBOLD, 0.22, ember)).child(div().font_family(CHAKRA).font_weight(FontWeight::BOLD).text_size(md.u(36.)).text_color(fg).child("~2 min")))
}

/// Background daemon: midna quits, midnad keeps the shells running.
fn guide_daemon(md: &Model) -> Div {
    let (glow, dim, fg, ember) = (rgb(md.pal.glow, 1.), rgb(md.pal.dim, 1.), rgb(md.pal.fg, 1.), rgb(md.pal.ember, 1.));
    let row = |name: &str, tag: &str, color: Hsla| div().flex().justify_between().items_center().child(gtext(md, name.to_string(), 16., FontWeight::SEMIBOLD, fg)).child(tracked(md, tag, MONO, 11., FontWeight::SEMIBOLD, 0.14, color));
    div()
        .w(md.u(400.))
        .flex()
        .flex_col()
        .child(gbox(md, false).px(md.u(18.)).py(md.u(16.)).child(row("midna", "QUIT · UPDATING", ember)))
        .child(div().ml(md.u(36.)).h(md.u(36.)).border_l_2().border_dashed().border_color(rgb(md.pal.line, 1.)))
        .child(
            gbox(md, true)
                .px(md.u(18.))
                .py(md.u(16.))
                .flex()
                .flex_col()
                .gap(md.u(10.))
                .child(row("midnad", "● KEEPS RUNNING", glow))
                .children(["api · claude", "migrate · waiting for you", "tests · npm run watch"].map(|t| gmono(md, format!("▸ {t}"), 13., dim))),
        )
}

/// Notifications: three examples, fading.
fn guide_notifications(md: &Model) -> Div {
    let (dim, fg) = (rgb(md.pal.dim, 1.), rgb(md.pal.fg, 1.));
    let card = |i: usize, title: &str, sub: &str, when: &str| {
        let fade = 1. - i as f32 * 0.28;
        div()
            .flex()
            .gap(md.u(12.))
            .px(md.u(16.))
            .py(md.u(14.))
            .rounded(md.u(14.))
            .bg(rgb(md.pal.toast, 0.95 - i as f32 * 0.25))
            .border_1()
            .border_color(rgb(md.pal.line, 1.))
            .opacity(fade)
            .child(div().size(md.u(34.)).flex_none().rounded(md.u(8.)).bg(rgb(md.pal.panel, 1.)).border_1().border_color(rgb(md.pal.line, 1.)).flex().items_center().justify_center().child(diamond(md, 9., Icon::Tile, rgb(md.pal.glow, 1.))))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .child(div().flex().justify_between().child(gtext(md, "midna", 13., FontWeight::SEMIBOLD, fg)).child(gtext(md, when.to_string(), 13., FontWeight::NORMAL, dim)))
                    .child(gtext(md, title.to_string(), 14., FontWeight::SEMIBOLD, fg))
                    .child(gtext(md, sub.to_string(), 13., FontWeight::NORMAL, dim)),
            )
    };
    div()
        .w(md.u(390.))
        .flex()
        .flex_col()
        .gap(md.u(12.))
        .child(card(0, "Approve db:migrate on staging?", "migrate · zonai", "now"))
        .child(card(1, "Asked a question", "golden · drops-app", "12m"))
        .child(card(2, "exit 1", "flutter build · drops-app", "9m"))
        .child(div().mt(md.u(6.)).child(tracked(md, "CLICK ONE → THAT TERMINAL", MONO, 11., FontWeight::SEMIBOLD, 0.1, dim)))
}

/// First project: a folder dropped in.
fn guide_project(md: &Model) -> Div {
    let (glow, dim, fg) = (rgb(md.pal.glow, 1.), rgb(md.pal.dim, 1.), rgb(md.pal.fg, 1.));
    let folder = div()
        .relative()
        .w(md.u(64.))
        .h(md.u(50.))
        .mt(md.u(10.))
        .border_2()
        .border_color(glow)
        .rounded(md.u(4.))
        .child(div().absolute().left(md.u(-2.)).top(md.u(-12.)).w(md.u(28.)).h(md.u(12.)).border_2().border_b_0().border_color(glow).rounded_t(md.u(3.)));
    div()
        .w(md.u(400.))
        .h(md.u(290.))
        .border_2()
        .border_dashed()
        .border_color(rgb(md.pal.glow, 0.5))
        .rounded(md.u(6.))
        .bg(rgb(md.pal.glow, 0.03))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(md.u(14.))
        .child(folder)
        .child(div().font_family(CHAKRA).font_weight(FontWeight::BOLD).text_size(md.u(20.)).text_color(fg).child("Drop a folder"))
        .child(gmono(md, "~/Development/my-app", 13., dim))
        .child(tracked(md, "⌘1 · SHELL · GIT STATUS", MONO, 11., FontWeight::SEMIBOLD, 0.12, glow))
}

/// Webhooks: a pull request starts an agent.
fn guide_webhooks(md: &Model) -> Div {
    let (dim, fg) = (rgb(md.pal.dim, 1.), rgb(md.pal.fg, 1.));
    let steps = [("GitHub", "PR #212 opened · drops-app"), ("Tailscale Funnel", "midnad’s public URL, free"), ("midnad", "matches your trigger"), ("claude", "starts in a new terminal")];
    let mut col = div().w(md.u(410.)).flex().flex_col();
    for (i, (name, what)) in steps.iter().enumerate() {
        if i > 0 {
            col = col.child(div().ml(md.u(26.)).h(md.u(14.)).border_l_2().border_color(rgb(md.pal.line, 1.)));
        }
        col = col.child(gbox(md, i == steps.len() - 1).px(md.u(18.)).py(md.u(13.)).flex().justify_between().items_center().child(gtext(md, *name, 15., FontWeight::SEMIBOLD, fg)).child(gtext(md, *what, 13., FontWeight::NORMAL, dim)));
    }
    col
}

/// Done: the jumps made in one line, then each one left, where to finish it and what to ask.
fn guide_done(md: &Model) -> Div {
    let (glow, dim, fg, ember) = (rgb(md.pal.glow, 1.), rgb(md.pal.dim, 1.), rgb(md.pal.fg, 1.), rgb(md.pal.ember, 1.));
    let made: Vec<&str> = md.rail.iter().filter(|r| r.done).map(|r| r.name).collect();
    let left: Vec<&RailRow> = md.rail.iter().filter(|r| !r.done).collect();
    let mut col = div().w_full().flex().flex_col().gap(md.u(12.));
    col = col.child(
        div()
            .flex()
            .items_center()
            .gap(md.u(12.))
            .child(diamond(md, 9., Icon::Tile, glow))
            .child(gtext(md, if made.len() == 1 { "1 jump made".to_string() } else { format!("{} jumps made", made.len()) }, 15., FontWeight::SEMIBOLD, fg))
            .child(div().flex_1().min_w_0().child(gtext(md, made.join(" · "), 13., FontWeight::NORMAL, dim))),
    );
    if left.is_empty() {
        return col.child(div().mt(md.u(10.)).child(gtext(md, "Nothing left to do.", 15., FontWeight::NORMAL, dim)));
    }
    col = col.child(div().mt(md.u(12.)).child(tracked(md, "WAITING FOR YOU", MONO, 11., FontWeight::SEMIBOLD, 0.2, dim)));
    for r in left {
        let (place, ask) = finish_later(r.step);
        col = col.child(
            div()
                .px(md.u(18.))
                .py(md.u(14.))
                .border_1()
                .border_color(rgb(md.pal.line, 1.))
                .rounded(md.u(4.))
                .flex()
                .flex_col()
                .gap(md.u(5.))
                .child(div().flex().justify_between().child(gtext(md, r.name, 15., FontWeight::SEMIBOLD, fg)).child(tracked(md, r.result, MONO, 11., FontWeight::SEMIBOLD, 0.14, ember)))
                .child(gtext(md, place, 13., FontWeight::NORMAL, dim))
                .children(ask.map(|a| gmono(md, format!("or {} → “{a}”", md.command_key), 12., dim))),
        );
    }
    col
}

/// Top bar, jumps, card and footer: the window's content above the tiles. The card is at
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
        .child(pips(md, cx.as_deref_mut()))
        .child(
            div()
                .absolute()
                .left(md.u(40.))
                .top(md.u(DESIGN_H - 28. - 15.))
                .w(md.u(DESIGN_W - 80.))
                .flex()
                .justify_between()
                .child(tracked(md, if md.preview.is_some() { "← → BROWSE THEMES · ⌘K ASK AN AGENT" } else { "↩ CONTINUE · ESC LATER · ⌘K ASK AN AGENT" }, MONO, 12., FontWeight::NORMAL, 0.12, dim))
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

// ------------------------------------------------------------------ changing colors

/// Where the tiles' color ripple starts (the theme step's browse controls), its pace, and the
/// fade everything else takes (ThemeStep-B: CSS transitions .45s, tiles .4s + 26ms per cell;
/// Reduce motion: everything .2s, no ripple).
const SHIFT_ORIGIN: (f32, f32) = (8., 9.);
const SHIFT_PER_CELL_MS: f32 = 26.;
const SHIFT_TILE_MS: f32 = 400.;
const SHIFT_FADE_MS: f32 = 450.;
const SHIFT_REDUCED_MS: f32 = 200.;

/// What the screen wears: its palette and (theme step) the preview's theme.
#[derive(Clone, PartialEq)]
struct Look {
    pal: Pal,
    preview: Option<ThemeDef>,
}

/// A change of colors in flight.
#[derive(Clone)]
struct Shift {
    seq: u64,
    from: Look,
    to: Look,
    start: std::time::Instant,
    reduced: bool,
    /// Both looks are the theme step (browsing): the content fades too. Otherwise the step
    /// changed and the card makes its own entrance.
    content: bool,
    total: f32,
}

thread_local! {
    static SHOWN: std::cell::RefCell<Option<Look>> = const { std::cell::RefCell::new(None) };
    static SHIFT: std::cell::RefCell<Option<Shift>> = const { std::cell::RefCell::new(None) };
}

/// CSS `ease`.
fn ease(k: f32) -> f32 {
    bezier(0.25, 0.1, 0.25, 1., k.clamp(0., 1.))
}

impl Shift {
    /// Everything but the tiles: one fade.
    fn k(&self, ms: f32) -> f32 {
        ease(ms / if self.reduced { SHIFT_REDUCED_MS } else { SHIFT_FADE_MS })
    }

    fn look(&self, ms: f32) -> Look {
        let k = self.k(ms);
        let preview = match (&self.from.preview, &self.to.preview) {
            (Some(a), Some(b)) => Some(mix_def(a, b, k)),
            (_, b) => b.clone(),
        };
        Look { pal: mix_pal(&self.from.pal, &self.to.pal, k), preview }
    }

    /// Tile (c, r): rippling out from the browse controls, or the plain fade with Reduce motion.
    fn tile_k(&self, ms: f32, c: usize, r: usize) -> f32 {
        if self.reduced {
            return self.k(ms);
        }
        let d = (c as f32 - SHIFT_ORIGIN.0).hypot(r as f32 - SHIFT_ORIGIN.1);
        ease((ms - d * SHIFT_PER_CELL_MS) / SHIFT_TILE_MS)
    }
}

/// Note what the screen wears now; a change starts a [`Shift`] from whatever is showing.
/// Returns the shift in flight, if any.
fn track_look(md: &Model, cx: &mut Context<MainWindow>) -> Option<Shift> {
    let to = Look { pal: md.pal, preview: md.preview.clone() };
    let now = std::time::Instant::now();
    let ms = |sh: &Shift| now.duration_since(sh.start).as_secs_f32() * 1000.;
    let prev = SHOWN.with(|c| c.replace(Some(to.clone())));
    if let Some(prev) = prev
        && prev != to
    {
        let running = SHIFT.with(|c| c.borrow().clone()).filter(|sh| ms(sh) < sh.total);
        let from = running.as_ref().map_or(prev.clone(), |sh| sh.look(ms(sh)));
        let reduced = crate::ui::twilight::reduce_motion();
        let total = if reduced {
            SHIFT_REDUCED_MS
        } else {
            let (cols, rows) = md.cells(CELL, CELL);
            let far = [(0., 0.), (cols as f32, 0.), (0., rows as f32), (cols as f32, rows as f32)].iter().map(|(x, y)| (x - SHIFT_ORIGIN.0).hypot(y - SHIFT_ORIGIN.1)).fold(0., f32::max);
            (far * SHIFT_PER_CELL_MS + SHIFT_TILE_MS).max(SHIFT_FADE_MS)
        };
        let seq = running.map_or(0, |sh| sh.seq) + 1;
        let content = prev.preview.is_some() && to.preview.is_some();
        SHIFT.with(|c| *c.borrow_mut() = Some(Shift { seq, from, to, start: now, reduced, content, total }));
        // Draw once more when it's over, without the fading copy.
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(total as u64 + 30)).await;
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
    }
    SHIFT.with(|c| c.borrow().clone()).filter(|sh| ms(sh) < sh.total)
}

/// The background and tiles mid-shift, `ms` in.
fn shifting_tiles(md: &Model, sh: &Shift, ms: f32) -> Div {
    let (cols, rows) = md.cells(CELL, CELL);
    let mut el = div().absolute().inset_0().bg(rgb(mix3(sh.from.pal.bg, sh.to.pal.bg, sh.k(ms)), 1.));
    for r in 0..rows {
        for c in 0..cols {
            let pal = mix_pal(&sh.from.pal, &sh.to.pal, sh.tile_k(ms, c, r));
            el = tile(md, &pal, el, c, r, None);
        }
    }
    el
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
    // A change of colors (browsing themes, or entering and leaving the theme step) ripples the
    // tiles into the new palette; while browsing, a fading copy of the content (drawn only, on
    // top, clicks pass through) covers the live one.
    let shift = track_look(&md, cx);
    let (ripple_el, cover): (AnyElement, Option<AnyElement>) = match shift {
        Some(sh) => {
            let (md2, sh2) = (md.clone(), sh.clone());
            let total = sh.total;
            let tiles = div()
                .absolute()
                .inset_0()
                .with_animation(SharedString::from(format!("setup-shift-{}", sh.seq)), Animation::new(Duration::from_millis(total as u64)), move |el, d| el.child(shifting_tiles(&md2, &sh2, d * total)))
                .into_any_element();
            let cover = sh.content.then(|| {
                let (md3, sh3) = (md.clone(), sh.clone());
                div()
                    .absolute()
                    .inset_0()
                    .with_animation(SharedString::from(format!("setup-shift-cover-{}", sh.seq)), Animation::new(Duration::from_millis(total as u64)), move |el, d| {
                        let look = sh3.look(d * total);
                        let lerped = Model { pal: look.pal, preview: look.preview, ..md3.clone() };
                        el.child(content(&lerped, 1., None, None))
                    })
                    .into_any_element()
            });
            (tiles, cover)
        }
        _ => (ripple_el, None),
    };
    // The card replays its entrance on step changes; the opening already played the first one.
    let seq = m.onboarding.card_seq;
    let animate = (seq != m.onboarding.card_seq_opened).then_some(seq);
    let live = content(&md, 1., animate, Some(cx)).when(cover.is_some(), |d| d.opacity(0.));
    let el = div().id("setup-screen").absolute().inset_0().occlude().bg(rgb(md.pal.bg, 1.)).child(ripple_el).child(live).children(cover);
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
