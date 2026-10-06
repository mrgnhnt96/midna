//! Color themes: the eight built-ins, custom theme files (`$MIDNA_HOME/themes/<id>.json`) and
//! `theme.colors` overrides. Plain data (no GUI types) so midnad, the CLI and the app share it.
//!
//! A theme is 13 UI tokens ([`UI_TOKENS`]) plus the terminal's 16 ANSI colors ([`ANSI_NAMES`]).
//! The board is docs/design/Themes.dc.html.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

pub type Rgb = [u8; 3];

/// The UI tokens, in [`ThemeDef::ui`] order.
pub const UI_TOKENS: [&str; 13] = ["bg", "panel", "raised", "line", "fg", "dim", "accent", "accent-fg", "need", "ok", "err", "work", "term"];

/// The terminal's 16 colors, in [`ThemeDef::ansi`] order (ANSI 0–15).
pub const ANSI_NAMES: [&str; 16] = [
    "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
    "bright-black", "bright-red", "bright-green", "bright-yellow", "bright-blue", "bright-magenta", "bright-cyan", "bright-white",
];

/// Built-in theme ids, dark ones first.
pub const BUILTIN_IDS: [&str; 8] = ["twilight", "nord", "dracula", "gruvbox", "tokyo-night", "daylight", "solarized-light", "latte"];

/// What `theme` follows when set to `system` and `theme.dark` / `theme.light` are unset.
pub const DEFAULT_DARK: &str = "twilight";
pub const DEFAULT_LIGHT: &str = "daylight";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeDef {
    pub id: String,
    pub name: String,
    pub dark: bool,
    pub ui: [Rgb; 13],
    pub ansi: [Rgb; 16],
    /// The file it came from (custom themes).
    pub path: Option<PathBuf>,
    pub extends: Option<String>,
}

impl ThemeDef {
    pub fn get(&self, token: &str) -> Option<Rgb> {
        if let Some(i) = UI_TOKENS.iter().position(|t| *t == token) {
            return Some(self.ui[i]);
        }
        ansi_index(token).map(|i| self.ansi[i])
    }

    fn set(&mut self, token: &str, c: Rgb) -> bool {
        if let Some(i) = UI_TOKENS.iter().position(|t| *t == token) {
            self.ui[i] = c;
        } else if let Some(i) = ansi_index(token) {
            self.ansi[i] = c;
        } else {
            return false;
        }
        true
    }

    /// The resolved theme as JSON (the app caches the last one for its first frames).
    pub fn to_cache(&self) -> Value {
        serde_json::json!({
            "id": self.id, "name": self.name, "dark": self.dark,
            "ui": self.ui.iter().map(|c| to_hex(*c)).collect::<Vec<_>>(),
            "ansi": self.ansi.iter().map(|c| to_hex(*c)).collect::<Vec<_>>(),
        })
    }

    pub fn from_cache(v: &Value) -> Option<ThemeDef> {
        let colors = |k: &str| -> Option<Vec<Rgb>> { v.get(k)?.as_array()?.iter().map(|c| c.as_str().and_then(parse_hex)).collect() };
        Some(ThemeDef {
            id: v.get("id")?.as_str()?.to_string(),
            name: v.get("name")?.as_str()?.to_string(),
            dark: v.get("dark")?.as_bool()?,
            ui: colors("ui")?.try_into().ok()?,
            ansi: colors("ansi")?.try_into().ok()?,
            path: None,
            extends: None,
        })
    }

    /// Same colors as built-in `id` (no overrides).
    pub fn is_builtin(&self, id: &str) -> bool {
        builtin(id).is_some_and(|b| b.ui == self.ui && b.ansi == self.ansi)
    }

    pub fn info(&self) -> ThemeInfo {
        ThemeInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            kind: if self.dark { "dark" } else { "light" }.into(),
            builtin: self.path.is_none(),
            path: self.path.as_ref().map(|p| p.display().to_string()),
            extends: self.extends.clone(),
            colors: UI_TOKENS.iter().zip(self.ui).map(|(k, c)| (k.to_string(), to_hex(c))).collect(),
            terminal: self.ansi.iter().map(|c| to_hex(*c)).collect(),
        }
    }
}

