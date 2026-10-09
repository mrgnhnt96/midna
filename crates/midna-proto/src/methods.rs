//! Params/result types for every RPC method. The catalog (`catalog.rs`) ties them to names.
//! Every params struct tolerates an extra `caller` field (see [`Caller`]); the daemon strips it.
use crate::types::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Optional identity hint attached to params by clients. `session` comes from `MIDNA_SESSION`;
/// `role: "agent"` lets any client voluntarily downgrade itself (it can never upgrade to human).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Caller {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Don't block on the human: if the call needs an approval, answer at once with error 6
    /// (`pending`, data.needs_you_id) while the call carries on and finishes when the human
    /// answers. Poll it with needs_you.get.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_wait: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct NoParams {}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct OkResult {
    pub ok: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct IdParams {
    pub id: Id,
}

// ------------------------------------------------------------------ subagent log

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SubagentLogParams {
    /// The terminal the subagent belongs to.
    pub id: Id,
    /// The subagent's `agent_id` (from `agent_info.subagents` / `finished_subagents`).
    pub agent: String,
    /// Byte offset to read from: 0 for the start, then the last result's `next`.
    #[serde(default)]
    pub from: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SubagentLog {
    /// What midna knows of it from the hooks (absent once it has scrolled out of `finished_subagents`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<Subagent>,
    pub running: bool,
    /// Entries from `from` on (complete lines only).
    pub entries: Vec<SubagentLogEntry>,
    /// Where the next read starts.
    pub next: u64,
    /// The model it runs on, when this chunk names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// One line of a subagent's transcript, as midna shows it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct SubagentLogEntry {
    /// `prompt` (what it was asked), `text` (what it said), `tool` (`Name(summary)`),
    /// `result` (first line of a tool's output) or `error` (a failed tool).
    pub kind: String,
    pub text: String,
}

// ------------------------------------------------------------------ links

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct LinksListParams {
    /// The terminal. Defaults to the caller's own terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Id>,
    /// Only links of this kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<LinkKind>,
    /// Only pinned links.
    #[serde(default)]
    pub pinned: bool,
    /// Only links that came up in this turn: a prompt number (`n` from `session.prompts`) or
    /// `"last"` for the latest prompt. E.g. `{kind: file, turn: "last"}` = the files the agent
    /// created or edited since the human's last prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<TurnRef>,
}

/// A prompt number, or `"last"` (also `"latest"`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum TurnRef {
    N(u32),
    Name(String),
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct LinksListResult {
    pub session: Id,
    /// The prompt number `turn` resolved to (absent when no turn was asked for, or when the
    /// terminal has no prompts yet).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u32>,
    /// Pinned first (most recently pinned first), then the rest, most recent first.
    pub links: Vec<Link>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct LinksPinParams {
    /// The terminal. Defaults to the caller's own terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Id>,
    /// A link id (`l_…`) or its exact target (URL or path).
    pub link: String,
    /// false unpins.
    #[serde(default = "yes")]
    pub pinned: bool,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct LinksRemoveParams {
    /// The terminal. Defaults to the caller's own terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Id>,
    /// A link id (`l_…`) or its exact target (URL or path).
    pub link: String,
    /// Put a removed link back as it was (undo).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub restore: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct LinksAddParams {
    /// The terminal. Defaults to the caller's own terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Id>,
    /// An http(s) URL or an absolute file path.
    pub target: String,
    /// Display name; derived from the target when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Why it matters (shown under the title).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Pin it too (default true: adding a link on purpose means it is important).
    #[serde(default = "yes")]
    pub pin: bool,
}

// ------------------------------------------------------------------ queued messages

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct QueueListParams {
    /// The terminal. Defaults to the caller's own terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Id>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct QueueListResult {
    pub session: Id,
    pub paused: bool,
    /// In the order they will be typed.
    pub items: Vec<QueuedMessage>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct QueueAddParams {
    /// The terminal. Defaults to the caller's own terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Id>,
    pub text: String,
    /// Press Enter after the text (default true).
    #[serde(default = "yes")]
    pub enter: bool,
    /// Absolute paths of images to attach before the text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
    /// When it may go in (default: as soon as the agent is ready).
    #[serde(default)]
    pub when: SendWhen,
    /// Where in the queue, 0 = first. Default: last.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<usize>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct QueueUpdateParams {
    /// The terminal. Defaults to the caller's own terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Id>,
    /// The message (`q_…`).
    pub id: Id,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enter: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<SendWhen>,
    /// A failed message goes back to waiting.
    #[serde(default)]
    pub retry: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct QueueItemParams {
    /// The terminal. Defaults to the caller's own terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Id>,
    /// The message (`q_…`).
    pub id: Id,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct QueueMoveParams {
    /// The terminal. Defaults to the caller's own terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Id>,
    /// The message (`q_…`).
    pub id: Id,
    /// New position, 0 = first.
    pub to: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct QueuePauseParams {
    /// The terminal. Defaults to the caller's own terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Id>,
    /// false resumes.
    #[serde(default = "yes")]
    pub paused: bool,
}

// ------------------------------------------------------------------ notifications

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct NotifyListParams {
    /// Also show this terminal's overrides and what applies to it. Defaults to the caller's own
    /// terminal; pass `global: true` for the global settings only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Id>,
    #[serde(default)]
    pub global: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NotifyCategoryInfo {
    pub key: String,
    pub label: String,
    pub description: String,
    pub default: bool,
    /// The global `notify.<key>` setting.
    pub global: bool,
    /// The terminal's override, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<bool>,
    /// What applies: the override, else the global setting (false whenever notifications are
    /// off globally or the terminal is muted).
    pub effective: bool,
    /// `notify.sound.<key>`: none, a macOS sound or an imported file's name.
    #[serde(default)]
    pub sound: String,
    /// `notify.volume.<key>`, 0–100 (before the master `notify.volume`).
    #[serde(default)]
    pub volume: i64,
    /// `notify.image.<key>`, or `notify.image` when that's empty (empty = none).
    #[serde(default)]
    pub image: String,
    /// `notify.stay.<key>`: seconds on screen, 0 = until handled or dismissed.
    #[serde(default)]
    pub stay: i64,
    /// `notify.color.<key>`: a theme color name or `#rrggbb`.
    #[serde(default)]
    pub color: String,
    /// A kind the human added (`notify.kinds.*`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub custom: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NotifyListResult {
    /// `notify.enabled`.
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Id>,
    /// The terminal is muted (`notify set enabled false`).
    #[serde(default)]
    pub muted: bool,
    pub categories: Vec<NotifyCategoryInfo>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NotifySetParams {
    /// The terminal to change. Defaults to the caller's own terminal; pass `global: true` to
    /// change the global setting (same as `settings.set notify.<key>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Id>,
    #[serde(default)]
    pub global: bool,
    /// A category key (see notify.list) or `enabled` (false mutes the terminal).
    pub key: String,
    /// true / false, or null to drop the terminal's override and follow the global setting.
    pub value: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NotifySendParams {
    pub title: String,
    #[serde(default)]
    pub body: String,
    /// The terminal it is about (clicking the notification selects it). Defaults to the
    /// caller's own terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Id>,
    /// Play the human's sound for agent notifications (`notify.sound.agent`, at its volume).
    #[serde(default)]
    pub sound: bool,
    /// A kind the human added (`notify.kinds.list`) to send it as, with that kind's sound,
    /// color and duration. Omitted: `agent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Your name for it (1–64 of `A-Z a-z 0-9 . _ : -`). Sending again with the same id
    /// replaces the one still showing instead of stacking another; `notify.withdraw` takes it
    /// away. Omitted: midna makes one up (returned as `id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// A URL to open when it's clicked (`https://…`, `file://…`, any `scheme:`), instead of
    /// selecting a terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open: Option<String>,
    /// Buttons to show (at most 4, each 1–40 characters). The human's pick comes back as
    /// `response` (with `wait_secs`), from `notify.response`, and as a `notify.responded` event.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<String>,
    /// What to do the moment the human responds, by response: a button's label, `clicked` or
    /// `dismissed`. Each is a trigger action (`run_command`, `attention`, `send_to_session`,
    /// `notify`, `start_agent`, `set_status`, `clear_status`) and runs once, like a local
    /// trigger firing on `notify.responded`: its run is a delivery (`trigger.deliveries`), and
    /// `notify.response` says what it did. A `project_id` may be a project's name, or empty for
    /// the terminal's project. Kept on disk until midnad forgets the notification, so a
    /// response after a restart still runs it.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub on: std::collections::BTreeMap<String, TriggerAction>,
    /// Wait up to this many seconds (at most 600) for the human to pick an action, click or
    /// dismiss it. Omitted: return at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_secs: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NotifyWithdrawParams {
    /// The `id` it was sent with (or the one `notify.send` returned).
    pub id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NotifyWithdrawResult {
    pub id: String,
    /// The app got the request (false: no app connected, nothing to remove from here).
    pub delivered: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NotifyResponseParams {
    pub id: String,
    /// Wait up to this many seconds (at most 600) for a response. Omitted: answer at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_secs: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NotifyResponseResult {
    pub id: String,
    /// What the human did; None: nothing yet (or not before `wait_secs` ran out).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<crate::notify::Response>,
    /// What the `on` action for that response did (None: it had none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback: Option<crate::notify::CallbackRun>,
}

/// The app reports what the human did with a notification (`notify.respond`, app only).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NotifyRespondParams {
    pub id: String,
    pub response: crate::notify::Response,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NotifyKindsList {
    /// The kinds the human added, with each one's settings resolved (as in notify.list).
    pub kinds: Vec<NotifyKindInfo>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NotifyKindInfo {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub description: String,
    /// `notify.<key>`.
    pub enabled: bool,
    /// `notify.stay.<key>`: seconds on screen, 0 = until handled or dismissed.
    pub stay: i64,
    /// `notify.color.<key>`.
    pub color: String,
    /// `notify.sound.<key>`.
    pub sound: String,
    /// `notify.push.<key>`.
    pub push: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NotifyKindsAddParams {
    /// 1–32 lowercase letters, digits and `_` (`deploys`); not a built-in kind.
    pub key: String,
    /// Shown in Settings and the notifications screen. Defaults to the key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Starting settings, by field (`stay`, `color`, `sound`, `volume`, `push`, `title`,
    /// `body`, …), same values as `notify.<field>.<key>` takes.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub settings: serde_json::Map<String, serde_json::Value>,
    /// Update the kind if it exists (its label and description; settings given are set).
    #[serde(default)]
    pub replace: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NotifyKindsRemoveParams {
    pub key: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct NotifyMediaParams {
    /// `sound` or `image`; both when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

/// A sound or image notifications can use.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NotifyMedia {
    /// `sound` or `image`.
    pub kind: String,
    /// What a `notify.sound.<kind>` / `notify.image[.<kind>]` setting holds to use it: a built-in
    /// sound's name, or an imported file's name.
    pub name: String,
    pub path: String,
    /// Ships with midna or macOS (can't be removed).
    #[serde(default)]
    pub builtin: bool,
    /// Which set a built-in sound belongs to: `twilight` (midna's own) or `macos`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub set: String,
    /// Settings that use it now.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub used_by: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NotifyMediaResult {
    pub sounds: Vec<NotifyMedia>,
    pub images: Vec<NotifyMedia>,
    /// Where imported files live (MIDNA_HOME/notify).
    pub dir: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NotifyImportParams {
    /// Absolute path to a sound (aiff, aif, wav, mp3, m4a, caf; up to 10 MB) or an image (png,
    /// jpg, jpeg, gif; up to 10 MB). The file is copied; the original can go.
    pub path: String,
    /// Use it right away for these settings, e.g. `["notify.sound.approval"]` or `["notify.image"]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub use_for: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NotifyRemoveParams {
    /// An imported file's name (see notify.media).
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NotifyRemoveResult {
    pub removed: String,
    /// Settings that used it, now back to their defaults.
    #[serde(default)]
    pub reset: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct NotifyClearParams {
    /// Only this terminal's notifications. Omitted: every notification midna has shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Id>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NotifyClearResult {
    /// The app got the request (false: no app connected, nothing to clear from here).
    pub delivered: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Id>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct NotifyHistoryParams {
    /// At most this many, newest first (default 200, at most 1000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Only this terminal's notifications.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Id>,
}

/// One notification midna recorded (a `notify.posted` event).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NotifyHistoryItem {
    pub seq: u64,
    pub at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<Id>,
    /// Posted after the human last opened the notifications (`notify.read`).
    pub unread: bool,
    /// Taken away with `notify.withdraw` (it no longer counts as unread).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub withdrawn: bool,
    pub notification: crate::notify::Posted,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NotifyHistoryResult {
    /// Newest first. Test notifications are left out.
    pub items: Vec<NotifyHistoryItem>,
    /// How many notifications are unread in all (not only the ones listed).
    pub unread: u32,
    /// Notifications up to this event seq are read.
    pub read_seq: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct NotifyReadParams {
    /// Mark notifications up to this event seq read. Omitted: every one so far.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NotifyReadResult {
    pub read_seq: u64,
    pub unread: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct NotifyTestParams {
    /// The category whose sound, volume and image to use (default `approval`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// The terminal the test is about (clicking it selects it). Defaults to the caller's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Id>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct NotifyPlayParams {
    /// A kind, to play its sound at its volume: a notification category (approval, failed, …)
    /// or a sound effect (approved, denied, queue_sent, …). Or a sound's name: a macOS sound
    /// (Glass, Pop, …) or an imported file's name (see notify.media).
    pub sound: String,
    /// 0–100 instead of the kind's volume (still scaled by `notify.volume`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<u8>,
    /// The terminal it's about. Defaults to the caller's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Id>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NotifyPlayResult {
    pub played: bool,
    /// The sound's name.
    pub sound: String,
    /// Why nothing played: `sounds_off` (notify.sounds), `silent` (volume 0), `no_sound` (the
    /// kind's sound is none), `rate_limited`, `no_app`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct NotifySendResult {
    pub posted: bool,
    /// Why it wasn't posted: `disabled`, `muted`, `category_off`, `duplicate`, `rate_limited`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// `notify.send`: its id (for `notify.withdraw` and `notify.response`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Who shows it: `app`, `system` (no app running: a plain banner, no buttons or click) or
    /// `none` (recorded only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    /// With `wait_secs`: what the human did (None: nothing before it ran out).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<crate::notify::Response>,
    /// With `wait_secs`: what the `on` action for that response did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback: Option<crate::notify::CallbackRun>,
}

// ------------------------------------------------------------------ daemon

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct DaemonInfo {
    pub version: String,
    pub pid: u32,
    pub uptime_secs: u64,
    pub started_at: Timestamp,
    pub home: String,
    pub socket: String,
    /// Your role as the daemon sees this connection: `human` or `agent`.
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Id>,
    /// Absolute path of the running midnad executable (lets the app tell whether the daemon
    /// runs the installed `bin/current` build).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct UpgradeParams {
    pub binary_path: String,
    /// Upgrade even though the Mac is busy (else refused with error BUSY; see system.busy_load).
    #[serde(default)]
    pub force: bool,
}

/// `daemon.restart`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct RestartParams {
    /// Restart even though the Mac is busy (else refused with error BUSY; see system.busy_load).
    #[serde(default)]
    pub force: bool,
}

/// `daemon.upgrade` / `daemon.restart`: the new binary passed `--selftest` and the handoff
/// (snapshot + execv) is scheduled. Watch for `daemon.upgraded` (or `daemon.upgrade_failed`).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct UpgradeResult {
    pub ok: bool,
    /// The binary that will be exec'd.
    pub binary: String,
    /// Terminals that will be carried over.
    pub sessions: u32,
}

// ------------------------------------------------------------------ projects

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ProjectAddParams {
    /// Absolute directory path.
    pub path: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ProjectUpdateParams {
    pub id: Id,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub commands: Option<Vec<ProjectCommand>>,
    /// Keep it in the sidebar with no terminals open (see `Project.pinned`).
    #[serde(default)]
    pub pinned: Option<bool>,
}

// ------------------------------------------------------------------ sessions

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct SessionListParams {
    /// Only this project's terminals, by id or name.
    #[serde(default)]
    pub project_id: Option<Id>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SessionOpenParams {
    /// Project to open in, by id or name. Defaults to the caller's project, else the project containing `cwd`
    /// (a project is added for `cwd` if none contains it). With no project, no caller project
    /// and no `cwd`, it opens at root (`project_id` "root", starting in `$HOME`).
    #[serde(default)]
    pub project_id: Option<Id>,
    pub kind: SessionKind,
    /// Required when kind is `agent`.
    #[serde(default)]
    pub agent: Option<AgentKind>,
    #[serde(default)]
    pub name: Option<String>,
    /// Working directory; defaults to the project path.
    #[serde(default)]
    pub cwd: Option<String>,
    /// argv for shell/monitor sessions; defaults to the login shell.
    #[serde(default)]
    pub command: Option<Vec<String>>,
    /// Initial prompt passed to the agent.
    #[serde(default)]
    pub prompt: Option<String>,
    /// kind=agent: the agent's own arguments, after midna's flags (`["--append-system-prompt", "…",
    /// "--settings", "board.json", "--model", "opus"]`). Claude takes one `--settings` and one
    /// `--append-system-prompt`, so midna merges yours into its own: the settings into one file
    /// (yours win on conflicts; hooks and permission lists from both apply), your text ahead of
    /// midna's hint. Codex `-c developer_instructions=…` is joined the same way, and a `-c notify=…`
    /// runs after midna's. Kept for restarts (minus the prompt and conversation options).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agent_args: Vec<String>,
    /// kind=agent: reopen this conversation (Claude `--resume <id>`, Codex `codex resume … <id>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<String>,
    #[serde(default)]
    pub cols: Option<u16>,
    #[serde(default)]
    pub rows: Option<u16>,
    /// Open it as a background terminal: tracked as usual, but folded away at the bottom of
    /// the sidebar instead of listed under its project, and never selected on open.
    #[serde(default)]
    pub background: bool,
    /// Close the terminal when its agent (or command) exits normally: exit 0, ctrl-c or hang-up.
    /// A failed exit stays open with its needs-you item so someone can see why. For agents that
    /// open a terminal for one job.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub close_on_exit: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SessionSetBackgroundParams {
    pub id: Id,
    /// True moves the terminal to the sidebar's Background group, false back to its project.
    pub background: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SessionCloseParams {
    pub id: Id,
    /// Close even if the session is working (sends SIGKILL after SIGHUP).
    #[serde(default)]
    pub force: bool,
    /// false: don't clean up after it (`cleanup.enabled`) this time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SessionRenameParams {
    pub id: Id,
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SessionInputParams {
    pub id: Id,
    /// Text to type. Use escape sequences for special keys (e.g. "\u0003" for ctrl-c).
    pub text: String,
    /// Press Enter (carriage return) after the text.
    #[serde(default)]
    pub enter: bool,
    /// Absolute paths of images to attach before the text (PNG, JPEG, GIF, WebP; other formats
    /// macOS can read are converted to PNG). Each is pasted as its path, which Claude Code and
    /// Codex turn into an image attachment.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SessionReadParams {
    pub id: Id,
    /// How many trailing lines of scrollback+screen to return (default 50). Ignored with `screen`.
    #[serde(default)]
    pub lines: Option<u32>,
    /// Return exactly the visible screen instead.
    #[serde(default)]
    pub screen: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SessionReadResult {
    pub text: String,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SessionResizeParams {
    pub id: Id,
    pub cols: u16,
    pub rows: u16,
    #[serde(default)]
    pub cell_w: Option<u32>,
    #[serde(default)]
    pub cell_h: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SessionKeyParams {
    pub id: Id,
    /// A keystroke: modifiers then a key, joined by `-`, e.g. `enter`, `ctrl-c`, `shift-tab`,
    /// `alt-b`, `ctrl-shift-up`, `f5`, `pageup`, `escape`, `a`. Modifiers: ctrl, alt (option),
    /// shift, super (cmd). It is encoded the way the app in the terminal asked for (legacy,
    /// modifyOtherKeys or the kitty keyboard protocol), exactly like a key typed in the GUI.
    pub key: String,
    /// press (default), repeat or release. Releases only produce bytes for apps that asked
    /// for kitty event reporting.
    #[serde(default)]
    pub action: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct SessionScrollParams {
    pub id: Id,
    /// `top` or `bottom` (the live screen).
    #[serde(default)]
    pub to: Option<String>,
    /// Scroll by this many rows (negative = up, into history).
    #[serde(default)]
    pub lines: Option<i32>,
    /// Scroll by this many pages (negative = up).
    #[serde(default)]
    pub pages: Option<i32>,
}

/// One prompt the human sent an agent terminal (`agent.prompt_submitted`), numbered from 1 in
/// the order sent.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PromptMark {
    pub n: u32,
    /// The prompt (first 200 characters).
    pub text: String,
    pub at: Timestamp,
    /// The agent's conversation id when it was sent (Claude's session id, Codex's thread id).
    #[serde(default)]
    pub conversation: Option<String>,
    /// Sent in the conversation the agent shows now, so session.jump_prompt can scroll to it.
    /// False for prompts before a /clear or a fresh start.
    pub on_screen: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct SessionPromptsResult {
    /// Oldest first.
    pub prompts: Vec<PromptMark>,
    /// The prompt the top of the agent's view belongs to (its `n`), when the screen shows it.
    #[serde(default)]
    pub here: Option<u32>,
    /// The agent's view is scrolled back from the live end.
    pub scrolled: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct SessionJumpPromptParams {
    pub id: Id,
    /// The prompt to scroll to (`n` from session.prompts).
    #[serde(default)]
    pub n: Option<u32>,
    /// Instead of `n`: `prev` or `next` from where the view is, `latest` (the last prompt), or
    /// `live` (back to the bottom).
    #[serde(default)]
    pub to: Option<String>,
    /// Wait for the agent to finish scrolling and report whether the prompt was found
    /// (default true). False returns at once.
    #[serde(default)]
    pub wait: Option<bool>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct JumpPromptResult {
    pub ok: bool,
    /// The prompt jumped to (none for `live`).
    #[serde(default)]
    pub n: Option<u32>,
    /// The prompt's row is on screen (false when waiting was off, or it couldn't be found).
    pub found: bool,
    /// Why nothing happened or the prompt wasn't found.
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SessionSelectionResult {
    /// The selected text (soft-wrapped lines joined, trailing spaces trimmed), or null when
    /// nothing is selected.
    pub text: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SessionLinkAtParams {
    pub id: Id,
    /// Viewport column and row (0-based), as on screen right now.
    pub col: u16,
    pub row: u16,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct LinkAtResult {
    /// `url` (OSC 8 hyperlink or a detected http(s) URL), `file` (an existing path), or `none`.
    pub kind: String,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub line: Option<u32>,
    #[serde(default)]
    pub column: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SessionFindParams {
    pub id: Id,
    /// Text to find in scrollback + screen (case-insensitive unless it contains capitals).
    /// Empty clears the search.
    pub query: String,
    /// Go to the previous (older) match instead of the next one.
    #[serde(default)]
    pub backwards: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct FindResult {
    /// Number of matches.
    pub total: usize,
    /// 1-based index of the current match, oldest first (0 = no match).
    pub index: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct StreamAttachParams {
    pub session: Id,
    pub cols: u16,
    pub rows: u16,
    #[serde(default)]
    pub cell_w: u32,
    #[serde(default)]
    pub cell_h: u32,
}

// ------------------------------------------------------------------ events

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct EventFilter {
    /// Kind prefixes, e.g. `["session.", "needs_you.raised"]`.
    #[serde(default)]
    pub kinds: Option<Vec<String>>,
    #[serde(default)]
    pub session_id: Option<Id>,
    #[serde(default)]
    pub project_id: Option<Id>,
}

impl EventFilter {
    pub fn matches(&self, e: &Event) -> bool {
        if let Some(k) = &self.kinds
            && !k.iter().any(|p| e.kind.starts_with(p.as_str())) {
                return false;
            }
        if self.session_id.is_some() && self.session_id != e.session_id {
            return false;
        }
        if self.project_id.is_some() && self.project_id != e.project_id {
            return false;
        }
        true
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct EventsListParams {
    /// Return events with seq > since_seq.
    #[serde(default)]
    pub since_seq: Option<u64>,
    /// Max events (default 200, newest kept).
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub filter: Option<EventFilter>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct EventsSubscribeParams {
    /// Replay events with seq > since_seq before streaming live ones. Omit for live only.
    #[serde(default)]
    pub since_seq: Option<u64>,
    #[serde(default)]
    pub filter: Option<EventFilter>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SubscribeResult {
    pub ok: bool,
    /// The latest seq at subscribe time.
    pub seq: u64,
}

// ------------------------------------------------------------------ needs-you

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct NeedsYouListParams {
    #[serde(default)]
    pub session_id: Option<Id>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NeedsYouRaiseParams {
    /// `blocked` (you cannot continue without the human) or `note` (FYI).
    pub kind: NeedsYouKind,
    pub message: String,
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NeedsYouResolveParams {
    pub id: Id,
    pub resolution: Resolution,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ResolveResult {
    pub ok: bool,
    /// The rule created by an approval with scope minutes/session/always.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<Rule>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NeedsYouGetParams {
    pub id: Id,
    /// Block up to this many seconds while the item is still open (0 or absent = answer now;
    /// capped at 600). Returns as soon as it is answered or withdrawn.
    #[serde(default)]
    pub wait_secs: Option<u64>,
}

/// Where a needs-you item stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NeedsYouState {
    /// Still waiting on the human.
    Open,
    /// Answered (approve, deny, dismiss, done, restart) or auto-resolved; see `resolution`.
    Resolved,
    /// Taken back before anyone answered: the terminal it was about closed, or the caller that
    /// asked went away.
    Withdrawn,
    /// Nobody answered within the asking call's timeout.
    Timeout,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct NeedsYouGetResult {
    pub id: Id,
    pub state: NeedsYouState,
    /// The item, while it is open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<NeedsYou>,
    /// How it ended, e.g. `{"kind":"approve","scope":{"kind":"once"}}`, `{"kind":"deny"}`,
    /// `{"kind":"withdrawn","reason":"…"}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_by: Option<Actor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<Timestamp>,
    /// For a call made with `caller.no_wait`: what that call returned once it finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// For a call made with `caller.no_wait`: its error, if it failed (a denial included).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
}

// ------------------------------------------------------------------ policy / rules

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct PolicyCheckParams {
    pub action: PolicyAction,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct PolicyRequestParams {
    pub action: PolicyAction,
    /// Max seconds to wait for a human (default: setting policy.request_timeout_secs).
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Shown on the approval card.
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct PolicyRequestResult {
    pub decision: Effect,
    pub source: DecisionSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<Rule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs_you_id: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct RuleAddParams {
    pub effect: Effect,
    pub matcher: Matcher,
    /// Defaults to global.
    #[serde(default)]
    pub scope: Option<RuleScope>,
    #[serde(default)]
    pub expires_in_secs: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct RuleRequestRemovalParams {
    pub id: Id,
    pub reason: String,
}

// ------------------------------------------------------------------ triggers / webhooks

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TriggerAddParams {
    pub name: String,
    pub source: TriggerSource,
    /// GitHub: `<X-GitHub-Event>[.<action>]`, e.g. `pull_request.opened`, `push`, `check_run.completed`.
    /// Bitbucket: the X-Event-Key, e.g. `pullrequest:created`. Globs (`*`) allowed.
    /// Local: `hook.<HookEvent>` (e.g. `hook.Stop`, `hook.Notification`), a midna event kind
    /// (e.g. `agent.prompt_blocked`, `agent.turn_ended`, `session.status`), `idle` (with
    /// `filter.idle_minutes`) or `schedule` (with `filter.cron`). Globs allowed.
    pub event: String,
    #[serde(default)]
    pub filter: TriggerFilter,
    pub action: TriggerAction,
    #[serde(default)]
    pub github_hook_id: Option<u64>,
    #[serde(default)]
    pub session_name_template: Option<String>,
    #[serde(default)]
    pub cooldown_secs: Option<u64>,
    /// Local triggers only: enable right away (agents may, when the human asked for the trigger).
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TriggerUpdateParams {
    pub id: Id,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub filter: Option<TriggerFilter>,
    #[serde(default)]
    pub action: Option<TriggerAction>,
    #[serde(default)]
    pub source: Option<TriggerSource>,
    #[serde(default)]
    pub event: Option<String>,
    #[serde(default)]
    pub github_hook_id: Option<u64>,
    #[serde(default)]
    pub session_name_template: Option<String>,
    #[serde(default)]
    pub cooldown_secs: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TriggerSetEnabledParams {
    pub id: Id,
    pub enabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TriggerSetSecretParams {
    pub id: Id,
    pub secret: String,
}

// ------------------------------------------------------------------ secrets

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct SecretListParams {
    /// Only the secrets this project can use (its own plus the global ones). Agents in a
    /// terminal default to their terminal's project.
    #[serde(default)]
    pub project_id: Option<Id>,
    /// Every secret, in any project.
    #[serde(default)]
    pub all: bool,
    /// The caller's working directory: outside a midna terminal, the project whose folder holds it.
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SecretSetParams {
    pub name: String,
    pub value: String,
    /// None = usable in every project.
    #[serde(default)]
    pub project_id: Option<Id>,
    /// What it looks like ("GitHub token").
    #[serde(default)]
    pub label: Option<String>,
    /// Usable in every project. Without it, an agent's secret belongs to its terminal's project.
    #[serde(default)]
    pub global: bool,
    /// The value was piped in (`<command> | midna secret save NAME`), so it never passed
    /// through the agent's context. An agent's value without this is marked `exposed`.
    #[serde(default)]
    pub piped: bool,
    /// The caller's working directory: outside a midna terminal, the project whose folder holds it.
    #[serde(default)]
    pub cwd: Option<String>,
}

/// Approving an agent's replacement of a secret the human stored.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SecretReplaceParams {
    /// Where the agent's value waits (a vault entry, never the value itself).
    pub pending: String,
    pub name: String,
    #[serde(default)]
    pub project_id: Option<Id>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub exposed: bool,
    pub added_by: Actor,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SecretNameParams {
    pub name: String,
    /// Which project's secret; None = the caller's project first, then the global one.
    #[serde(default)]
    pub project_id: Option<Id>,
    /// The caller's working directory: outside a midna terminal, the project whose folder holds it.
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SecretWriteParams {
    pub name: String,
    /// Absolute path of the file (created 0600 if missing).
    pub path: String,
    /// The variable to set in the file; default = the secret's name.
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub project_id: Option<Id>,
    /// The caller's working directory: outside a midna terminal, the project whose folder holds it.
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SecretWriteResult {
    pub path: String,
    pub key: String,
    /// True when the file already set this key and the line was replaced.
    pub replaced: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SecretExecEnvParams {
    /// Secret names to resolve.
    pub names: Vec<String>,
    /// The command about to run (audit trail only).
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub project_id: Option<Id>,
    /// The caller's working directory: outside a midna terminal, the project whose folder holds it.
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SecretExecEnvResult {
    /// name → value.
    pub values: std::collections::BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct TriggerDeliveriesParams {
    #[serde(default)]
    pub trigger_id: Option<Id>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TriggerReplayParams {
    pub delivery_id: Id,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct TriggerTestParams {
    pub trigger_id: Id,
    /// A sample webhook body (the JSON GitHub/Bitbucket would POST). Local triggers: the hook
    /// payload or event data to match against (optional; a `schedule` trigger tests against
    /// its next run).
    #[serde(default)]
    pub payload: Value,
    /// The event header value (X-GitHub-Event / X-Event-Key). Defaults to the trigger's event.
    #[serde(default)]
    pub event: Option<String>,
    /// Local triggers: the terminal the event is about (filters and `{{session.*}}`).
    #[serde(default)]
    pub session: Option<Id>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct WebhooksConfigureParams {
    /// `tailscale_funnel`, `self_relay`, `midna_relay` or `off`.
    pub path: String,
    /// Local receiver port (setting webhooks.port).
    #[serde(default)]
    pub port: Option<u16>,
    /// Relay URL for self_relay (setting webhooks.relay_url).
    #[serde(default)]
    pub relay_url: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct WebhookReceiverStatus {
    /// Listening on 127.0.0.1.
    pub listening: bool,
    /// The configured port (webhooks.port).
    pub port: u16,
    /// The port actually bound (differs from `port` only in tests).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct TailscaleStatus {
    /// Path of the tailscale CLI, if found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_state: Option<String>,
    /// MagicDNS name without the trailing dot, e.g. `morgans-macbook-pro.tail439d44.ts.net`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dns_name: Option<String>,
    pub online: bool,
    /// The tailnet allows Funnel for this node (node capability `funnel`).
    pub funnel_allowed: bool,
    /// Funnel serves :8443 and proxies it to this receiver.
    pub funnel_on: bool,
    /// Where :8443 currently proxies to, if anywhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub funnel_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct ReconcileStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<Timestamp>,
    /// Why it last ran: `startup`, `wake` or `manual`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Deliveries found missing and redelivered as `recovered` on the last run.
    pub recovered: u32,
    /// Hooks checked on the last run.
    pub hooks_checked: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Webhook delivery path health. `health` is `healthy`, `degraded`, `down` or `off`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct WebhooksStatus {
    /// Setting webhooks.path.
    pub path: String,
    /// Human name of the path, e.g. `Tailscale Funnel`.
    pub path_name: String,
    pub health: String,
    /// One line explaining the health.
    pub detail: String,
    /// Public URL to paste into GitHub (`…/hooks/github`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
    /// Public URL to paste into Bitbucket (`…/hooks/bitbucket`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bitbucket_url: Option<String>,
    /// Public host:port, e.g. `morgans-macbook-pro.tail439d44.ts.net:8443`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    pub receiver: WebhookReceiverStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tailscale: Option<TailscaleStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_delivery_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_delivery: Option<Delivery>,
    pub reconcile: ReconcileStatus,
    /// `keychain` or `file` (MIDNA_SECRETS=file).
    pub secret_store: String,
    /// Suggested fix when not healthy, phrased as something to ask an agent or do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct WebhooksConfigureResult {
    pub ok: bool,
    pub message: String,
    /// Tailscale's "enable Funnel for your tailnet" URL, when Funnel isn't enabled yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_url: Option<String>,
    pub status: WebhooksStatus,
}

// ------------------------------------------------------------------ settings

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SettingEntry {
    pub key: String,
    #[serde(rename = "type")]
    pub ty: crate::settings::SettingType,
    pub value: Value,
    pub default: Value,
    pub description: String,
    pub human_only: bool,
    pub section: String,
    /// The CLI command that changes this setting.
    pub cli: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SettingKeyParams {
    pub key: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SettingSetParams {
    pub key: String,
    /// JSON value; strings like "true" or "42" are coerced to the setting's type.
    pub value: Value,
}

// ------------------------------------------------------------------ insights

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InsightsRange {
    Today,
    Yesterday,
    /// The last 7 local days, today included.
    Week,
    /// The last 30 local days, today included.
    Month,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InsightsBy {
    Project,
    Agent,
    Terminal,
    Day,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct InsightsSummaryParams {
    #[serde(default = "default_range")]
    pub range: InsightsRange,
    #[serde(default)]
    pub by: Option<InsightsBy>,
}

fn default_range() -> InsightsRange {
    InsightsRange::Today
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InsightsTotals {
    pub turns: i64,
    /// Human prompt submits.
    pub messages: i64,
    pub spend_usd: f64,
    pub working_secs: i64,
    pub waiting_secs: i64,
    pub approvals: i64,
    pub triggers_fired: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct InsightsRow {
    pub key: String,
    pub label: String,
    pub totals: InsightsTotals,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct InsightsSummary {
    pub range: InsightsRange,
    pub from: Timestamp,
    pub to: Timestamp,
    pub totals: InsightsTotals,
    #[serde(default)]
    pub rows: Vec<InsightsRow>,
    /// totals minus the previous period of the same length.
    pub vs_previous: InsightsTotals,
}

/// What `insights.series` measures per bucket.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InsightsMetric {
    /// Agent turns started (`agent.turn_started`).
    Turns,
    /// Human prompt submits (`agent.prompt_submitted`).
    Messages,
    /// USD spent (`agent.cost` deltas).
    Spend,
    /// Seconds terminals spent in `working`.
    Working,
    /// Seconds terminals spent in `needs_you` (waiting on the human).
    Waiting,
    /// Approvals granted (`needs_you.resolved` with approve).
    Approvals,
    /// Triggers fired (`trigger.fired`).
    Triggers,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InsightsBucketSize {
    Hour,
    Day,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct InsightsSeriesParams {
    #[serde(default = "default_range")]
    pub range: InsightsRange,
    pub metric: InsightsMetric,
    /// Default: hour for today/yesterday, day for week/month.
    #[serde(default)]
    pub bucket: Option<InsightsBucketSize>,
    /// Split each bucket by project, agent kind or terminal. Omit for one total per bucket.
    #[serde(default)]
    pub by: Option<InsightsBy>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InsightsSeriesBucket {
    /// Bucket start (RFC 3339, local hour or local midnight).
    pub start: Timestamp,
    pub end: Timestamp,
    pub total: f64,
    /// Group key -> value in this bucket (only non-zero groups; empty without `by`).
    #[serde(default)]
    pub values: std::collections::BTreeMap<String, f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InsightsSeriesGroup {
    pub key: String,
    pub label: String,
    pub total: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct InsightsSeries {
    pub range: InsightsRange,
    pub metric: InsightsMetric,
    pub bucket: InsightsBucketSize,
    /// `count`, `usd` or `secs`.
    pub unit: String,
    pub from: Timestamp,
    pub to: Timestamp,
    /// Every bucket in the range, oldest first, including empty and not-yet-reached ones.
    pub buckets: Vec<InsightsSeriesBucket>,
    /// Groups seen in the range, largest total first.
    #[serde(default)]
    pub groups: Vec<InsightsSeriesGroup>,
    pub total: f64,
    /// The same metric over the period of the same length immediately before.
    pub previous_total: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct InsightsDetailParams {
    #[serde(default = "default_range")]
    pub range: InsightsRange,
}

/// The Insights widgets' data for a range (`insights.detail`), all from the event log.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InsightsDetail {
    pub from: Timestamp,
    pub to: Timestamp,
    pub concurrency: InsightsConcurrency,
    pub turns: InsightsTurnLengths,
    /// Needs-you items resolved in the range, oldest first.
    pub waits: Vec<InsightsWait>,
    /// Per terminal, most time lost to you first.
    pub agent_time: Vec<InsightsAgentTime>,
    /// Approved needs-you items grouped by title, most first (at most 10).
    pub approved: Vec<InsightsCount>,
    /// One per local day in the range.
    pub corrections: Vec<InsightsCorrections>,
    /// The last 7 local days (oldest first) × 24 hours, whatever the range.
    pub heatmap: Vec<InsightsHeatDay>,
    /// Agent working time per project, most first.
    pub projects: Vec<InsightsCount>,
    /// Spend per model, most first.
    pub models: Vec<InsightsCount>,
    /// Records over the whole log.
    pub bests: InsightsBests,
}

/// Agents in the `working` state at once.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InsightsConcurrency {
    /// Width of each sample: 600 (today / yesterday) or 3600.
    pub step_secs: i64,
    /// Average number of agents working during each step, from `from` to the axis end.
    pub samples: Vec<f64>,
    /// Most agents working at the same moment.
    pub peak: u32,
    #[serde(default)]
    pub peak_at: Option<Timestamp>,
    /// Average count over the time at least one agent worked.
    pub avg_while_working: f64,
    /// Seconds with two or more agents working.
    pub multi_secs: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InsightsTurnLengths {
    /// Turns ended in the range by length: <1m, 1–5m, 5–15m, 15–30m, 30–60m, 1h+.
    pub bins: Vec<i64>,
    pub median_secs: i64,
    pub longest_secs: i64,
    #[serde(default)]
    pub longest_session: Option<Id>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InsightsWait {
    /// Raised → resolved.
    pub secs: i64,
    pub resolved_at: Timestamp,
    #[serde(default)]
    pub session_id: Option<Id>,
    pub title: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InsightsAgentTime {
    /// Session id.
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub project_id: Option<Id>,
    pub working_secs: i64,
    /// In `needs_you`.
    pub blocked_secs: i64,
    /// From a turn's end to your next prompt (gaps over 4 hours left out: you were away).
    pub idle_secs: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InsightsCount {
    pub key: String,
    pub label: String,
    pub value: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InsightsCorrections {
    /// Local midnight.
    pub day: Timestamp,
    /// Needs-you items you denied.
    pub denied: i64,
    /// Turns you stopped (prompt dismissed / Esc).
    pub stopped: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InsightsHeatDay {
    /// Local midnight.
    pub day: Timestamp,
    /// Agent working seconds per hour (summed over agents, so it can pass 3600).
    pub working_secs: Vec<i64>,
    /// Hours you sent a prompt or answered a needs-you item.
    pub you: Vec<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InsightsBests {
    #[serde(default)]
    pub busiest_day: Option<Timestamp>,
    /// Agent working seconds on that day.
    pub busiest_day_secs: i64,
    pub longest_turn_secs: i64,
    #[serde(default)]
    pub longest_turn_at: Option<Timestamp>,
    pub peak_agents: u32,
    #[serde(default)]
    pub peak_at: Option<Timestamp>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct InsightsActivityParams {
    /// RFC 3339 lower bound (default: 24h ago).
    #[serde(default)]
    pub since: Option<Timestamp>,
    #[serde(default)]
    pub filter: Option<EventFilter>,
    #[serde(default)]
    pub limit: Option<u32>,
}

// ------------------------------------------------------------------ window

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WindowAction {
    Front,
    KeepOnTop,
    PopOut,
    Snap,
    Close,
    OpenScreen,
    /// Show `target` (a session) in the main window's split pane: `value` = `side` (default)
    /// or `stacked`; `close` folds the split (the session keeps running).
    Split,
}

impl WindowAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            WindowAction::Front => "front",
            WindowAction::KeepOnTop => "keep_on_top",
            WindowAction::PopOut => "pop_out",
            WindowAction::Snap => "snap",
            WindowAction::Close => "close",
            WindowAction::OpenScreen => "open_screen",
            WindowAction::Split => "split",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct WindowCommandParams {
    pub action: WindowAction,
    /// Usually a session id; for open_screen a screen name (rules, triggers, insights, notifications, settings, needs_you).
    #[serde(default)]
    pub target: Option<String>,
    /// Action-specific value, e.g. `true`/`false` for keep_on_top, `left`/`right` for snap.
    #[serde(default)]
    pub value: Option<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct WindowCommandResult {
    pub ok: bool,
    /// How many GUI connections received the command (0 = GUI not running).
    pub delivered: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct WindowList {
    pub gui_connected: bool,
    /// Sessions marked keep-on-top.
    pub keep_on_top: Vec<Id>,
}

// ------------------------------------------------------------------ restart

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SessionRestartParams {
    pub id: Id,
    /// Agent terminals: reopen the same conversation (`claude --resume`, `codex resume`) with
    /// the model and permission mode it had. Default: true when the conversation id is known.
    #[serde(default)]
    pub resume: Option<bool>,
    /// `now` (default) or `idle`: queue it until the agent is idle with no background work,
    /// subagents or scheduled wakeups in flight and nothing typed in its input box.
    #[serde(default)]
    pub when: Option<String>,
    /// Restart now even though background work would be lost (`when: now` only).
    #[serde(default)]
    pub force: bool,
    /// Shown with a queued restart.
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SessionReplaceParams {
    pub id: Id,
    /// Replace it even if it's working or waiting on the human (agents only; the human isn't asked).
    #[serde(default)]
    pub force: bool,
}

// ------------------------------------------------------------------ adopted agents

/// `midna shim`: an agent was typed into a shell terminal. The reply says whether midna runs it
/// (`run`, the full command) or the shim runs the agent as typed.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SessionAdoptParams {
    pub agent: AgentKind,
    /// The agent binary the shell would have run (the next one on PATH after midna's shim).
    pub bin: String,
    pub args: Vec<String>,
    /// The shim's pid; it must be a process in the terminal.
    pub pid: i32,
    /// Defaults to the caller's session.
    #[serde(default)]
    pub session: Option<Id>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct SessionAdoptResult {
    /// The command to run (binary first); none = run the agent as typed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<Vec<String>>,
    /// Environment the command gets on top of the shell's.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub env: std::collections::BTreeMap<String, String>,
    /// The directory to run it in, when not the shell's (a restart of an agent that had moved
    /// into a worktree).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// `midna shim`: the adopted agent exited. The reply's `run` is the next command when a
/// restart asked for one; none = the shim exits with the agent's code.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SessionAdoptEndParams {
    pub pid: i32,
    #[serde(default)]
    pub code: Option<i32>,
    #[serde(default)]
    pub session: Option<Id>,
}

// ------------------------------------------------------------------ agent hooks

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct AgentHookParams {
    pub agent: AgentKind,
    /// Hook event name, e.g. `PreToolUse`, `Stop`, `statusline`, `agent-turn-complete`.
    pub event: String,
    #[serde(default)]
    pub payload: Value,
    /// The midna session the hook ran in (defaults to the caller's session).
    #[serde(default)]
    pub session: Option<Id>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct AgentHookResult {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<StatusState>,
}

// ------------------------------------------------------------------ global hooks

/// midna's hooks in one agent's global config (`~/.claude/settings.json` or
/// `~/.codex/config.toml`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HookInstall {
    pub agent: AgentKind,
    /// `not_installed`, `current`, `stale` (installed but out of date: reinstall),
    /// `unavailable` (the agent's config folder doesn't exist) or `error` (unreadable config).
    pub state: String,
    /// The config file.
    pub path: String,
    /// Why it is stale or unreadable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// midna adds its hooks to each agent it starts itself (true unless the global install is
    /// current, which already covers them).
    pub per_session: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HooksStatus {
    pub claude: HookInstall,
    pub codex: HookInstall,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct HooksTargetParams {
    /// Which agents (`claude`, `codex`). Empty = every agent whose config folder exists.
    #[serde(default)]
    pub agents: Vec<AgentKind>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct HooksPreviewParams {
    #[serde(default)]
    pub agents: Vec<AgentKind>,
    /// Preview removing midna's hooks instead of installing them.
    #[serde(default)]
    pub uninstall: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DiffLine {
    /// `+` added, `-` removed, ` ` context, `…` skipped unchanged lines.
    pub op: String,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct HookFileDiff {
    pub agent: AgentKind,
    pub path: String,
    /// The file doesn't exist yet and would be created.
    pub creates: bool,
    /// Nothing would change.
    pub unchanged: bool,
    pub lines: Vec<DiffLine>,
    /// The file can't be changed safely (e.g. it doesn't parse); nothing will be written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct HooksPreview {
    pub files: Vec<HookFileDiff>,
}

// ------------------------------------------------------------------ scripts

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScriptSlot {
    Header,
    Row,
    /// The status bar's `script` item, run for the selected terminal.
    Status,
    /// A custom header button (a script path in `ui.header.buttons`; pass it as `script`).
    Button,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ScriptRunParams {
    pub session_id: Id,
    pub slot: ScriptSlot,
    /// With slot `status`: one of the script paths listed in `ui.status.items`, run instead
    /// of `ui.status.script`. With slot `button`: one of the paths in `ui.header.buttons`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ScriptClickParams {
    pub session_id: Id,
    /// A script path listed in `ui.header.buttons`.
    pub script: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ScriptRunResult {
    pub segments: Vec<Segment>,
}

// ------------------------------------------------------------------ rule.restore

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct RuleRestoreParams {
    /// The removed rule exactly as `rule.list` / the `rule.removed` event returned it. It comes
    /// back with its original id, author, origin and fired count (removal_request is cleared).
    pub rule: Rule,
}

// ------------------------------------------------------------------ daemon.reset

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct DaemonResetParams {
    /// Keep rules (default true). The event log is always kept.
    #[serde(default)]
    pub keep_rules: Option<bool>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct DaemonResetResult {
    pub ok: bool,
    pub sessions_closed: u32,
    pub projects_removed: u32,
    pub triggers_removed: u32,
    pub needs_you_cleared: u32,
    pub settings_reset: u32,
    pub rules_removed: u32,
}

// ------------------------------------------------------------------ ui.commands

/// What a palette user command does when the human runs it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiCommandRun {
    /// Call a daemon method as the human (a `session.open` result is selected afterwards).
    /// Human-only methods are allowed but always get the "destructive" two-step confirm.
    Rpc {
        method: String,
        #[serde(default)]
        params: Value,
    },
    /// Select a terminal.
    Focus { session: Id },
    /// Select project N (sidebar order, 0-based).
    Project { index: usize },
    /// Show a screen: rules | triggers | insights | notifications | settings | needs_you.
    Screen { screen: String },
    /// Open a terminal in its own always-on-top window.
    PopOut { session: Id },
    /// Replace the palette's query with this text (e.g. a prompt for "Ask an agent").
    Prefill { text: String },
}

/// One user command in the human's ⌘K palette (`$MIDNA_HOME/commands.json`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct UiCommand {
    /// Stable id. Defaults to `user:<slugified title>`.
    #[serde(default)]
    pub id: String,
    pub title: String,
    /// Second line (where/what).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sub: String,
    /// Extra words matched by prefix ("ship" finds "Deploy staging").
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub keywords: String,
    /// Shortcut label shown on the row (display only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keys: Option<String>,
    /// CLI equivalent shown for discoverability.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli: Option<String>,
    /// Row icon: claude|codex|monitor|shell|project|run|screen|approve|deny|rule|pin|new|trigger|restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Show with an empty query under this heading (e.g. "Suggested").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub featured: Option<String>,
    /// Why it is destructive; set = the human presses enter twice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub danger: Option<String>,
    /// Shown as "you only".
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub human_only: bool,
    pub run: Option<UiCommandRun>,
    /// Filled by the daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_by: Option<Actor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_at: Option<Timestamp>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct UiCommandsList {
    /// The backing file (`$MIDNA_HOME/commands.json`); scripts may edit it directly.
    pub path: String,
    pub commands: Vec<UiCommand>,
    /// Entries in the file that failed validation (they are not shown in the palette).
    #[serde(default)]
    pub invalid: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct UiCommandsAddParams {
    pub command: UiCommand,
    /// Replace an existing command with the same id instead of failing.
    #[serde(default)]
    pub replace: bool,
}

// ------------------------------------------------------------------ updates

/// The app's updater state, as the GUI last reported it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct UpdatesStatus {
    /// Whether a GUI is connected (the GUI owns the updater; nothing happens without it).
    #[serde(default)]
    pub gui_connected: bool,
    /// unknown (no GUI report yet) | disabled (dev build) | idle | checking | up_to_date |
    /// downloading | ready (restart to apply) | error.
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checked_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_at: Option<Timestamp>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct UpdatesReportParams {
    pub status: UpdatesStatus,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct UpdatesCommandResult {
    pub ok: bool,
    /// GUI connections that received the request (0 = no GUI running; nothing happens).
    pub delivered: u32,
    pub status: UpdatesStatus,
}

// ------------------------------------------------------------------ permissions

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct PermissionEntry {
    /// accessibility | notifications | login-items
    pub name: String,
    /// granted | not_granted | enabled | disabled | not_requested | unknown
    pub state: String,
    /// What the daemon can actually see (and what it can't).
    pub detail: String,
    /// The command that opens the right System Settings pane (only the human can grant).
    pub open_cli: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct PermissionsStatus {
    pub permissions: Vec<PermissionEntry>,
}

// ---------------------------------------------------------------- themes

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ThemesListResult {
    /// Built-in themes (dark ones first), then custom ones.
    pub themes: Vec<crate::themes::ThemeInfo>,
    /// Custom theme files that failed to load: `<file>: <why>`.
    pub errors: Vec<String>,
    /// Where custom theme files go.
    pub dir: String,
    /// The `theme` setting (system, or a theme id).
    pub setting: String,
    /// The theme used in dark mode / light mode when `setting` is system.
    pub dark: String,
    pub light: String,
    /// The theme the app is showing right now, as it last reported (None: no GUI yet).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub showing: Option<String>,
    /// How to write a custom theme file.
    pub format: String,
}

/// The terminal colors of the theme the app shows, as #RRGGBB.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalColors {
    /// The theme id these come from.
    pub theme: String,
    pub foreground: String,
    pub background: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// ANSI 0–15.
    pub ansi: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ThemesReportParams {
    pub colors: TerminalColors,
}

// ------------------------------------------------------------------ clock

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct ClockSleepsParams {
    /// Only the sleeps that ended after this (default: every one kept, the last 14 days).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<Timestamp>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct ClockSleepsResult {
    /// When the Mac slept, oldest first.
    pub sleeps: Vec<SleepPeriod>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct SleepPeriod {
    pub from: Timestamp,
    pub to: Timestamp,
    /// `to` - `from`, in seconds.
    pub secs: i64,
}

// ------------------------------------------------------------------ usage

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct UsageGetResult {
    /// Claude's plan usage limits, the latest any Claude terminal's status line reported (they
    /// are account-wide). Absent until one has: API-key accounts never report them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude: Option<AgentUsage>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct AgentUsage {
    /// The rolling 5-hour window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub five_hour: Option<RateLimitWindow>,
    /// The weekly window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seven_day: Option<RateLimitWindow>,
    /// When a status line last reported them.
    pub observed_at: Timestamp,
    /// The terminal whose status line reported them (it may be closed since).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Id>,
    /// A window is at 100% and has not reset yet: new turns will be refused until `limited_until`.
    pub limited: bool,
    /// When the last limiting window resets (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limited_until: Option<Timestamp>,
}

// ------------------------------------------------------------------ keep awake

/// Why the keep-awake assertion is held, or isn't.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum KeepAwakeReason {
    /// Held: there is work (see `work`), or it lingers after it (`keep_awake.linger_mins`).
    Work,
    /// Held: `keep_awake.mode` is always and the hours are open.
    Always,
    /// `keep_awake.enabled` is off.
    Disabled,
    /// Outside today's hours.
    OutsideHours,
    /// Today isn't a keep-awake day.
    DayOff,
    /// The today override turned it off.
    TodayOff,
    /// On battery below `keep_awake.min_battery` (until 5 points above it, or AC).
    BatteryLow,
    /// with_work mode and nothing to do.
    NoWork,
    /// macOS refused the assertion (see `line`); retried every few seconds.
    Failed,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct KeepAwakeSettings {
    /// `keep_awake.enabled`.
    pub enabled: bool,
    /// `keep_awake.mode`: with_work | always.
    pub mode: String,
    /// `keep_awake.start`, HH:MM local.
    pub start: String,
    /// `keep_awake.end`, HH:MM local (at or before start = past midnight; equal = all day).
    pub end: String,
    /// `keep_awake.days`: mon … sun.
    pub days: Vec<String>,
    /// `keep_awake.hours` as day -> `HH:MM-HH:MM` | `off` | `all day`.
    pub hours: std::collections::BTreeMap<String, String>,
    /// `keep_awake.min_battery`, percent (0 = no limit).
    pub min_battery: i64,
    /// `keep_awake.linger_mins`.
    pub linger_mins: i64,
    /// `keep_awake.wake`: wake the Mac for work scheduled inside the hours.
    #[serde(default)]
    pub wake: bool,
}

/// The one-off override for today (ends by itself at local midnight, or at `until` on
/// `until_date` when it's on past midnight).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct KeepAwakeToday {
    /// The local date it applies to, YYYY-MM-DD.
    pub date: String,
    /// false = off for the rest of today; true = on until `until` (then the schedule).
    pub on: bool,
    /// HH:MM local; absent = midnight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    /// YYYY-MM-DD local: the next day, when `until` is past midnight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until_date: Option<String>,
    /// In words: `on until 5 PM today`, `on until 1 AM tomorrow`.
    pub line: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct KeepAwakeBattery {
    pub percent: u8,
    pub on_ac: bool,
    /// Below keep_awake.min_battery (with hysteresis) on battery.
    pub low: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct KeepAwakeStatus {
    /// midnad holds the power assertion right now (the Mac won't idle-sleep).
    pub held: bool,
    pub reason: KeepAwakeReason,
    /// In words: `Keeping awake: 2 agents working` / `Not keeping awake: outside hours (9 AM–6 PM weekdays); on again Mon 9 AM`.
    pub line: String,
    /// The hours (with the today override) are open now.
    pub window_open: bool,
    /// When the hours next open, if they are closed (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_on: Option<Timestamp>,
    /// When the hours next close, if they are open (RFC 3339). Held only while there is work in
    /// with_work mode, so it can end sooner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_off: Option<Timestamp>,
    /// What is keeping it awake (with_work): `2 agents working`, `1 terminal with queued input`.
    pub work: Vec<String>,
    /// When the assertion was taken (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_since: Option<Timestamp>,
    /// None on a Mac without a battery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub battery: Option<KeepAwakeBattery>,
    /// The hours in words: `9 AM–6 PM weekdays; Fri 9 AM–3 PM`.
    pub schedule: String,
    pub settings: KeepAwakeSettings,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub today: Option<KeepAwakeToday>,
    /// Waking the Mac for scheduled work (`keep_awake.wake`).
    #[serde(default)]
    pub wake: KeepAwakeWake,
}

/// The scheduled wake: midnad asks macOS to wake the Mac a little before the next work due
/// inside the hours (a schedule trigger, a message queued for a time, an agent's wakeup).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct KeepAwakeWake {
    /// The one-time admin grant is installed (`keep_awake.wake_setup`), so midnad may schedule wakes.
    pub ready: bool,
    /// `keep_awake.wake_setup` is waiting for the human to answer macOS's password dialog.
    #[serde(default)]
    pub asking: bool,
    /// The wake macOS has scheduled for midna (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<Timestamp>,
    /// The work it wakes for, in words: `Morning kickoff at 7 AM`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// In words: `Waking the Mac tomorrow 6:58 AM for Morning kickoff at 7 AM`.
    pub line: String,
    /// The last scheduling error, until a wake is scheduled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `keep_awake.wake_setup`: install (or, with `remove`, remove) the admin grant that lets midnad
/// schedule wakes. macOS asks for an administrator's password.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct KeepAwakeWakeSetupParams {
    #[serde(default)]
    pub remove: bool,
}

/// Every field is optional; only the ones given change (each is the `keep_awake.*` setting of
/// the same name). All are checked before any is saved.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct KeepAwakeSetParams {
    /// Master switch (`on` works too, like Taskboard's work hours).
    #[serde(default, alias = "on", skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// with_work | always.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// `8am`, `8:30 AM`, `08:00`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,
    /// `weekdays`, `weekends`, `daily`, `mon-fri`, `mon,wed,fri` or an array.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub days: Option<Value>,
    /// Per-day hours, merged into the current ones: `{"fri": "9am-3pm", "sat": "off", "sun": "all day", "mon": null}`
    /// (null or "default" = that day follows start/end/days again). A list of `day = hours` rules
    /// or a string replaces them all ("" clears them).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hours: Option<Value>,
    /// Percent, 0-100 (0 = no limit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_battery: Option<Value>,
    /// Minutes, 0-240.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linger_mins: Option<Value>,
    /// Wake the Mac for work scheduled inside the hours (needs `keep_awake.wake_setup` once).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake: Option<bool>,
    /// Today only: `off` (rest of today), `on` (until midnight), `until 5pm` / `5pm`,
    /// `for 5 hours` / `5h`, `{"on": true, "until": "17:00"}`, `{"on": true, "for": "5h"}`, or
    /// `clear` (back to the schedule). On until a time ends there, even past midnight (`until
    /// 1am`, `for 5 hours` at 8 PM; 24 hours at most), and the schedule takes over; the rest end
    /// at midnight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub today: Option<Value>,
}
