//! Nearby: Midnas on the same network find each other over Bonjour (`peers.list`).
//!
//! Each midna home has a persistent Ed25519 device key. Its fingerprint (SHA-256 of the public
//! key, lowercase hex) is the device's real id; the name is for display only and anyone can
//! claim any name. midnad advertises and browses [`SERVICE`] (Midna Dev and every home other
//! than the installed Midna's use [`DEV_SERVICE`], so they never see production) with a TXT
//! record carrying [`TXT_PROTOCOL`], [`TXT_FINGERPRINT`] and [`TXT_NAME`].
//!
//! Devices pair once (`peers.pair`, then `peers.confirm` on both sides once both screens show
//! the same code) and are kept as a [`Tier`]: a contact or one of my devices. While visibility is
//! `paired`, a device advertises a nameless instance whose TXT carries [`TXT_SALT`] and tags
//! ([`TXT_TAGS`]) only its paired devices can match. Every connection between Midnas is Noise
//! (`Noise_XX_25519_ChaChaPoly_SHA256`) bound to the device keys; unpaired devices may only ask
//! to pair.
use crate::types::{AgentKind, Timestamp};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The installed Midna's Bonjour service type.
pub const SERVICE: &str = "_midna._tcp";
/// Midna Dev's, and any home other than the installed Midna's.
pub const DEV_SERVICE: &str = "_midna-dev._tcp";
/// The Nearby protocol this build speaks. A peer advertising another one is listed but can't
/// be sent to until one side updates.
pub const PROTOCOL: u32 = 1;
/// How long `nearby.visibility` stays `everyone` before going back to `paired`.
pub const EVERYONE_SECS: i64 = 600;

/// TXT record keys.
pub const TXT_PROTOCOL: &str = "v";
pub const TXT_FINGERPRINT: &str = "fp";
pub const TXT_NAME: &str = "name";
/// Paired-only advertisements: a random salt (hex) …
pub const TXT_SALT: &str = "s";
/// … and tags, [`TAG_HEX`] hex digits each, run together (keys `t`, `t2`, `t3`, …), one per
/// paired device: the start of HMAC-SHA256(the pair's presence key, salt ‖ the advertising
/// device's fingerprint), so a device's own tags never match its own entries.
pub const TXT_TAGS: &str = "t";
pub const TAG_HEX: usize = 8;
/// Digits in a pairing code.
pub const CODE_DIGITS: usize = 6;
/// How long a pairing waits for both people to confirm.
pub const PAIRING_SECS: i64 = 120;

/// Who can find this Mac (setting `nearby.visibility`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// Nearby is off: midnad neither advertises nor looks for other Midnas.
    Off,
    /// Only paired devices can find this Mac. midnad still looks for others.
    #[default]
    Paired,
    /// Every Midna on the network can find this Mac, for 10 minutes, then back to `paired`.
    Everyone,
}

impl Visibility {
    pub fn parse(s: &str) -> Option<Visibility> {
        match s {
            "off" => Some(Visibility::Off),
            "paired" => Some(Visibility::Paired),
            "everyone" => Some(Visibility::Everyone),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Visibility::Off => "off",
            Visibility::Paired => "paired",
            Visibility::Everyone => "everyone",
        }
    }
}

/// Is `s` a Bonjour service type midnad may use (`_name._tcp`, name 1–15 of a-z, 0-9, `-`)?
pub fn valid_service(s: &str) -> bool {
    let Some(name) = s.strip_prefix('_').and_then(|r| r.strip_suffix("._tcp")) else { return false };
    (1..=15).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

/// A pairing code for people: `123 456`.
pub fn spaced_code(code: &str) -> String {
    if code.len() == CODE_DIGITS { format!("{} {}", &code[..3], &code[3..]) } else { code.to_string() }
}

/// A device id shortened for people: the first 16 hex digits in groups of four.
pub fn short_fingerprint(fp: &str) -> String {
    let head: Vec<String> = fp.as_bytes().chunks(4).take(4).map(|c| String::from_utf8_lossy(c).into_owned()).collect();
    head.join(" ")
}

/// Build a DNS-SD TXT record from key=value pairs. Values longer than a TXT string allows are cut
/// at a character boundary.
pub fn txt_record(pairs: &[(&str, &str)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (k, v) in pairs {
        let room = 255 - k.len() - 1;
        let mut end = v.len().min(room);
        while !v.is_char_boundary(end) {
            end -= 1;
        }
        let entry = format!("{k}={}", &v[..end]);
        out.push(entry.len() as u8);
        out.extend_from_slice(entry.as_bytes());
    }
    out
}

/// Read a DNS-SD TXT record into key=value pairs (keys lowercased; malformed entries skipped).
pub fn parse_txt(bytes: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let len = bytes[i] as usize;
        let Some(entry) = bytes.get(i + 1..i + 1 + len) else { break };
        i += 1 + len;
        let entry = String::from_utf8_lossy(entry);
        if let Some((k, v)) = entry.split_once('=')
            && !k.is_empty()
        {
            out.push((k.to_ascii_lowercase(), v.to_string()));
        }
    }
    out
}

/// Another Midna seen on the network.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Peer {
    /// SHA-256 of its device public key, lowercase hex: the device's real id. Display names can
    /// repeat or lie; this can't.
    pub fingerprint: String,
    /// The name it advertises (its `nearby.name`, else its computer name). Display only.
    pub name: String,
    /// The Nearby protocol it speaks (see `me.protocol`).
    pub protocol: u32,
    /// Its host name on the network (`Name.local.`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// The port it advertises.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// When this daemon first saw it (since it last appeared).
    pub seen_since: Timestamp,
    /// Paired with this device, as this tier; none = not paired (it may only ask to pair).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<Tier>,
    /// Flagged: it uses the name of a paired device (this fingerprint) but has another key. It
    /// isn't that device, whatever it's called.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claims_name_of: Option<String>,
}

/// How much a paired device is trusted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Someone else's Midna: whatever it sends asks first.
    #[default]
    Contact,
    /// One of my own Macs.
    MyDevice,
}

impl Tier {
    pub fn parse(s: &str) -> Option<Tier> {
        match s.replace('-', "_").as_str() {
            "contact" => Some(Tier::Contact),
            "my_device" | "mine" => Some(Tier::MyDevice),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Contact => "contact",
            Tier::MyDevice => "my_device",
        }
    }
}

/// A device this one paired with (`peers.list` `paired`; event `peers.paired` / `peers.forgotten`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PairedPeer {
    pub fingerprint: String,
    /// Its name when it paired, updated when it shows up under another one. Display only.
    pub name: String,
    pub tier: Tier,
    pub paired_at: Timestamp,
    /// It can be seen on the network now.
    #[serde(default)]
    pub nearby: bool,
    /// What this Mac accepts from it without asking (`peers.set_auto_accept`), all off by
    /// default. Only one of my devices can have any; a contact never does. Even then, Midna
    /// objects that change rules, triggers or settings (and queued prompts) still ask.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub auto_accept: Vec<ShareKind>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PairingRole {
    /// This device asked (`peers.pair`).
    Outgoing,
    /// The other device asked.
    Incoming,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PairingState {
    /// Connecting and agreeing on the code.
    Connecting,
    /// Both sides show `code`; waiting for the people to confirm it matches.
    Comparing,
    /// Paired: both people confirmed.
    Paired,
    /// Someone said no or the codes didn't match (`error` says which side).
    Declined,
    /// Timed out, the connection broke, or the other device broke the protocol (`error`).
    Failed,
}