/// One theme as `themes.list` reports it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ThemeInfo {
    pub id: String,
    pub name: String,
    /// dark | light
    pub kind: String,
    pub builtin: bool,
    /// The theme file, for custom themes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extends: Option<String>,
    /// UI token -> #RRGGBB.
    pub colors: BTreeMap<String, String>,
    /// ANSI 0–15 as #RRGGBB.
    pub terminal: Vec<String>,
}

/// `black`, `bright-red`, … or `ansi0`…`ansi15`.
pub fn ansi_index(token: &str) -> Option<usize> {
    if let Some(n) = token.strip_prefix("ansi") {
        return n.parse::<usize>().ok().filter(|n| *n < 16);
    }
    ANSI_NAMES.iter().position(|n| *n == token)
}

pub fn is_token(token: &str) -> bool {
    UI_TOKENS.contains(&token) || ansi_index(token).is_some()
}

/// `#RRGGBB` (or `RRGGBB`, `#RGB`).
pub fn parse_hex(s: &str) -> Option<Rgb> {
    let h = s.trim();
    let h = h.strip_prefix('#').unwrap_or(h);
    let full: String = match h.len() {
        3 => h.chars().flat_map(|c| [c, c]).collect(),
        6 => h.to_string(),
        _ => return None,
    };
    let n = u32::from_str_radix(&full, 16).ok()?;
    Some([(n >> 16) as u8, (n >> 8) as u8, n as u8])
}

