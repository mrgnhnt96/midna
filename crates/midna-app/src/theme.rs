//! The UI's colors, resolved from a `midna_proto::themes` theme (built-in, custom file, plus
//! `theme.colors` overrides), and font families.
use gpui_kit::*;
use midna_proto::themes::{self, ThemeDef};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

pub const UI_FONT: &str = "Atkinson Hyperlegible Next";
pub const MONO_FONT: &str = "JetBrains Mono";
pub const UI_FALLBACK: &str = "Helvetica Neue";
pub const MONO_FALLBACK: &str = "Menlo";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeMode {
    Dark,
    Light,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct Theme {
    /// The resolved theme this was built from (`theme.colors` applied).
    pub def: ThemeDef,
    /// The theme's id (`midna_proto::themes`) and display name.
    pub id: SharedString,
    pub name: SharedString,
    /// Dark or light kind; mode-dependent styling (scrims, chart palettes) keys off this.
    pub mode: ThemeMode,
    pub bg: Hsla,
    pub panel: Hsla,
    pub raised: Hsla,
    pub line: Hsla,
    pub fg: Hsla,
    pub dim: Hsla,
    pub accent: Hsla,
    pub accent_fg: Hsla,
    pub accent_soft: Hsla,
    pub need: Hsla,
    pub need_ring: Hsla,
    pub need_soft: Hsla,
    pub ok: Hsla,
    pub err: Hsla,
    pub work: Hsla,
    pub term: Hsla,
    /// Text on the need-colored badge (the theme's bg in dark themes, white in light ones).
    pub badge_fg: Hsla,
    /// Selected text in a terminal.
    pub selection: Hsla,
    /// The terminal's 16 ANSI colors (also pushed to midnad, which owns the engines).
    pub ansi: [Hsla; 16],
    pub ui_font: SharedString,
    pub mono_font: SharedString,
    pub compact: bool,
}

impl Global for Theme {}

fn hex(s: u32) -> Hsla {
    rgb(s).into()
}

pub fn rgb3(c: [u8; 3], a: f32) -> Hsla {
    Rgba { r: c[0] as f32 / 255., g: c[1] as f32 / 255., b: c[2] as f32 / 255., a }.into()
}

impl Theme {
    pub fn from_def(d: &ThemeDef, ui_font: SharedString, mono_font: SharedString) -> Theme {
        let c = |k: &str| rgb3(d.get(k).unwrap_or([255, 0, 255]), 1.);
        let a = themes::alphas(d.dark);
        let soft = |k: &str, al: f32| rgb3(d.get(k).unwrap_or([255, 0, 255]), al);
        Theme {
            def: d.clone(),
            id: d.id.clone().into(),
            name: d.name.clone().into(),
            mode: if d.dark { ThemeMode::Dark } else { ThemeMode::Light },
            bg: c("bg"),
            panel: c("panel"),
            raised: c("raised"),
            line: c("line"),
            fg: c("fg"),
            dim: c("dim"),
            accent: c("accent"),
            accent_fg: c("accent-fg"),
            accent_soft: soft("accent", a.accent_soft),
            need: c("need"),
            need_ring: soft("need", a.need_ring),
            need_soft: soft("need", a.need_soft),
            ok: c("ok"),
            err: c("err"),
            work: c("work"),
            term: c("term"),
            badge_fg: rgb3(themes::badge_fg(d), 1.),
            selection: soft("accent", a.selection),
            ansi: d.ansi.map(|x| rgb3(x, 1.)),
            ui_font,
            mono_font,
            compact: false,
        }
    }

    /// A custom status color (`set_status`): a named color from `midna_proto::STATUS_COLORS`,
    /// mapped onto this theme's tokens where one fits, or `#rrggbb`. Unknown values read as `need`.
    pub fn status_color(&self, c: &str) -> Hsla {
        let dark = self.mode == ThemeMode::Dark;
        match c.trim().to_ascii_lowercase().as_str() {
            "red" => self.err,
            "amber" => self.need,
            "green" => self.ok,
            "blue" => self.work,
            "purple" => self.accent,
            "gray" | "grey" => self.dim,
            "orange" => hex(if dark { 0xF28A4B } else { 0xB4480C }),
            "yellow" => hex(if dark { 0xE6D25A } else { 0x8A7300 }),
            "teal" => hex(if dark { 0x3FC4C0 } else { 0x0E7A78 }),
            "pink" => hex(if dark { 0xF07AB6 } else { 0xB0286D }),
            h if h.len() == 7 && h.starts_with('#') => u32::from_str_radix(&h[1..], 16).map(hex).unwrap_or(self.need),
            _ => self.need,
        }
    }

    pub fn tone(&self, t: Option<crate::model::Tone>) -> Hsla {
        use crate::model::Tone::*;
        match t {
            Some(Dim) => self.dim,
            Some(Ok) => self.ok,
            Some(Err) => self.err,
            Some(Need) => self.need,
            Some(Work) => self.work,
            Some(Accent) => self.accent,
            _ => self.fg,
        }
    }
}

/// The theme for the first frames, before the daemon's settings arrive: the one shown last
/// (`app-state.json` "theme", written by `MainWindow::show_theme`), else Twilight or Daylight
/// by the macOS appearance. No flash of the wrong colors on launch.
pub fn startup_def(backend: &std::sync::Arc<dyn crate::backend::Backend>, system_dark: bool) -> ThemeDef {
    let cached: serde_json::Value = crate::ui::statusbar::load_state(backend, "theme");
    ThemeDef::from_cache(&cached).unwrap_or_else(|| themes::builtin(if system_dark { themes::DEFAULT_DARK } else { themes::DEFAULT_LIGHT }).expect("built-in"))
}

/// Remember `def` for the next launch's first frames (only when it changed).
pub fn cache_def(backend: &std::sync::Arc<dyn crate::backend::Backend>, def: &ThemeDef) {
    static LAST: Mutex<Option<serde_json::Value>> = Mutex::new(None);
    let v = def.to_cache();
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if last.as_ref() == Some(&v) {
        return;
    }
    *last = Some(v.clone());
    crate::ui::statusbar::update_state(backend, "theme", v);
}

/// Custom theme files are re-read only when the folder's listing or a file's mtime changes.
type Sig = Vec<(PathBuf, Option<SystemTime>)>;
static CACHE: Mutex<Option<(PathBuf, Sig, Vec<ThemeDef>, Vec<String>)>> = Mutex::new(None);

fn signature(dir: &Path) -> Sig {
    let mut v: Sig = std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| (e.path(), e.metadata().and_then(|m| m.modified()).ok())).filter(|(p, _)| p.extension().is_some_and(|x| x == "json")).collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// Every theme (built-ins, then `$MIDNA_HOME/themes/*.json`) and the files that failed to load.
pub fn all_themes(home: &Path) -> (Vec<ThemeDef>, Vec<String>) {
    let dir = themes::themes_dir(home);
    let sig = signature(&dir);
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((d, s, t, e)) = c.as_ref()
        && *d == dir
        && *s == sig
    {
        return (t.clone(), e.clone());
    }
    let (t, e) = themes::load_all(&dir);
    for err in &e {
        eprintln!("midna-app: theme {err}");
    }
    *c = Some((dir, sig, t.clone(), e.clone()));
    (t, e)
}

/// The themes [`all_themes`] last loaded (built-ins only before the first load).
pub fn cached_themes() -> Vec<ThemeDef> {
    CACHE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|c| c.2.clone()).unwrap_or_else(themes::builtins)
}

