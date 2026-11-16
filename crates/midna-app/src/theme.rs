//! The UI's colors, resolved from a `midna_proto::themes` theme (built-in, custom file, plus
//! `theme.colors` overrides), and font families.
use gpui_kit::*;
use midna_proto::themes::{self, ThemeDef};
use std::collections::HashMap;
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
    /// Another Mac's window showing a theme this Mac picked for it: that Mac's own ANSI
    /// colors, which its engines paint cells in. The terminal shows each as the same slot of
    /// `def.ansi`. None everywhere else.
    pub ansi_from: Option<[[u8; 3]; 16]>,
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
            ansi_from: None,
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

/// The themes other Macs' windows show (each Mac's own theme and look settings, resolved by its
/// window), and which windows show which Mac. Every other window shows this Mac's [`Theme`].
#[derive(Default)]
pub struct MacThemes {
    themes: HashMap<String, Theme>,
    windows: Vec<(AnyWindowHandle, String)>,
}

impl Global for MacThemes {}

/// The theme `window` shows: its Mac's when it shows another Mac, else this Mac's.
pub fn of<'a>(window: &Window, cx: &'a App) -> &'a Theme {
    let id = window.window_handle().window_id();
    cx.try_global::<MacThemes>()
        .and_then(|m| m.windows.iter().find(|(w, _)| w.window_id() == id).and_then(|(_, mac)| m.themes.get(mac)))
        .unwrap_or_else(|| cx.global::<Theme>())
}

/// `window` shows `mac` (a paired Mac's peer id; None: this Mac) from now on.
pub fn window_shows(window: &Window, mac: Option<&str>, cx: &mut App) {
    let handle = window.window_handle();
    let m = cx.default_global::<MacThemes>();
    m.windows.retain(|(w, _)| w.window_id() != handle.window_id());
    if let Some(mac) = mac {
        m.windows.push((handle, mac.to_string()));
    }
}

/// `window` closed.
pub fn window_closed(id: WindowId, cx: &mut App) {
    let m = cx.default_global::<MacThemes>();
    m.windows.retain(|(w, _)| w.window_id() != id);
    let shown: Vec<String> = m.windows.iter().map(|(_, mac)| mac.clone()).collect();
    m.themes.retain(|mac, _| shown.contains(mac));
}

/// Show `t` in every window of `mac` (the one that called this redraws itself).
pub fn set_mac(mac: &str, t: Theme, cx: &mut App) {
    let m = cx.default_global::<MacThemes>();
    if m.themes.get(mac).is_some_and(|old| old.def == t.def && old.ansi_from == t.ansi_from && old.compact == t.compact && old.ui_font == t.ui_font && old.mono_font == t.mono_font) {
        return;
    }
    m.themes.insert(mac.to_string(), t);
    let wins: Vec<AnyWindowHandle> = m.windows.iter().filter(|(_, x)| x == mac).map(|(w, _)| *w).collect();
    for w in wins {
        let _ = w.update(cx, |_, window, _| window.refresh());
    }
}

/// This Mac's picks for its other Macs' windows (Settings → Nearby): `nearby.mac_themes`, and
/// this Mac's `theme.colors` to go with a picked theme. Kept on this Mac, never sent over.
#[derive(Clone, Default, PartialEq, Debug)]
pub struct MacPicks {
    pub rules: Vec<String>,
    pub colors: Vec<String>,
}

impl Global for MacPicks {}

impl MacPicks {
    /// The theme picked for Mac `mac`'s windows: one of this Mac's themes, with this Mac's
    /// colors. None when there's no pick, or the picked theme is gone (that Mac's own shows).
    pub fn def_for(&self, mac: &str) -> Option<ThemeDef> {
        let id = midna_proto::settings::mac_theme(&self.rules, mac)?;
        resolve_mac(None, id, &self.colors)
    }
}

/// The theme this Mac picked for Mac `mac`'s windows, if any (see [`MacPicks`]).
pub fn picked_for(mac: &str, cx: &App) -> Option<ThemeDef> {
    cx.try_global::<MacPicks>()?.def_for(mac)
}

