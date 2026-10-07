//! Notifications: which daemon signals become a macOS notification.
//!
//! midnad decides (`midnad/src/notify.rs`): it watches its own event log, checks the category's
//! global setting (`notify.<category>`) and the terminal's override (`Session::notify`), and logs
//! a `notify.posted` event. The app posts that as a native notification (clicking it selects the
//! terminal); with no app running midnad posts it itself (`notify.when_app_closed`).
//!
//! Each category also has a sound, a volume and an image (`notify.sound.<key>`,
//! `notify.volume.<key>`, `notify.image.<key>`). Sounds and images you bring are imported into
//! `MIDNA_HOME/notify/{sounds,images}` (`notify.import`) and named by their file name. Its title
//! and text are templates (`notify.title.<key>`, `notify.body.<key>`; empty = midna's own, see
//! `NotifyCategory::vars`). The text may be left out; the title never is.
//!
//! Add a category here, add its `notify.<key>`, sound, volume, image, title and body rows to the
//! settings catalog, and map a signal to it in midnad.
//!
//! Sound effects (`EFFECTS`) share that sound library and the `notify.sound.<key>` /
//! `notify.volume.<key>` settings, but have no banner: the app plays them itself when you do
//! something (approve, a queued message goes in, switch terminals, …). A category's sound also
//! plays in the app when its banner is skipped because you're looking at that terminal.
//! `notify.sounds` turns every sound off; `notify.sounds_in_app` keeps only the ones that come
//! with a banner. Agents play a sound on purpose with `notify.play`.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub struct NotifyCategory {
    pub key: &'static str,
    /// Short name for the Settings window and menus.
    pub label: &'static str,
    /// On out of the box: midna records it (and the app can show it). Only what a human must
    /// act on, or would hate to miss, is on.
    pub default: bool,
    /// A macOS banner out of the box for a terminal you aren't looking at (`notify.push.<key>`).
    /// Only what needs you is pushed; a banner for the terminal in front of you
    /// (`notify.push_focused.<key>`) is off for every kind.
    pub push: bool,
    /// Its sound out of the box (`notify.sound.<key>`): a macOS sound name, or `none`.
    pub sound: &'static str,
    pub description: &'static str,
    /// The variables its `notify.title.<key>` / `notify.body.<key>` templates get besides the ones every category
    /// gets (`BODY_VARS`) and the event's own data.
    pub vars: &'static [&'static str],
}

macro_rules! c {
    ($key:literal, $label:literal, $default:literal, $sound:literal, $desc:literal, $vars:expr) => {
        NotifyCategory { key: $key, label: $label, default: $default, push: false, sound: $sound, description: $desc, vars: $vars }
    };
    (push $key:literal, $label:literal, $default:literal, $sound:literal, $desc:literal, $vars:expr) => {
        NotifyCategory { push: true, ..c!($key, $label, $default, $sound, $desc, $vars) }
    };
}

/// What every `notify.title.<key>` / `notify.body.<key>` template can use: midna's own title
/// and text, the project's name, the category, and the terminal (as in triggers).
pub static BODY_VARS: &[&str] = &["heading", "text", "project", "category", "event", "session.name", "session.id", "session.agent", "session.status"];

/// A `notify.body.<key>` value that shows no text, only the title.
pub const NO_BODY: &str = "none";

const NEEDS_YOU: &[&str] = &["title", "detail", "kind"];

