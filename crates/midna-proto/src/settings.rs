//! The settings catalog. Values live in daemon state; this table defines keys, types,
//! defaults and who may write them. Add a row here to add a setting.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind", content = "options")]
pub enum SettingType {
    Bool,
    String,
    Int,
    Keybinding,
    Enum(Vec<String>),
    /// A list of directory paths (JSON array of strings).
    PathList,
    /// A list of `<match> = <value>` rules (JSON array of strings), first match wins.
    RuleList,
    /// An ordered list of names from the options (JSON array of strings, no repeats).
    ItemList(Vec<String>),
}

#[derive(Clone, Debug)]
pub struct SettingSpec {
    pub key: &'static str,
    pub ty: SettingKind,
    pub default: DefaultValue,
    pub description: &'static str,
    pub human_only: bool,
    pub section: &'static str,
    /// Inclusive bounds for an Int setting.
    pub range: Option<(i64, i64)>,
}

/// Static-friendly mirror of [`SettingType`].
#[derive(Clone, Copy, Debug)]
pub enum SettingKind {
    Bool,
    String,
    Int,
    Keybinding,
    /// JSON array of paths. A string is split on commas and newlines (`~` stays as typed).
    PathList,
    /// JSON array of `<match> = <value>` strings, kept as `match = value`. A string is split on
    /// commas and newlines.
    RuleList,
    /// Enum options; `allow_other` accepts any string too (e.g. a custom script path).
    Enum { options: &'static [&'static str], allow_other: bool },
    /// JSON array of names from `options` (and, with `allow_paths`, absolute paths), in order,
    /// no repeats. A string is split on commas and newlines. Empty is allowed (show nothing).
    ItemList { options: &'static [&'static str], allow_paths: bool },
}

#[derive(Clone, Copy, Debug)]
pub enum DefaultValue {
    Bool(bool),
    Str(&'static str),
    Int(i64),
    List(&'static [&'static str]),
}

impl DefaultValue {
    pub fn to_json(self) -> Value {
        match self {
            DefaultValue::Bool(b) => json!(b),
            DefaultValue::Str(s) => json!(s),
            DefaultValue::Int(i) => json!(i),
            DefaultValue::List(l) => json!(l),
        }
    }
}

impl SettingSpec {
    pub fn setting_type(&self) -> SettingType {
        match self.ty {
            SettingKind::Bool => SettingType::Bool,
            SettingKind::String => SettingType::String,
            SettingKind::Int => SettingType::Int,
            SettingKind::Keybinding => SettingType::Keybinding,
            SettingKind::PathList => SettingType::PathList,
            SettingKind::RuleList => SettingType::RuleList,
            SettingKind::Enum { options, .. } => SettingType::Enum(options.iter().map(|s| s.to_string()).collect()),
            SettingKind::ItemList { options, .. } => SettingType::ItemList(options.iter().map(|s| s.to_string()).collect()),
        }
    }

    /// Validate and normalize a value. Accepts strings for bool/int so the CLI can pass raw text.
    pub fn coerce(&self, v: &Value) -> Result<Value, String> {
        let as_text = v.as_str().map(str::to_string);
        match self.ty {
            SettingKind::Bool => match (v, as_text.as_deref()) {
                (Value::Bool(b), _) => Ok(json!(b)),
                (_, Some("true" | "on" | "yes" | "1")) => Ok(json!(true)),
                (_, Some("false" | "off" | "no" | "0")) => Ok(json!(false)),
                _ => Err(format!("{} expects true or false", self.key)),
            },
            SettingKind::Int => match (v.as_i64(), as_text.and_then(|t| t.trim().trim_end_matches('%').parse::<i64>().ok())) {
                (Some(i), _) | (None, Some(i)) => match self.range {
                    Some((lo, hi)) if i < lo || i > hi => Err(format!("{} expects {lo} to {hi}", self.key)),
                    _ => Ok(json!(i)),
                },
                _ => Err(format!("{} expects an integer", self.key)),
            },
            SettingKind::String if self.key.starts_with("notify.color.") => match as_text.map(|t| t.trim().to_ascii_lowercase()) {
                Some(t) if crate::notify::valid_color(&t) => Ok(json!(t)),
                _ => Err(format!("{} expects a theme color ({}) or #rrggbb", self.key, crate::notify::COLOR_TOKENS.join(", "))),
            },
            SettingKind::String if matches!(self.key, "keep_awake.start" | "keep_awake.end") => match as_text {
                Some(t) => crate::keep_awake::parse_time(&t).map(|m| json!(crate::keep_awake::hhmm(m))).map_err(|e| format!("{}: {e}", self.key)),
                None => Err(format!("{} expects a time of day like 8am or 17:30", self.key)),
            },
            SettingKind::ItemList { .. } if self.key == "keep_awake.days" => crate::keep_awake::parse_days(v).map(|d| json!(d)).map_err(|e| format!("{}: {e}", self.key)),
            SettingKind::RuleList if self.key == "keep_awake.hours" => crate::keep_awake::coerce_hours(v),
            SettingKind::String | SettingKind::Keybinding => match as_text {
                Some(t) => Ok(json!(t)),
                None => Err(format!("{} expects a string", self.key)),
            },
            SettingKind::PathList => {
                let items: Vec<String> = match v {
                    Value::Array(a) => a.iter().map(|x| x.as_str().map(str::to_string).ok_or_else(|| format!("{} expects a list of paths", self.key))).collect::<Result<_, _>>()?,
                    Value::String(t) => t.split([',', '\n']).map(str::to_string).collect(),
                    _ => return Err(format!("{} expects a list of paths", self.key)),
                };
                let mut out: Vec<String> = vec![];
                for i in items.iter().map(|i| i.trim()).filter(|i| !i.is_empty()) {
                    if !(i.starts_with('/') || i == "~" || i.starts_with("~/")) {
                        return Err(format!("{}: `{i}` must be an absolute path or start with ~/", self.key));
                    }
                    let i = if i.len() > 1 { i.trim_end_matches('/') } else { i };
                    if !out.iter().any(|o| o == i) {
                        out.push(i.to_string());
                    }
                }
                Ok(json!(out))
            }
            SettingKind::RuleList => {
                let items: Vec<String> = match v {
                    Value::Array(a) => a.iter().map(|x| x.as_str().map(str::to_string).ok_or_else(|| format!("{} expects a list of `match = value` rules", self.key))).collect::<Result<_, _>>()?,
                    Value::String(t) => t.split([',', '\n']).map(str::to_string).collect(),
                    _ => return Err(format!("{} expects a list of `match = value` rules", self.key)),
                };
                let mut out: Vec<String> = vec![];
                for i in items.iter().map(|i| i.trim()).filter(|i| !i.is_empty()) {
                    let Some((m, val)) = i.split_once('=').map(|(m, v)| (m.trim(), v.trim())).filter(|(m, v)| !m.is_empty() && !v.is_empty()) else {
                        return Err(format!("{}: `{i}` must look like `match = value`", self.key));
                    };
                    let m = if m.len() > 1 { m.trim_end_matches('/') } else { m };
                    if self.key == "ui.status.looks" {
                        check_status_look(m, val).map_err(|e| format!("{}: `{i}`: {e}", self.key))?;
                    }
                    if self.key == "theme.colors" {
                        crate::themes::check_override(m, val).map_err(|e| format!("{}: `{i}`: {e}", self.key))?;
                    }
                    if self.key == "insights.layouts" {
                        check_insights_layout(val).map_err(|e| format!("{}: `{i}`: {e}", self.key))?;
                        if out.iter().any(|r| r.split_once(" = ").is_some_and(|(n, _)| n == m)) {
                            return Err(format!("{}: two layouts are named `{m}`", self.key));
                        }
                    }
                    let rule = format!("{m} = {val}");
                    if !out.contains(&rule) {
                        out.push(rule);
                    }
                }
                Ok(json!(out))
            }
            SettingKind::ItemList { options, allow_paths } => {
                let bad = || format!("{} expects a list of: {}", self.key, options.join(", "));
                let items: Vec<String> = match v {
                    Value::Array(a) => a.iter().map(|x| x.as_str().map(str::to_string).ok_or_else(bad)).collect::<Result<_, _>>()?,
                    Value::String(t) => t.split([',', '\n']).map(str::to_string).collect(),
                    _ => return Err(bad()),
                };
                let mut out: Vec<&str> = vec![];
                for i in items.iter().map(|i| i.trim()).filter(|i| !i.is_empty()) {
                    if !(options.contains(&i) || (allow_paths && i.starts_with('/'))) {
                        let paths = if allow_paths { ", or an absolute path to a script" } else { "" };
                        return Err(format!("{}: unknown item `{i}` (one of: {}{paths})", self.key, options.join(", ")));
                    }
                    if !out.contains(&i) {
                        out.push(i);
                    }
                }
                Ok(json!(out))
            }
            SettingKind::Enum { .. } if self.key.starts_with("theme") => match as_text.map(|t| t.trim().to_string()) {
                Some(t) if crate::themes::valid_id(&t) => Ok(json!(t)),
                _ => Err(format!("{} expects a theme id: system, {} or a custom theme's id (`midna themes`)", self.key, crate::themes::BUILTIN_IDS.join(", "))),
            },
            SettingKind::Enum { options, allow_other } => match as_text {
                Some(t) if options.contains(&t.as_str()) || (allow_other && !t.is_empty()) => Ok(json!(t)),
                _ => Err(format!("{} expects one of: {}", self.key, options.join(", "))),
            },
        }
    }
}