impl PairingState {
    pub fn is_over(self) -> bool {
        matches!(self, PairingState::Paired | PairingState::Declined | PairingState::Failed)
    }
}

/// A pairing in progress or just ended (`peers.pair`, `peers.confirm`, `peers.list` `pairings`;
/// events `peers.pairing` and `peers.pairing_ended`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Pairing {
    /// `pr_…`; what `peers.confirm` takes.
    pub id: String,
    pub role: PairingRole,
    /// The other device's id (proven by the connection, not just advertised).
    pub fingerprint: String,
    /// The name it gave. Display only.
    pub name: String,
    /// It used the name of a paired device (this fingerprint) but has another key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claims_name_of: Option<String>,
    pub state: PairingState,
    /// The 6-digit code both screens show once state is `comparing`. Confirm only if the other
    /// device shows the same one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// This side's person confirmed the code.
    pub confirmed: bool,
    /// The other side's person confirmed it.
    pub peer_confirmed: bool,
    /// The tier this side keeps the other device as, once confirmed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<Tier>,
    /// The approval asking this side's person to compare the codes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs_you_id: Option<String>,
    pub started_at: Timestamp,
    /// When it gives up waiting for the people.
    pub expires_at: Timestamp,
    /// Why it was declined or failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PeersPairParams {
    /// The device to pair with: its fingerprint, as `peers.list` shows it. It must be visible to
    /// everyone (or already paired).
    pub fingerprint: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PeersConfirmParams {
    /// The pairing (`pr_…`).
    pub pairing: String,
    /// true (default): the code on the other device matches. false: it doesn't, or no thanks;
    /// the pairing ends on both sides.
    #[serde(default = "yes")]
    pub accept: bool,
    /// How to keep the other device (default contact).
    #[serde(default, rename = "as", skip_serializing_if = "Option::is_none")]
    pub tier: Option<Tier>,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PeersForgetParams {
    /// The paired device's fingerprint.
    pub fingerprint: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PeersSetTierParams {
    /// The paired device's fingerprint.
    pub fingerprint: String,
    /// How to keep it from now on.
    #[serde(rename = "as")]
    pub tier: Tier,
}

/// `peers.set_auto_accept` (human only).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PeersSetAutoAcceptParams {
    /// The paired device's fingerprint. It must be kept as my_device.
    pub fingerprint: String,
    /// The kind of share to accept from it without asking.
    pub kind: ShareKind,
    /// true: accept that kind from it without asking. false (default): ask, as for any device.
    #[serde(default)]
    pub on: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PeersForgetResult {
    /// The device that was forgotten.
    pub forgotten: PairedPeer,
}

/// This daemon's side of Nearby.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NearbyMe {
    /// This device's id (SHA-256 of its public key, lowercase hex).
    pub fingerprint: String,
    /// The name others see (setting `nearby.name`, else the Mac's computer name).
    pub name: String,
    /// Setting `nearby.visibility`.
    pub visibility: Visibility,
    /// While visibility is `everyone`: when it goes back to `paired`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub everyone_until: Option<Timestamp>,
    /// The Bonjour service type this daemon uses; none = Nearby is off for this daemon (an
    /// in-process test daemon, or `MIDNA_NEARBY_SERVICE=off`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    /// Advertising itself right now: to everyone, or (visibility `paired`, with at least one
    /// paired device) a nameless instance only paired devices recognise.
    pub advertising: bool,
    /// Looking for other Midnas right now (visibility isn't `off`).
    pub browsing: bool,
    /// The TCP port it takes Nearby connections on, when it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    pub protocol: u32,
    /// Why Bonjour isn't working, when it isn't (midnad retries every few seconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PeersListResult {
    pub me: NearbyMe,
    /// Midnas this daemon can see now, by name. A peer that only shows itself to paired devices
    /// isn't listed until it is paired with this one.
    pub peers: Vec<Peer>,
    /// Every device this one paired with, nearby or not.
    #[serde(default)]
    pub paired: Vec<PairedPeer>,
    /// Pairings in progress.
    #[serde(default)]
    pub pairings: Vec<Pairing>,
}

/// Data of the `peers.left` event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PeerLeft {
    pub fingerprint: String,
    pub name: String,
}

/// Sharing between paired devices (`share.*`, `inbox.list`): the most a message's text may be,
/// in bytes.
pub const MAX_MESSAGE_TEXT: usize = 4096;
/// … a snippet's text.
pub const MAX_SNIPPET: usize = 256 * 1024;
/// … a link's URL.
pub const MAX_URL: usize = 4096;
/// … a title.
pub const MAX_TITLE: usize = 200;
/// Offers from one device that may wait in the inbox at once; more are refused ("inbox full").
pub const MAX_PENDING_FROM: usize = 50;
/// … the files (and folders) in one file share.
pub const MAX_FILES: usize = 4000;
/// … a path inside a file share, in bytes.
pub const MAX_PATH: usize = 1024;
/// … one name (a path component), in bytes.
pub const MAX_NAME: usize = 255;
/// … how deep a folder's tree goes.
pub const MAX_DEPTH: usize = 64;
/// … a tab's scrollback snapshot (its newest lines are kept).
pub const MAX_SCROLLBACK: usize = 256 * 1024;
/// … a tab's directory, or its path inside its repo.
pub const MAX_DIR: usize = 4096;
/// … the name of a tab's shell.
pub const MAX_SHELL: usize = 64;
/// … an agent session's transcript (it is fetched in chunks after accept, like a file).
pub const MAX_TRANSCRIPT: u64 = 256 * 1024 * 1024;
/// … an agent's conversation id.
pub const MAX_CONVERSATION: usize = 128;

/// What a share carries. Nothing in it ever runs: it is data until the receiving person
/// accepts it, and even then a snippet is pasted (never submitted), a link opened or pinned, a
/// tab opened as a fresh shell under the sender's scrollback, and an agent session resumed idle,
/// waiting for its new person's first prompt.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SharePayload {
    /// A note for the other person: shown as a notification they can reply to.
    Message {
        text: String,
        /// The share (`sh_…`) this answers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to: Option<String>,
    },
    /// Text for the inbox, which the receiver can paste into a terminal of their choosing.
    Snippet {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    /// An http(s) link the receiver can open or pin to a terminal's links.
    Link {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    /// A file (an image is one too) or a folder. Only the list travels with the offer; once the
    /// receiver accepts, its midnad fetches the contents in chunks, checks each file's SHA-256 and
    /// saves them under its `nearby.downloads` folder, picking up where it left off when the
    /// connection drops.
    File {
        /// The file's or the folder's name: one path component.
        name: String,
        /// A folder: `files` and `dirs` are paths inside it and keep its tree. Otherwise `files`
        /// is the one file, whose path is `name`.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        folder: bool,
        files: Vec<ShareFile>,
        /// A folder's folders that hold no files (the rest follow from the files' paths).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        dirs: Vec<String>,
    },
    /// A terminal tab: where it was and what it showed, never its processes or environment.
    /// The receiver opens a new idle shell in its own checkout of the same project (or where its
    /// person says), with this scrollback shown read-only above it.
    Tab {
        /// The terminal's name on the sending Mac.
        title: String,
        /// The git repo it was in, when it had a remote. None: `cwd` is all there is.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<TabProject>,
        /// Its directory on the sending Mac (absolute).
        cwd: String,
        /// The shell's program name (`zsh`), when it was a shell. Display only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shell: Option<String>,
        /// Its scrollback and screen as plain text (no escape sequences), the newest
        /// [`MAX_SCROLLBACK`] bytes.
        scrollback: String,
    },
    /// One of Midna's own things: a Summary record, policy rules, triggers, a theme, settings or
    /// queued prompts. The sending midnad builds it from its stores; secrets never travel. The
    /// receiver's approval reviews what accepting would change on its Mac, and only the human's
    /// accept applies it.
    Object { object: MidnaObject },
    /// An agent's conversation (Claude Code or Codex), to carry on on the receiving Mac. Like a
    /// tab it maps to the receiver's checkout of the same project. Only its id and where it was
    /// travel with the offer; once the receiver accepts, its midnad fetches the transcript in
    /// chunks (like a file), rewrites the sending Mac's paths in it to its own, puts it where
    /// the agent looks for it and opens a tab resuming the conversation. Never the agent's model,
    /// permission mode or anything else it ran with: the receiver's own settings apply.
    Agent {
        /// The terminal's name on the sending Mac.
        title: String,
        agent: AgentKind,
        /// The git repo it was in, when it had a remote. None: `cwd` is all there is.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<TabProject>,
        /// The agent's directory on the sending Mac (absolute).
        cwd: String,
        /// The sending person's home folder (absolute): its paths in the transcript become the
        /// receiver's.
        home: String,
        /// The agent's id for the conversation (Claude Code's session id, Codex's thread id).
        conversation: String,
        /// The transcript: `<conversation>.jsonl` for Claude Code; for Codex its path under
        /// `sessions/` (`2026/11/12/rollout-…-<conversation>.jsonl`).
        transcript: ShareFile,
    },
}

/// Which project a tab was in, the same on every Mac that has a checkout of it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TabProject {
    /// The repo's remote (`origin`, else its first), as [`remote_key`] gives it: host and path,
    /// no scheme, user or password (`github.com/acme/widget`).
    pub remote: String,
    /// Where the tab was inside the repo: relative, `/`-separated, empty at its top.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
}