pub static CATEGORIES: &[NotifyCategory] = &[
    c!(push "approval", "Approvals and questions", true, "Portal",
        "An agent waits on you: an approval request, a permission prompt or a question it asked in its terminal.", &["title", "detail", "kind", "action"]),
    c!(push "attention", "Agent asks for you", true, "Call", "An agent raised a needs-you note or said it's blocked (`midna attention`).", NEEDS_YOU),
    c!("failed", "Failures", true, "Uh-oh", "A terminal's command failed (non-zero exit, killed) or an agent's turn ended in an error.", &["title", "detail", "kind", "reason"]),
    c!("turn_done", "Agent finished", true, "Strum",
        "An agent finished a turn that took at least notify.turn_done_min_secs, with the start of its reply.", &["elapsed", "secs", "reply", "message"]),
    c!("agent", "Sent by an agent", true, "Hm", "An agent sent you a notification on purpose (`midna notify send`).", &["title", "body"]),
    c!("from_trigger", "Sent by a trigger", true, "Hm", "A trigger you set up sent a notification (its `notify` action).", &["title", "body"]),
    c!("requests", "Other requests", false, "none",
        "Needs-you items that can wait: a trigger waiting to be enabled, a webhook secret to set, a rule an agent wants removed.", NEEDS_YOU),
    c!("background", "Background task finished", false, "none", "A background shell an agent started (Claude's run_in_background) finished.", &["count"]),
    c!("pr_checks", "PR checks", false, "none", "The checks on a terminal's pull request finished: all passing, or some failing.", &["number", "checks", "failing"]),
    c!("exited", "Terminal exited", false, "none", "A terminal's process exited cleanly (a shell's `exit`, a monitor or command that finished).", &[]),
    c!("triggers", "Trigger fired", false, "none", "A webhook trigger fired and started an agent or a command.", &["name", "outcome"]),
    c!("restarted", "Agent restarted", false, "none", "midna restarted an agent into the same conversation (an agent update was installed).", &["reason"]),
];

/// A sound with no notification: something you did, or a small UI cue.
pub struct SoundEffect {
    pub key: &'static str,
    pub label: &'static str,
    /// `action` (something you did) or `ui` (small cues while you move around).
    pub group: &'static str,
    /// Its sound and volume out of the box (`notify.sound.<key>`, `notify.volume.<key>`).
    pub sound: &'static str,
    pub volume: i64,
    pub description: &'static str,
}

macro_rules! fx {
    ($key:literal, $label:literal, $group:literal, $sound:literal, $vol:literal, $desc:literal) => {
        SoundEffect { key: $key, label: $label, group: $group, sound: $sound, volume: $vol, description: $desc }
    };
}

pub static EFFECTS: &[SoundEffect] = &[
    fx!("approved", "You approve", "action", "Rise", 100, "You approve something: an approval or permission prompt, \"I've done it\", a trigger you start."),
    fx!("denied", "You deny", "action", "Nn-nn", 100, "You deny an approval or permission prompt, or keep a rule an agent wanted removed."),
    fx!("queue_sent", "Queued message sent", "action", "Whoosh", 100, "A message you queued went into its terminal (the pill's \"Sent\")."),
    fx!("image_added", "Image added to chat", "action", "Fwip", 100, "You paste, drop or pick an image into the image sheet."),
    fx!("closed", "Terminal closed", "action", "Close", 100, "You closed a terminal."),
    fx!("switched", "Switch terminal", "ui", "Tick", 100, "You select another terminal."),
    fx!("command_bar", "Open ⌘K", "ui", "Thump", 100, "The command bar opens."),
    fx!("copied", "Copy", "ui", "Tick-tick", 100, "midna copies something for you (a selection, a link, a session id)."),
];

pub fn effect(key: &str) -> Option<&'static SoundEffect> {
    EFFECTS.iter().find(|e| e.key == key)
}

/// A key with a sound setting: a notification category or a sound effect.
pub fn has_sound(key: &str) -> bool {
    category(key).is_some() || effect(key).is_some()
}

pub fn category(key: &str) -> Option<&'static NotifyCategory> {
    CATEGORIES.iter().find(|c| c.key == key)
}

/// The global setting behind a category (or `enabled`, the master switch).
pub fn setting_key(key: &str) -> String {
    format!("notify.{key}")
}

/// Whether a category shows as a macOS banner for a terminal you aren't looking at
/// (`notify.push.<key>`). Off: it's still recorded, just not pushed.
pub fn push_key(key: &str) -> String {
    format!("notify.push.{key}")
}

/// Whether a category shows as a macOS banner for the terminal you're looking at
/// (`notify.push_focused.<key>`). Off: only its sound plays, and only if it would push.
pub fn push_focused_key(key: &str) -> String {
    format!("notify.push_focused.{key}")
}

/// A category's sound (`notify.sound.<key>`): `none`, a macOS sound, or an imported file name.
pub fn sound_key(key: &str) -> String {
    format!("notify.sound.{key}")
}