macro_rules! s {
    ($key:expr, $ty:expr, $def:expr, $section:literal, $human:literal, $desc:expr) => {
        SettingSpec { key: $key, ty: $ty, default: $def, description: $desc, human_only: $human, section: $section, range: None }
    };
}

/// A notification category's sound, volume and image (see `notify::{sound_key, volume_key, image_key}`).
const SOUNDS: SettingKind = en_path(&[
    "none", "Portal", "Call", "Uh-oh", "Strum", "Hm", "Rise", "Nn-nn", "Whoosh", "Fwip", "Close", "Tick", "Thump", "Tick-tick", "Basso", "Blow", "Bottle", "Frog", "Funk",
    "Glass", "Hero", "Morse", "Ping", "Pop", "Purr", "Sosumi", "Submarine", "Tink",
]);
macro_rules! snd {
    ($cat:literal, $def:literal) => {
        s!(concat!("notify.sound.", $cat), SOUNDS, S($def), "notifications", false,
            concat!("Sound for “notify.", $cat, "”: none, one of midna's Twilight sounds (Portal, Call, …), a macOS sound (Glass, Ping, …) or a sound imported with `midna notify import <file>` (by its file name)."))
    };
}
macro_rules! vol {
    ($cat:literal) => {
        SettingSpec { range: Some((0, 100)), ..s!(concat!("notify.volume.", $cat), SettingKind::Int, I(100), "notifications", false,
            concat!("How loud “notify.", $cat, "” plays, 0–100 (scaled by notify.volume).")) }
    };
}
macro_rules! pic {
    ($cat:literal) => {
        s!(concat!("notify.image.", $cat), SettingKind::String, S(""), "notifications", false,
            concat!("Image on “notify.", $cat, "” notifications: empty = the notify.image every notification uses, none, or an imported image's file name."))
    };
}

macro_rules! txt {
    ($cat:literal, $vars:literal) => {
        s!(concat!("notify.body.", $cat), SettingKind::String, S(""), "notifications", false,
            concat!("Text of “notify.", $cat, "” notifications, as a template: empty = midna's own, none = no text (just the title). ", $vars,
                "Every kind also has {{text}} (midna's own text), {{heading}} (midna's own title), {{project}}, {{session.name}} and the event's data ({{data.<path>}})."))
    };
}
macro_rules! push {
    ($cat:literal, $def:literal) => {
        s!(concat!("notify.push.", $cat), SettingKind::Bool, B($def), "notifications", false,
            concat!("Show “notify.", $cat, "” as a macOS banner (with its sound) for a terminal you aren't looking at. Off: it's still recorded, just not pushed."))
    };
}
macro_rules! pushf {
    ($cat:literal) => {
        s!(concat!("notify.push_focused.", $cat), SettingKind::Bool, B(false), "notifications", false,
            concat!("Show “notify.", $cat, "” as a macOS banner for the terminal you're looking at too. Off: only its sound plays, and only when notify.push.", $cat, " is on."))
    };
}
macro_rules! ttl {
    ($cat:literal) => {
        s!(concat!("notify.title.", $cat), SettingKind::String, S(""), "notifications", false,
            concat!("Title of “notify.", $cat, "” notifications, as a template with the same variables as notify.body.", $cat,
                ": empty = midna's own ({{heading}}: the terminal · its project). A title can't be empty: one that renders empty is midna's own."))
    };
}

macro_rules! stay {
    ($cat:literal, $def:literal) => {
        SettingSpec { range: Some((0, 3600)), ..s!(concat!("notify.stay.", $cat), SettingKind::Int, I($def), "notifications", false,
            concat!("How long “notify.", $cat, "” stays on screen in the floating badge and the in-app card, in seconds. 0 = it stays until it's handled or dismissed.")) }
    };
}
macro_rules! color {
    ($cat:literal, $def:literal) => {
        s!(concat!("notify.color.", $cat), SettingKind::String, S($def), "notifications", false,
            concat!("Color of “notify.", $cat, "” notifications in the floating badge, the in-app card and the notifications screen: a theme color (need, ok, err, work, accent, dim) or #rrggbb."))
    };
}

macro_rules! bell {
    ($cat:literal, $def:literal) => {
        s!(concat!("notify.bell.", $cat), SettingKind::Bool, B($def), "notifications", false,
            concat!("Count “notify.", $cat, "” toward the unread number on the status bar's bell. Off: it's still in the notifications screen, just not counted."))
    };
}

use DefaultValue::{Bool as B, Int as I, List as L, Str as S};

/// A per-kind setting of a kind you added (`notify::CustomKind`), by its field
/// (`notify::KIND_FIELDS`): the type and default its `notify.<field>.<key>` key has. These
/// specs' `key` is a pattern; the real key is the one asked for.
pub fn custom_kind_spec(field: &str) -> Option<&'static SettingSpec> {
    CUSTOM_KIND.iter().find(|s| s.key.strip_prefix("notify.").and_then(|k| k.strip_suffix("<kind>")).is_some_and(|f| f.trim_end_matches('.') == field))
}

static CUSTOM_KIND: &[SettingSpec] = &[
    s!("notify.<kind>", SettingKind::Bool, B(true), "notifications", false, "Show notifications of this kind you added."),
    s!("notify.push.<kind>", SettingKind::Bool, B(true), "notifications", false,
        "Show this kind as a macOS banner (with its sound) for a terminal you aren't looking at. Off: it's still recorded, just not pushed."),
    s!("notify.push_focused.<kind>", SettingKind::Bool, B(false), "notifications", false, "Show this kind as a macOS banner for the terminal you're looking at too."),
    s!("notify.sound.<kind>", SOUNDS, S("Hm"), "notifications", false, "Sound for this kind: none, a Twilight sound, a macOS sound or an imported sound."),
    SettingSpec { range: Some((0, 100)), ..s!("notify.volume.<kind>", SettingKind::Int, I(100), "notifications", false, "How loud this kind plays, 0–100 (scaled by notify.volume).") },
    s!("notify.image.<kind>", SettingKind::String, S(""), "notifications", false, "Image on this kind's notifications: empty = notify.image, none, or an imported image."),
    s!("notify.title.<kind>", SettingKind::String, S(""), "notifications", false, "Title of this kind's notifications, as a template: empty = midna's own ({{heading}}: the terminal · its project). {{title}} and {{body}} are what was sent."),
    s!("notify.body.<kind>", SettingKind::String, S(""), "notifications", false, "Text of this kind's notifications, as a template: empty = what was sent, none = no text. {{title}} and {{body}} are what was sent."),
    SettingSpec { range: Some((0, 3600)), ..s!("notify.stay.<kind>", SettingKind::Int, I(6), "notifications", false,
        "How long this kind stays on screen, in seconds. 0 = it stays until it's handled or dismissed.") },
    s!("notify.color.<kind>", SettingKind::String, S("accent"), "notifications", false, "Color of this kind: a theme color (need, ok, err, work, accent, dim) or #rrggbb."),
    s!("notify.bell.<kind>", SettingKind::Bool, B(false), "notifications", false, "Count this kind toward the unread number on the status bar's bell."),
];
const fn en(options: &'static [&'static str]) -> SettingKind {
    SettingKind::Enum { options, allow_other: false }
}
const fn en_path(options: &'static [&'static str]) -> SettingKind {
    SettingKind::Enum { options, allow_other: true }
}
/// Named options plus any other value (a `#RRGGBB` color).
const fn en_other(options: &'static [&'static str]) -> SettingKind {
    SettingKind::Enum { options, allow_other: true }
}
const KB: SettingKind = SettingKind::Keybinding;

/// The numbers the sidebar's Insights card can show (`sidebar.footer.stats`).
pub const FOOTER_STATS: &[&str] = &["turns", "messages", "spend", "working", "waiting", "approvals", "triggers", "peak", "reply", "longest"];
pub const DEFAULT_FOOTER_STATS: &[&str] = &["turns", "messages", "spend"];
/// The sidebar footer's buttons (`sidebar.footer.buttons`).
pub const FOOTER_BUTTONS: &[&str] = &["triggers", "rules", "settings", "insights", "needs_you"];
pub const DEFAULT_FOOTER_BUTTONS: &[&str] = &["triggers", "rules", "settings"];
/// The Insights screen's widgets: id, usual size, the sizes it comes in (`insights.layouts`).
/// small = one grid cell, wide = two across, large = two across and two down.
pub const INSIGHTS_WIDGETS: &[(&str, &str, &[&str])] = &[
    ("headline", "wide", &["wide"]),
    ("turns", "wide", &["wide", "large"]),
    ("spend", "small", &["small", "wide"]),
    ("working_waiting", "small", &["small", "wide"]),
    ("approvals", "small", &["small", "wide"]),
    ("triggers", "small", &["small", "wide"]),
    ("parallelism", "wide", &["small", "wide", "large"]),
    ("autonomy", "small", &["small", "wide"]),
    ("latency", "wide", &["small", "wide", "large"]),
    ("agent_time", "wide", &["small", "wide", "large"]),
    ("approved", "small", &["small", "wide", "large"]),
    ("corrections", "small", &["small", "wide"]),
    ("heatmap", "wide", &["small", "wide", "large"]),
    ("projects", "small", &["small", "wide"]),
    ("models", "small", &["small", "wide"]),
    ("bests", "small", &["small", "wide"]),
];
pub const DEFAULT_INSIGHTS_LAYOUTS: &[&str] = &[
    "My layout = headline turns parallelism agent_time heatmap latency approved spend working_waiting",
    "Overview = headline turns spend working_waiting approvals triggers",
    "Am I the bottleneck? = latency:large agent_time approved corrections parallelism:small",
    "Spend = models:wide spend:wide projects bests",
];

