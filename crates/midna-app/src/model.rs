//! Domain types the GUI reads from midnad.
//!
//! These mirror docs/ARCHITECTURE.md ("Domain model"). They are deliberately lenient
//! (`#[serde(default)]` everywhere, unknown enum values fall back to `Other`) so that the
//! GUI keeps rendering while the daemon grows fields. When `midna-proto` exports the
//! canonical types this module can become a set of re-exports.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: String,
    pub icon: Option<String>,
    pub order: u32,
    pub commands: Vec<ProjectCommand>,
    pub last_opened_at: Option<String>,
}

/// A folder under a `projects.roots` folder (`project.discover`).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct ProjectCandidate {
    pub name: String,
    pub path: String,
    pub root: String,
    pub git: bool,
    pub project_id: Option<String>,
    pub modified_at: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct ProjectCommand {
    pub name: String,
    pub run: String,
    pub pinned: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    #[default]
    Shell,
    Agent,
    Monitor,
    #[serde(other)]
    Other,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Claude,
    Codex,
    #[serde(other)]
    Other,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StatusState {
    #[default]
    Idle,
    Working,
    NeedsYou,
    Done,
    Failed,
    Exited,
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Status {
    pub state: StatusState,
    pub reason: Option<String>,
    pub exit_code: Option<i32>,
    pub since: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Checks {
    #[default]
    None,
    Pending,
    Passing,
    Failing,
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct PrInfo {
    pub number: u64,
    pub url: String,
    pub checks: Checks,
    pub failing_count: u32,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct GitInfo {
    pub branch: String,
    pub ahead: u32,
    pub behind: u32,
    pub added: u32,
    pub removed: u32,
    pub files: u32,
    pub pr: Option<PrInfo>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Session {
    pub id: String,
    pub project_id: Option<String>,
    pub name: String,
    pub kind: SessionKind,
    pub agent: Option<AgentKind>,
    pub cwd: String,
    pub command: Vec<String>,
    pub pid: Option<i32>,
    pub title: String,
    pub status: Status,
    pub created_at: String,
    pub last_activity_at: String,
    pub keep_on_top: bool,
    /// Kept out of the project lists, in the sidebar's folded Background group.
    pub background: bool,
    pub git: Option<GitInfo>,
    /// Agents: conversation, background work in flight, update available, queued restart.
    pub agent_info: Option<midna_proto::AgentInfo>,
    /// Notification overrides for this terminal (`enabled: false` = muted; see `notify.set`).
    pub notify: std::collections::BTreeMap<String, bool>,
    /// A label + color a local trigger put on this terminal (`set_status`). Shown instead of the
    /// built-in status; `status.state` still drives sorting, counts and notifications.
    pub custom_status: Option<CustomStatus>,
    /// Messages waiting to be typed into this terminal, in order (`queue.*`).
    pub queue: Vec<midna_proto::QueuedMessage>,
    /// Nothing is typed from the queue while paused.
    pub queue_paused: bool,
}

/// Lenient copy of `midna_proto::CustomStatus` (only what the GUI shows).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct CustomStatus {
    pub label: String,
    /// Named (`amber`, `teal`, … see `midna_proto::STATUS_COLORS`) or `#rrggbb`.
    pub color: String,
    pub icon: Option<String>,
    pub base: StatusState,
    pub clear_on: String,
    pub trigger_id: Option<String>,
    pub detail: Option<String>,
    pub since: Option<String>,
}

impl Session {
    pub fn notify_muted(&self) -> bool {
        self.notify.get("enabled") == Some(&false)
    }
}

/// What a session row/header shows as its icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    Claude,
    Codex,
    Monitor,
    Shell,
}

impl Session {
    pub fn glyph(&self) -> Glyph {
        match (self.kind, self.agent) {
            (_, Some(AgentKind::Claude)) => Glyph::Claude,
            (_, Some(AgentKind::Codex)) => Glyph::Codex,
            (SessionKind::Monitor, _) => Glyph::Monitor,
            _ => Glyph::Shell,
        }
    }
}

impl Glyph {}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NeedsYouKind {
    Approval,
    PermissionPrompt,
    Blocked,
    #[default]
    Note,
    Failed,
    TriggerWaiting,
    RuleRemoval,
    SecretNeeded,
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Actor {
    pub kind: String,
    pub session: Option<String>,
    pub name: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct PolicyAction {
    pub kind: String,
    pub value: String,
    pub session: Option<String>,
    pub project: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct ApprovalRequest {
    pub action: PolicyAction,
    pub matched_rule: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct NeedsYou {
    pub id: String,
    pub session_id: Option<String>,
    pub project_id: Option<String>,
    pub kind: NeedsYouKind,
    pub title: String,
    pub detail: String,
    pub screen_excerpt: Option<Vec<String>>,
    pub asked_by: Actor,
    pub created_at: String,
    pub bulk_safe: bool,
    pub approval: Option<ApprovalRequest>,
}

impl NeedsYou {
    /// Items the approval banner can answer with approve/deny.
    pub fn is_approval(&self) -> bool {
        matches!(self.kind, NeedsYouKind::Approval | NeedsYouKind::PermissionPrompt) || self.approval.is_some()
    }
}

/// Resolutions and approval scopes use the daemon's exact wire form (`{"kind":"approve",...}`).
pub use midna_proto::{ApprovalScope, Resolution};

/// One entry of `settings.list`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct SettingEntry {
    pub key: String,
    pub value: Value,
    pub default: Value,
    pub description: String,
    pub human_only: bool,
    pub cli: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    Dim,
    Ok,
    Err,
    Need,
    Work,
    Accent,
    #[serde(other)]
    Other,
}

/// A segment returned by `script.run`. `icon`, `mono` and `join` are optional GUI hints
/// (see docs/DECISIONS.md); scripts that only send `{text, tone, link}` still render.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Segment {
    pub text: String,
    pub tone: Option<Tone>,
    pub link: Option<String>,
    pub icon: Option<String>,
    pub mono: bool,
    /// Attach to the previous segment with a single space instead of the normal gap.
    pub join: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Event {
    pub seq: u64,
    pub at: String,
    pub kind: String,
    pub actor: Actor,
    pub project_id: Option<String>,
    pub session_id: Option<String>,
    pub data: Value,
}

/// The numbers the Today card and the status bar show.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Today {
    pub turns: u64,
    pub messages: u64,
    pub spend_usd: f64,
    pub triggers_fired: u64,
}

impl Today {
    pub fn from_value(v: &Value) -> Today {
        // `insights.summary` returns totals; accept them nested under "totals" or at the top.
        let t = v.get("totals").unwrap_or(v);
        let n = |k: &str| t.get(k).and_then(|x| x.as_u64().or_else(|| x.as_f64().map(|f| f as u64))).unwrap_or(0);
        Today { turns: n("turns"), messages: n("messages"), spend_usd: t.get("spend_usd").and_then(|x| x.as_f64()).unwrap_or(0.0), triggers_fired: n("triggers_fired") }
    }
}

/// Parse a list result leniently: a bare array, or an object holding exactly one array
/// (e.g. `{"sessions": [...]}`), or `{"items": [...]}`.
pub fn parse_list<T: serde::de::DeserializeOwned>(v: &Value) -> Vec<T> {
    let arr = match v {
        Value::Array(a) => Some(a),
        Value::Object(m) => m.get("items").and_then(|x| x.as_array()).or_else(|| m.values().find_map(|x| x.as_array())),
        _ => None,
    };
    arr.map(|a| {
        a.iter()
            .filter_map(|x| match serde_json::from_value::<T>(x.clone()) {
                Ok(t) => Some(t),
                Err(e) => {
                    eprintln!("midna-app: skipping unparsable item: {e}: {x}");
                    None
                }
            })
            .collect()
    })
    .unwrap_or_default()
}

/// "4m", "2h", "3d" since an RFC 3339 timestamp (best effort; empty when unparsable).
pub fn since_short(ts: &str) -> String {
    let Some(t) = parse_rfc3339(ts) else {
        return String::new();
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let d = (now - t).max(0);
    match d {
        0..=59 => "now".into(),
        60..=3599 => format!("{}m", d / 60),
        3600..=86399 => format!("{}h", d / 3600),
        _ => format!("{}d", d / 86400),
    }
}

/// Minimal RFC 3339 parser (UTC or numeric offset) -> unix seconds.
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (num(0..4)?, num(5..7)?, num(8..10)?, num(11..13)?, num(14..16)?, num(17..19)?);
    // days from civil (Howard Hinnant)
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let mut t = days * 86400 + h * 3600 + mi * 60 + se;
    // offset
    let rest = &s[19..];
    let rest = rest.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    if let Some(sign) = rest.chars().next()
        && (sign == '+' || sign == '-')
        && rest.len() >= 6
    {
        let oh: i64 = rest[1..3].parse().ok()?;
        let om: i64 = rest[4..6].parse().ok()?;
        let off = oh * 3600 + om * 60;
        t -= if sign == '+' { off } else { -off };
    }
    Some(t)
}

pub fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    rfc3339_from_unix(secs)
}

pub fn rfc3339_from_unix(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    // civil from days (Howard Hinnant)
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_roundtrip() {
        for t in [0i64, 1_700_000_000, 1_790_000_123] {
            assert_eq!(parse_rfc3339(&rfc3339_from_unix(t)), Some(t));
        }
        assert_eq!(parse_rfc3339("2026-10-03T12:00:00+02:00"), parse_rfc3339("2026-10-03T10:00:00Z"));
    }

    #[test]
    fn resolution_wire_shape() {
        let r = Resolution::Approve { scope: ApprovalScope::Minutes { minutes: 15 } };
        assert_eq!(serde_json::to_string(&r).unwrap(), r#"{"kind":"approve","scope":{"kind":"minutes","minutes":15}}"#);
    }

    /// The daemon serializes proto's strict types; the GUI's lenient copies must read them.
    #[test]
    fn reads_proto_wire_types() {
        use midna_proto as p;
        let s = p::Session {
            id: "abcd1234".into(),
            project_id: "p_aaaaaa".into(),
            name: "api".into(),
            kind: p::SessionKind::Agent,
            agent: Some(p::AgentKind::Codex),
            cwd: "/tmp".into(),
            command: vec!["codex".into()],
            pid: Some(1),
            title: String::new(),
            status: p::Status { state: p::StatusState::NeedsYou, reason: Some("x".into()), exit_code: None, since: "2026-10-03T10:00:00Z".into() },
            created_at: "2026-10-03T10:00:00Z".into(),
            last_activity_at: "2026-10-03T10:00:00Z".into(),
            keep_on_top: false,
            background: true,
            git: Some(p::GitInfo {
                branch: "main".into(),
                files: 2,
                pr: Some(p::PrInfo { number: 3, url: "u".into(), checks: p::ChecksState::Failing, failing_count: 1 }),
                ..Default::default()
            }),
            agent_info: None,
            notify: Default::default(),
            custom_status: Some(p::CustomStatus {
                label: "Prompt blocked".into(),
                color: "amber".into(),
                icon: None,
                base: p::StatusState::NeedsYou,
                clear_on: p::StatusClear::Prompt,
                trigger_id: Some("t_1".into()),
                detail: Some("Compact first".into()),
                needs_you_id: None,
                since: "2026-10-03T10:00:00Z".into(),
            }),
            queue: vec![p::QueuedMessage {
                id: "q_1".into(),
                text: "/compact".into(),
                enter: true,
                images: vec![],
                when: p::SendWhen::IdleFor { minutes: 5 },
                by: p::Actor::human(),
                queued_at: "2026-10-03T10:00:00Z".into(),
                state: p::QueueState::Waiting,
                error: None,
                trigger_id: None,
                waiting_for: vec![],
            }],
            queue_paused: true,
        };
        let v = serde_json::json!({"sessions": [s]});
        let got: Vec<Session> = parse_list(&v);
        assert_eq!(got[0].glyph(), Glyph::Codex);
        assert_eq!(got[0].status.state, StatusState::NeedsYou);
        assert_eq!(got[0].git.as_ref().unwrap().pr.as_ref().unwrap().checks, Checks::Failing);
        let cs = got[0].custom_status.as_ref().unwrap();
        assert_eq!((cs.label.as_str(), cs.color.as_str(), cs.base, cs.clear_on.as_str()), ("Prompt blocked", "amber", StatusState::NeedsYou, "prompt"));
        assert_eq!((got[0].queue[0].when.clone(), got[0].queue_paused), (p::SendWhen::IdleFor { minutes: 5 }, true));
        assert!(got[0].background);
        let seg = p::Segment { text: "#3".into(), tone: Some(p::Tone::Accent), link: Some("u".into()) };
        let got: Vec<Segment> = parse_list(&serde_json::json!({"segments": [seg]}));
        assert_eq!(got[0].tone, Some(Tone::Accent));
    }

    #[test]
    fn lenient_lists() {
        let v = serde_json::json!({"sessions": [{"id": "abcd1234", "name": "x", "status": {"state": "needs_you"}, "kind": "weird"}]});
        let s: Vec<Session> = parse_list(&v);
        assert_eq!(s[0].status.state, StatusState::NeedsYou);
        assert_eq!(s[0].kind, SessionKind::Other);
    }
}