/// A category's volume, 0–100 (`notify.volume.<key>`), scaled by the master `notify.volume`.
pub fn volume_key(key: &str) -> String {
    format!("notify.volume.{key}")
}

/// A category's image (`notify.image.<key>`): empty = the `notify.image` every notification
/// uses, `none`, or an imported image's file name.
pub fn image_key(key: &str) -> String {
    format!("notify.image.{key}")
}

/// A category's text (`notify.body.<key>`): a template like a trigger's (`{{reply}}`,
/// `{{session.name}}`, `{{data.<path>}}`), empty = midna's own text, `none` = no text.
pub fn body_key(key: &str) -> String {
    format!("notify.body.{key}")
}

/// A category's title (`notify.title.<key>`): a template like the text's, empty = midna's own
/// (`<terminal> · <project>`). A title that renders empty is midna's own too.
pub fn title_key(key: &str) -> String {
    format!("notify.title.{key}")
}

/// How long a category's notification stays on screen in the floating badge and the in-app
/// card (`notify.stay.<key>`), in seconds: 0 = it stays until it's handled or dismissed.
pub fn stay_key(key: &str) -> String {
    format!("notify.stay.{key}")
}

/// A category's color (`notify.color.<key>`): a theme color (`COLOR_TOKENS`) or `#rrggbb`.
pub fn color_key(key: &str) -> String {
    format!("notify.color.{key}")
}

/// Whether a category counts toward the unread number on the status bar's bell
/// (`notify.bell.<key>`). Off: it's still in the notifications screen, just not counted.
pub fn bell_key(key: &str) -> String {
    format!("notify.bell.{key}")
}

/// Theme colors a `notify.color.<key>` may name; they follow the theme.
pub static COLOR_TOKENS: &[&str] = &["need", "ok", "err", "work", "accent", "dim"];

/// A `notify.color.<key>` value: a theme color or `#rrggbb`.
pub fn valid_color(v: &str) -> bool {
    COLOR_TOKENS.contains(&v) || (v.len() == 7 && v.starts_with('#') && v[1..].chars().all(|c| c.is_ascii_hexdigit()))
}

/// A notification kind you added (`notify.kinds.add`): agents send to it with
/// `midna notify send --kind <key>`, triggers with their `notify` action's `kind`. Its
/// switches, sound, text, duration, color and bell are settings like a built-in kind's
/// (`notify.<key>`, `notify.sound.<key>`, …; see `settings::custom_kind_spec`), stored with the
/// rest and dropped when the kind is removed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CustomKind {
    /// Lowercase letters, digits and `_`; not a built-in kind or sound effect.
    pub key: String,
    /// Short name for Settings and the notifications screen.
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// The per-kind settings every kind has, by field: `""` is the kind's own switch
/// (`notify.<key>`), the rest are `notify.<field>.<key>`.
pub static KIND_FIELDS: &[&str] = &["", "push", "push_focused", "sound", "volume", "image", "title", "body", "stay", "color", "bell"];

/// A kind's setting for one of `KIND_FIELDS`: `notify.<key>` for `""`, else
/// `notify.<field>.<key>`.
pub fn kind_key(field: &str, kind: &str) -> String {
    if field.is_empty() { setting_key(kind) } else { format!("notify.{field}.{kind}") }
}

/// Every per-kind setting key of a kind.
pub fn kind_keys(kind: &str) -> impl Iterator<Item = String> + '_ {
    KIND_FIELDS.iter().map(move |f| kind_key(f, kind))
}

/// Split a per-kind setting key into (field, kind): `notify.stay.deploys` -> ("stay",
/// "deploys"), `notify.deploys` -> ("", "deploys"). Says nothing about whether the kind exists.
pub fn split_kind_key(key: &str) -> Option<(&'static str, &str)> {
    let rest = key.strip_prefix("notify.")?;
    for f in KIND_FIELDS.iter().filter(|f| !f.is_empty()) {
        if let Some(kind) = rest.strip_prefix(f).and_then(|r| r.strip_prefix('.')) {
            return valid_kind_key(kind).then_some((f, kind));
        }
    }
    valid_kind_key(rest).then_some(("", rest))
}