pub fn to_hex(c: Rgb) -> String {
    format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

const fn h(n: u32) -> Rgb {
    [(n >> 16) as u8, (n >> 8) as u8, n as u8]
}

macro_rules! theme {
    ($id:literal, $name:literal, $dark:literal, [$($u:literal),* $(,)?], [$($a:literal),* $(,)?]) => {
        ($id, $name, $dark, [$(h($u)),*], [$(h($a)),*])
    };
}

type Builtin = (&'static str, &'static str, bool, [Rgb; 13], [Rgb; 16]);

// UI order: bg panel raised line fg dim accent accent-fg need ok err work term.
static BUILTINS: [Builtin; 8] = [
    theme!("twilight", "Midna Twilight", true,
        [0x0F1117, 0x151821, 0x1D2130, 0x282D3B, 0xE4E7EE, 0x9AA2B3, 0xB79AE8, 0x15111C, 0xF2A93B, 0x4CC38A, 0xF2665C, 0x6EA2FF, 0x0B0D12],
        [0x1D2130, 0xF2665C, 0x4CC38A, 0xF2A93B, 0x6EA2FF, 0xB79AE8, 0x3FC4C0, 0xC9CEDA, 0x4A5168, 0xFF8A80, 0x6FDBA6, 0xFFC56B, 0x93BBFF, 0xCDB6F2, 0x6ADBD6, 0xFFFFFF]),
    theme!("nord", "Nord", true,
        [0x262B35, 0x2E3440, 0x3B4252, 0x434C5E, 0xECEFF4, 0xA7B1C2, 0x88C0D0, 0x2E3440, 0xEBCB8B, 0xA3BE8C, 0xBF616A, 0x81A1C1, 0x2B303B],
        [0x3B4252, 0xBF616A, 0xA3BE8C, 0xEBCB8B, 0x81A1C1, 0xB48EAD, 0x88C0D0, 0xE5E9F0, 0x4C566A, 0xD0777F, 0xB5CE9E, 0xF2D89E, 0x93B3D2, 0xC5A1BF, 0x8FBCBB, 0xECEFF4]),
    theme!("dracula", "Dracula", true,
        [0x1E1F29, 0x282A36, 0x343746, 0x44475A, 0xF8F8F2, 0xA9AFCC, 0xBD93F9, 0x1E1F29, 0xFFB86C, 0x50FA7B, 0xFF5555, 0x8BE9FD, 0x22232E],
        [0x21222C, 0xFF5555, 0x50FA7B, 0xF1FA8C, 0xBD93F9, 0xFF79C6, 0x8BE9FD, 0xF8F8F2, 0x6272A4, 0xFF6E6E, 0x69FF94, 0xFFFFA5, 0xD6ACFF, 0xFF92DF, 0xA4FFFF, 0xFFFFFF]),
    theme!("gruvbox", "Gruvbox", true,
        [0x1D2021, 0x282828, 0x3C3836, 0x504945, 0xEBDBB2, 0xA89984, 0xD3869B, 0x1D2021, 0xFABD2F, 0xB8BB26, 0xFB4934, 0x83A598, 0x232323],
        [0x282828, 0xCC241D, 0x98971A, 0xD79921, 0x458588, 0xB16286, 0x689D6A, 0xA89984, 0x928374, 0xFB4934, 0xB8BB26, 0xFABD2F, 0x83A598, 0xD3869B, 0x8EC07C, 0xEBDBB2]),
    theme!("tokyo-night", "Tokyo Night", true,
        [0x13141C, 0x1A1B26, 0x24283B, 0x2F334D, 0xC0CAF5, 0x959FC9, 0xBB9AF7, 0x13141C, 0xE0AF68, 0x9ECE6A, 0xF7768E, 0x7AA2F7, 0x16161E],
        [0x15161E, 0xF7768E, 0x9ECE6A, 0xE0AF68, 0x7AA2F7, 0xBB9AF7, 0x7DCFFF, 0xA9B1D6, 0x414868, 0xFF8FA3, 0xB3E085, 0xEBC284, 0x94B6FA, 0xCCB0F9, 0x9AD9FF, 0xC0CAF5]),
    theme!("daylight", "Midna Daylight", false,
        [0xE6E9EF, 0xF5F6F8, 0xFFFFFF, 0xD3D8E0, 0x151922, 0x535D6E, 0x6237A0, 0xFFFFFF, 0x9A5300, 0x1B7346, 0xAE2219, 0x2758B8, 0xFCFCFD],
        [0x151922, 0xAE2219, 0x1B7346, 0x8A5A00, 0x2758B8, 0x6237A0, 0x0E7A78, 0x8A93A3, 0x535D6E, 0xD23B2F, 0x23915A, 0xB86A00, 0x3B72D9, 0x7B4FC0, 0x12918E, 0xD3D8E0]),
    theme!("solarized-light", "Solarized Light", false,
        [0xEEE8D5, 0xFDF6E3, 0xFFFBF0, 0xDDD6C1, 0x073642, 0x586E75, 0x5A5FB0, 0xFFFFFF, 0xA9420F, 0x5F7000, 0xC0282A, 0x1F6FAE, 0xFDF6E3],
        [0x073642, 0xDC322F, 0x859900, 0xB58900, 0x268BD2, 0xD33682, 0x2AA198, 0xEEE8D5, 0x002B36, 0xCB4B16, 0x586E75, 0x657B83, 0x839496, 0x6C71C4, 0x93A1A1, 0xFDF6E3]),
    theme!("latte", "Catppuccin Latte", false,
        [0xDCE0E8, 0xE6E9EF, 0xEFF1F5, 0xCCD0DA, 0x4C4F69, 0x5C5F77, 0x8839EF, 0xFFFFFF, 0xB3600F, 0x2D7D1E, 0xC20E35, 0x1E66F5, 0xEFF1F5],
        [0x5C5F77, 0xD20F39, 0x40A02B, 0xDF8E1D, 0x1E66F5, 0xEA76CB, 0x179299, 0xACB0BE, 0x6C6F85, 0xE2304F, 0x4FB33A, 0xE9A03F, 0x4A82F7, 0xEE8FD4, 0x1FAAB2, 0xBCC0CC]),
];

/// The eight built-in themes, dark ones first.
pub fn builtins() -> Vec<ThemeDef> {
    BUILTINS.iter().map(|(id, name, dark, ui, ansi)| ThemeDef { id: id.to_string(), name: name.to_string(), dark: *dark, ui: *ui, ansi: *ansi, path: None, extends: None }).collect()
}

pub fn builtin(id: &str) -> Option<ThemeDef> {
    builtins().into_iter().find(|t| t.id == id)
}

/// A theme id: lowercase letters, digits, `-` and `_`, at most 64.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

pub fn themes_dir(home: &Path) -> PathBuf {
    home.join("themes")
}

/// Built-ins plus every custom theme in `dir`. Bad files are skipped and reported as
/// `<file>: <why>`; a missing folder is no custom themes.
pub fn load_all(dir: &Path) -> (Vec<ThemeDef>, Vec<String>) {
    let mut files: Vec<(String, PathBuf, String)> = vec![];
    let mut errors = vec![];
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let id = p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
            match std::fs::read_to_string(&p) {
                Ok(t) => files.push((id, p, t)),
                Err(e) => errors.push(format!("{}: {e}", p.display())),
            }
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let (themes, errs) = resolve_files(files);
    errors.extend(errs);
    (themes, errors)
}

/// Resolve custom theme sources `(id, path, json)` against the built-ins and each other.
pub fn resolve_files(files: Vec<(String, PathBuf, String)>) -> (Vec<ThemeDef>, Vec<String>) {
    let mut out = builtins();
    let mut errors = vec![];
    let mut raw: HashMap<String, (PathBuf, Map<String, Value>)> = HashMap::new();
    let mut order = vec![];
    for (id, path, text) in files {
        let shown = path.display().to_string();
        if !valid_id(&id) {
            errors.push(format!("{shown}: the file name must be the theme id (lowercase letters, digits, - and _)"));
            continue;
        }
        if BUILTIN_IDS.contains(&id.as_str()) {
            errors.push(format!("{shown}: `{id}` is a built-in theme; pick another file name and use \"extends\": \"{id}\""));
            continue;
        }
        match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(m)) => {
                order.push(id.clone());
                raw.insert(id, (path, m));
            }
            Ok(_) => errors.push(format!("{shown}: expected a JSON object")),
            Err(e) => errors.push(format!("{shown}: not valid JSON ({e})")),
        }
    }
    let mut done: HashMap<String, Result<ThemeDef, String>> = HashMap::new();
    for id in &order {
        let mut stack = vec![];
        let r = resolve_one(id, &raw, &mut done, &mut stack);
        match r {
            Ok(t) => out.push(t),
            Err(e) => errors.push(format!("{}: {e}", raw[id].0.display())),
        }
    }
    (out, errors)
}