/// A git remote URL as a key two checkouts of one repo share, whichever URL each cloned:
/// `git@github.com:acme/widget.git`, `https://me:token@github.com/acme/widget` and
/// `ssh://git@github.com:22/acme/widget/` all give `github.com/acme/widget`. A credential in the
/// URL never survives. Lowercase. None for an empty URL.
pub fn remote_key(url: &str) -> Option<String> {
    let url = url.trim();
    let (host, path) = match url.split_once("://") {
        Some((scheme, rest)) => {
            let (auth, path) = rest.split_once('/').unwrap_or((rest, ""));
            let host = auth.rsplit_once('@').map_or(auth, |(_, h)| h);
            let host = match host.rsplit_once(':') {
                Some((h, port)) if port.bytes().all(|b| b.is_ascii_digit()) => h,
                _ => host,
            };
            if scheme == "file" { ("", rest) } else { (host, path) }
        }
        // scp-like `user@host:path` (a `:` before any `/`); anything else is a local path.
        None => match url.split_once(':') {
            Some((auth, path)) if !auth.contains('/') && !auth.is_empty() => (auth.rsplit_once('@').map_or(auth, |(_, h)| h), path),
            _ => ("", url),
        },
    };
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path).trim_matches('/');
    let key = if host.is_empty() { format!("/{path}") } else { format!("{host}/{path}") };
    (!key.trim_matches('/').is_empty()).then(|| key.to_lowercase())
}

/// The Midna object kinds a share can carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ObjectType {
    /// One closed terminal's record from a project's Summary (`summary.list`).
    Summary,
    /// Policy rules (`rule.list`).
    Rules,
    /// Triggers (`trigger.list`). They arrive switched off, without a secret.
    Triggers,
    /// A custom theme (`themes.list`).
    Theme,
    /// Settings (`settings.list`); none named sends every changed one that may travel.
    Settings,
    /// Queued prompts (`queue.list`).
    Queue,
}

/// What a [`SharePayload::Object`] carries. Project ids never travel: a project is named, and
/// the receiver maps the name to its own project.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MidnaObject {
    /// A closed terminal's Summary record, without its conversation id or agent arguments.
    Summary {
        /// The sender's project name (`Root` for terminals in no project).
        project: String,
        record: crate::summary::ClosedTerminal,
    },
    Rules { rules: Vec<SharedRule> },
    Triggers { triggers: Vec<SharedTrigger> },
    Theme { theme: SharedTheme },
    /// Setting key -> value.
    Settings { values: std::collections::BTreeMap<String, serde_json::Value> },
    /// Queued prompts, in order (text only).
    Queue { prompts: Vec<SharedPrompt> },
}

/// A policy rule as it travels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SharedRule {
    pub effect: crate::types::Effect,
    pub matcher: crate::types::Matcher,
    /// A project rule's project, by name; none for a global rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// What was left of its time when it was sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in_secs: Option<u64>,
}

/// A trigger as it travels: its definition only (no secret, state, history or hook id). The
/// project ids in `filter.project` and the action's `project_id` are project names instead.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SharedTrigger {
    pub name: String,
    pub source: crate::types::TriggerSource,
    pub event: String,
    #[serde(default)]
    pub filter: crate::types::TriggerFilter,
    pub action: crate::types::TriggerAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_name_template: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_secs: Option<u64>,
}

/// A custom theme, every color spelled out (it extends nothing).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SharedTheme {
    /// Its id, which names its file (`themes/<id>.json`).
    pub id: String,
    pub name: String,
    pub dark: bool,
    /// UI token -> `#RRGGBB` ([`crate::themes::UI_TOKENS`]).
    pub colors: std::collections::BTreeMap<String, String>,
    /// The 16 terminal colors, `#RRGGBB`.
    pub ansi: Vec<String>,
}

/// A queued prompt as it travels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SharedPrompt {
    pub text: String,
    /// Press Enter after it.
    pub enter: bool,
}

/// The most an object share may hold, serialized.
pub const MAX_OBJECT: usize = 256 * 1024;
/// … rules, triggers, settings or prompts in one object share.
pub const MAX_OBJECT_ITEMS: usize = 200;

impl ObjectType {
    pub fn word(self) -> &'static str {
        match self {
            ObjectType::Summary => "summary",
            ObjectType::Rules => "rules",
            ObjectType::Triggers => "triggers",
            ObjectType::Theme => "theme",
            ObjectType::Settings => "settings",
            ObjectType::Queue => "queue",
        }
    }
}

impl MidnaObject {
    pub fn object_type(&self) -> ObjectType {
        match self {
            MidnaObject::Summary { .. } => ObjectType::Summary,
            MidnaObject::Rules { .. } => ObjectType::Rules,
            MidnaObject::Triggers { .. } => ObjectType::Triggers,
            MidnaObject::Theme { .. } => ObjectType::Theme,
            MidnaObject::Settings { .. } => ObjectType::Settings,
            MidnaObject::Queue { .. } => ObjectType::Queue,
        }
    }

    /// What it is, for people: `3 rules`, `the theme “My Night”`, `1 queued prompt`.
    pub fn label(&self) -> String {
        let n = |k: usize, one: &str, many: &str| format!("{k} {}", if k == 1 { one } else { many });
        match self {
            MidnaObject::Summary { record, .. } => format!("the Summary of “{}”", record.name),
            MidnaObject::Rules { rules } => n(rules.len(), "rule", "rules"),
            MidnaObject::Triggers { triggers } => n(triggers.len(), "trigger", "triggers"),
            MidnaObject::Theme { theme } => format!("the theme “{}”", theme.name),
            MidnaObject::Settings { values } => n(values.len(), "setting", "settings"),
            MidnaObject::Queue { prompts } => n(prompts.len(), "queued prompt", "queued prompts"),
        }
    }