/// Read this Mac's picks from its daemon (`local`); when they changed, every other Mac's window
/// shows its theme again. A daemon too old to know `nearby.mac_themes` leaves them as they were.
pub fn refresh_picks(local: std::sync::Arc<dyn crate::backend::Backend>, cx: &mut App) {
    cx.spawn(async move |cx| {
        let picks = cx
            .background_executor()
            .spawn(async move {
                let get = |k: &str| -> Option<Vec<String>> {
                    let v = local.call("settings.get", serde_json::json!({ "key": k })).ok()?;
                    Some(v.get("value")?.as_array()?.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                };
                Some(MacPicks { rules: get("nearby.mac_themes")?, colors: get("theme.colors").unwrap_or_default() })
            })
            .await;
        let Some(picks) = picks else { return };
        let _ = cx.update(|cx| set_picks(picks, cx));
    })
    .detach();
}

/// This Mac's picks are `picks` from now on; other Macs' windows follow when they changed.
pub fn set_picks(picks: MacPicks, cx: &mut App) {
    if cx.try_global::<MacPicks>() == Some(&picks) {
        return;
    }
    cx.set_global(picks);
    crate::windows::reapply_mac_themes(cx);
}

/// A theme as another Mac's `themes.list` sends it.
pub fn from_info(i: &themes::ThemeInfo) -> Option<ThemeDef> {
    let ui: Vec<String> = themes::UI_TOKENS.iter().map(|k| i.colors.get(*k).cloned()).collect::<Option<_>>()?;
    let v = serde_json::json!({ "id": i.id, "name": i.name, "dark": i.kind == "dark", "ui": ui, "ansi": i.terminal });
    ThemeDef::from_cache(&v)
}

/// The theme another Mac's settings pick: `id` from that Mac's themes (`themes.list`; None
/// before it arrives), else this Mac's copy of it, with that Mac's `theme.colors` applied. A
/// theme neither has is None: the window keeps this Mac's theme.
pub fn resolve_mac(mac_themes: Option<&[ThemeDef]>, id: &str, rules: &[String]) -> Option<ThemeDef> {
    let local = cached_themes();
    let mut t = mac_themes.and_then(|all| all.iter().find(|t| t.id == id)).or_else(|| local.iter().find(|t| t.id == id)).cloned()?;
    themes::apply_overrides(&mut t, rules);
    Some(t)
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

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    fn theme(id: &str) -> Theme {
        Theme::from_def(&themes::builtin(id).unwrap(), "ui".into(), "mono".into())
    }

    struct Probe;

    impl Render for Probe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    #[test]
    fn another_macs_themes_come_over_as_data() {
        let mut custom = themes::builtin("nord").unwrap();
        custom.id = "ember".into();
        custom.name = "Ember".into();
        custom.ui[0] = [28, 18, 14];
        for t in [themes::builtin("dracula").unwrap(), custom.clone()] {
            let back = from_info(&t.info()).unwrap();
            assert_eq!((back.id.as_str(), back.dark, back.ui, back.ansi), (t.id.as_str(), t.dark, t.ui, t.ansi));
        }
        let mut broken = custom.info();
        broken.colors.remove("bg");
        assert!(from_info(&broken).is_none(), "a theme missing a color isn't shown");
    }

    #[test]
    fn another_macs_theme_falls_back_never_fails() {
        let mut ember = themes::builtin("nord").unwrap();
        ember.id = "ember".into();
        let list = vec![ember.clone()];
        assert_eq!(resolve_mac(Some(&list), "ember", &[]).map(|t| t.ui), Some(ember.ui), "that Mac's own theme");
        assert_eq!(resolve_mac(None, "gruvbox", &[]).map(|t| t.id), Some("gruvbox".into()), "this Mac's copy before its list arrives");
        assert!(resolve_mac(None, "ember", &[]).is_none(), "neither Mac has it: this Mac's theme shows");
        let red = resolve_mac(Some(&list), "ember", &["accent = #FF0000".into(), "nord:bg = #000000".into()]).unwrap();
        assert_eq!(red.get("accent"), Some([255, 0, 0]), "that Mac's theme.colors apply");
        assert_eq!(red.get("bg"), ember.get("bg"), "a rule for another theme doesn't");
    }

    #[test]
    fn a_picked_theme_is_this_macs_with_this_macs_colors() {
        let mini = "0a1b2c3d4e5f60718293a4b5c6d7e8f9";
        let picks = MacPicks { rules: vec!["0a1b2c3d4e5f = gruvbox".into(), "99887766 = gone".into()], colors: vec!["gruvbox:accent = #00FF00".into()] };
        let def = picks.def_for(mini).expect("picked");
        assert_eq!(def.id, "gruvbox");
        assert_eq!(def.get("accent"), Some([0, 255, 0]), "this Mac's theme.colors come with it");
        assert!(picks.def_for("ffeeddccbbaa").is_none(), "no pick: that Mac's own theme");
        assert!(picks.def_for("998877665544").is_none(), "a theme this Mac no longer has: that Mac's own");
    }

    #[gpui_kit::test]
    fn only_that_macs_windows_show_its_theme(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_global(theme("twilight")));
        let here = cx.add_window(|_, _| Probe);
        let there = cx.add_window(|_, _| Probe);
        let other = cx.add_window(|_, _| Probe);
        let shows = |w: WindowHandle<Probe>, cx: &mut TestAppContext| w.update(cx, |_, window, cx| of(window, cx).id.clone()).unwrap();
        there.update(cx, |_, window, cx| window_shows(window, Some("mini"), cx)).unwrap();
        other.update(cx, |_, window, cx| window_shows(window, Some("studio"), cx)).unwrap();
        assert_eq!(shows(there, cx), "twilight", "this Mac's theme until that Mac's arrives");
        cx.update(|cx| set_mac("mini", theme("gruvbox"), cx));
        assert_eq!((shows(here, cx), shows(there, cx), shows(other, cx)), ("twilight".into(), "gruvbox".into(), "twilight".into()));
        cx.update(|cx| cx.set_global(theme("daylight")));
        assert_eq!((shows(here, cx), shows(there, cx)), ("daylight".into(), "gruvbox".into()), "this Mac's theme changes only its windows");
        let id = there.update(cx, |_, window, _| window.window_handle().window_id()).unwrap();
        cx.update(|cx| window_closed(id, cx));
        assert!(cx.update(|cx| cx.global::<MacThemes>().themes.is_empty()), "a Mac's theme goes with its last window");
    }
}