fn resolve_one(id: &str, raw: &HashMap<String, (PathBuf, Map<String, Value>)>, done: &mut HashMap<String, Result<ThemeDef, String>>, stack: &mut Vec<String>) -> Result<ThemeDef, String> {
    if let Some(r) = done.get(id) {
        return r.clone();
    }
    if stack.iter().any(|s| s == id) {
        return Err(format!("\"extends\" loops: {} -> {id}", stack.join(" -> ")));
    }
    stack.push(id.to_string());
    let (path, m) = &raw[id];
    let r = (|| {
        let kind = match m.get("kind") {
            None => None,
            Some(Value::String(k)) if k == "dark" => Some(true),
            Some(Value::String(k)) if k == "light" => Some(false),
            Some(_) => return Err("\"kind\" must be \"dark\" or \"light\"".to_string()),
        };
        let ext = match m.get("extends") {
            None => None,
            Some(Value::String(e)) => Some(e.clone()),
            Some(_) => return Err("\"extends\" must be a theme id".to_string()),
        };
        let base_id = ext.clone().unwrap_or_else(|| if kind == Some(false) { DEFAULT_LIGHT } else { DEFAULT_DARK }.to_string());
        let base = if let Some(b) = builtin(&base_id) {
            b
        } else if raw.contains_key(&base_id) {
            resolve_one(&base_id, raw, done, stack).map_err(|e| format!("extends `{base_id}`, which is broken: {e}"))?
        } else {
            return Err(format!("extends unknown theme `{base_id}`"));
        };
        let mut t = ThemeDef { id: id.to_string(), name: id.to_string(), dark: kind.unwrap_or(base.dark), ui: base.ui, ansi: base.ansi, path: Some(path.clone()), extends: ext };
        if let Some(n) = m.get("name") {
            t.name = n.as_str().filter(|n| !n.trim().is_empty()).ok_or("\"name\" must be a non-empty string")?.trim().to_string();
        }
        if let Some(c) = m.get("colors") {
            let c = c.as_object().ok_or("\"colors\" must be an object of token: \"#RRGGBB\"")?;
            for (k, v) in c {
                if !UI_TOKENS.contains(&k.as_str()) {
                    return Err(format!("unknown color `{k}` (one of: {})", UI_TOKENS.join(", ")));
                }
                let rgb = v.as_str().and_then(parse_hex).ok_or_else(|| format!("colors.{k} must be \"#RRGGBB\""))?;
                t.set(k, rgb);
            }
        }
        if let Some(term) = m.get("terminal") {
            let term = term.as_object().ok_or("\"terminal\" must be an object")?;
            for (k, v) in term {
                if k == "ansi" {
                    let a = v.as_array().filter(|a| a.len() <= 16).ok_or("terminal.ansi must be a list of up to 16 \"#RRGGBB\"")?;
                    for (i, c) in a.iter().enumerate() {
                        t.ansi[i] = c.as_str().and_then(parse_hex).ok_or_else(|| format!("terminal.ansi[{i}] must be \"#RRGGBB\""))?;
                    }
                    continue;
                }
                let i = ansi_index(k).ok_or_else(|| format!("unknown terminal color `{k}` (one of: {}, or ansi)", ANSI_NAMES.join(", ")))?;
                t.ansi[i] = v.as_str().and_then(parse_hex).ok_or_else(|| format!("terminal.{k} must be \"#RRGGBB\""))?;
            }
        }
        for k in m.keys() {
            if !["name", "kind", "extends", "colors", "terminal", "$schema"].contains(&k.as_str()) {
                return Err(format!("unknown key `{k}` (name, kind, extends, colors, terminal)"));
            }
        }
        Ok(t)
    })();
    stack.pop();
    done.insert(id.to_string(), r.clone());
    r
}