/// A kind's key: 1–32 lowercase letters, digits and `_`, starting with a letter.
pub fn valid_kind_key(k: &str) -> bool {
    (1..=32).contains(&k.len()) && k.starts_with(|c: char| c.is_ascii_lowercase()) && k.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// A key a custom kind can't take: a built-in kind, a sound effect, a per-kind field name or
/// a global `notify.<key>` setting (`notify.enabled`, `notify.volume`, …).
pub fn reserved_kind_key(k: &str) -> bool {
    category(k).is_some() || effect(k).is_some() || KIND_FIELDS.contains(&k) || k == "kinds" || crate::settings::setting(&setting_key(k)).is_some()
}

/// The sounds in /System/Library/Sounds, by name (no extension). A sound setting's value is
/// one of these, `none`, or an imported sound's file name (which has an extension).
pub static SYSTEM_SOUNDS: &[&str] = &["Basso", "Blow", "Bottle", "Frog", "Funk", "Glass", "Hero", "Morse", "Ping", "Pop", "Purr", "Sosumi", "Submarine", "Tink"];

/// midna's own sounds (the "Twilight" set, synthesized for midna; source in tools/twilight).
/// midnad ships them inside its binary and writes them to `MIDNA_HOME/notify/twilight/<name>.wav`
/// at startup. Every kind's default sound is one of these. Names have no extension, like the
/// macOS ones, so they never clash with an imported file's name.
pub static TWILIGHT: &[&str] = &["Portal", "Call", "Uh-oh", "Strum", "Hm", "Rise", "Nn-nn", "Whoosh", "Fwip", "Close", "Tick", "Thump", "Tick-tick"];

/// Where a Twilight sound lives under `home` (MIDNA_HOME), whether or not it's there yet.
pub fn twilight_path(home: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    TWILIGHT.contains(&name).then(|| home.join("notify/twilight").join(format!("{name}.wav")))
}

/// A sound that ships with midna or macOS (can't be removed): its name is a setting value as is.
pub fn is_builtin_sound(name: &str) -> bool {
    SYSTEM_SOUNDS.contains(&name) || TWILIGHT.contains(&name)
}

/// A built-in sound's proper name for any spelling (`glass` -> Glass, `uh-oh` -> Uh-oh).
pub fn builtin_sound_named(name: &str) -> Option<&'static str> {
    SYSTEM_SOUNDS.iter().chain(TWILIGHT).find(|s| s.eq_ignore_ascii_case(name)).copied()
}

/// What `notify.import` takes. Sounds play through NSSound / afplay; images must be formats
/// macOS shows as a notification attachment.
pub static SOUND_EXTS: &[&str] = &["aiff", "aif", "wav", "mp3", "m4a", "caf"];
pub static IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif"];

/// Where a built-in sound's file is.
pub fn system_sound_path(name: &str) -> Option<String> {
    SYSTEM_SOUNDS.contains(&name).then(|| format!("/System/Library/Sounds/{name}.aiff"))
}

/// The file a sound setting's value plays: a macOS sound, a Twilight sound, or a sound
/// imported into `home` (`MIDNA_HOME`). None: `none`, or a file that isn't there.
pub fn sound_file(home: &std::path::Path, value: &str) -> Option<std::path::PathBuf> {
    if let Some(p) = system_sound_path(value) {
        return Some(p.into());
    }
    if let Some(p) = twilight_path(home, value) {
        return p.is_file().then_some(p);
    }
    let ext = std::path::Path::new(value).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let plain = !value.is_empty() && !value.starts_with('.') && !value.contains(['/', '\\', '\0']);
    (plain && SOUND_EXTS.contains(&ext.as_str())).then(|| home.join("notify/sounds").join(value)).filter(|p| p.is_file())
}

/// The file an image setting's value shows: an image imported into `home` (`MIDNA_HOME`).
/// None: empty, `none`, or a file that isn't there.
pub fn image_file(home: &std::path::Path, value: &str) -> Option<std::path::PathBuf> {
    let ext = std::path::Path::new(value).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let plain = !value.is_empty() && !value.starts_with('.') && !value.contains(['/', '\\', '\0']);
    (plain && IMAGE_EXTS.contains(&ext.as_str())).then(|| home.join("notify/images").join(value)).filter(|p| p.is_file())
}

