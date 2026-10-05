//! Notifications: which daemon signals become a macOS notification.
//!
//! midnad decides (`midnad/src/notify.rs`): it watches its own event log, checks the category's
//! global setting (`notify.<category>`) and the terminal's override (`Session::notify`), and logs
//! a `notify.posted` event. The app posts that as a native notification (clicking it selects the
//! terminal); with no app running midnad posts it itself (`notify.when_app_closed`).
//!
//! Each category also has a sound, a volume and an image (`notify.sound.<key>`,
//! `notify.volume.<key>`, `notify.image.<key>`). Sounds and images you bring are imported into
//! `MIDNA_HOME/notify/{sounds,images}` (`notify.import`) and named by their file name.
//!
//! Add a category here, add its `notify.<key>`, sound, volume and image rows to the settings
//! catalog, and map a signal to it in midnad.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub struct NotifyCategory {
    pub key: &'static str,
    /// Short name for the Settings window and menus.
    pub label: &'static str,
    /// On out of the box. Only what a human must act on, or would hate to miss, is on.
    pub default: bool,
    /// Its sound out of the box (`notify.sound.<key>`): a macOS sound name, or `none`.
    pub sound: &'static str,
    pub description: &'static str,
}

macro_rules! c {
    ($key:literal, $label:literal, $default:literal, $sound:literal, $desc:literal) => {
        NotifyCategory { key: $key, label: $label, default: $default, sound: $sound, description: $desc }
    };
}

pub static CATEGORIES: &[NotifyCategory] = &[
    c!("approval", "Approvals and questions", true, "Glass",
        "An agent waits on you: an approval request, a permission prompt or a question it asked in its terminal."),
    c!("attention", "Agent asks for you", true, "Glass", "An agent raised a needs-you note or said it's blocked (`midna attention`)."),
    c!("failed", "Failures", true, "Basso", "A terminal's command failed (non-zero exit, killed) or an agent's turn ended in an error."),
    c!("turn_done", "Agent finished", true, "none",
        "An agent finished a turn that took at least notify.turn_done_min_secs, with the start of its reply."),
    c!("agent", "Sent by an agent", true, "Ping", "An agent sent you a notification on purpose (`midna notify send`)."),
    c!("requests", "Other requests", false, "none",
        "Needs-you items that can wait: a trigger waiting to be enabled, a webhook secret to set, a rule an agent wants removed."),
    c!("background", "Background task finished", false, "none", "A background shell an agent started (Claude's run_in_background) finished."),
    c!("pr_checks", "PR checks", false, "none", "The checks on a terminal's pull request finished: all passing, or some failing."),
    c!("exited", "Terminal exited", false, "none", "A terminal's process exited cleanly (a shell's `exit`, a monitor or command that finished)."),
    c!("triggers", "Trigger fired", false, "none", "A webhook trigger fired and started an agent or a command."),
    c!("restarted", "Agent restarted", false, "none", "midna restarted an agent into the same conversation (an agent update was installed)."),
];

pub fn category(key: &str) -> Option<&'static NotifyCategory> {
    CATEGORIES.iter().find(|c| c.key == key)
}

/// The global setting behind a category (or `enabled`, the master switch).
pub fn setting_key(key: &str) -> String {
    format!("notify.{key}")
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

/// The sounds in /System/Library/Sounds, by name (no extension). A sound setting's value is
/// one of these, `none`, or an imported sound's file name (which has an extension).
pub static SYSTEM_SOUNDS: &[&str] = &["Basso", "Blow", "Bottle", "Frog", "Funk", "Glass", "Hero", "Morse", "Ping", "Pop", "Purr", "Sosumi", "Submarine", "Tink"];

/// What `notify.import` takes. Sounds play through NSSound / afplay; images must be formats
/// macOS shows as a notification attachment.
pub static SOUND_EXTS: &[&str] = &["aiff", "aif", "wav", "mp3", "m4a", "caf"];
pub static IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif"];

/// Where a built-in sound's file is.
pub fn system_sound_path(name: &str) -> Option<String> {
    SYSTEM_SOUNDS.contains(&name).then(|| format!("/System/Library/Sounds/{name}.aiff"))
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
    /// `sound_file` rendered at `volume` (a WAV in `MIDNA_HOME/notify/cache`): the app names it
    /// in the notification so macOS plays it with the banner, under Focus and its sound
    /// settings. None when rendering failed; the app then plays `sound_file` itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification_sound: Option<String>,
    /// An image to attach (absolute path to an imported image).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// A test notification (`notify.test`): shown even for the terminal you're looking at.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub test: bool,
    /// Who shows it: `app` (the GUI was connected) or `system` (midnad, no app running).
    pub via: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs_you_id: Option<String>,
}