/// The theme id `theme` picks: `system` follows macOS (`theme.dark` / `theme.light`), the
/// legacy values `dark` / `light` mean Midna Twilight / Daylight.
pub fn choose(theme: &str, dark_pick: &str, light_pick: &str, system_dark: bool) -> String {
    let pick = |v: &str, def: &str| if v.trim().is_empty() { def.to_string() } else { alias(v) };
    match theme.trim() {
        "" | "system" => if system_dark { pick(dark_pick, DEFAULT_DARK) } else { pick(light_pick, DEFAULT_LIGHT) },
        other => alias(other),
    }
}

fn alias(v: &str) -> String {
    match v.trim() {
        "dark" => DEFAULT_DARK.into(),
        "light" => DEFAULT_LIGHT.into(),
        o => o.to_string(),
    }
}

/// The theme `id` from `all`, falling back to Twilight / Daylight by `system_dark` when it
/// doesn't exist (a deleted custom theme), with the `theme.colors` rules applied.
pub fn resolve(all: &[ThemeDef], id: &str, system_dark: bool, rules: &[String]) -> ThemeDef {
    let mut t = all.iter().find(|t| t.id == id).cloned().unwrap_or_else(|| builtin(if system_dark { DEFAULT_DARK } else { DEFAULT_LIGHT }).expect("built-in"));
    apply_overrides(&mut t, rules);
    t
}

/// `theme.colors` rules: `<token> = #RRGGBB` for every theme, or `<theme id>:<token> = …`
/// for one. Later rules win; bad rules are skipped (settings.set already rejects them).
pub fn apply_overrides(t: &mut ThemeDef, rules: &[String]) {
    for r in rules {
        let Some((m, v)) = r.split_once('=') else { continue };
        let (scope, token) = match m.trim().split_once(':') {
            Some((s, k)) => (Some(s.trim()), k.trim()),
            None => (None, m.trim()),
        };
        if scope.is_some_and(|s| alias(s) != t.id) {
            continue;
        }
        if let Some(c) = parse_hex(v) {
            t.set(token, c);
        }
    }
}

/// Validate one `theme.colors` rule (`settings.set`).
pub fn check_override(m: &str, v: &str) -> Result<(), String> {
    let (scope, token) = match m.split_once(':') {
        Some((s, k)) => (Some(s.trim()), k.trim()),
        None => (None, m.trim()),
    };
    if let Some(s) = scope
        && !valid_id(s)
    {
        return Err(format!("`{s}` is not a theme id"));
    }
    if !is_token(token) {
        return Err(format!("`{token}` is not a color (one of: {}, {}, or ansi0–ansi15)", UI_TOKENS.join(", "), ANSI_NAMES.join(", ")));
    }
    if parse_hex(v).is_none() {
        return Err(format!("`{v}` is not a #RRGGBB color"));
    }
    Ok(())
}

