//! The devices this home paired with: `nearby/peers.json` (mode 0600). Each entry keeps the
//! device's public key (its fingerprint must match, or the entry is skipped on load), the tier
//! this side chose, and the pair's presence key, which makes the tags of paired-only
//! advertisements. Only pairing (both people confirmed) adds an entry; `peers.forget` removes it.
use super::transport::{hex, unhex};
use hmac::{Hmac, Mac};
use midna_proto::nearby::{PairedPeer, TAG_HEX, Tier};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stored {
    pub fingerprint: String,
    /// Ed25519 public key, hex.
    pub public_key: String,
    pub name: String,
    pub tier: Tier,
    pub paired_at: String,
    /// The pair's presence key, hex (both sides have the same one).
    pub presence: String,
}

impl Stored {
    pub fn paired(&self, nearby: bool) -> PairedPeer {
        PairedPeer { fingerprint: self.fingerprint.clone(), name: self.name.clone(), tier: self.tier, paired_at: self.paired_at.clone(), nearby }
    }

    /// This pair's tag for an advertisement with `salt` (the advertisement's salt followed by
    /// the advertising device's fingerprint).
    pub fn tag(&self, salt: &[u8]) -> Option<String> {
        tag(&unhex(&self.presence)?, salt)
    }
}

/// The start of HMAC-SHA256(presence key, salt), as [`TAG_HEX`] hex digits.
pub fn tag(presence: &[u8], salt: &[u8]) -> Option<String> {
    let mut mac = Hmac::<Sha256>::new_from_slice(presence).ok()?;
    mac.update(salt);
    let mut h = hex(&mac.finalize().into_bytes());
    h.truncate(TAG_HEX);
    Some(h)
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    peers: Vec<Stored>,
}

#[derive(Clone)]
pub struct Store {
    path: PathBuf,
    /// By fingerprint.
    pub peers: BTreeMap<String, Stored>,
}

pub fn path(home: &Path) -> PathBuf {
    home.join("nearby").join("peers.json")
}

impl Store {
    pub fn load(home: &Path) -> Store {
        let path = path(home);
        let file: File = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                eprintln!("midnad: nearby: {} can't be read ({e}); no devices are paired", path.display());
                File::default()
            }),
            Err(_) => File::default(),
        };
        let peers = file.peers.into_iter().filter(valid).map(|p| (p.fingerprint.clone(), p)).collect();
        Store { path, peers }
    }

    pub fn get(&self, fp: &str) -> Option<&Stored> {
        self.peers.get(fp)
    }

    pub fn insert(&mut self, s: Stored) -> std::io::Result<()> {
        self.peers.insert(s.fingerprint.clone(), s);
        self.save()
    }

    pub fn remove(&mut self, fp: &str) -> std::io::Result<Option<Stored>> {
        let gone = self.peers.remove(fp);
        if gone.is_some() {
            self.save()?;
        }
        Ok(gone)
    }

    /// A paired device (other than `fp`) whose name is `name`, ignoring case.
    pub fn named(&self, name: &str, fp: &str) -> Option<&Stored> {
        let n = name.trim().to_lowercase();
        (!n.is_empty()).then(|| self.peers.values().find(|p| p.fingerprint != fp && p.name.trim().to_lowercase() == n)).flatten()
    }

    pub fn save(&self) -> std::io::Result<()> {
        midna_proto::paths::guard_write(&self.path)?;
        let dir = self.path.parent().expect("peers path has a parent");
        std::fs::create_dir_all(dir)?;
        let _ = std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700));
        let text = serde_json::to_string_pretty(&File { peers: self.peers.values().cloned().collect() })?;
        let tmp = self.path.with_extension(format!("tmp-{}", std::process::id()));
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, &self.path)
    }
}

/// The key is a real Ed25519 key whose fingerprint is the entry's.
fn valid(s: &Stored) -> bool {
    let ok = unhex(&s.public_key)
        .and_then(|k| <[u8; 32]>::try_from(k).ok())
        .and_then(|k| ed25519_dalek::VerifyingKey::from_bytes(&k).ok())
        .is_some_and(|k| super::identity::fingerprint(&k) == s.fingerprint)
        && unhex(&s.presence).is_some_and(|p| p.len() == 32);
    if !ok {
        eprintln!("midnad: nearby: skipping paired device {:?}: its key doesn't match its id", s.name);
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nearby::identity;

    #[test]
    fn saved_and_loaded_and_checked() {
        let h = std::env::temp_dir().join(format!("midna-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&h);
        let id = identity::load_or_create(&h.join("other"));
        let good = Stored {
            fingerprint: id.fingerprint.clone(),
            public_key: hex(id.public_key().as_bytes()),
            name: "Bravo".into(),
            tier: Tier::MyDevice,
            paired_at: "2026-11-12T00:00:00Z".into(),
            presence: hex(&[7u8; 32]),
        };
        let mut s = Store::load(&h);
        assert!(s.peers.is_empty());
        s.insert(good.clone()).unwrap();
        // Someone edits the file to put another key under the paired fingerprint.
        let other = identity::load_or_create(&h.join("third"));
        let forged = Stored { public_key: hex(other.public_key().as_bytes()), name: "Forged".into(), ..good.clone() };
        let mut raw = s.peers.clone();
        raw.insert("x".into(), forged);
        let text = serde_json::to_string(&File { peers: raw.into_values().collect() }).unwrap();
        std::fs::write(path(&h), text).unwrap();
        let back = Store::load(&h);
        assert_eq!(back.peers.len(), 1);
        assert_eq!(back.get(&id.fingerprint), Some(&good));
        assert_eq!(back.named("bravo ", "someone-else").map(|p| p.fingerprint.as_str()), Some(id.fingerprint.as_str()));
        assert!(back.named("Bravo", &id.fingerprint).is_none(), "its own name isn't a clash");
        let mode = std::fs::metadata(path(&h)).unwrap().permissions();
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777, 0o600);
        let t = good.tag(b"salt").unwrap();
        assert_eq!(t.len(), TAG_HEX);
        assert_ne!(Some(t), good.tag(b"other salt"));
        let mut s = back;
        assert!(s.remove(&id.fingerprint).unwrap().is_some());
        assert!(Store::load(&h).peers.is_empty());
        let _ = std::fs::remove_dir_all(&h);
    }
}