    fn check(&self) -> Result<(), String> {
        let count = |n: usize, what: &str| match n {
            0 => Err(format!("no {what} to send")),
            n if n > MAX_OBJECT_ITEMS => Err(format!("at most {MAX_OBJECT_ITEMS} {what} per share")),
            _ => Ok(()),
        };
        match self {
            MidnaObject::Summary { project, record } => {
                if project.trim().is_empty() || project.len() > MAX_TITLE {
                    return Err("a Summary record needs its project's name".into());
                }
                if record.name.len() > MAX_TITLE * 4 || record.id.is_empty() {
                    return Err("a Summary record needs an id and a name".into());
                }
            }
            MidnaObject::Rules { rules } => {
                count(rules.len(), "rules")?;
                if rules.iter().any(|r| r.matcher.pattern.trim().is_empty()) {
                    return Err("a rule's pattern is empty".into());
                }
            }
            MidnaObject::Triggers { triggers } => {
                count(triggers.len(), "triggers")?;
                if triggers.iter().any(|t| t.name.trim().is_empty()) {
                    return Err("a trigger has no name".into());
                }
            }
            MidnaObject::Theme { theme } => {
                let hex = |c: &String| crate::themes::parse_hex(c).is_some();
                let builtin = crate::themes::BUILTIN_IDS.contains(&theme.id.as_str());
                if !crate::themes::valid_id(&theme.id) || builtin || theme.name.trim().is_empty() || theme.name.len() > MAX_TITLE {
                    return Err("a theme needs an id (a-z 0-9 - _) that isn't a built-in one, and a name".into());
                }
                let tokens = crate::themes::UI_TOKENS.iter().all(|t| theme.colors.get(*t).is_some_and(hex));
                if !tokens || theme.colors.len() != crate::themes::UI_TOKENS.len() || theme.ansi.len() != 16 || !theme.ansi.iter().all(hex) {
                    return Err("a theme carries every UI color and 16 terminal colors, each #RRGGBB".into());
                }
            }
            MidnaObject::Settings { values } => {
                count(values.len(), "settings")?;
                if values.keys().any(|k| k.is_empty() || k.len() > MAX_TITLE) {
                    return Err("a setting's key is empty or too long".into());
                }
            }
            MidnaObject::Queue { prompts } => {
                count(prompts.len(), "queued prompts")?;
                if prompts.iter().any(|p| p.text.is_empty()) {
                    return Err("a queued prompt is empty".into());
                }
            }
        }
        let size = serde_json::to_vec(self).map(|b| b.len()).unwrap_or(usize::MAX);
        if size > MAX_OBJECT {
            return Err(format!("an object share may be at most {} KiB; send fewer", MAX_OBJECT / 1024));
        }
        Ok(())
    }
}

fn plain(s: &str) -> bool {
    !s.chars().any(|c| c.is_control())
}

/// One file in a file share.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ShareFile {
    /// Its path inside the share: relative, `/`-separated, no `.` or `..` parts.
    pub path: String,
    pub size: u64,
    /// SHA-256 of the whole file, lowercase hex.
    pub sha256: String,
}

/// Is `s` a name a share may use for a file or folder: not empty, `.` or `..`, no `/`, NUL or
/// other control character, at most [`MAX_NAME`] bytes?
pub fn valid_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_NAME && s != "." && s != ".." && !s.chars().any(|c| c == '/' || c.is_control())
}

/// Is `p` a path a share may use inside its folder: relative, `/`-separated [`valid_name`]s, at
/// most [`MAX_PATH`] bytes and [`MAX_DEPTH`] deep? Nothing like it can point outside the folder.
pub fn valid_path(p: &str) -> bool {
    p.len() <= MAX_PATH && p.split('/').count() <= MAX_DEPTH && p.split('/').all(valid_name)
}

/// `name` made safe to use as one file name, whatever it is: `/`, `:` and control characters
/// become `-`, an empty name, `.` or `..` becomes `untitled`, and it is cut to [`MAX_NAME`] bytes.
pub fn safe_name(name: &str) -> String {
    let mut out: String = name.chars().map(|c| if c == '/' || c == ':' || c.is_control() { '-' } else { c }).collect();
    let mut end = out.len().min(MAX_NAME);
    while !out.is_char_boundary(end) {
        end -= 1;
    }
    out.truncate(end);
    if out.trim().is_empty() || out == "." || out == ".." { "untitled".into() } else { out }
}

/// Bytes for people: `512 B`, `4.2 KB`, `31.0 MB`, `2.4 GB`.
pub fn size_label(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64 / 1024.;
    let mut unit = 0;
    while v >= 1024. && unit < UNITS.len() - 1 {
        v /= 1024.;
        unit += 1;
    }
    format!("{v:.1} {}", UNITS[unit])
}

fn valid_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A file share's list, well formed: valid names and paths, no path twice (ignoring case, as
/// macOS does), no file where another entry needs a folder, and sizes that add up.
fn check_files(name: &str, folder: bool, files: &[ShareFile], dirs: &[String]) -> Result<(), String> {
    use std::collections::HashSet;
    if !valid_name(name) {
        return Err("a shared file's name must be one plain name (no / or control characters, not . or ..)".into());
    }
    if files.len() + dirs.len() > MAX_FILES {
        return Err(format!("a share may hold at most {MAX_FILES} files and folders; zip the folder"));
    }
    for f in files {
        if !valid_sha256(&f.sha256) {
            return Err(format!("{}: the checksum isn't a SHA-256", f.path));
        }
    }
    if files.iter().try_fold(0u64, |n, f| n.checked_add(f.size)).is_none() {
        return Err("the sizes don't add up".into());
    }
    if !folder {
        return match files {
            [one] if one.path == name && dirs.is_empty() => Ok(()),
            _ => Err("a single file's share holds that one file, by its name".into()),
        };
    }
    let mut seen = HashSet::new();
    let mut needed_dirs = HashSet::new();
    for p in files.iter().map(|f| &f.path).chain(dirs) {
        if !valid_path(p) {
            return Err(format!("{p:?} isn't a path inside the folder"));
        }
        if !seen.insert(p.to_lowercase()) {
            return Err(format!("{p} is in the share twice"));
        }
        let parts: Vec<&str> = p.split('/').collect();
        for n in 1..parts.len() {
            needed_dirs.insert(parts[..n].join("/").to_lowercase());
        }
    }
    if let Some(f) = files.iter().find(|f| needed_dirs.contains(&f.path.to_lowercase())) {
        return Err(format!("{} is a file and a folder", f.path));
    }
    Ok(())
}

impl SharePayload {
    pub fn kind(&self) -> ShareKind {
        match self {
            SharePayload::Message { .. } => ShareKind::Message,
            SharePayload::Snippet { .. } => ShareKind::Snippet,
            SharePayload::Link { .. } => ShareKind::Link,
            SharePayload::File { .. } => ShareKind::File,
            SharePayload::Tab { .. } => ShareKind::Tab,
            SharePayload::Object { .. } => ShareKind::Object,
            SharePayload::Agent { .. } => ShareKind::Agent,
        }
    }

    /// The files fetched after accept: a file share's, or an agent session's transcript.
    pub fn files(&self) -> &[ShareFile] {
        match self {
            SharePayload::File { files, .. } => files,
            SharePayload::Agent { transcript, .. } => std::slice::from_ref(transcript),
            _ => &[],
        }
    }