/// Alphas the UI derives from a theme's colors (same in every theme of a kind).
pub struct Alphas {
    pub accent_soft: f32,
    pub need_soft: f32,
    pub need_ring: f32,
    pub selection: f32,
}

pub fn alphas(dark: bool) -> Alphas {
    if dark {
        Alphas { accent_soft: 0.12, need_soft: 0.10, need_ring: 0.22, selection: 0.32 }
    } else {
        Alphas { accent_soft: 0.08, need_soft: 0.08, need_ring: 0.20, selection: 0.32 }
    }
}

/// Text on the need-colored badge: the theme's background in dark themes, white in light ones.
pub fn badge_fg(t: &ThemeDef) -> Rgb {
    if t.dark { t.get("bg").unwrap_or([0x15, 0x11, 0x1C]) } else { [0xFF, 0xFF, 0xFF] }
}

/// What a custom theme file looks like (`midna themes`, the guide).
pub const FILE_FORMAT: &str = r##"$MIDNA_HOME/themes/<id>.json (the file name is the theme id):
{
  "name": "My Night",
  "kind": "dark",                 // dark | light (default: the extended theme's)
  "extends": "nord",              // any theme id (default: twilight, or daylight for kind light)
  "colors": { "accent": "#FF79C6", "need": "#FFB86C" },
  "terminal": { "red": "#FF5555", "bright-black": "#6272A4" }   // or "ansi": ["#...", ... up to 16]
}
UI colors: bg panel raised line fg dim accent accent-fg need ok err work term.
Terminal colors: black red green yellow blue magenta cyan white, bright-<each>, or ansi0–ansi15.
Anything left out comes from "extends". The app picks up edits live; bad files show in `midna themes`."##;

#[cfg(test)]
mod tests {
    use super::*;

    fn file(id: &str, json: &str) -> (String, PathBuf, String) {
        (id.into(), PathBuf::from(format!("/t/{id}.json")), json.into())
    }

    #[test]
    fn eight_builtins_with_unique_ids() {
        let b = builtins();
        assert_eq!(b.len(), 8);
        for id in BUILTIN_IDS {
            assert!(builtin(id).is_some(), "{id}");
        }
        assert_eq!(b.iter().filter(|t| t.dark).count(), 5);
    }

    #[test]
    fn twilight_matches_the_original_dark_tokens() {
        let t = builtin("twilight").unwrap();
        assert_eq!(t.get("bg"), Some(h(0x0F1117)));
        assert_eq!(t.get("accent"), Some(h(0xB79AE8)));
        assert_eq!(t.get("term"), Some(h(0x0B0D12)));
        let d = builtin("daylight").unwrap();
        assert_eq!(d.get("accent"), Some(h(0x6237A0)));
        assert!(!d.dark);
    }

    #[test]
    fn cache_round_trips() {
        let mut t = builtin("nord").unwrap();
        apply_overrides(&mut t, &["accent = #FF79C6".to_string()]);
        let back = ThemeDef::from_cache(&t.to_cache()).unwrap();
        assert_eq!((back.id.as_str(), back.dark, back.ui, back.ansi), ("nord", true, t.ui, t.ansi));
        assert!(!back.is_builtin("nord") && builtin("twilight").unwrap().is_builtin("twilight"));
        assert!(ThemeDef::from_cache(&serde_json::json!({"id": "x"})).is_none());
    }

    #[test]
    fn hex_parsing() {
        assert_eq!(parse_hex("#FF79c6"), Some([0xFF, 0x79, 0xC6]));
        assert_eq!(parse_hex("abc"), Some([0xAA, 0xBB, 0xCC]));
        assert_eq!(parse_hex("#12345"), None);
        assert_eq!(parse_hex("#GGGGGG"), None);
        assert_eq!(to_hex([1, 2, 255]), "#0102FF");
    }