/// The image a kind's notifications carry: `notify.image.<kind>`, or `notify.image` when that's
/// empty (`setting` reads a setting's string value).
pub fn kind_image(home: &std::path::Path, kind: &str, setting: impl Fn(&str) -> String) -> Option<std::path::PathBuf> {
    match setting(&image_key(kind)).as_str() {
        "" => image_file(home, &setting("notify.image")),
        v => image_file(home, v),
    }
}

/// `notify.sound` event data (`notify.play`): the app plays it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct Played {
    /// The sound's name (what a setting takes) and its file.
    pub sound: String,
    pub file: String,
    /// 1–100, already scaled by `notify.volume`.
    pub volume: u8,
    /// The kind whose sound it is, when it was asked for by kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Who plays it: `app`, `system` (midnad with `afplay`, no app running) or `none`.
    pub via: String,
}

/// A terminal's `notify` map may hold `enabled` (false = muted) and any category key.
pub fn is_override_key(key: &str) -> bool {
    key == "enabled" || category(key).is_some()
}

/// `notify.posted` event data (and the app's input for a native notification).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct Posted {
    pub category: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
    /// Play a sound. Apps from before custom sounds play the default sound for it.
    #[serde(default)]
    pub sound: bool,
    /// The sound file to play (absolute path) and how loud, 1–100. Set whenever `sound` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sound_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<u8>,
    /// The file name of `sound_file` rendered at `volume`, a WAV in `~/Library/Sounds`
    /// (`Midna Strum.wav`): the app names it in the notification so macOS plays it with the
    /// banner, under Focus and its sound settings. Older daemons sent a full path, which
    /// macOS ignores (it plays its default sound). None when rendering failed; the app then plays
    /// `sound_file` itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification_sound: Option<String>,
    /// An image to attach (absolute path to an imported image).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// Show a banner for a terminal you aren't looking at (`notify.push.<category>`). Daemons
    /// from before it pushed everything they posted.
    #[serde(default = "yes")]
    pub push: bool,
    /// Show a banner for the terminal you're looking at too (`notify.push_focused.<category>`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub push_focused: bool,
    /// A test notification (`notify.test`): shown even for the terminal you're looking at.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub test: bool,
    /// Who shows it: `app` (the GUI was connected) or `system` (midnad, no app running).
    pub via: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs_you_id: Option<String>,
    /// Seconds it stays on screen (`notify.stay.<category>`): 0 = until it's handled or
    /// dismissed. None from daemons before it (the app's own default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stay_secs: Option<u32>,
    /// Its color (`notify.color.<category>`): a theme color name or `#rrggbb`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// A kind you added: its label (built-in kinds are named by the app).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

fn yes() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_image_falls_back_to_every_notifications_image() {
        let home = std::env::temp_dir().join(format!("midna-kind-image-{}", std::process::id()));
        std::fs::create_dir_all(home.join("notify/images")).unwrap();
        for f in ["all.png", "approval.jpg"] {
            std::fs::write(home.join("notify/images").join(f), b"x").unwrap();
        }
        let settings = |pairs: &'static [(&'static str, &'static str)]| move |k: &str| pairs.iter().find(|(n, _)| *n == k).map_or(String::new(), |(_, v)| v.to_string());
        let set = settings(&[("notify.image", "all.png"), ("notify.image.approval", "approval.jpg"), ("notify.image.failed", "none")]);
        assert_eq!(kind_image(&home, "approval", &set), Some(home.join("notify/images/approval.jpg")));
        assert_eq!(kind_image(&home, "done", &set), Some(home.join("notify/images/all.png")));
        assert_eq!(kind_image(&home, "failed", &set), None);
        assert_eq!(kind_image(&home, "done", settings(&[])), None);
        assert_eq!(image_file(&home, "missing.png"), None);
        assert_eq!(image_file(&home, "../notify/images/all.png"), None);
        std::fs::remove_dir_all(&home).unwrap();
    }
}
