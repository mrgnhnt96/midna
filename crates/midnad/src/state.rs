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
}

impl State {
    pub fn load(path: &Path) -> State {
        match std::fs::read(path) {
            Ok(b) => match serde_json::from_slice::<State>(&b) {
                Ok(s) => s,
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

    pub fn project(&self, id: &str) -> Option<&Project> {
        self.projects.iter().find(|p| p.id == id)
    }
    pub fn session(&self, id: &str) -> Option<&Session> {
        self.sessions.iter().find(|s| s.id == id)
    }
    pub fn session_mut(&mut self, id: &str) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    /// Current value of a setting (stored or catalog default).
    pub fn setting(&self, key: &str) -> Value {
        self.settings
            .get(key)
            .cloned()
            .or_else(|| settings::setting(key).map(|s| s.default.to_json()))
            .unwrap_or(Value::Null)
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