    #[test]
    fn legacy_and_system_choices() {
        assert_eq!(choose("dark", "", "", false), "twilight");
        assert_eq!(choose("light", "", "", true), "daylight");
        assert_eq!(choose("system", "", "", true), "twilight");
        assert_eq!(choose("system", "nord", "latte", true), "nord");
        assert_eq!(choose("system", "nord", "latte", false), "latte");
        assert_eq!(choose("dracula", "nord", "latte", false), "dracula");
    }

    #[test]
    fn custom_theme_extends_and_overrides() {
        let (all, errs) = resolve_files(vec![
            file("mine", r##"{"name":"Mine","extends":"nord","colors":{"accent":"#FF0000"},"terminal":{"red":"#00FF00","ansi":["#010101"]}}"##),
            file("child", r##"{"extends":"mine","colors":{"ok":"#0000FF"}}"##),
            file("pale", r##"{"kind":"light"}"##),
        ]);
        assert!(errs.is_empty(), "{errs:?}");
        let mine = all.iter().find(|t| t.id == "mine").unwrap();
        assert_eq!(mine.name, "Mine");
        assert!(mine.dark);
        assert_eq!(mine.get("accent"), Some([255, 0, 0]));
        assert_eq!(mine.get("bg"), builtin("nord").unwrap().get("bg"));
        assert_eq!(mine.ansi[1], [0, 255, 0]);
        assert_eq!(mine.ansi[0], [1, 1, 1]);
        let child = all.iter().find(|t| t.id == "child").unwrap();
        assert_eq!(child.get("accent"), Some([255, 0, 0]));
        assert_eq!(child.get("ok"), Some([0, 0, 255]));
        let pale = all.iter().find(|t| t.id == "pale").unwrap();
        assert_eq!(pale.ui, builtin("daylight").unwrap().ui);
    }

    #[test]
    fn bad_files_are_reported_not_fatal() {
        let (all, errs) = resolve_files(vec![
            file("broken", "{nope"),
            file("nord", "{}"),
            file("Bad Name", "{}"),
            file("badcolor", r##"{"colors":{"accent":"red"}}"##),
            file("badkey", r##"{"colours":{}}"##),
            file("loop-a", r##"{"extends":"loop-b"}"##),
            file("loop-b", r##"{"extends":"loop-a"}"##),
            file("ghost", r##"{"extends":"nowhere"}"##),
            file("good", "{}"),
        ]);
        assert_eq!(all.len(), 9, "built-ins + good");
        assert_eq!(errs.len(), 8, "{errs:#?}");
        assert!(errs.iter().any(|e| e.contains("loops")));
        assert!(errs.iter().any(|e| e.contains("built-in")));
    }

    #[test]
    fn overrides_global_and_scoped() {
        let all = builtins();
        let rules = vec!["accent = #FF79C6".to_string(), "nord:need = #010203".to_string(), "dracula:ok = #000000".to_string(), "bright-red = #111111".to_string()];
        let nord = resolve(&all, "nord", true, &rules);
        assert_eq!(nord.get("accent"), Some([0xFF, 0x79, 0xC6]));
        assert_eq!(nord.get("need"), Some([1, 2, 3]));
        assert_eq!(nord.get("ok"), builtin("nord").unwrap().get("ok"));
        assert_eq!(nord.ansi[9], [0x11; 3]);
        // `dark:` scopes to the legacy alias's theme.
        let tw = resolve(&all, "twilight", true, &["dark:accent = #000001".to_string()]);
        assert_eq!(tw.get("accent"), Some([0, 0, 1]));
    }

    #[test]
    fn unknown_theme_falls_back_by_appearance() {
        let all = builtins();
        assert_eq!(resolve(&all, "deleted", true, &[]).id, "twilight");
        assert_eq!(resolve(&all, "deleted", false, &[]).id, "daylight");
    }

    #[test]
    fn override_rules_are_checked() {
        assert!(check_override("accent", "#FF00FF").is_ok());
        assert!(check_override("nord:bright-red", "#FF00FF").is_ok());
        assert!(check_override("ansi15", "#FFF").is_ok());
        assert!(check_override("sparkle", "#FF00FF").is_err());
        assert!(check_override("accent", "pink").is_err());
        assert!(check_override("No Way:accent", "#FF00FF").is_err());
    }
}
