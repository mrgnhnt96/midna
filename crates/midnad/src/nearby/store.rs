//! The devices this home paired with: `nearby/peers.json` (mode 0600). Each entry keeps the
//! device's public key (its fingerprint must match, or the entry is skipped on load), the tier
//! this side chose, and the pair's presence key, which makes the tags of paired-only
//! advertisements. Only pairing (both people confirmed) adds an entry; `peers.forget` removes it.
//! One of my devices also keeps the kinds of share its person set to auto-accept; a contact
//! never keeps any.
use super::transport::{hex, unhex};
use hmac::{Hmac, Mac};
use midna_proto::nearby::{PairedPeer, ShareKind, TAG_HEX, Tier};
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
    /// The kinds of share accepted from it without asking (my devices only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub auto_accept: Vec<ShareKind>,
}

impl Stored {
    pub fn paired(&self, nearby: bool) -> PairedPeer {
        PairedPeer {
            fingerprint: self.fingerprint.clone(),
            name: self.name.clone(),
            tier: self.tier,
            paired_at: self.paired_at.clone(),
            nearby,
            auto_accept: ShareKind::ALL.into_iter().filter(|k| self.auto_accepts(*k)).collect(),
        }
    }

    /// Shares of `kind` from it are accepted without asking: it is one of my devices and its
    /// person turned that kind on.
    pub fn auto_accepts(&self, kind: ShareKind) -> bool {
        self.tier == Tier::MyDevice && self.auto_accept.contains(&kind)
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
        let peers = file
            .peers
            .into_iter()
            .filter(valid)
            .map(|mut p| {
                if p.tier != Tier::MyDevice {
                    p.auto_accept.clear();
                }
                (p.fingerprint.clone(), p)
            })
            .collect();
        Store { path, peers }
    }

    pub fn get(&self, fp: &str) -> Option<&Stored> {
        self.peers.get(fp)
    }

    pub fn insert(&mut self, s: Stored) -> std::io::Result<()> {
        self.peers.insert(s.fingerprint.clone(), s);
        self.save()
    }

    /// Change a paired device's tier; None if it isn't paired. A contact auto-accepts nothing.
    pub fn set_tier(&mut self, fp: &str, tier: Tier) -> std::io::Result<Option<Stored>> {
        let Some(s) = self.peers.get_mut(fp) else { return Ok(None) };
        s.tier = tier;
        if tier != Tier::MyDevice {
            s.auto_accept.clear();
        }
        let s = s.clone();
        self.save()?;
        Ok(Some(s))
    }

    /// Accept `kind` from one of my devices without asking (`on`), or ask again. None if it
    /// isn't paired; the entry unchanged if it is a contact (the caller refuses that).
    pub fn set_auto_accept(&mut self, fp: &str, kind: ShareKind, on: bool) -> std::io::Result<Option<Stored>> {
        let Some(s) = self.peers.get_mut(fp) else { return Ok(None) };
        if s.tier != Tier::MyDevice {
            return Ok(Some(s.clone()));
        }
        s.auto_accept.retain(|k| *k != kind);
        if on {
            s.auto_accept.push(kind);
        }
        s.auto_accept.sort_by_key(|k| ShareKind::ALL.iter().position(|x| x == k));
        let s = s.clone();
        self.save()?;
        Ok(Some(s))
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
            auto_accept: vec![],
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

    #[test]
    fn only_my_devices_auto_accept_and_demoting_clears_it() {
        let h = std::env::temp_dir().join(format!("midna-store-auto-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&h);
        let id = identity::load_or_create(&h.join("other"));
        let fp = id.fingerprint.clone();
        let contact = Stored {
            fingerprint: fp.clone(),
            public_key: hex(id.public_key().as_bytes()),
            name: "Bravo".into(),
            tier: Tier::Contact,
            paired_at: "2026-11-12T00:00:00Z".into(),
            presence: hex(&[7u8; 32]),
            auto_accept: vec![],
        };
        let mut s = Store::load(&h);
        s.insert(contact.clone()).unwrap();
        assert!(s.get(&fp).unwrap().paired(false).auto_accept.is_empty());
        let got = s.set_auto_accept(&fp, ShareKind::File, true).unwrap().unwrap();
        assert!(got.auto_accept.is_empty() && !got.auto_accepts(ShareKind::File), "a contact can't");
        assert!(s.set_auto_accept("nope", ShareKind::File, true).unwrap().is_none());

        s.set_tier(&fp, Tier::MyDevice).unwrap();
        assert!(!s.get(&fp).unwrap().auto_accepts(ShareKind::Message), "off by default");
        s.set_auto_accept(&fp, ShareKind::Tab, true).unwrap();
        s.set_auto_accept(&fp, ShareKind::Message, true).unwrap();
        s.set_auto_accept(&fp, ShareKind::Message, true).unwrap();
        let back = Store::load(&h);
        assert_eq!(back.get(&fp).unwrap().auto_accept, vec![ShareKind::Message, ShareKind::Tab], "kept, once each, in order");
        assert_eq!(back.get(&fp).unwrap().paired(true).auto_accept, vec![ShareKind::Message, ShareKind::Tab]);
        s.set_auto_accept(&fp, ShareKind::Message, false).unwrap();
        assert_eq!(s.get(&fp).unwrap().auto_accept, vec![ShareKind::Tab]);

        // Kept as a contact again: everything is off, and stays off when promoted again.
        s.set_tier(&fp, Tier::Contact).unwrap();
        assert!(s.get(&fp).unwrap().auto_accept.is_empty());
        s.set_tier(&fp, Tier::MyDevice).unwrap();
        assert!(Store::load(&h).get(&fp).unwrap().auto_accept.is_empty());

        // A contact edited by hand to auto-accept still can't.
        let edited = Stored { auto_accept: vec![ShareKind::Link], ..contact };
        std::fs::write(path(&h), serde_json::to_string(&File { peers: vec![edited.clone()] }).unwrap()).unwrap();
        let loaded = Store::load(&h);
        assert!(loaded.get(&fp).unwrap().auto_accept.is_empty());
        assert!(!edited.auto_accepts(ShareKind::Link));
        assert!(edited.paired(false).auto_accept.is_empty());
        let _ = std::fs::remove_dir_all(&h);
    }
}
