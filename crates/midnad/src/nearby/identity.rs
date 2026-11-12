//! This home's device identity: an Ed25519 key whose seed lives in `nearby/device.key` (hex,
//! mode 0600). The fingerprint, SHA-256 of the public key in lowercase hex, is the device's id.
//! A key file that can't be read as one is moved aside and a new key made (the device then
//! looks new to its peers).
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub struct Identity {
    key: SigningKey,
    pub fingerprint: String,
}

impl Identity {
    pub fn public_key(&self) -> VerifyingKey {
        self.key.verifying_key()
    }

    pub fn sign(&self, msg: &[u8]) -> Signature {
        self.key.sign(msg)
    }
}

/// SHA-256 of a public key, lowercase hex.
pub fn fingerprint(key: &VerifyingKey) -> String {
    hex(&Sha256::digest(key.as_bytes()))
}

pub fn key_path(home: &Path) -> PathBuf {
    home.join("nearby").join("device.key")
}

/// Load this home's key, making (and saving) one the first time.
pub fn load_or_create(home: &Path) -> Identity {
    let path = key_path(home);
    match std::fs::read_to_string(&path) {
        Ok(text) => match parse_seed(&text) {
            Some(seed) => return identity(SigningKey::from_bytes(&seed)),
            None => {
                let aside = path.with_extension(format!("bad-{}", midna_proto::time::now_unix()));
                eprintln!("midnad: nearby: {} isn't a device key; moved to {} and made a new one", path.display(), aside.display());
                let _ = std::fs::rename(&path, &aside);
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!("midnad: nearby: reading {}: {e}; making a new device key", path.display()),
    }
    let mut seed = [0u8; 32];
    // SAFETY: arc4random_buf fills exactly `seed.len()` bytes of a valid buffer.
    unsafe { libc::arc4random_buf(seed.as_mut_ptr().cast(), seed.len()) };
    if let Err(e) = save_seed(&path, &seed) {
        eprintln!("midnad: nearby: saving {}: {e} (this device gets a new id when midnad restarts)", path.display());
    }
    identity(SigningKey::from_bytes(&seed))
}

fn identity(key: SigningKey) -> Identity {
    let fingerprint = fingerprint(&key.verifying_key());
    Identity { key, fingerprint }
}

fn parse_seed(text: &str) -> Option<[u8; 32]> {
    let t = text.trim();
    if t.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(t.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

fn save_seed(path: &Path, seed: &[u8; 32]) -> std::io::Result<()> {
    midna_proto::paths::guard_write(path)?;
    let dir = path.parent().expect("key path has a parent");
    std::fs::create_dir_all(dir)?;
    let _ = std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700));
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    f.write_all(format!("{}\n", hex(seed)).as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(tag: &str) -> PathBuf {
        let h = std::env::temp_dir().join(format!("midna-id-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&h);
        h
    }

    #[test]
    fn made_once_then_loaded() {
        let h = home("once");
        let a = load_or_create(&h);
        assert_eq!(a.fingerprint.len(), 64);
        assert!(a.fingerprint.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        let b = load_or_create(&h);
        assert_eq!(a.fingerprint, b.fingerprint, "the same key comes back");
        let mode = std::fs::metadata(key_path(&h)).unwrap().permissions();
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777, 0o600);
        let sig = a.sign(b"hello");
        assert!(b.public_key().verify_strict(b"hello", &sig).is_ok());
        assert_eq!(fingerprint(&b.public_key()), b.fingerprint);
        let _ = std::fs::remove_dir_all(&h);
    }

    #[test]
    fn a_broken_key_is_moved_aside() {
        let h = home("bad");
        std::fs::create_dir_all(h.join("nearby")).unwrap();
        std::fs::write(key_path(&h), "not a key").unwrap();
        let a = load_or_create(&h);
        assert_eq!(a.fingerprint, load_or_create(&h).fingerprint);
        let aside = std::fs::read_dir(h.join("nearby")).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().contains(".bad-")).count();
        assert_eq!(aside, 1);
        let _ = std::fs::remove_dir_all(&h);
    }
}
