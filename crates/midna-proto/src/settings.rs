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

use DefaultValue::{Bool as B, Int as I, List as L, Str as S};
const fn en(options: &'static [&'static str]) -> SettingKind {
    SettingKind::Enum { options, allow_other: false }
}
const fn en_path(options: &'static [&'static str]) -> SettingKind {
    SettingKind::Enum { options, allow_other: true }
}
const KB: SettingKind = SettingKind::Keybinding;

/// Built-in parts of `ui.header.script` / `ui.row.script` / `ui.status.script`. Join them
/// with `+` (`worktree+branch`); `github` = worktree+branch+sync+diff+files+pr.
pub const SCRIPT_PARTS: &[&str] = &["none", "github", "agent", "worktree", "branch", "sync", "diff", "git-diff-stats", "files", "pr"];

/// True when a script setting's value is built-in parts only (not a custom executable).
pub fn is_builtin_script(v: &str) -> bool {
    v.is_empty() || v.split('+').all(|p| SCRIPT_PARTS.contains(&p.trim()))
}

/// The terminal header's built-in toolbar buttons, for `ui.header.buttons` (More is always there, last).
pub const HEADER_BUTTONS: &[&str] = &["subagents", "links", "ide", "image", "split", "popout", "restart"];

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
pub const STATUS_ITEMS: &[&str] = &["daemon", "policy", "webhooks", "triggers", "hooks", "accessibility", "script", "spacer", "update", "keys"];

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
    s!("ui.row.script", en_path(&["worktree+diff", "worktree+branch", "diff", "none"]), S("worktree+diff"), "appearance", false,
        "Script that renders the extra text on each sidebar terminal row: built-in parts joined with + (worktree, branch, diff, …) or an executable path (`midna explain scripts`)."),
    s!("ui.header.buttons", SettingKind::ItemList { options: HEADER_BUTTONS, allow_paths: true }, L(&["subagents", "links", "ide", "image", "split", "popout", "restart"]), "appearance", false,
        "Header toolbar buttons, left to right (More is always last): subagents, links, ide, image, split, popout, restart, or an absolute path to your own button script (it prints the button's look and runs again with MIDNA_CLICK=1 when clicked; `midna explain scripts`). A built-in left out moves into the More (…) menu; its shortcut still works. Adding a script path is human only."),
    s!("ui.status.looks", SettingKind::RuleList, L(&[]), "appearance", false,
        "Restyle built-in statuses, one rule per status, only what you list: `<status> = <color> icon:<name> label:<text>` (each part optional). status: idle, working, needs_you, done, failed, exited, optionally for one kind of terminal (`claude.working`, `codex.done`, `shell.failed`; its fields override the plain rule's). color = the dot (red, orange, amber, yellow, green, teal, blue, purple, pink, gray or #rrggbb); icon replaces the agent icon (check, cross, bell, lock, bolt, play, …); label shows in the header and the row's second line. A trigger's custom status still wins. E.g. `needs_you = pink icon:bell label:Your turn`."),
    s!("ui.status.script", en_path(&["worktree+branch", "branch", "github", "none"]), S("worktree+branch"), "appearance", false,
        "Script behind the status bar's `script` item, run for the selected terminal: built-in parts joined with + or an executable path (`midna explain scripts`)."),
    s!("ui.status.items", SettingKind::ItemList { options: STATUS_ITEMS, allow_paths: true }, L(&["daemon", "policy", "webhooks", "triggers", "hooks", "accessibility", "spacer", "script", "update", "keys"]), "appearance", false,
        "What the status bar shows, left to right: daemon, policy, webhooks, triggers, hooks, accessibility, script (ui.status.script), spacer (the rest goes right), update, keys, or an absolute path to your own script (one item each; see `midna explain scripts`). Leave one out to hide it. Adding a script path is human only."),
    s!("ui.haptics", SettingKind::Bool, B(true), "appearance", false,
        "A light tap on a Force Touch trackpad when you click a button, row or link anywhere in the app. A mouse or an older trackpad ignores it."),
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
    s!("approve.from_cli", SettingKind::Bool, B(false), "agents", true,
        "Allow agents to approve approval requests of their own session from the CLI. Human only."),
    s!("agents.claude.statusline", SettingKind::Bool, B(true), "agents", false,
        "Register midna's status line in Claude sessions midna launches; it reports real cost to insights (prints model and cost)."),
    s!("agents.mcp", SettingKind::Bool, B(true), "agents", false,
        "Give agents midna launches the `midna` MCP server (Claude: --mcp-config MIDNA_HOME/hooks/mcp.json; Codex: -c mcp_servers.midna.*), so they can drive midna as tools. No global config is touched. Applies to agents launched afterwards."),
    s!("agents.system_hint", SettingKind::Bool, B(true), "agents", false,
        "Tell agents midna launches, in two lines of system prompt (Claude: --append-system-prompt; Codex: -c developer_instructions), that they run in midna and can run `midna capabilities`. Applies to agents launched afterwards."),
    s!("agents.restart_on_update", en(&["ask", "when_idle", "off"]), S("ask"), "agents", false,
        "When a newer version of an agent is installed than the one a terminal runs (Claude: its status line reports an older version, or it shows \"Restart to update\"), restart that terminal into the same conversation. ask: when you open the terminal, offer Restart / When idle / Not now (no needs-you item; Not now hides it until a newer update). when_idle: queue the restart without asking. A queued restart runs once the agent is idle with no background work, subagents or scheduled wakeups in flight and an empty input box. off: do nothing."),
    s!("agents.adopt_typed", SettingKind::Bool, B(true), "agents", false,
        "A `claude` or `codex` typed into a midna shell terminal (zsh, bash or fish; also through an alias to the binary) runs under midna: midna's own `claude` / `codex` come first on PATH there and run the real one, so the terminal gets an agent terminal's status, hooks, update prompt (Claude) and restarts into the same conversation, inside the same shell. Subcommands and one-shot runs (`claude -p`, `codex exec`) run as typed. Off: everything runs exactly as typed. New shells pick up a change."),
    s!("agents.restart_idle_secs", SettingKind::Int, I(60), "agents", false,
        "How long an agent terminal must be quiet (no output, no turn) before a queued restart runs."),
    s!("agents.resume_after_sleep", SettingKind::Bool, B(true), "agents", false,
        "When the Mac wakes, resume agents that were mid-turn when it went to sleep: if one stops on an error (Claude's turn dies when its connection drops), queue agents.resume_after_sleep_prompt for it, up to 3 tries over the next 30 minutes. An agent that carries on by itself is left alone."),
    s!("agents.resume_after_sleep_prompt", SettingKind::String, S("continue"), "agents", false,
        "What agents.resume_after_sleep types into an agent to resume it."),
    s!("policy.default", en(&["auto", "allow", "ask", "deny"]), S("auto"), "policy", true,
        "Decision when no rule matches. auto = built-in defaults table (ask for destructive CLI verbs, allow otherwise). Human only."),
    s!("policy.request_timeout_secs", SettingKind::Int, I(300), "policy", false,
        "How long policy.request blocks waiting for a human to answer an approval before giving up."),
    s!("ui.ask.agent", en(&["claude", "codex"]), S("claude"), "general", false,
        "Agent the command bar's \"Ask an agent\" starts (tab toggles it there, and the choice is saved here)."),
    s!("ui.ask.scope", en(&["project", "root"]), S("project"), "general", false,
        "Where the command bar's \"Ask an agent\" starts the agent: the current project or the root (shift-tab toggles it)."),
    s!("finder.quick_action", SettingKind::Bool, B(true), "general", false,
        "Show \"Open in Midna\" when you right-click a folder in Finder (under Quick Actions): it opens a terminal in that folder. midna keeps a Quick Action in ~/Library/Services while this is on and removes it when off."),
    s!("terminal.option_as_meta", SettingKind::Bool, B(true), "terminal", false,
        "Option (alt) acts as Meta in terminals: option-b sends ESC b (word back in shells). Off = option types macOS characters (option-e e = é)."),
    s!("terminal.link_preview", en(&["hover", "cmd", "off"]), S("hover"), "terminal", false,
        "Preview a path or link in a card at the bottom right of the terminal: hover (resting the pointer on it), cmd (⌘-hover only) or off. The card stays while the pointer is on it and for a moment after it leaves."),
    s!("terminal.preview_path_click", en(&["reveal", "ide", "copy"]), S("reveal"), "terminal", false,
        "What clicking the path in a link preview's header does: reveal it in Finder, open it in your IDE (ide.rules / ide.app), or copy it."),
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
    snd!("approval", "Portal"), vol!("approval"), pic!("approval"),
    snd!("attention", "Call"), vol!("attention"), pic!("attention"),
    snd!("failed", "Uh-oh"), vol!("failed"), pic!("failed"),
    snd!("turn_done", "Strum"), vol!("turn_done"), pic!("turn_done"),
    snd!("agent", "Hm"), vol!("agent"), pic!("agent"),
    snd!("from_trigger", "Hm"), vol!("from_trigger"), pic!("from_trigger"),
    snd!("requests", "none"), vol!("requests"), pic!("requests"),
    snd!("background", "none"), vol!("background"), pic!("background"),
    snd!("pr_checks", "none"), vol!("pr_checks"), pic!("pr_checks"),
    snd!("exited", "none"), vol!("exited"), pic!("exited"),
    snd!("triggers", "none"), vol!("triggers"), pic!("triggers"),
    snd!("restarted", "none"), vol!("restarted"), pic!("restarted"),
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
    s!("notify.when_focused", SettingKind::Bool, B(false), "notifications", false,
        "Also notify about the terminal you're looking at while midna is the frontmost app."),
    s!("notify.when_app_closed", SettingKind::Bool, B(true), "notifications", false,
        "When the midna app isn't running, midnad posts the notification itself (shown as a system notification; clicking it doesn't open midna)."),
    s!("keys.command_bar", KB, S("cmd-k"), "keys", false, "Open the command bar."),
    s!("keys.next_needs_you", KB, S("cmd-j"), "keys", false, "Jump to the next needs-you item."),
    s!("keys.new_terminal", KB, S("cmd-t"), "keys", false, "New terminal in the current project."),
    s!("keys.new_agent", KB, S("cmd-shift-t"), "keys", false, "New agent terminal in the current project."),
    s!("keys.new_terminal_root", KB, S("cmd-alt-t"), "keys", false, "New terminal at root (no project, starts in $HOME)."),
    s!("keys.new_agent_root", KB, S("cmd-alt-shift-t"), "keys", false, "New agent terminal at root (no project, starts in $HOME)."),
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
    s!("keys.prev_prompt", KB, S("cmd-alt-up"), "keys", false, "In an agent terminal, scroll to the previous prompt you sent."),
    s!("keys.next_prompt", KB, S("cmd-alt-down"), "keys", false, "In an agent terminal, scroll to the next prompt you sent (past the last: back to live)."),
    s!("keys.prompts", KB, S("cmd-p"), "keys", false, "List the prompts you sent the selected agent terminal, to search and jump to one."),
    s!("keys.queue", KB, S("cmd-u"), "keys", false,
        "Open the selected terminal's queued messages: what will be typed into it next, once the agent is ready."),
    s!("keys.links", KB, S("cmd-l"), "keys", false,
        "Open the selected agent terminal's links: the URLs, PRs, artifacts and files that came up in its conversation."),
    s!("keys.subagents", KB, S("cmd-alt-a"), "keys", false,
        "Open the selected agent terminal's subagents: what each is doing, and a read-only window that follows one live."),
    s!("keys.needs_you", KB, S("cmd-shift-j"), "keys", false, "Open the needs-you cards (approvals and questions, one at a time)."),
    s!("keys.approve_options", KB, S("cmd-shift-enter"), "keys", false, "Show the approve options (15 min, 1 h, session, always) for the focused approval."),
    s!("keys.next_terminal", KB, S(""), "keys", false, "Select the next terminal in the sidebar. Unbound by default (ctrl-tab would be taken from terminal apps)."),
    s!("keys.prev_terminal", KB, S(""), "keys", false, "Select the previous terminal in the sidebar. Unbound by default."),
    s!("keys.project_menu", KB, S("cmd-alt-p"), "keys", false, "Open the current project's … menu in the sidebar."),
    s!("keys.fold_project", KB, S("cmd-alt-f"), "keys", false, "Fold or unfold the current project's terminals in the sidebar."),
    s!("keys.sidebar", KB, S("cmd-b"), "keys", false, "Collapse the sidebar to a rail of status dots, or expand it again."),
    s!("keys.rename", KB, S("cmd-shift-e"), "keys", false, "Rename the selected terminal."),
    s!("keys.restart", KB, S("cmd-alt-r"), "keys", false, "Restart the selected terminal."),
    s!("keys.terminal_menu", KB, S("cmd-alt-m"), "keys", false, "Open the selected terminal's … menu."),
    s!("keys.copy_session_id", KB, S("cmd-alt-c"), "keys", false, "Copy the selected terminal's session id."),
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
    fn rule_lists_normalize_and_reject_half_rules() {
        let s = setting("ide.rules").unwrap();
        assert_eq!(s.coerce(&json!("~/Development/app/=cursor, pubspec.yaml = android-studio,~/Development/app = cursor")), Ok(json!(["~/Development/app = cursor", "pubspec.yaml = android-studio"])));
        assert_eq!(s.coerce(&json!(["*.xcodeproj=xcode"])), Ok(json!(["*.xcodeproj = xcode"])));
        assert_eq!(s.coerce(&json!("")), Ok(json!([])));
        assert!(s.coerce(&json!("pubspec.yaml")).is_err());
        assert!(s.coerce(&json!("= xcode")).is_err());
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
        assert_eq!(setting("ui.header.buttons").unwrap().default.to_json(), json!(HEADER_BUTTONS));
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
