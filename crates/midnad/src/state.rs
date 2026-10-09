//! Persisted daemon state (`state.json`), written atomically (write temp + rename).
use midna_proto::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

pub const STATE_VERSION: u32 = 1;

/// A human-only call an agent asked for; executed as the human if they approve the needs-you item.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Deferred {
    pub method: String,
    pub params: Value,
    /// Fingerprint of what the human was shown (the trigger's definition, the binary's hash).
    /// Re-checked before running, so the agent can't change it between ask and approve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub projects: Vec<Project>,
    #[serde(default)]
    pub sessions: Vec<Session>,
    #[serde(default)]
    pub rules: Vec<Rule>,
    #[serde(default)]
    pub triggers: Vec<Trigger>,
    #[serde(default)]
    pub deliveries: Vec<Delivery>,
    /// The human's stored secrets (metadata; values are in the Keychain).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<Secret>,
    #[serde(default)]
    pub needs_you: Vec<NeedsYou>,
    /// Only values that differ from the catalog default are stored.
    #[serde(default)]
    pub settings: BTreeMap<String, Value>,
    #[serde(default)]
    pub deferred: BTreeMap<String, Deferred>,
    /// Built-ins already added once (e.g. `prompt_blocked_status`); removing one keeps it gone.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub seeded: Vec<String>,
    /// The latest plan usage limits any agent reported (`usage.get`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<crate::usage::StoredUsage>,
    /// Notifications up to this event seq are read (`notify.read`). None until first asked:
    /// it then starts at the log's end, so an upgrade doesn't flag every old one unread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify_read_seq: Option<u64>,
    /// Notification kinds the human added (`notify.kinds.*`). Their settings live in
    /// `settings` under the same per-kind keys as a built-in kind's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notify_kinds: Vec<midna_proto::notify::CustomKind>,
    /// `notify.send` notifications midnad remembers, oldest first: what clicks and buttons are
    /// checked against and recorded on, and the `on` actions they run.
    #[serde(default, skip_serializing_if = "std::collections::VecDeque::is_empty")]
    pub notify_sent: std::collections::VecDeque<crate::notify::Sent>,
    /// Today's keep-awake override (`keep_awake.set` today); dropped once its date is past.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_awake_today: Option<midna_proto::keep_awake::Today>,
}

impl State {
    pub fn load(path: &Path) -> State {
        match std::fs::read(path) {
            Ok(b) => match serde_json::from_slice::<State>(&b) {
                Ok(mut s) => {
                    s.drop_filesystem_root_project();
                    crate::headless::settle_lost(&mut s);
                    s
                }
                Err(e) => {
                    // Keep the unreadable file for inspection rather than overwriting it silently.
                    let bad = path.with_extension(format!("json.bad-{}", time::now_unix()));
                    let _ = std::fs::rename(path, &bad);
                    eprintln!("midnad: state.json unreadable ({e}); moved to {}", bad.display());
                    State { version: STATE_VERSION, ..Default::default() }
                }
            },
            Err(_) => State { version: STATE_VERSION, ..Default::default() },
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let tmp = path.with_extension("json.tmp");
        let mut bytes = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        bytes.push(b'\n');
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, path)
    }

    /// `/` is never a project: older daemons auto-created one (named "root") for an open in
    /// `/`, and it then covered every path. Its terminals move to root, which has no heading.
    pub fn drop_filesystem_root_project(&mut self) {
        let gone: Vec<Id> = self.projects.iter().filter(|p| p.path == "/").map(|p| p.id.clone()).collect();
        if gone.is_empty() {
            return;
        }
        self.projects.retain(|p| p.path != "/");
        for s in self.sessions.iter_mut().filter(|s| gone.contains(&s.project_id)) {
            s.project_id = ROOT_PROJECT_ID.into();
        }
    }

    pub fn project(&self, id: &str) -> Option<&Project> {
        self.projects.iter().find(|p| p.id == id)
    }
    pub fn session(&self, id: &str) -> Option<&Session> {
        self.sessions.iter().find(|s| s.id == id)
    }
    pub fn session_mut(&mut self, id: &str) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    /// Current value of a setting (stored or catalog default; a kind you added has its
    /// per-kind settings' defaults too).
    pub fn setting(&self, key: &str) -> Value {
        self.settings
            .get(key)
            .cloned()
            .or_else(|| self.setting_spec(key).map(|s| s.default.to_json()))
            .unwrap_or(Value::Null)
    }

    /// A setting's spec: the catalog's, or for a per-kind key of a kind you added
    /// (`notify.stay.deploys`) the pattern spec of its field.
    pub fn setting_spec(&self, key: &str) -> Option<&'static settings::SettingSpec> {
        settings::setting(key).or_else(|| {
            let (field, kind) = midna_proto::notify::split_kind_key(key)?;
            self.notify_kind(kind)?;
            settings::custom_kind_spec(field)
        })
    }

    /// Every setting key: the catalog's, then the per-kind keys of the kinds you added.
    pub fn setting_keys(&self) -> Vec<String> {
        let custom = self.notify_kinds.iter().flat_map(|k| midna_proto::notify::kind_keys(&k.key));
        settings::SETTINGS.iter().map(|s| s.key.to_string()).chain(custom).collect()
    }

    pub fn notify_kind(&self, key: &str) -> Option<&midna_proto::notify::CustomKind> {
        self.notify_kinds.iter().find(|k| k.key == key)
    }

    /// A built-in notification category or a kind you added.
    pub fn is_notify_kind(&self, key: &str) -> bool {
        midna_proto::notify::category(key).is_some() || self.notify_kind(key).is_some()
    }
    pub fn setting_bool(&self, key: &str) -> bool {
        self.setting(key).as_bool().unwrap_or(false)
    }
    pub fn setting_i64(&self, key: &str) -> i64 {
        self.setting(key).as_i64().unwrap_or(0)
    }
    pub fn setting_str(&self, key: &str) -> String {
        self.setting(key).as_str().unwrap_or("").to_string()
    }
}

/// Random lowercase hex id (arc4random).
pub fn hex_id(len: usize) -> String {
    let mut s = String::with_capacity(len);
    while s.len() < len {
        s.push_str(&format!("{:08x}", unsafe { libc::arc4random() }));
    }
    s.truncate(len);
    s
}
