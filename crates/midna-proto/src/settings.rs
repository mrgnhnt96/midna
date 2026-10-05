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
    /// Enum options; `allow_other` accepts any string too (e.g. a custom script path).
    Enum { options: &'static [&'static str], allow_other: bool },
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
            SettingKind::Enum { options, .. } => SettingType::Enum(options.iter().map(|s| s.to_string()).collect()),
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
const SOUNDS: SettingKind = en_path(&["none", "Basso", "Blow", "Bottle", "Frog", "Funk", "Glass", "Hero", "Morse", "Ping", "Pop", "Purr", "Sosumi", "Submarine", "Tink"]);
macro_rules! snd {
    ($cat:literal, $def:literal) => {
        s!(concat!("notify.sound.", $cat), SOUNDS, S($def), "notifications", false,
            concat!("Sound for “notify.", $cat, "”: none, a macOS sound (Glass, Ping, …) or a sound imported with `midna notify import <file>` (by its file name)."))
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

/// Where the app looks for updates by default: the manifests the release workflow
/// (.github/workflows/release.yml) keeps on the `channels` GitHub release.
pub const DEFAULT_FEED_URL: &str = "https://github.com/mrgnhnt96/midna/releases/download/channels/{channel}.json";

pub static SETTINGS: &[SettingSpec] = &[
    s!("theme", en(&["dark", "light", "system"]), S("system"), "appearance", false, "Color theme of the midna UI."),
    s!("density", en(&["comfortable", "compact"]), S("comfortable"), "appearance", false, "Spacing density of sidebar rows and headers."),
    s!("ui.header.script", en_path(&["github", "github+agent", "none"]), S("github"), "appearance", false,
        "Script that renders the terminal header line: a built-in name or an absolute path to an executable printing JSON segments."),
    s!("ui.row.script", en_path(&["none", "git-diff-stats"]), S("git-diff-stats"), "appearance", false,
        "Script that renders the second line of each sidebar terminal row: a built-in name or an executable path."),
    s!("updates.channel", en(&["stable", "beta"]), S("stable"), "general", false, "Which update channel midna follows."),
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
    s!("approve.from_cli", SettingKind::Bool, B(false), "agents", true,
        "Allow agents to approve approval requests of their own session from the CLI. Human only."),
    s!("agents.claude.statusline", SettingKind::Bool, B(true), "agents", false,
        "Register midna's status line in Claude sessions midna launches; it reports real cost to insights (prints model and cost)."),
    s!("agents.mcp", SettingKind::Bool, B(true), "agents", false,
        "Give agents midna launches the `midna` MCP server (Claude: --mcp-config MIDNA_HOME/hooks/mcp.json; Codex: -c mcp_servers.midna.*), so they can drive midna as tools. No global config is touched. Applies to agents launched afterwards."),
    s!("agents.system_hint", SettingKind::Bool, B(true), "agents", false,
        "Tell agents midna launches, in two lines of system prompt (Claude: --append-system-prompt; Codex: -c developer_instructions), that they run in midna and can run `midna capabilities`. Applies to agents launched afterwards."),
    s!("agents.restart_on_update", en(&["when_idle", "ask", "off"]), S("when_idle"), "agents", false,
        "When a newer version of an agent is installed than the one a terminal runs (Claude), restart that terminal into the same conversation. when_idle: queue the restart and run it once the agent is idle with no background work, subagents or scheduled wakeups in flight and an empty input box. ask: raise a needs-you item. off: do nothing."),
    s!("agents.restart_idle_secs", SettingKind::Int, I(60), "agents", false,
        "How long an agent terminal must be quiet (no output, no turn) before a queued restart runs."),
    s!("policy.default", en(&["auto", "allow", "ask", "deny"]), S("auto"), "policy", true,
        "Decision when no rule matches. auto = built-in defaults table (ask for destructive CLI verbs, allow otherwise). Human only."),
    s!("policy.request_timeout_secs", SettingKind::Int, I(300), "policy", false,
        "How long policy.request blocks waiting for a human to answer an approval before giving up."),
    s!("ui.ask.agent", en(&["claude", "codex"]), S("claude"), "general", false,
        "Agent the command bar's \"Ask an agent\" starts (tab toggles it there, and the choice is saved here)."),
    s!("ui.ask.scope", en(&["project", "root"]), S("project"), "general", false,
        "Where the command bar's \"Ask an agent\" starts the agent: the current project or the root (shift-tab toggles it)."),
    s!("terminal.option_as_meta", SettingKind::Bool, B(true), "terminal", false,
        "Option (alt) acts as Meta in terminals: option-b sends ESC b (word back in shells). Off = option types macOS characters (option-e e = é)."),
    s!("projects.roots", SettingKind::PathList, L(&[]), "general", false,
        "Folders your projects live in (e.g. ~/Development). Their subfolders show up in the command bar and on the empty screen, ready to open as projects; a subfolder that isn't a git repo but holds some is looked into one level deeper. Nothing is added to the sidebar until you open one. Comma-separated on the CLI."),
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
    snd!("approval", "Glass"), vol!("approval"), pic!("approval"),
    snd!("attention", "Glass"), vol!("attention"), pic!("attention"),
    snd!("failed", "Basso"), vol!("failed"), pic!("failed"),
    snd!("turn_done", "none"), vol!("turn_done"), pic!("turn_done"),
    snd!("agent", "Ping"), vol!("agent"), pic!("agent"),
    snd!("requests", "none"), vol!("requests"), pic!("requests"),
    snd!("background", "none"), vol!("background"), pic!("background"),
    snd!("pr_checks", "none"), vol!("pr_checks"), pic!("pr_checks"),
    snd!("exited", "none"), vol!("exited"), pic!("exited"),
    snd!("triggers", "none"), vol!("triggers"), pic!("triggers"),
    snd!("restarted", "none"), vol!("restarted"), pic!("restarted"),
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
    s!("keys.composer", KB, S("cmd-shift-d"), "keys", false,
        "Open the composer under the terminal to type or paste a long prompt; enter sends it to the terminal, esc cancels."),
    s!("keys.add_image", KB, S("cmd-i"), "keys", false,
        "Add images with numbered notes to the selected terminal's next message (pasted in when you press enter there)."),
    s!("keys.prev_prompt", KB, S("cmd-alt-up"), "keys", false, "In an agent terminal, scroll to the previous prompt you sent."),
    s!("keys.next_prompt", KB, S("cmd-alt-down"), "keys", false, "In an agent terminal, scroll to the next prompt you sent (past the last: back to live)."),
    s!("keys.prompts", KB, S("cmd-p"), "keys", false, "List the prompts you sent the selected agent terminal, to search and jump to one."),
    s!("keys.links", KB, S("cmd-l"), "keys", false,
        "Open the selected agent terminal's links: the URLs, PRs, artifacts and files that came up in its conversation."),
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
        let SettingKind::Enum { options, .. } = setting("notify.sound.approval").unwrap().ty else { panic!() };
        assert_eq!(&options[1..], crate::notify::SYSTEM_SOUNDS);
    }

    #[test]
    fn volumes_stay_in_range() {
        let v = setting("notify.volume.approval").unwrap();
        assert_eq!(v.coerce(&json!("60%")), Ok(json!(60)));
        assert!(v.coerce(&json!(101)).is_err());
        assert!(v.coerce(&json!(-1)).is_err());
    }
}