    /// Its size in bytes (the text, the URL, every file's contents, the scrollback or the
    /// transcript).
    pub fn size(&self) -> u64 {
        match self {
            SharePayload::Message { text, .. } | SharePayload::Snippet { text, .. } => text.len() as u64,
            SharePayload::Link { url, .. } => url.len() as u64,
            SharePayload::File { files, .. } => files.iter().fold(0u64, |n, f| n.saturating_add(f.size)),
            SharePayload::Tab { scrollback, .. } => scrollback.len() as u64,
            SharePayload::Object { object } => serde_json::to_vec(object).map(|b| b.len() as u64).unwrap_or(0),
            SharePayload::Agent { transcript, .. } => transcript.size,
        }
    }

    /// Within the limits and well formed (both the sender and the receiver check).
    pub fn check(&self) -> Result<(), String> {
        let title_ok = |t: &Option<String>| t.as_ref().is_none_or(|t| t.len() <= MAX_TITLE);
        match self {
            SharePayload::Message { text, reply_to } => {
                if text.trim().is_empty() {
                    return Err("the message is empty".into());
                }
                if text.len() > MAX_MESSAGE_TEXT {
                    return Err(format!("a message may be at most {MAX_MESSAGE_TEXT} bytes; send longer text as a snippet"));
                }
                if reply_to.as_deref().is_some_and(|r| !valid_share_id(r)) {
                    return Err("reply_to isn't a share id (sh_…)".into());
                }
            }
            SharePayload::Snippet { text, title } => {
                if text.is_empty() {
                    return Err("the snippet is empty".into());
                }
                if text.len() > MAX_SNIPPET {
                    return Err(format!("a snippet may be at most {} KiB", MAX_SNIPPET / 1024));
                }
                if !title_ok(title) {
                    return Err(format!("a title may be at most {MAX_TITLE} bytes"));
                }
            }
            SharePayload::Link { url, title } => {
                let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"));
                if rest.is_none_or(|r| r.is_empty()) || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
                    return Err("a link must be an http(s) URL".into());
                }
                if url.len() > MAX_URL {
                    return Err(format!("a link may be at most {MAX_URL} bytes"));
                }
                if !title_ok(title) {
                    return Err(format!("a title may be at most {MAX_TITLE} bytes"));
                }
            }
            SharePayload::File { name, folder, files, dirs } => check_files(name, *folder, files, dirs)?,
            SharePayload::Tab { title, project, cwd, shell, scrollback } => {
                if title.trim().is_empty() || title.len() > MAX_TITLE || !plain(title) {
                    return Err(format!("a tab's title is one line of at most {MAX_TITLE} bytes"));
                }
                if !cwd.starts_with('/') || cwd.len() > MAX_DIR || !plain(cwd) {
                    return Err("a tab's directory must be an absolute path".into());
                }
                if shell.as_ref().is_some_and(|s| s.is_empty() || s.len() > MAX_SHELL || s.contains('/') || !plain(s)) {
                    return Err("a tab's shell is a program name".into());
                }
                check_project(project.as_ref())?;
                if scrollback.len() > MAX_SCROLLBACK {
                    return Err(format!("a tab's scrollback may be at most {} KiB", MAX_SCROLLBACK / 1024));
                }
                if scrollback.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
                    return Err("a tab's scrollback is plain text".into());
                }
            }
            SharePayload::Object { object } => object.check()?,
            SharePayload::Agent { title, agent, project, cwd, home, conversation, transcript } => {
                if title.trim().is_empty() || title.len() > MAX_TITLE || !plain(title) {
                    return Err(format!("an agent session's title is one line of at most {MAX_TITLE} bytes"));
                }
                for (what, dir) in [("directory", cwd), ("home", home)] {
                    if !dir.starts_with('/') || dir.len() > MAX_DIR || !plain(dir) {
                        return Err(format!("an agent session's {what} must be an absolute path"));
                    }
                }
                check_project(project.as_ref())?;
                if !valid_conversation(conversation) {
                    return Err("an agent's conversation id is letters, digits, - and _".into());
                }
                if !valid_sha256(&transcript.sha256) {
                    return Err("the transcript's sha256 must be 64 lowercase hex digits".into());
                }
                if transcript.size == 0 || transcript.size > MAX_TRANSCRIPT {
                    return Err(format!("a transcript is 1 byte to {} MiB", MAX_TRANSCRIPT / 1024 / 1024));
                }
                let file = format!("{conversation}.jsonl");
                let ok = match agent {
                    AgentKind::Claude => transcript.path == file,
                    AgentKind::Codex => {
                        let last = transcript.path.rsplit('/').next().unwrap_or_default();
                        valid_path(&transcript.path) && transcript.path.split('/').count() <= 4 && last.starts_with("rollout-") && last.ends_with(&format!("-{file}"))
                    }
                };
                if !ok {
                    return Err(format!("{} isn't where {} keeps conversation {conversation}", transcript.path, agent.as_str()));
                }
            }
        }
        Ok(())
    }
}

/// A tab's or an agent session's project: its remote, and a relative path inside it.
fn check_project(project: Option<&TabProject>) -> Result<(), String> {
    if let Some(TabProject { remote, path }) = project {
        if remote.is_empty() || remote.len() > MAX_URL || remote.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err("a tab's project needs its remote".into());
        }
        let bad_part = |p: &str| p.is_empty() || p == "." || p == "..";
        if path.len() > MAX_DIR || !plain(path) || (!path.is_empty() && path.split('/').any(bad_part)) {
            return Err("a tab's path in its repo must be relative, without . or .. parts".into());
        }
    }
    Ok(())
}

