//! Color tokens from the design boards (`.t-dark` / `.t-light` in docs/design/*.dc.html)
//! and font families.
use gpui_kit::*;

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
    /// Text on the need-colored badge (design: #15111C in both themes).
    pub badge_fg: Hsla,
    pub ui_font: SharedString,
    pub mono_font: SharedString,
    pub compact: bool,
}

impl Global for Theme {}

fn hex(s: u32) -> Hsla {
    rgb(s).into()
}

fn rgba_(r: u8, g: u8, b: u8, a: f32) -> Hsla {
    Rgba { r: r as f32 / 255., g: g as f32 / 255., b: b as f32 / 255., a }.into()
}

impl Theme {
    pub fn new(mode: ThemeMode, ui_font: SharedString, mono_font: SharedString) -> Theme {
        match mode {
            ThemeMode::Dark => Theme {
                mode,
                bg: hex(0x0F1117),
                panel: hex(0x151821),
                raised: hex(0x1D2130),
                line: hex(0x282D3B),
                fg: hex(0xE4E7EE),
                dim: hex(0x9AA2B3),
                accent: hex(0xB79AE8),
                accent_fg: hex(0x15111C),
                accent_soft: rgba_(183, 154, 232, 0.12),
                need: hex(0xF2A93B),
                need_ring: rgba_(242, 169, 59, 0.22),
                need_soft: rgba_(242, 169, 59, 0.10),
                ok: hex(0x4CC38A),
                err: hex(0xF2665C),
                work: hex(0x6EA2FF),
                term: hex(0x0B0D12),
                badge_fg: hex(0x15111C),
                ui_font,
                mono_font,
                compact: false,
            },
            ThemeMode::Light => Theme {
                mode,
                bg: hex(0xE6E9EF),
                panel: hex(0xF5F6F8),
                raised: hex(0xFFFFFF),
                line: hex(0xD3D8E0),
                fg: hex(0x151922),
                dim: hex(0x535D6E),
                accent: hex(0x6237A0),
                accent_fg: hex(0xFFFFFF),
                accent_soft: rgba_(98, 55, 160, 0.08),
                need: hex(0x9A5300),
                need_ring: rgba_(154, 83, 0, 0.20),
                need_soft: rgba_(154, 83, 0, 0.08),
                ok: hex(0x1B7346),
                err: hex(0xAE2219),
                work: hex(0x2758B8),
                term: hex(0xFCFCFD),
                badge_fg: hex(0xFFFFFF),
                ui_font,
                mono_font,
                compact: false,
            },
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