/// One layout's widgets in order, each with its size (unknown widgets and sizes dropped; a
/// size the widget doesn't come in falls back to its usual one).
pub fn parse_insights_layout(value: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = vec![];
    for tok in value.split_whitespace() {
        let (id, size) = tok.split_once(':').unwrap_or((tok, ""));
        let Some((_, usual, sizes)) = INSIGHTS_WIDGETS.iter().find(|w| w.0 == id) else { continue };
        if out.iter().any(|(i, _)| i == id) {
            continue;
        }
        let size = if sizes.contains(&size) { size } else { usual };
        out.push((id.to_string(), size.to_string()));
    }
    out
}

/// A layout's value written back: `id` alone when it has its usual size.
pub fn format_insights_layout(items: &[(String, String)]) -> String {
    items
        .iter()
        .map(|(id, size)| match INSIGHTS_WIDGETS.iter().find(|w| w.0 == id) {
            Some((_, usual, _)) if usual == size => id.clone(),
            _ => format!("{id}:{size}"),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn check_insights_layout(value: &str) -> Result<(), String> {
    for tok in value.split_whitespace() {
        let (id, size) = tok.split_once(':').unwrap_or((tok, ""));
        let Some((_, _, sizes)) = INSIGHTS_WIDGETS.iter().find(|w| w.0 == id) else {
            return Err(format!("unknown widget `{id}` (one of: {})", INSIGHTS_WIDGETS.iter().map(|w| w.0).collect::<Vec<_>>().join(", ")));
        };
        if !size.is_empty() && !sizes.contains(&size) {
            return Err(format!("{id} comes in {}, not {size}", sizes.join(", ")));
        }
    }
    Ok(())
}
/// Theme colors an Insights color setting can name (or `#RRGGBB`).
pub const CHART_COLORS: &[&str] = &["accent", "work", "need", "ok", "err", "fg", "dim"];

/// Built-in parts of `ui.header.script` / `ui.row.script` / `ui.status.script`. Join them
/// with `+` (`worktree+branch`); `github` = worktree+branch+sync+diff+files+pr.
pub const SCRIPT_PARTS: &[&str] = &["none", "github", "agent", "worktree", "branch", "sync", "diff", "git-diff-stats", "files", "pr"];

/// True when a script setting's value is built-in parts only (not a custom executable).
pub fn is_builtin_script(v: &str) -> bool {
    v.is_empty() || v.split('+').all(|p| SCRIPT_PARTS.contains(&p.trim()))
}

/// The terminal header's built-in toolbar buttons, for `ui.header.buttons` (More is always there, last).
pub const HEADER_BUTTONS: &[&str] = &["subagents", "links", "ide", "image", "split", "popout", "restart"];
/// `ui.header.buttons` out of the box: restart lives in the More menu.
pub const DEFAULT_HEADER_BUTTONS: &[&str] = &["subagents", "links", "ide", "image", "split", "popout"];

/// The built-in statuses `ui.status.looks` can restyle.
pub const STATUS_STATES: &[&str] = &["idle", "working", "needs_you", "done", "failed", "exited"];

/// How a built-in status looks, from `ui.status.looks`. Every field is optional: what a rule
/// leaves out keeps the built-in look.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StatusLook {
    /// The dot's color: a named color (`crate::STATUS_COLORS`) or `#rrggbb`.
    pub color: Option<String>,
    /// Shown in place of the agent icon.
    pub icon: Option<String>,
    /// Shown as a chip in the header and as the row's second line.
    pub label: Option<String>,
}

/// `<color>? icon:<name>? label:<text>?` (label takes the rest of the line).
fn parse_look(val: &str) -> Result<StatusLook, String> {
    let mut look = StatusLook::default();
    let mut rest = val.trim();
    while !rest.is_empty() {
        if let Some(l) = rest.strip_prefix("label:") {
            look.label = Some(l.trim().to_string()).filter(|l| !l.is_empty());
            break;
        }
        let (tok, tail) = rest.split_once(' ').unwrap_or((rest, ""));
        if let Some(i) = tok.strip_prefix("icon:") {
            look.icon = Some(i.to_string()).filter(|i| !i.is_empty());
        } else if crate::types::valid_status_color(tok) {
            look.color = Some(tok.to_string());
        } else {
            return Err(format!("`{tok}` is not a color ({} or #rrggbb), icon:<name> or label:<text>", crate::types::STATUS_COLORS.join(", ")));
        }
        rest = tail.trim_start();
    }
    Ok(look)
}

/// A `ui.status.looks` rule: `<status> = …` or `<agent>.<status> = …` (claude, codex, shell, monitor).
fn check_status_look(m: &str, val: &str) -> Result<(), String> {
    let state = m.split_once('.').map_or(m, |(who, st)| if ["claude", "codex", "shell", "monitor"].contains(&who) { st } else { m });
    if !STATUS_STATES.contains(&state) {
        return Err(format!("`{m}` is not a status ({}), optionally after claude., codex., shell. or monitor.", STATUS_STATES.join(", ")));
    }
    parse_look(val).map(|_| ())
}

/// The look for a terminal of kind `who` (claude, codex, shell, monitor) in `state`: the
/// `<state>` rule, then the `<who>.<state>` rule's fields on top. Bad rules are skipped.
pub fn status_look(rules: &[String], who: &str, state: &str) -> StatusLook {
    let mut out = StatusLook::default();
    for key in [state.to_string(), format!("{who}.{state}")] {
        for r in rules {
            let Some((m, val)) = r.split_once('=') else { continue };
            if m.trim() != key {
                continue;
            }
            if let Ok(l) = parse_look(val) {
                out.color = l.color.or(out.color);
                out.icon = l.icon.or(out.icon);
                out.label = l.label.or(out.label);
            }
        }
    }
    out
}

/// What the status bar can show, for `ui.status.items`. `script` is `ui.status.script`'s
/// segments for the selected terminal; `spacer` pushes what follows to the right.
pub const STATUS_ITEMS: &[&str] = &["daemon", "usage", "cache", "policy", "webhooks", "triggers", "hooks", "accessibility", "awake", "script", "spacer", "update", "keys"];

/// `theme`'s listed choices (any custom theme id is accepted too).
pub const THEME_CHOICES: &[&str] = &["system", "twilight", "nord", "dracula", "gruvbox", "tokyo-night", "daylight", "solarized-light", "latte"];

/// Where the app looks for updates by default: the manifests the release workflow
/// (.github/workflows/release.yml) keeps on the `channels` GitHub release.
pub const DEFAULT_FEED_URL: &str = "https://github.com/mrgnhnt96/midna/releases/download/channels/{channel}.json";

pub static SETTINGS: &[SettingSpec] = &[
    s!("theme", en_path(THEME_CHOICES), S("system"), "appearance", false,
        "Color theme of the midna UI and its terminals: system (follow macOS with theme.dark / theme.light), a built-in theme (twilight, nord, dracula, gruvbox, tokyo-night, daylight, solarized-light, latte) or a custom theme's id ($MIDNA_HOME/themes/<id>.json; `midna themes`). dark and light still mean twilight and daylight."),
    s!("theme.dark", en_path(&crate::themes::BUILTIN_IDS), S("twilight"), "appearance", false,
        "The theme used while macOS is in dark mode, when theme is system: a theme id (`midna themes`)."),
    s!("theme.light", en_path(&crate::themes::BUILTIN_IDS), S("daylight"), "appearance", false,
        "The theme used while macOS is in light mode, when theme is system: a theme id (`midna themes`)."),
    s!("theme.colors", SettingKind::RuleList, L(&[]), "appearance", false,
        "Change single colors on top of the theme, like VS Code's colorCustomizations: `<color> = #RRGGBB` for every theme, or `<theme id>:<color> = #RRGGBB` for one. Colors: bg, panel, raised, line, fg, dim, accent, accent-fg, need, ok, err, work, term, and the terminal's black … white, bright-black … bright-white (or ansi0–ansi15). E.g. `accent = #FF79C6`, `nord:need = #EBCB8B`."),
    s!("density", en(&["comfortable", "compact"]), S("comfortable"), "appearance", false, "Spacing density of sidebar rows and headers."),
    s!("ui.header.script", en_path(&["github", "github+agent", "worktree+branch", "none"]), S("github"), "appearance", false,
        "Script that renders the terminal header line: built-in parts joined with + (github, agent, worktree, branch, sync, diff, files, pr) or an absolute path to an executable printing JSON segments (`midna explain scripts`)."),
    s!("ui.row.script", en_path(&["diff", "worktree+diff", "worktree+branch", "none"]), S("diff"), "appearance", false,
        "Script that renders the extra text on each sidebar terminal row: built-in parts joined with + (worktree, branch, diff, …) or an executable path (`midna explain scripts`)."),
    s!("ui.header.buttons", SettingKind::ItemList { options: HEADER_BUTTONS, allow_paths: true }, L(DEFAULT_HEADER_BUTTONS), "appearance", false,
        "Header toolbar buttons, left to right (More is always last): subagents, links, ide, image, split, popout, restart, or an absolute path to your own button script (it prints the button's look and runs again with MIDNA_CLICK=1 when clicked; `midna explain scripts`). A built-in left out moves into the More (…) menu; its shortcut still works. Adding a script path is human only."),
    s!("ui.status.looks", SettingKind::RuleList, L(&[]), "appearance", false,
        "Restyle built-in statuses, one rule per status, only what you list: `<status> = <color> icon:<name> label:<text>` (each part optional). status: idle, working, needs_you, done, failed, exited, optionally for one kind of terminal (`claude.working`, `codex.done`, `shell.failed`; its fields override the plain rule's). color = the dot (red, orange, amber, yellow, green, teal, blue, purple, pink, gray or #rrggbb); icon replaces the agent icon (check, cross, bell, lock, bolt, play, …); label shows in the header and the row's second line. A trigger's custom status still wins. E.g. `needs_you = pink icon:bell label:Your turn`."),
    s!("ui.status.script", en_path(&["worktree+branch", "branch", "github", "none"]), S("worktree+branch"), "appearance", false,
        "Script behind the status bar's `script` item, run for the selected terminal: built-in parts joined with + or an executable path (`midna explain scripts`)."),
    s!("ui.status.items", SettingKind::ItemList { options: STATUS_ITEMS, allow_paths: true }, L(&["daemon", "usage", "cache", "webhooks", "triggers", "hooks", "accessibility", "awake", "spacer", "script", "update", "keys"]), "appearance", false,
        "What the status bar shows, left to right: daemon, usage (Claude's 5-hour and weekly plan limits, once a Claude terminal has reported them), cache (whether the selected Claude terminal's prompt cache is still warm), policy (the default policy and how many approval rules; off by default), webhooks, triggers, hooks, accessibility, awake (while keep-awake holds the Mac awake), script (ui.status.script), spacer (the rest goes right), update, keys, or an absolute path to your own script (one item each; see `midna explain scripts`). Leave one out to hide it. Adding a script path is human only."),
    s!("ui.haptics", SettingKind::Bool, B(true), "appearance", false,
        "A light tap on a Force Touch trackpad when you click a button, row or link anywhere in the app. A mouse or an older trackpad ignores it."),
    s!("sidebar.footer.stats", SettingKind::ItemList { options: FOOTER_STATS, allow_paths: false }, L(DEFAULT_FOOTER_STATS), "appearance", false,
        "The numbers on the sidebar's Insights card, left to right (the first three show): turns, messages, spend, working (agent hours), waiting (blocked on you), approvals, triggers, peak (most agents working at once), reply (median time to answer a needs-you item), longest (longest turn). Empty hides the card."),
    s!("sidebar.footer.range", en(&["today", "week"]), S("today"), "appearance", false,
        "What the sidebar's Insights card counts: today, or the last 7 days."),
    s!("sidebar.footer.buttons", SettingKind::ItemList { options: FOOTER_BUTTONS, allow_paths: false }, L(DEFAULT_FOOTER_BUTTONS), "appearance", false,
        "The buttons under the sidebar's Insights card, left to right (at most four): triggers, rules, settings, insights, needs_you. Empty hides the row."),
    s!("sidebar.footer.compact", SettingKind::Bool, B(false), "appearance", false,
        "A shorter sidebar footer: the Insights card on one line and icon-only buttons. Terminal rows keep the `density` setting."),
    s!("insights.layouts", SettingKind::RuleList, L(DEFAULT_INSIGHTS_LAYOUTS), "appearance", false,
        "Saved layouts of the Insights screen, one per line: `<name> = <widget>[:<size>] …` in order, size small, wide or large (left out = the widget's usual size). Widgets: headline, turns, spend, working_waiting, approvals, triggers, parallelism, autonomy, latency, agent_time, approved, corrections, heatmap, projects, models, bests (`midna explain insights.layouts` for their sizes). E.g. `Mine = parallelism:large heatmap approved:small`. Arrange them on the Insights screen, or ask an agent."),
    s!("insights.layout", SettingKind::String, S("My layout"), "appearance", false,
        "The name of the layout in insights.layouts the Insights screen shows."),
    s!("insights.colors.agents", en_other(CHART_COLORS), S("accent"), "appearance", false,
        "The color for agent work in Insights charts: a theme color (accent, work, need, ok, err, fg, dim) or #RRGGBB."),
    s!("insights.colors.you", en_other(CHART_COLORS), S("fg"), "appearance", false,
        "The color for your own activity in Insights charts: a theme color (accent, work, need, ok, err, fg, dim) or #RRGGBB."),
    s!("insights.colors.waiting", en_other(CHART_COLORS), S("need"), "appearance", false,
        "The color for agents blocked on you in Insights charts: a theme color (accent, work, need, ok, err, fg, dim) or #RRGGBB."),
    s!("updates.channel", en(&["stable", "beta"]), S("stable"), "general", false, "Which update channel midna follows."),
    s!("windows.close_with_terminals", en(&["ask", "close", "move"]), S("ask"), "general", false,
        "Closing a main window that still has terminals while another is open: ask, close its terminals, or move them to the window you used last."),
    s!("updates.feed_url", SettingKind::String, S(DEFAULT_FEED_URL), "general", true,
        "Update feed the app checks every 6 hours; {channel} is replaced by updates.channel. Updates must carry midna's ed25519 signature whatever the URL. Human only."),
    s!("webhooks.path", en(&["tailscale_funnel", "self_relay", "midna_relay", "off"]), S("off"), "webhooks", true,
        "How GitHub/Bitbucket webhooks reach this Mac."),
    s!("webhooks.port", SettingKind::Int, I(7787), "webhooks", false, "Local port the webhook receiver listens on."),
    s!("webhooks.relay_url", SettingKind::String, S(""), "webhooks", true, "URL of the self-hosted relay when webhooks.path is self_relay."),
    s!("triggers.agent_mode", en(&["supervised", "inherit"]), S("supervised"), "webhooks", true,
        "How agents started by a webhook trigger run. Their prompt contains text from the webhook (untrusted). supervised: Claude gets --permission-mode default and Codex approval_policy=on-request + sandbox_mode=workspace-write, so tool calls still ask (as needs-you items) even if your own config bypasses permissions. inherit: your normal agent config applies. Human only."),
    s!("agents.may_move_windows", SettingKind::Bool, B(false), "agents", true,
        "Allow agents to move, pop out, snap, keep-on-top or close windows via window.command. Human only."),
    s!("agents.may_close_idle", SettingKind::Bool, B(true), "agents", true,
        "Allow agents to close terminals that are idle, done or exited (other than their own) without asking. Human only."),
    s!("agents.may_force_close", SettingKind::Bool, B(false), "agents", true,
        "Allow agents to close any terminal without asking, including working ones and `close --force` (an unattended task board closing tabs when usage runs out or a PR is done). A rule on `close …` still applies. Off: those closes ask you. Human only."),
    s!("agents.may_install_updates", SettingKind::Bool, B(false), "agents", true,
        "Allow agents to install a downloaded update (`midna updates install`) without asking, e.g. an agent that tests each beta as it lands. The update's signature is still checked, and agents.restart_on_update still decides when running agents restart. Off: an agent's install asks you. Human only."),
    s!("agents.trust_folders", SettingKind::PathList, L(&[]), "agents", true,
        "Folders where midna answers an agent's \"trust this folder?\" startup dialog with Yes for you, instead of raising a needs-you item. An entry covers its folder and everything under it (~/Development trusts every project there). `*` matches within one folder name (~/work/client-*), `**` any number of folders (~/src/**/sandbox). Trusting a folder any other way adds it here: Trust on its needs-you item, or Yes in the agent's own dialog. Comma-separated on the CLI. Human only."),
    s!("approve.from_cli", SettingKind::Bool, B(false), "agents", true,
        "Allow agents to approve approval requests of their own session from the CLI. Human only."),
    s!("agents.claude.statusline", SettingKind::Bool, B(true), "agents", false,
        "Register midna's status line in Claude sessions midna launches; it reports real cost to insights (prints model and cost)."),
    s!("agents.mcp", SettingKind::Bool, B(true), "agents", false,
        "Give agents midna launches the `midna` MCP server (Claude: --mcp-config MIDNA_HOME/hooks/mcp.json; Codex: -c mcp_servers.midna.*), so they can drive midna as tools. No global config is touched. Applies to agents launched afterwards."),
    s!("agents.system_hint", SettingKind::Bool, B(true), "agents", false,
        "Tell agents midna launches, in two lines of system prompt (Claude: --append-system-prompt; Codex: -c developer_instructions), that they run in midna and can run `midna capabilities`. Applies to agents launched afterwards."),
    s!("agents.restart_on_update", en(&["ask", "when_idle", "off"]), S("ask"), "agents", false,
        "When a newer version of an agent is installed than the one a terminal runs (Claude: its status line reports an older version, or it shows \"Restart to update\"), restart that terminal into the same conversation. ask: when you open the terminal, offer Restart / When idle / Not now (no needs-you item; Not now hides it until a newer update). when_idle: queue the restart without asking. A queued restart runs once the agent is idle with no background work, subagents or scheduled wakeups in flight and an empty input box. off: do nothing. Changing it also answers prompts already showing (when_idle queues them, off hides them)."),
    s!("agents.adopt_typed", SettingKind::Bool, B(true), "agents", false,
        "A `claude` or `codex` typed into a midna shell terminal (zsh, bash or fish; also through an alias to the binary) runs under midna: midna's own `claude` / `codex` come first on PATH there and run the real one, so the terminal gets an agent terminal's status, hooks, update prompt (Claude) and restarts into the same conversation, inside the same shell. Subcommands and one-shot runs (`claude -p`, `codex exec`) run as typed. Off: everything runs exactly as typed. New shells pick up a change."),
    s!("agents.shell_on_exit", SettingKind::Bool, B(true), "agents", false,
        "When an agent midna started (⌘T, `midna open --agent`) exits, the terminal becomes your login shell in its folder, below the agent's last screen, as if you had typed the agent into that shell. Not for terminals opened to close on exit, or when the terminal was hung up. Off: the terminal stays on the agent's last screen, exited."),
    s!("agents.restart_idle_secs", SettingKind::Int, I(60), "agents", false,
        "How long an agent terminal must be quiet (no output, no turn) before a queued restart runs."),
    s!("agents.resume_after_sleep", SettingKind::Bool, B(true), "agents", false,
        "When the Mac wakes, resume agents that were mid-turn when it went to sleep: if one stops on an error (Claude's turn dies when its connection drops), queue agents.resume_after_sleep_prompt for it, up to 3 tries over the next 30 minutes, each once the network is back. An agent that carries on by itself is left alone."),
    s!("agents.resume_after_network", SettingKind::Bool, B(true), "agents", false,
        "When an agent's turn dies on a lost connection (Claude stops with a connection error when the network drops), wait until the network is back, then queue agents.resume_after_sleep_prompt for it, up to 3 tries. Gives up if the network is still down after 6 hours. An agent that carries on by itself is left alone."),
    s!("agents.resume_after_sleep_prompt", SettingKind::String, S("continue"), "agents", false,
        "What agents.resume_after_sleep and agents.resume_after_network type into an agent to resume it."),
    s!("keep_awake.enabled", SettingKind::Bool, B(false), "agents", false,
        "Keep the Mac from idle-sleeping during keep_awake hours, so agents, queued messages, schedule triggers and resumes keep going while you're away. midnad holds a macOS power assertion (\"midna: keeping awake for agents\"); the display may still sleep and the screen lock. Closing the lid still sleeps. `midna keep-awake` says whether it is held and why."),
    s!("keep_awake.mode", en(&["with_work", "always"]), S("with_work"), "agents", false,
        "When to hold it inside the hours. with_work: only while there is work (an agent working or with background work or scheduled wakeups, queued input, an agent waiting to resume, a schedule trigger due before the hours end), plus keep_awake.linger_mins after it. always: the whole time (pick this if webhook triggers should reach you)."),
    s!("keep_awake.start", SettingKind::String, S("09:00"), "agents", false,
        "When keep-awake hours start each day, local time: 8am, 8:30 AM or 08:00 (stored as HH:MM)."),
    s!("keep_awake.end", SettingKind::String, S("18:00"), "agents", false,
        "When keep-awake hours end, local time. At or before keep_awake.start runs past midnight (10pm to 2am); the same as start is all day."),
    s!("keep_awake.days", SettingKind::ItemList { options: &crate::keep_awake::DAYS, allow_paths: false }, L(&["mon", "tue", "wed", "thu", "fri"]), "agents", false,
        "Days keep-awake hours apply: mon … sun, weekdays, weekends, daily, a range (mon-fri) or a list (mon,wed,fri)."),
    s!("keep_awake.hours", SettingKind::RuleList, L(&[]), "agents", false,
        "Days with their own hours, overriding keep_awake.start/end/days: `fri = 9am-3pm`, `sat = off`, `sun = all day`, `mon-thu = 8am-7pm`. A day listed here is on (or off) whatever keep_awake.days says."),
    SettingSpec { range: Some((0, 100)), ..s!("keep_awake.min_battery", SettingKind::Int, I(20), "agents", false,
        "On battery, stop keeping awake below this percent, and start again only 5 points above it or once plugged in. 0 = no limit.") },
    SettingSpec { range: Some((0, 240)), ..s!("keep_awake.linger_mins", SettingKind::Int, I(5), "agents", false,
        "In with_work mode, keep holding this many minutes after the last work, so the next queued message, hook or reply still finds the Mac awake.") },
    s!("keep_awake.wake", SettingKind::Bool, B(false), "agents", false,
        "Wake the Mac from sleep for work scheduled inside the keep-awake hours (a schedule trigger, a message queued for a time, an agent's wakeup): midnad asks macOS for a wake 2 minutes before it, then keeps the Mac awake until the work has run. Needs a one-time admin grant (`midna keep-awake wake setup`). Closing the lid still keeps a laptop asleep."),
    SettingSpec { range: Some((50, 2000)), ..s!("system.busy_load", SettingKind::Int, I(150), "general", false,
        "When midna counts the Mac as busy: the 1-minute load average per core, in percent (100 = every core busy, 150 = half again as much work waiting). A busy Mac postpones a daemon upgrade or restart (under heavy load the handoff can time out and hang up every terminal); the app asks once whether to wait or update anyway, then updates by itself when the load drops. `midna system` shows the load.") },
    s!("guard.overload_alert", SettingKind::Bool, B(true), "general", false,
        "When the Mac stays busy (system.busy_load) for guard.overload_secs, show which terminals are using the CPU, with Pause, Stop processes and Resume. Event system.overloaded fires either way."),
    SettingSpec { range: Some((10, 3600)), ..s!("guard.overload_secs", SettingKind::Int, I(120), "general", false,
        "How long the Mac must stay busy before system.overloaded fires (and guard.overload_alert shows).") },
    SettingSpec { range: Some((0, 168)), ..s!("guard.loop_max_hours", SettingKind::Int, I(3), "general", false,
        "Stop background shell loops an agent left running (a `while`/`until` loop around a sleep, polling for a build, a file or a process) once they are this many hours old. Only processes under agent terminals; the agent and its shell keep running. 0 = never.") },
    SettingSpec { range: Some((0, 8760)), ..s!("worktrees.auto_clean_hours", SettingKind::Int, I(24), "general", false,
        "Remove linked git worktrees (of repos midna's terminals use) after this many hours without activity, when no terminal or process is in them, they have no uncommitted changes and no live `git worktree lock`. Their branches stay. `midna worktrees` lists them. 0 = never.") },
    s!("policy.default", en(&["auto", "allow", "ask", "deny"]), S("auto"), "policy", true,
        "Decision when no rule matches. auto = built-in defaults table (ask for destructive CLI verbs, allow otherwise). Human only."),
    s!("policy.request_timeout_secs", SettingKind::Int, I(300), "policy", false,
        "How long policy.request blocks waiting for a human to answer an approval before giving up."),
    s!("ui.ask.agent", en(&["claude", "codex"]), S("claude"), "general", false,
        "Agent the command bar's \"Ask an agent\" starts (tab toggles it there, and the choice is saved here)."),
    s!("ui.ask.scope", en(&["project", "root"]), S("project"), "general", false,
        "Where the command bar's \"Ask an agent\" starts the agent: the current project or the root (shift-tab toggles it)."),
    s!("finder.quick_action", SettingKind::Bool, B(true), "general", false,
        "Show \"Open in Midna\", with Midna's icon, when you right-click a folder in Finder: it opens a terminal in that folder. It's midna's Finder extension, the same switch as System Settings › Login Items & Extensions › Finder."),
    s!("terminal.option_as_meta", SettingKind::Bool, B(true), "terminal", false,
        "Option (alt) acts as Meta in terminals: option-b sends ESC b (word back in shells). Off = option types macOS characters (option-e e = é)."),
    s!("terminal.link_preview", en(&["hover", "cmd", "off"]), S("hover"), "terminal", false,
        "Preview a path or link in a card at the bottom right of the terminal: hover (resting the pointer on it), cmd (⌘-hover only) or off. The card stays while the pointer is on it and for a moment after it leaves."),
    s!("terminal.auto_name", en(&["agent", "prompt", "context", "off"]), S("agent"), "terminal", false,
        "Name terminals by themselves, from what they're doing. agent: the short summary Claude or Codex puts in the terminal title (no extra model call), falling back to prompt, then context, until it has one. prompt: the prompt shortened to a few words, filler dropped, without any model. context: the git branch (feat/auto-tab-names = Auto tab names) or worktree, else the folder; this one names shell terminals too. off: keep the names terminals were given. A name you set yourself always stays; rename a terminal back to its default (claude, codex, zsh, …) to let midna name it again."),
    s!("terminal.auto_name_updates", en(&["follow", "first"]), S("follow"), "terminal", false,
        "follow: keep renaming a terminal as its work changes (the agent's summary changes, a new prompt reads as a new task). first: name it once from each source and keep that name; a better source (the agent's summary over the prompt over the branch) still replaces it."),
    s!("terminal.prompt_bar", en(&["scrolled", "always"]), S("always"), "terminal", false,
        "The prompt bar in agent terminals (which prompt you're reading; click it for the list; Live ↓ to go back). It floats on the terminal's top row. always: even at the live end. scrolled: only while the view is scrolled back. Either way it hides Claude Code's own pinned copy of the prompt."),
    s!("terminal.preview_path_click", en(&["reveal", "ide", "copy"]), S("reveal"), "terminal", false,
        "What clicking the path in a link preview's header does: reveal it in Finder, open it in your IDE (ide.rules / ide.app), or copy it."),
    s!("terminal.image_paste", en(&["sheet", "inline"]), S("sheet"), "terminal", false,
        "What ⌘V of an image (a screenshot, or image files copied in Finder) does in a terminal. sheet: open the image sheet to add notes before it's sent. inline: paste the image's path right away (Claude Code shows it as [Image #N]). keys.paste_image_inline always pastes inline."),
    s!("projects.roots", SettingKind::PathList, L(&[]), "general", false,
        "Folders your projects live in (e.g. ~/Development). Their subfolders show up in the command bar and on the empty screen, ready to open as projects; a subfolder that isn't a git repo but holds some is looked into one level deeper. Nothing is added to the sidebar until you open one. Comma-separated on the CLI."),
    s!("ide.app", en_path(&["auto", "cursor", "vscode", "vscode-insiders", "windsurf", "kiro", "zed", "zed-preview", "sublime", "nova", "bbedit", "textmate", "intellij", "rustrover",
        "webstorm", "pycharm", "goland", "clion", "phpstorm", "rider", "rubymine", "android-studio", "xcode"]), S("auto"), "general", false,
        "Default IDE for “Open in IDE” (the header button, keys.open_ide) when no ide.rules entry matches: an editor id, an absolute path to an .app, or auto (the first one installed). ⌥-picking one in the button's menu saves it here."),
    s!("ide.rules", SettingKind::RuleList, L(&[]), "general", false,
        "Which IDE opens which folders, as `match = ide` rules (ide = an editor id or .app path, like ide.app). match is a project folder (`~/Development/app = cursor`, covers everything inside it) or a file in the folder (`pubspec.yaml = android-studio`, `*.xcodeproj = xcode`). Folder rules win (the longest), then file rules in order, then ide.app. Picking an IDE in the header menu saves a folder rule for that project. Comma-separated on the CLI."),
    s!("git.refresh_secs", SettingKind::Int, I(10), "general", false, "How often midnad refreshes git info for terminals."),
    s!("notify.enabled", SettingKind::Bool, B(true), "notifications", false,
        "Post macOS notifications at all. Each kind has its own notify.<kind> switch, and a terminal can override any of them (or mute itself) with `midna notify set`."),
    s!("notify.approval", SettingKind::Bool, B(true), "notifications", false,
        "Notify when an agent waits on you: an approval request, a permission prompt or a question in its terminal."),
    s!("notify.attention", SettingKind::Bool, B(true), "notifications", false, "Notify when an agent raises a needs-you note or says it's blocked."),
    s!("needs_you.replace", SettingKind::Bool, B(true), "notifications", false,
        "A terminal raising a new blocked or note item (`midna attention`) takes back the open ones it raised before, so only its latest shows."),
    s!("needs_you.clear_notes_on_open", SettingKind::Bool, B(true), "notifications", false,
        "Opening a terminal marks its notes (FYI needs-you items) done: you've seen them."),
    s!("needs_you.withdraw_orphans", SettingKind::Bool, B(true), "notifications", false,
        "Take back approvals nobody can act on any more: no caller is waiting on the answer (say midnad restarted under it), or an agent's human-only request whose target changed since it asked (approving it would be turned down)."),
    s!("needs_you.clear_failed_on_run", SettingKind::Bool, B(true), "notifications", false,
        "A terminal starting work again clears its failures: failed needs-you items, and failure notifications held in the floating badge."),
    s!("notify.badge.clear_on_open", SettingKind::Bool, B(true), "notifications", false,
        "Opening a terminal takes its notifications off the floating badge (kinds that stay until dismissed); needs-you items stay until they're handled."),
    SettingSpec { range: Some((0, 720)), ..s!("needs_you.expire_hours", SettingKind::Int, I(24), "notifications", true,
        "Take back needs-you items nobody answered after this many hours: approvals, permission prompts, blocked, notes and failures (a caller waiting on one hears it was withdrawn). 0 = never. Rule-removal requests, missing secrets and waiting triggers never expire. Human only.") },
    s!("notify.failed", SettingKind::Bool, B(true), "notifications", false, "Notify when a terminal's command fails or an agent's turn ends in an error."),
    s!("notify.turn_done", SettingKind::Bool, B(true), "notifications", false,
        "Notify when an agent finishes a turn that took at least notify.turn_done_min_secs."),
    s!("notify.agent", SettingKind::Bool, B(true), "notifications", false, "Show notifications agents send on purpose (`midna notify send`)."),
    s!("notify.from_trigger", SettingKind::Bool, B(true), "notifications", false, "Show notifications triggers send (a trigger's `notify` action)."),
    s!("notify.requests", SettingKind::Bool, B(false), "notifications", false,
        "Notify for needs-you items that can wait: a trigger to enable, a webhook secret to set, a rule an agent wants removed."),
    s!("notify.background", SettingKind::Bool, B(false), "notifications", false, "Notify when a background shell an agent started finishes."),
    s!("notify.pr_checks", SettingKind::Bool, B(false), "notifications", false, "Notify when the checks on a terminal's pull request finish (passing or failing)."),
    s!("notify.exited", SettingKind::Bool, B(false), "notifications", false, "Notify when a terminal's process exits cleanly."),
    s!("notify.triggers", SettingKind::Bool, B(false), "notifications", false, "Notify when a webhook trigger fires."),
    s!("notify.restarted", SettingKind::Bool, B(false), "notifications", false, "Notify when midna restarts an agent into the same conversation after an update."),
    s!("notify.turn_done_min_secs", SettingKind::Int, I(30), "notifications", false,
        "An agent's turn must take at least this long to notify when it finishes (quick replies you watched don't). 0 = every turn."),
    SettingSpec { range: Some((0, 100)), ..s!("notify.volume", SettingKind::Int, I(100), "notifications", false,
        "Volume of every notification sound, 0–100 (0 = silent). Each kind's notify.volume.<kind> is scaled by it; each kind picks its sound with notify.sound.<kind>.") },
    s!("notify.image", SettingKind::String, S(""), "notifications", false,
        "Image shown on every notification (an image imported with `midna notify import <file>`, by its file name; empty = none). A kind's notify.image.<kind> overrides it."),
    snd!("approval", "Portal"), push!("approval", true), pushf!("approval"), vol!("approval"), pic!("approval"), ttl!("approval"), txt!("approval", "{{title}}, {{detail}}, {{kind}} and {{action}} (what it wants to run). "), stay!("approval", 0), color!("approval", "need"), bell!("approval", true),
    snd!("attention", "Call"), push!("attention", true), pushf!("attention"), vol!("attention"), pic!("attention"), ttl!("attention"), txt!("attention", "{{title}}, {{detail}} and {{kind}}. "), stay!("attention", 0), color!("attention", "need"), bell!("attention", true),
    snd!("failed", "Uh-oh"), push!("failed", false), pushf!("failed"), vol!("failed"), pic!("failed"), ttl!("failed"), txt!("failed", "{{title}}, {{detail}} and {{kind}} (a failed command), or {{reason}} (an agent turn that failed). "), stay!("failed", 0), color!("failed", "err"), bell!("failed", true),
    snd!("turn_done", "Strum"), push!("turn_done", false), pushf!("turn_done"), vol!("turn_done"), pic!("turn_done"), ttl!("turn_done"), txt!("turn_done", "{{elapsed}} (2m 5s), {{secs}}, {{reply}} (the first line of its reply) and {{message}} (all of it). "), stay!("turn_done", 6), color!("turn_done", "ok"), bell!("turn_done", false),
    snd!("agent", "Hm"), push!("agent", false), pushf!("agent"), vol!("agent"), pic!("agent"), ttl!("agent"), txt!("agent", "{{title}} and {{body}} (what the agent sent). "), stay!("agent", 6), color!("agent", "accent"), bell!("agent", false),
    snd!("from_trigger", "Hm"), push!("from_trigger", false), pushf!("from_trigger"), vol!("from_trigger"), pic!("from_trigger"), ttl!("from_trigger"), txt!("from_trigger", "{{title}} and {{body}} (the trigger’s notify action). "), stay!("from_trigger", 6), color!("from_trigger", "work"), bell!("from_trigger", false),
    snd!("requests", "none"), push!("requests", false), pushf!("requests"), vol!("requests"), pic!("requests"), ttl!("requests"), txt!("requests", "{{title}}, {{detail}} and {{kind}}. "), stay!("requests", 0), color!("requests", "accent"), bell!("requests", true),
    snd!("background", "none"), push!("background", false), pushf!("background"), vol!("background"), pic!("background"), ttl!("background"), txt!("background", "{{count}} (tasks that finished). "), stay!("background", 6), color!("background", "ok"), bell!("background", false),
    snd!("pr_checks", "none"), push!("pr_checks", false), pushf!("pr_checks"), vol!("pr_checks"), pic!("pr_checks"), ttl!("pr_checks"), txt!("pr_checks", "{{number}}, {{checks}} (passing or failing) and {{failing}} (how many). "), stay!("pr_checks", 6), color!("pr_checks", "ok"), bell!("pr_checks", false),
    snd!("exited", "none"), push!("exited", false), pushf!("exited"), vol!("exited"), pic!("exited"), ttl!("exited"), txt!("exited", ""), stay!("exited", 6), color!("exited", "dim"), bell!("exited", false),
    snd!("triggers", "none"), push!("triggers", false), pushf!("triggers"), vol!("triggers"), pic!("triggers"), ttl!("triggers"), txt!("triggers", "{{name}} and {{outcome}}. "), stay!("triggers", 6), color!("triggers", "work"), bell!("triggers", false),
    snd!("restarted", "none"), push!("restarted", false), pushf!("restarted"), vol!("restarted"), pic!("restarted"), ttl!("restarted"), txt!("restarted", "{{reason}}. "), stay!("restarted", 6), color!("restarted", "work"), bell!("restarted", false),
    snd!("approved", "Rise"), vol!("approved"),
    snd!("denied", "Nn-nn"), vol!("denied"),
    snd!("queue_sent", "Whoosh"), vol!("queue_sent"),
    snd!("image_added", "Fwip"), vol!("image_added"),
    snd!("closed", "Close"), vol!("closed"),
    snd!("switched", "Tick"), vol!("switched"),
    snd!("command_bar", "Thump"), vol!("command_bar"),
    snd!("copied", "Tick-tick"), vol!("copied"),
    s!("notify.sounds", SettingKind::Bool, B(true), "notifications", false,
        "Play sounds at all: notification sounds and sound effects (approve, deny, a queued message sent, switching terminals, …). ⌘K “Mute sounds” turns this off."),
    s!("notify.sounds_in_app", SettingKind::Bool, B(true), "notifications", false,
        "Play sounds while midna is the frontmost app: sound effects for what you do, and a notification's sound when its banner is skipped because you're looking at that terminal. Off: midna is quiet while you use it, and you hear only what happens while you're in another app."),
    s!("notify.badge", en(&["background", "always", "off"]), S("background"), "notifications", false,
        "The floating badge in a corner of the screen: it counts what needs you, and a line springs out beside it when a notification comes in. While you're in midna it springs onto the window's top right (a band sweeps across it when something comes in, and now and then while anything waits); leaving, it springs back to its corner. background = hidden while another midna build is the app in front, always = shown then too, off = no badge (in midna, cards show in the window instead)."),
    s!("notify.badge.corner", en(&["top_right", "top_left", "bottom_right", "bottom_left"]), S("top_right"), "notifications", false,
        "Which corner of the screen the floating badge sits in (the stack and the lines open toward the middle). Drag the badge to move it."),
    s!("notify.badge.snap", en(&["corner", "free"]), S("corner"), "notifications", false,
        "Where the badge goes when you drop it: corner = it slings to the nearest corner (notify.badge.inset_x / inset_y away from it), free = it stays where you drop it (saved as the nearest corner and its distances from it)."),
    SettingSpec { range: Some((0, 4000)), ..s!("notify.badge.inset_x", SettingKind::Int, I(14), "notifications", false,
        "How far the badge sits from the left or right edge of the screen, in points.") },
    SettingSpec { range: Some((0, 4000)), ..s!("notify.badge.inset_y", SettingKind::Int, I(14), "notifications", false,
        "How far the badge sits from the top or bottom of the screen (below the menu bar, above the Dock), in points.") },
    s!("notify.badge.sharing", en(&["hide", "count", "show"]), S("hide"), "notifications", false,
        "While your screen is being shared or recorded: hide = no badge until it stops (what came in is counted; the lines aren't replayed), count = the badge and its number only, no lines and no text, show = as usual. midna asks macOS whether something is watching the screen, so it covers Zoom, Meet, Teams, screen recording and Screen Sharing."),
    s!("notify.badge.idle", SettingKind::Bool, B(false), "notifications", false,
        "Keep the floating badge out when nothing is waiting (it shows 0) instead of hiding it until something comes in. notify.badge off and screen sharing still hide it."),
    s!("notify.when_app_closed", SettingKind::Bool, B(true), "notifications", false,
        "When the midna app isn't running, midnad posts the notification itself (shown as a system notification; clicking it doesn't open midna)."),
    s!("keys.command_bar", KB, S("cmd-k"), "keys", false, "Open the command bar."),
    s!("keys.next_needs_you", KB, S("cmd-j"), "keys", false, "Jump to the next needs-you item."),
    s!("keys.new_agent", KB, S("cmd-t"), "keys", false, "New agent terminal in the current project, running the agent last started there."),
    s!("keys.new_terminal", KB, S("cmd-shift-t"), "keys", false, "New terminal in the current project."),
    s!("keys.new_agent_root", KB, S("cmd-alt-t"), "keys", false, "New agent terminal at root (no project, starts in $HOME)."),
    s!("keys.new_terminal_root", KB, S("cmd-alt-shift-t"), "keys", false, "New terminal at root (no project, starts in $HOME)."),
    s!("keys.approve", KB, S("cmd-enter"), "keys", false, "Approve the focused approval."),
    s!("keys.deny", KB, S("cmd-backspace"), "keys", false, "Deny the focused approval."),
    s!("keys.settings", KB, S("cmd-comma"), "keys", false, "Open the Settings window."),
    s!("keys.rules", KB, S("cmd-shift-r"), "keys", false, "Show Rules."),
    s!("keys.triggers", KB, S("cmd-shift-g"), "keys", false, "Show Triggers."),
    s!("keys.insights", KB, S("cmd-shift-i"), "keys", false, "Show Insights."),
    s!("keys.open_project", KB, S("cmd-o"), "keys", false, "Open a folder as a project (folder picker)."),
    s!("keys.new_window", KB, S("cmd-shift-n"), "keys", false, "Open another main window."),
    s!("keys.composer", KB, S("cmd-shift-d"), "keys", false,
        "Open the composer under the terminal to type or paste a long prompt; enter sends it to the terminal, esc cancels."),
    s!("keys.add_image", KB, S("cmd-i"), "keys", false,
        "Add images with numbered notes to the selected terminal's next message (pasted in when you press enter there)."),
    s!("keys.prev_prompt", KB, S(""), "keys", false, "In an agent terminal, scroll to the previous prompt you sent. Unbound by default."),
    s!("keys.next_prompt", KB, S(""), "keys", false, "In an agent terminal, scroll to the next prompt you sent (past the last: back to live). Unbound by default."),
    s!("keys.prompts", KB, S("cmd-p"), "keys", false, "List the prompts you sent the selected agent terminal, to search and jump to one."),
    s!("keys.queue", KB, S("cmd-u"), "keys", false,
        "Open the selected terminal's queued messages: what will be typed into it next, once the agent is ready."),
    s!("keys.links", KB, S("cmd-l"), "keys", false,
        "Open the selected agent terminal's links: the URLs, PRs, artifacts and files that came up in its conversation."),
    s!("keys.subagents", KB, S("cmd-alt-a"), "keys", false,
        "Open the selected agent terminal's subagents: what each is doing, and a read-only window that follows one live."),
    s!("keys.needs_you", KB, S("cmd-shift-j"), "keys", false, "Open the needs-you cards (approvals and questions, one at a time)."),
    s!("keys.approve_options", KB, S("cmd-shift-enter"), "keys", false, "Show the approve options (15 min, 1 h, session, always) for the focused approval."),
    s!("keys.next_terminal", KB, S("cmd-alt-down"), "keys", false, "Select the next terminal in the sidebar (past the last: the first)."),
    s!("keys.prev_terminal", KB, S("cmd-alt-up"), "keys", false, "Select the previous terminal in the sidebar (past the first: the last)."),
    s!("keys.project_menu", KB, S("cmd-alt-p"), "keys", false, "Open the current project's … menu in the sidebar."),
    s!("keys.fold_project", KB, S("cmd-alt-f"), "keys", false, "Fold or unfold the current project's terminals in the sidebar."),
    s!("keys.sidebar", KB, S("cmd-b"), "keys", false, "Collapse the sidebar to a rail of status dots, or expand it again."),
    s!("keys.rename", KB, S("cmd-shift-e"), "keys", false, "Rename the selected terminal."),
    s!("keys.restart", KB, S("cmd-alt-r"), "keys", false, "Restart the selected terminal."),
    s!("keys.replace", KB, S("cmd-r"), "keys", false, "Replace the selected terminal with a new session in its place (like a new tab, then closing the old one)."),
    s!("keys.terminal_menu", KB, S("cmd-alt-m"), "keys", false, "Open the selected terminal's … menu."),
    s!("keys.copy_session_id", KB, S("cmd-alt-c"), "keys", false, "Copy the selected terminal's session id: its Claude or Codex conversation id when it has one, else Midna's."),
    s!("keys.mute", KB, S("cmd-shift-m"), "keys", false, "Mute or unmute notifications for the selected terminal."),
    s!("keys.clear", KB, S("cmd-shift-k"), "keys", false, "Clear the focused terminal's screen and scrollback."),
    s!("keys.split", KB, S("cmd-d"), "keys", false, "Split: open a shell next to the selected terminal, or close the split."),
    s!("keys.split_orientation", KB, S("cmd-alt-d"), "keys", false, "Flip the split between side by side and stacked."),
    s!("keys.split_to_main", KB, S("cmd-alt-enter"), "keys", false, "Show the split's terminal in the main pane."),
    s!("keys.focus_pane", KB, S("cmd-bracketright"), "keys", false, "Move focus to the other split pane."),
    s!("keys.open_ide", KB, S("cmd-alt-e"), "keys", false, "Open the selected terminal's folder in your IDE (ide.app)."),
    s!("keys.choose_ide", KB, S("cmd-alt-shift-e"), "keys", false, "Choose an IDE to open the selected terminal's folder in; the choice is remembered (ide.app)."),
    s!("keys.pop_out", KB, S("cmd-shift-o"), "keys", false, "Pop the selected terminal out into its own window (in a pop-out: back to the main window)."),
    s!("keys.keep_on_top", KB, S("cmd-alt-o"), "keys", false, "In a pop-out window, keep it on top of other windows (or stop)."),
    s!("keys.edit_attachment", KB, S("cmd-e"), "keys", false, "Edit the images added to the selected terminal but not sent yet."),
    s!("keys.paste_image_inline", KB, S("cmd-shift-v"), "keys", false,
        "Paste the image on the clipboard straight into the terminal as its path, without the image sheet (Claude Code shows it as [Image #N]). With no image on the clipboard it pastes text, like ⌘V."),
    s!("keys.note_newline", KB, S("shift-enter"), "keys", false, "In the image sheet, start a new line in the note you're editing (enter saves the note)."),
    s!("keys.close", KB, S("cmd-w"), "keys", false, "Close the selected terminal (in Settings or a pop-out: that window)."),
    s!("keys.quit", KB, S("cmd-q"), "keys", false, "Quit midna (hold it in the main window). Terminals keep running in midnad."),
    s!("kass.auto_send", SettingKind::Bool, B(false), "kass", false,
        "Send the composer's text to the terminal as soon as a Kass dictation ends, instead of keeping it open for review."),
];

pub fn setting(key: &str) -> Option<&'static SettingSpec> {
    SETTINGS.iter().find(|s| s.key == key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_lists_take_arrays_or_comma_separated_text() {
        let s = setting("projects.roots").unwrap();
        assert_eq!(s.coerce(&json!("~/Development, /Users/me/work/,~/Development")), Ok(json!(["~/Development", "/Users/me/work"])));
        assert_eq!(s.coerce(&json!(["~"])), Ok(json!(["~"])));
        assert_eq!(s.coerce(&json!("")), Ok(json!([])));
        assert!(s.coerce(&json!("Development")).is_err());
        assert!(s.coerce(&json!([1])).is_err());
        assert_eq!(s.default.to_json(), json!([]));
    }

    #[test]
    fn every_notify_category_has_a_setting_with_its_default() {
        for c in crate::notify::CATEGORIES {
            let s = setting(&crate::notify::setting_key(c.key)).unwrap_or_else(|| panic!("no setting for notify.{}", c.key));
            assert_eq!(s.default.to_json(), json!(c.default), "{}", c.key);
            let snd = setting(&crate::notify::sound_key(c.key)).unwrap_or_else(|| panic!("no sound setting for {}", c.key));
            assert_eq!(snd.default.to_json(), json!(c.sound), "{}", c.key);
            assert!(setting(&crate::notify::volume_key(c.key)).is_some_and(|v| v.range == Some((0, 100))), "{}", c.key);
            assert!(setting(&crate::notify::image_key(c.key)).is_some(), "{}", c.key);
            let push = setting(&crate::notify::push_key(c.key)).unwrap_or_else(|| panic!("no push setting for {}", c.key));
            assert_eq!(push.default.to_json(), json!(c.push), "{}", c.key);
            let focused = setting(&crate::notify::push_focused_key(c.key)).unwrap_or_else(|| panic!("no push_focused setting for {}", c.key));
            assert_eq!(focused.default.to_json(), json!(false), "{}", c.key);
            assert!(setting(&crate::notify::stay_key(c.key)).is_some_and(|v| v.range == Some((0, 3600))), "{}", c.key);
            let color = setting(&crate::notify::color_key(c.key)).unwrap_or_else(|| panic!("no color setting for {}", c.key));
            assert!(crate::notify::valid_color(color.default.to_json().as_str().unwrap()), "{}", c.key);
            // Only what needs you, or went wrong, counts on the bell out of the box.
            let counts = ["approval", "attention", "requests", "failed"].contains(&c.key);
            assert_eq!(setting(&crate::notify::bell_key(c.key)).map(|b| b.default.to_json()), Some(json!(counts)), "{}", c.key);
        }
        for f in crate::notify::KIND_FIELDS {
            assert!(custom_kind_spec(f).is_some(), "no custom kind spec for `{f}`");
        }
        for e in crate::notify::EFFECTS {
            let snd = setting(&crate::notify::sound_key(e.key)).unwrap_or_else(|| panic!("no sound setting for {}", e.key));
            assert_eq!(snd.default.to_json(), json!(e.sound), "{}", e.key);
            let vol = setting(&crate::notify::volume_key(e.key)).unwrap_or_else(|| panic!("no volume setting for {}", e.key));
            assert_eq!((vol.default.to_json(), vol.range), (json!(e.volume), Some((0, 100))), "{}", e.key);
            assert!(crate::notify::category(e.key).is_none(), "{} is both a category and an effect", e.key);
        }
        let SettingKind::Enum { options, .. } = setting("notify.sound.approval").unwrap().ty else { panic!() };
        let builtin: Vec<&str> = crate::notify::TWILIGHT.iter().chain(crate::notify::SYSTEM_SOUNDS).copied().collect();
        assert_eq!(&options[1..], builtin.as_slice());
    }

    #[test]
    fn notify_colors_take_theme_names_or_hex() {
        let s = setting("notify.color.approval").unwrap();
        assert_eq!(s.coerce(&json!("ERR")), Ok(json!("err")));
        assert_eq!(s.coerce(&json!(" #A1b2C3 ")), Ok(json!("#a1b2c3")));
        assert!(s.coerce(&json!("pink")).is_err());
        assert!(s.coerce(&json!("#12345")).is_err());
        assert!(custom_kind_spec("color").unwrap().coerce(&json!("#00ff00")).is_ok());
    }

    #[test]
    fn kind_keys_split_into_field_and_kind() {
        use crate::notify::{reserved_kind_key, split_kind_key};
        assert_eq!(split_kind_key("notify.stay.deploys"), Some(("stay", "deploys")));
        assert_eq!(split_kind_key("notify.push_focused.deploys"), Some(("push_focused", "deploys")));
        assert_eq!(split_kind_key("notify.bell.deploys"), Some(("bell", "deploys")));
        assert_eq!(split_kind_key("notify.deploys"), Some(("", "deploys")));
        assert_eq!(split_kind_key("notify.stay.Deploys"), None);
        assert_eq!(split_kind_key("theme.deploys"), None);
        for k in ["approval", "copied", "enabled", "volume", "badge", "stay", "kinds"] {
            assert!(reserved_kind_key(k), "{k}");
        }
        assert!(!reserved_kind_key("deploys"));
    }

    #[test]
    fn rule_lists_normalize_and_reject_half_rules() {
        let s = setting("ide.rules").unwrap();
        assert_eq!(s.coerce(&json!("~/Development/app/=cursor, pubspec.yaml = android-studio,~/Development/app = cursor")), Ok(json!(["~/Development/app = cursor", "pubspec.yaml = android-studio"])));
        assert_eq!(s.coerce(&json!(["*.xcodeproj=xcode"])), Ok(json!(["*.xcodeproj = xcode"])));
        assert_eq!(s.coerce(&json!("")), Ok(json!([])));
        assert!(s.coerce(&json!("pubspec.yaml")).is_err());
        assert!(s.coerce(&json!("= xcode")).is_err());
    }

    #[test]
    fn insights_layouts_check_widgets_sizes_and_names() {
        let s = setting("insights.layouts").unwrap();
        assert_eq!(s.coerce(&json!(["Mine = heatmap:large approved"])).unwrap(), json!(["Mine = heatmap:large approved"]));
        assert!(s.coerce(&json!(["Mine = nope"])).unwrap_err().contains("unknown widget `nope`"));
        assert!(s.coerce(&json!(["Mine = headline:small"])).unwrap_err().contains("headline comes in wide"));
        assert!(s.coerce(&json!(["A = turns", "A = spend"])).unwrap_err().contains("two layouts"));
        for l in DEFAULT_INSIGHTS_LAYOUTS {
            assert!(s.coerce(&json!([l])).is_ok(), "{l}");
        }
        let items = parse_insights_layout("heatmap:large approved spend:huge heatmap nope");
        assert_eq!(items, vec![("heatmap".into(), "large".into()), ("approved".into(), "small".into()), ("spend".into(), "small".into())]);
        assert_eq!(format_insights_layout(&items), "heatmap:large approved spend");
    }

    #[test]
    fn status_items_keep_order_and_reject_unknown_names() {
        let s = setting("ui.status.items").unwrap();
        assert_eq!(s.coerce(&json!("script, daemon,spacer,daemon")), Ok(json!(["script", "daemon", "spacer"])));
        assert_eq!(s.coerce(&json!(["daemon", "/Users/me/bin/ci.sh"])), Ok(json!(["daemon", "/Users/me/bin/ci.sh"])));
        assert_eq!(s.coerce(&json!([])), Ok(json!([])));
        assert!(s.coerce(&json!("daemon, clock")).is_err());
        assert!(s.coerce(&json!("bin/ci.sh")).is_err());
        assert!(s.default.to_json().as_array().unwrap().iter().all(|v| STATUS_ITEMS.contains(&v.as_str().unwrap())));
    }

    #[test]
    fn script_values_are_built_in_only_when_every_part_is() {
        assert!(is_builtin_script("worktree+branch"));
        assert!(is_builtin_script("github+agent"));
        assert!(is_builtin_script(""));
        assert!(!is_builtin_script("worktree+/bin/date"));
        assert!(!is_builtin_script("/usr/local/bin/seg"));
        assert_eq!(setting("ui.header.buttons").unwrap().default.to_json(), json!(DEFAULT_HEADER_BUTTONS));
        for k in ["ui.header.script", "ui.row.script", "ui.status.script"] {
            let SettingKind::Enum { options, .. } = setting(k).unwrap().ty else { panic!() };
            assert!(options.iter().all(|o| is_builtin_script(o)), "{k}");
        }
    }

    #[test]
    fn status_looks_merge_per_field_and_reject_typos() {
        let s = setting("ui.status.looks").unwrap();
        let rules = s.coerce(&json!(["needs_you = pink icon:bell label:Your turn", "claude.needs_you = icon:lock", "done=green"])).unwrap();
        let rules: Vec<String> = serde_json::from_value(rules).unwrap();
        assert_eq!(status_look(&rules, "claude", "needs_you"), StatusLook { color: Some("pink".into()), icon: Some("lock".into()), label: Some("Your turn".into()) });
        assert_eq!(status_look(&rules, "codex", "needs_you").icon.as_deref(), Some("bell"));
        assert_eq!(status_look(&rules, "shell", "done"), StatusLook { color: Some("green".into()), ..Default::default() });
        assert_eq!(status_look(&rules, "shell", "working"), StatusLook::default());
        assert!(s.coerce(&json!("needsyou = red")).is_err());
        assert!(s.coerce(&json!("working = bleu")).is_err());
        assert!(s.coerce(&json!("claude.working = #7aa2f7 label:Thinking hard")).is_ok());
    }

    #[test]
    fn volumes_stay_in_range() {
        let v = setting("notify.volume.approval").unwrap();
        assert_eq!(v.coerce(&json!("60%")), Ok(json!(60)));
        assert!(v.coerce(&json!(101)).is_err());
        assert!(v.coerce(&json!(-1)).is_err());
    }
}