/// An agent's conversation id: 1 to [`MAX_CONVERSATION`] letters, digits, `-` and `_` (what
/// Claude Code's session ids and Codex's thread ids are). It names the transcript's file, so
/// nothing else may be in it.
pub fn valid_conversation(id: &str) -> bool {
    (1..=MAX_CONVERSATION).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A share id: `sh_` and 4–32 letters or digits (the sender picks it; both sides use it).
pub fn valid_share_id(id: &str) -> bool {
    id.strip_prefix("sh_").is_some_and(|r| (4..=32).contains(&r.len()) && r.bytes().all(|b| b.is_ascii_alphanumeric()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShareKind {
    Message,
    Snippet,
    Link,
    /// A file, an image or a folder.
    File,
    /// A terminal tab (`session`): its project, directory and scrollback.
    Tab,
    /// One of Midna's own things ([`ObjectType`]).
    Object,
    /// An agent's terminal (`session`): its conversation, resumed on the receiving Mac.
    Agent,
}

impl ShareKind {
    /// The kinds a device can auto-accept (`peers.set_auto_accept`). An agent session always
    /// asks: accepting it starts an agent here.
    pub const ALL: [ShareKind; 6] = [ShareKind::Message, ShareKind::Snippet, ShareKind::Link, ShareKind::File, ShareKind::Tab, ShareKind::Object];

    pub fn as_str(self) -> &'static str {
        match self {
            ShareKind::Message => "message",
            ShareKind::Snippet => "snippet",
            ShareKind::Link => "link",
            ShareKind::File => "file",
            ShareKind::Tab => "tab",
            ShareKind::Object => "object",
            ShareKind::Agent => "agent",
        }
    }

    /// A kind by name; plurals (`links`) too.
    pub fn parse(s: &str) -> Option<ShareKind> {
        let s = s.trim().to_lowercase();
        let s = s.strip_suffix('s').unwrap_or(&s);
        ShareKind::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShareDirection {
    /// Another device sent it to this one.
    Incoming,
    /// This device sent it.
    Outgoing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShareState {
    /// Waiting for the receiving person.
    Offered,
    /// The receiving person accepted it.
    Accepted,
    /// The receiving person declined it.
    Declined,
}

/// One share, in either direction (`inbox.list`, `share.*`; events `share.*`). Both devices
/// keep it under the same id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ShareItem {
    /// `sh_…`.
    pub id: String,
    pub direction: ShareDirection,
    /// The other device's fingerprint.
    pub peer: String,
    /// Its name (from the paired devices). Display only.
    pub peer_name: String,
    /// The sending device's fingerprint (for an incoming share, proven by the connection).
    pub from: String,
    /// Bytes of text or URL.
    pub size: u64,
    pub payload: SharePayload,
    pub state: ShareState,
    pub sent_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<Timestamp>,
    /// What accepting did on this Mac: `pasted into t_…`, `opened`, `pinned to t_…`, `read`,
    /// `opened as t_… in <dir>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    /// Incoming and still offered: the needs-you approval that asks this Mac's person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs_you_id: Option<String>,
    /// A file share once accepted: how far its contents got.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transfer: Option<Transfer>,
    /// Incoming and accepted without asking: it came from one of my devices whose kind this
    /// Mac's person set to auto-accept (`peers.set_auto_accept`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TransferState {
    /// Accepted; waiting its turn, or for the other device to be reachable again (`error` says
    /// what went wrong last). It resumes where it stopped.
    Waiting,
    /// The contents are coming in (incoming) or being fetched (outgoing).
    Moving,
    /// Every file arrived with the right checksum and was saved (`saved_to`).
    Done,
    /// Given up (`error`); accepting it again starts over.
    Failed,
}

/// How far a file share's contents got (event `share.progress`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Transfer {
    pub state: TransferState,
    /// Bytes that arrived (incoming) or were sent (outgoing) so far.
    pub done: u64,
    pub total: u64,
    /// Files finished, of `files`.
    pub files_done: usize,
    pub files: usize,
    /// How many times it picked up part way through a file after the connection dropped.
    #[serde(default)]
    pub resumed: u32,
    /// Incoming and done: where it was saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Transfer {
    pub fn new(total: u64, files: usize) -> Transfer {
        Transfer { state: TransferState::Waiting, done: 0, total, files_done: 0, files, resumed: 0, saved_to: None, error: None }
    }

    pub fn is_over(&self) -> bool {
        matches!(self.state, TransferState::Done | TransferState::Failed)
    }
}

/// `share.send`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ShareSendParams {
    /// The paired device: its fingerprint (`peers.list` `paired`). It must be on the network now.
    pub peer: String,
    pub kind: ShareKind,
    /// A message's or a snippet's text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// A link's http(s) URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// A snippet's or a link's title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// A message only: the share (`sh_…`) it answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    /// A file: the absolute path of the file or folder to send. midnad reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// A tab or an agent session only: the terminal to send (default the caller's). midnad reads
    /// its project and directory, and a tab's scrollback or an agent's conversation; `title`
    /// names it instead of the terminal's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// An object: which kind. midnad reads it from its stores.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object: Option<ObjectType>,
    /// An object: what to send. One Summary record id (`summary.list`) or theme id; rule ids
    /// (r_…), trigger ids (t_…) or queued prompt ids (q_…); setting keys (none: every changed
    /// setting that may travel).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ids: Vec<String>,
}

impl ShareSendParams {
    /// The payload these params describe, checked.
    pub fn payload(&self) -> Result<SharePayload, String> {
        let text = || self.text.clone().ok_or_else(|| format!("a {} needs `text`", serde_json::to_value(self.kind).unwrap_or_default().as_str().unwrap_or_default()));
        let title = self.title.as_deref().map(str::trim).filter(|t| !t.is_empty()).map(str::to_string);
        if self.reply_to.is_some() && self.kind != ShareKind::Message {
            return Err("only a message can reply_to".into());
        }
        if self.url.is_some() && self.kind != ShareKind::Link {
            return Err("only a link has a `url`".into());
        }
        if self.path.is_some() != (self.kind == ShareKind::File) {
            return Err(if self.path.is_some() { "only a file has a `path`".into() } else { "a file needs `path`".into() });
        }
        if self.session.is_some() && !matches!(self.kind, ShareKind::Tab | ShareKind::Agent) {
            return Err("only a tab or an agent session takes a `session`".into());
        }
        if self.object.is_some() != (self.kind == ShareKind::Object) {
            return Err(if self.object.is_some() { "only an object share names an `object`".into() } else { "an object share needs `object`".into() });
        }
        if !self.ids.is_empty() && self.kind != ShareKind::Object {
            return Err("only an object share takes `ids`".into());
        }
        let p = match self.kind {
            ShareKind::Message => {
                if title.is_some() {
                    return Err("a message has no title".into());
                }
                SharePayload::Message { text: text()?, reply_to: self.reply_to.clone() }
            }
            ShareKind::Snippet => SharePayload::Snippet { text: text()?, title },
            ShareKind::Link => {
                if self.text.is_some() {
                    return Err("a link has a `url`, not `text`".into());
                }
                let url = self.url.as_deref().map(str::trim).ok_or("a link needs `url`")?.to_string();
                SharePayload::Link { url, title }
            }
            ShareKind::File => return Err("a file share is read from `path` by midnad".into()),
            ShareKind::Tab => return Err("a tab is read from its terminal by midnad".into()),
            ShareKind::Object => return Err("an object share is read from midnad's stores".into()),
            ShareKind::Agent => return Err("an agent session is read from its terminal by midnad".into()),
        };
        p.check()?;
        Ok(p)
    }
}

/// What accepting a link does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShareLinkAction {
    /// Open it in the default browser.
    Open,
    /// Pin it to a terminal's links (`session`, else the caller's terminal).
    Pin,
}

/// `share.accept` (human only).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ShareAcceptParams {
    /// The incoming share (`sh_…`).
    pub id: String,
    /// A snippet: paste it into this terminal (never followed by Enter). A link with action
    /// pin: pin it to this terminal (default the caller's). Without it a snippet is kept in the
    /// inbox to paste later (accepting an accepted snippet again pastes it again).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// A link: open it (default) or pin it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<ShareLinkAction>,
    /// A tab or an agent session: open it in this folder on this Mac instead of where its
    /// project maps to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// `share.decline` (human only).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ShareDeclineParams {
    /// The incoming share (`sh_…`).
    pub id: String,
}

/// `inbox.list`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InboxListParams {
    /// Only incoming or only outgoing shares (default both).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<ShareDirection>,
    /// Only shares in this state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<ShareState>,
    /// Only shares with this device (fingerprint).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<String>,
    /// At most this many, newest first (default 100).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InboxListResult {
    /// Newest first.
    pub items: Vec<ShareItem>,
    /// Incoming shares still waiting for this Mac's person.
    pub waiting: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_payloads() {
        let send = |v: serde_json::Value| serde_json::from_value::<ShareSendParams>(v).unwrap().payload();
        let p = send(serde_json::json!({ "peer": "f", "kind": "snippet", "text": "ls\n", "title": " " })).unwrap();
        assert_eq!(p, SharePayload::Snippet { text: "ls\n".into(), title: None });
        assert_eq!((p.kind(), p.size()), (ShareKind::Snippet, 3));
        assert_eq!(serde_json::to_value(&p).unwrap(), serde_json::json!({ "kind": "snippet", "text": "ls\n" }));
        assert!(send(serde_json::json!({ "peer": "f", "kind": "link", "url": "https://x.dev/a" })).is_ok());
        for bad in [
            serde_json::json!({ "peer": "f", "kind": "link", "url": "file:///etc/passwd" }),
            serde_json::json!({ "peer": "f", "kind": "link", "url": "https://" }),
            serde_json::json!({ "peer": "f", "kind": "link", "url": "https://a b" }),
            serde_json::json!({ "peer": "f", "kind": "link", "text": "https://x.dev" }),
            serde_json::json!({ "peer": "f", "kind": "message", "text": "  " }),
            serde_json::json!({ "peer": "f", "kind": "message", "text": "x".repeat(MAX_MESSAGE_TEXT + 1) }),
            serde_json::json!({ "peer": "f", "kind": "message", "text": "hi", "reply_to": "nope" }),
            serde_json::json!({ "peer": "f", "kind": "snippet", "text": "hi", "reply_to": "sh_abcd" }),
            serde_json::json!({ "peer": "f", "kind": "snippet" }),
        ] {
            assert!(send(bad.clone()).is_err(), "{bad}");
        }
        assert!(valid_share_id("sh_0a1b2c3d") && !valid_share_id("sh_") && !valid_share_id("sh_ab/cd") && !valid_share_id("pr_abcd"));
    }

    #[test]
    fn remote_keys() {
        for u in [
            "git@github.com:Acme/widget.git",
            "https://github.com/acme/widget",
            "https://me:s3cret@github.com/acme/widget.git/",
            "ssh://git@github.com:22/acme/widget",
            "github.com:acme/widget",
        ] {
            assert_eq!(remote_key(u).as_deref(), Some("github.com/acme/widget"), "{u}");
        }
        assert_eq!(remote_key("/srv/git/widget.git").as_deref(), Some("/srv/git/widget"));
        assert_eq!(remote_key("file:///srv/git/widget").as_deref(), Some("/srv/git/widget"));
        assert_eq!(remote_key("  "), None);
    }

    #[test]
    fn tab_payloads() {
        let tab = |v: serde_json::Value| serde_json::from_value::<SharePayload>(v).unwrap().check();
        let good = serde_json::json!({ "kind": "tab", "title": "api", "project": { "remote": "github.com/a/b", "path": "src/x" }, "cwd": "/w/b/src/x", "shell": "zsh", "scrollback": "$ ls\n\tfile" });
        assert_eq!(tab(good.clone()), Ok(()));
        for (k, v) in [
            ("title", serde_json::json!(" ")),
            ("cwd", serde_json::json!("relative")),
            ("shell", serde_json::json!("/bin/zsh")),
            ("project", serde_json::json!({ "remote": "github.com/a/b", "path": "../etc" })),
            ("project", serde_json::json!({ "remote": "github.com/a/b", "path": "/etc" })),
            ("project", serde_json::json!({ "remote": "", "path": "" })),
            ("scrollback", serde_json::json!("ok\u{1b}]52;c;aGk=\u{7}")),
            ("scrollback", serde_json::json!("x".repeat(MAX_SCROLLBACK + 1))),
        ] {
            let mut bad = good.clone();
            bad[k] = v;
            assert!(tab(bad.clone()).is_err(), "{bad}");
        }
        let p: ShareSendParams = serde_json::from_value(serde_json::json!({ "peer": "f", "kind": "snippet", "text": "x", "session": "t1" })).unwrap();
        assert!(p.payload().is_err(), "only a tab names a terminal");
    }

    #[test]
    fn object_shares() {
        let send = |v: serde_json::Value| serde_json::from_value::<ShareSendParams>(v).unwrap().payload();
        assert!(send(serde_json::json!({ "peer": "f", "kind": "object", "object": "rules", "ids": ["r_1"] })).unwrap_err().contains("midnad's stores"));
        for bad in [
            serde_json::json!({ "peer": "f", "kind": "object" }),
            serde_json::json!({ "peer": "f", "kind": "message", "text": "hi", "object": "rules" }),
            serde_json::json!({ "peer": "f", "kind": "message", "text": "hi", "ids": ["r_1"] }),
        ] {
            assert!(!send(bad.clone()).unwrap_err().contains("midnad's stores"), "{bad}");
        }

        let object = |v: serde_json::Value| SharePayload::Object { object: serde_json::from_value(v).unwrap() };
        let rules = object(serde_json::json!({ "type": "rules", "rules": [{ "effect": "allow", "matcher": { "kind": "cli", "pattern": "git push*" }, "project": "web" }] }));
        assert_eq!((rules.kind(), rules.check()), (ShareKind::Object, Ok(())));
        let SharePayload::Object { object: o } = &rules else { unreachable!() };
        assert_eq!((o.object_type(), o.label().as_str()), (ObjectType::Rules, "1 rule"));
        let wire = serde_json::to_value(&rules).unwrap();
        assert_eq!((wire["kind"].as_str(), wire["object"]["type"].as_str()), (Some("object"), Some("rules")), "{wire}");
        assert_eq!(serde_json::from_value::<SharePayload>(wire).unwrap(), rules);

        let ui: serde_json::Map<String, serde_json::Value> = crate::themes::UI_TOKENS.iter().map(|t| (t.to_string(), serde_json::json!("#112233"))).collect();
        let theme = |id: &str, ansi: usize| object(serde_json::json!({ "type": "theme", "theme": { "id": id, "name": "Mine", "dark": true, "colors": ui, "ansi": vec!["#000000"; ansi] } }));
        assert_eq!(theme("mine", 16).check(), Ok(()));
        for bad in [
            theme("twilight", 16),
            theme("../mine", 16),
            theme("mine", 15),
            object(serde_json::json!({ "type": "rules", "rules": [] })),
            object(serde_json::json!({ "type": "rules", "rules": [{ "effect": "allow", "matcher": { "kind": "cli", "pattern": " " } }] })),
            object(serde_json::json!({ "type": "settings", "values": { "": 1 } })),
            object(serde_json::json!({ "type": "queue", "prompts": [{ "text": "", "enter": true }] })),
            object(serde_json::json!({ "type": "queue", "prompts": vec![serde_json::json!({ "text": "x", "enter": true }); MAX_OBJECT_ITEMS + 1] })),
            object(serde_json::json!({ "type": "queue", "prompts": [{ "text": "x".repeat(MAX_OBJECT), "enter": true }] })),
        ] {
            assert!(bad.check().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn agent_payloads() {
        let check = |v: serde_json::Value| serde_json::from_value::<SharePayload>(v).unwrap().check();
        let sha = "a".repeat(64);
        let conv = "4f1c2b9e-0d3a-4c51-9a77-3e1f0b2c8d40";
        let claude = serde_json::json!({ "kind": "agent", "title": "api", "agent": "claude", "project": { "remote": "github.com/a/b", "path": "src" }, "cwd": "/w/b/src", "home": "/Users/me", "conversation": conv, "transcript": { "path": format!("{conv}.jsonl"), "size": 10, "sha256": sha } });
        assert_eq!(check(claude.clone()), Ok(()));
        let p: SharePayload = serde_json::from_value(claude.clone()).unwrap();
        assert_eq!((p.kind(), p.size(), p.files().len()), (ShareKind::Agent, 10, 1));
        let mut codex = claude.clone();
        codex["agent"] = "codex".into();
        codex["transcript"]["path"] = format!("2026/11/12/rollout-2026-11-12T10-00-00-{conv}.jsonl").into();
        assert_eq!(check(codex.clone()), Ok(()));
        for (base, k, v) in [
            (&claude, "title", serde_json::json!("two\nlines")),
            (&claude, "cwd", serde_json::json!("relative")),
            (&claude, "home", serde_json::json!("")),
            (&claude, "conversation", serde_json::json!("../../etc/x")),
            (&claude, "conversation", serde_json::json!("")),
            (&claude, "project", serde_json::json!({ "remote": "github.com/a/b", "path": "../x" })),
            (&claude, "transcript", serde_json::json!({ "path": "other.jsonl", "size": 10, "sha256": sha })),
            (&claude, "transcript", serde_json::json!({ "path": format!("../{conv}.jsonl"), "size": 10, "sha256": sha })),
            (&claude, "transcript", serde_json::json!({ "path": format!("{conv}.jsonl"), "size": 0, "sha256": sha })),
            (&claude, "transcript", serde_json::json!({ "path": format!("{conv}.jsonl"), "size": MAX_TRANSCRIPT + 1, "sha256": sha })),
            (&claude, "transcript", serde_json::json!({ "path": format!("{conv}.jsonl"), "size": 10, "sha256": "x" })),
            (&codex, "transcript", serde_json::json!({ "path": format!("2026/11/12/{conv}.jsonl"), "size": 10, "sha256": sha })),
            (&codex, "transcript", serde_json::json!({ "path": format!("../12/rollout-x-{conv}.jsonl"), "size": 10, "sha256": sha })),
            (&codex, "transcript", serde_json::json!({ "path": format!("/abs/rollout-x-{conv}.jsonl"), "size": 10, "sha256": sha })),
            (&codex, "transcript", serde_json::json!({ "path": format!("a/b/c/d/rollout-x-{conv}.jsonl"), "size": 10, "sha256": sha })),
        ] {
            let mut bad = base.clone();
            bad[k] = v;
            assert!(check(bad.clone()).is_err(), "{bad}");
        }
        let p: ShareSendParams = serde_json::from_value(serde_json::json!({ "peer": "f", "kind": "agent", "session": "t1" })).unwrap();
        assert!(p.payload().is_err(), "midnad reads an agent session itself");
    }

    #[test]
    fn service_types() {
        for ok in [SERVICE, DEV_SERVICE, "_midna-t0a1b2c._tcp", "_a._tcp"] {
            assert!(valid_service(ok), "{ok}");
        }
        for bad in ["midna._tcp", "_midna._udp", "_._tcp", "_Midna._tcp", "_-midna._tcp", "_midna-._tcp", "_a-very-long-name1._tcp", "_mid na._tcp"] {
            assert!(!valid_service(bad), "{bad}");
        }
    }

    #[test]
    fn txt_round_trips_and_cuts_long_values() {
        let long = "é".repeat(200);
        let rec = txt_record(&[(TXT_PROTOCOL, "1"), (TXT_FINGERPRINT, "abcd"), (TXT_NAME, &long)]);
        let back = parse_txt(&rec);
        assert_eq!(back[0], ("v".into(), "1".into()));
        assert_eq!(back[1], ("fp".into(), "abcd".into()));
        assert!(back[2].1.len() <= 250 && long.starts_with(&back[2].1), "cut at a char boundary");
        assert_eq!(parse_txt(&[5, b'x', b'=', b'1']), vec![], "a truncated entry is dropped");
        assert_eq!(parse_txt(&[3, b'=', b'1', b'2', 3, b'K', b'=', b'v']), vec![("k".into(), "v".into())]);
    }

    #[test]
    fn tiers_and_codes() {
        for t in [Tier::Contact, Tier::MyDevice] {
            assert_eq!(Tier::parse(t.as_str()), Some(t));
            assert_eq!(serde_json::to_value(t).unwrap(), t.as_str());
        }
        assert_eq!(Tier::parse("my-device"), Some(Tier::MyDevice));
        assert_eq!(spaced_code("012345"), "012 345");
        let p: PeersConfirmParams = serde_json::from_value(serde_json::json!({ "pairing": "pr_1", "as": "my_device" })).unwrap();
        assert!(p.accept);
        assert_eq!(p.tier, Some(Tier::MyDevice));
    }

    #[test]
    fn short_fingerprints() {
        assert_eq!(short_fingerprint("0123456789abcdef0123"), "0123 4567 89ab cdef");
        assert_eq!(short_fingerprint("01234"), "0123 4");
    }

    #[test]
    fn visibility_names() {
        for v in [Visibility::Off, Visibility::Paired, Visibility::Everyone] {
            assert_eq!(Visibility::parse(v.as_str()), Some(v));
            assert_eq!(serde_json::to_value(v).unwrap(), v.as_str());
        }
        assert_eq!(Visibility::default(), Visibility::Paired);
    }

    #[test]
    fn file_share_names_and_paths_stay_inside_the_folder() {
        for bad in ["", ".", "..", "a/b", "a\0b", "a\nb", &"x".repeat(MAX_NAME + 1)] {
            assert!(!valid_name(bad), "{bad:?}");
        }
        assert!(valid_name("a:b") && valid_name(".hidden") && valid_name("héllo wörld.txt"));
        for bad in ["/etc/passwd", "a/../../b", "../a", "a/./b", "a//b", "a/", "", &"a/".repeat(MAX_DEPTH).to_string()] {
            assert!(!valid_path(bad), "{bad:?}");
        }
        assert!(valid_path("a/b/c.txt") && valid_path("x"));

        assert_eq!(safe_name("../x/y:z"), "..-x-y-z");
        assert_eq!(safe_name(".."), "untitled");
        assert_eq!(safe_name("  "), "untitled");
        assert_eq!(safe_name("a\tb"), "a-b");
        let long = safe_name(&"é".repeat(MAX_NAME));
        assert!(long.len() <= MAX_NAME && valid_name(&long));

        assert_eq!(size_label(0), "0 B");
        assert_eq!(size_label(1023), "1023 B");
        assert_eq!(size_label(1536), "1.5 KB");
        assert_eq!(size_label(3 << 30), "3.0 GB");
        assert_eq!(size_label(u64::MAX), "16777216.0 TB");
    }

    #[test]
    fn a_file_share_list_must_be_well_formed() {
        let sum = "0".repeat(64);
        let f = |path: &str, size: u64| ShareFile { path: path.into(), size, sha256: sum.clone() };
        let dirs = |d: &[&str]| d.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(check_files("a.txt", false, &[f("a.txt", 3)], &[]).is_ok());
        assert!(check_files("a.txt", false, &[f("b.txt", 3)], &[]).is_err());
        assert!(check_files("a.txt", false, &[f("a.txt", 3), f("b", 1)], &[]).is_err());
        assert!(check_files("a.txt", false, &[f("a.txt", 3)], &dirs(&["d"])).is_err());
        assert!(check_files("../a", false, &[f("../a", 3)], &[]).is_err());
        let bad_sum = ShareFile { path: "a.txt".into(), size: 1, sha256: "A".repeat(64) };
        assert!(check_files("a.txt", false, &[bad_sum], &[]).is_err());

        assert!(check_files("d", true, &[f("x/y.txt", 1), f("z", 2)], &dirs(&["empty", "x/deeper"])).is_ok());
        assert!(check_files("d", true, &[], &[]).is_ok());
        for (files, ds) in [
            (vec![f("../out", 1)], vec![]),
            (vec![f("/abs", 1)], vec![]),
            (vec![f("a", 1), f("A", 1)], vec![]),
            (vec![f("a", 1)], dirs(&["a"])),
            (vec![f("a", 1), f("a/b", 1)], vec![]),
            (vec![f("a", u64::MAX), f("b", 1)], vec![]),
            (vec![], dirs(&["x/../.."])),
        ] {
            assert!(check_files("d", true, &files, &ds).is_err(), "{files:?} {ds:?}");
        }
        let many: Vec<ShareFile> = (0..=MAX_FILES).map(|i| f(&format!("f{i}"), 0)).collect();
        assert!(check_files("d", true, &many, &[]).is_err());
    }
}
