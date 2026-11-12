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
use crate::types::Timestamp;
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

/// What a share carries. Nothing in it ever runs: it is data until the receiving person
/// accepts it, and even then a snippet is pasted (never submitted) and a link opened or pinned.
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
}

impl SharePayload {
    pub fn kind(&self) -> ShareKind {
        match self {
            SharePayload::Message { .. } => ShareKind::Message,
            SharePayload::Snippet { .. } => ShareKind::Snippet,
            SharePayload::Link { .. } => ShareKind::Link,
        }
    }

    /// Its size in bytes (the text or URL).
    pub fn size(&self) -> u64 {
        match self {
            SharePayload::Message { text, .. } | SharePayload::Snippet { text, .. } => text.len() as u64,
            SharePayload::Link { url, .. } => url.len() as u64,
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
        }
        Ok(())
    }
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
    /// What accepting did on this Mac: `pasted into t_…`, `opened`, `pinned to t_…`, `read`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    /// Incoming and still offered: the needs-you approval that asks this Mac's person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs_you_id: Option<String>,
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
}