/// True when a theme file was added, removed or edited since the last [`all_themes`].
pub fn themes_changed(home: &Path) -> bool {
    let dir = themes::themes_dir(home);
    let c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    c.as_ref().is_none_or(|(d, s, _, _)| *d != dir || *s != signature(&dir))
}

/// Load the bundled OFL fonts; returns the (ui, mono) family names to use, falling back to
/// system fonts when embedding fails.
pub fn load_fonts(cx: &mut App) -> (SharedString, SharedString) {
    use std::borrow::Cow;
    let fonts: Vec<Cow<'static, [u8]>> = vec![
        Cow::Borrowed(include_bytes!("../assets/fonts/AtkinsonHyperlegibleNext-Regular.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/AtkinsonHyperlegibleNext-Medium.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/AtkinsonHyperlegibleNext-Bold.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/JetBrainsMono-SemiBold.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/JetBrainsMono-Bold.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/JetBrainsMono-Italic.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/JetBrainsMono-BoldItalic.ttf")),
        // Setup's Twilight Tiles screen (ui/setup_screen.rs): display and body faces.
        Cow::Borrowed(include_bytes!("../assets/fonts/ChakraPetch-SemiBold.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/ChakraPetch-Bold.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/IBMPlexSans-Medium.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/IBMPlexSans-SemiBold.ttf")),
    ];
    if let Err(e) = cx.text_system().add_fonts(fonts) {
        eprintln!("midna-app: could not load bundled fonts: {e:#}");
    }
    let names = cx.text_system().all_font_names();
    let has = |n: &str| names.iter().any(|x| x == n);
    let ui = if has(UI_FONT) { UI_FONT } else { UI_FALLBACK };
    let mono = if has(MONO_FONT) { MONO_FONT } else { MONO_FALLBACK };
    (ui.into(), mono.into())
}
