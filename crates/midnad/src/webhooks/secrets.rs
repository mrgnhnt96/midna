//! Secret values: webhook signing secrets and the human's pasted secrets (the vault).
//! Human-only to set; never returned over RPC (except `secret.exec_env`) or logged.
//!
//! - Keychain (default): generic password, service `com.mrgnhnt.midna.webhook` (account =
//!   trigger id) or `com.mrgnhnt.midna.secret` (account = secret id).
//! - File (`MIDNA_SECRETS=file`, used by tests): `$MIDNA_HOME/secrets/<trigger id>` or
//!   `$MIDNA_HOME/vault/<secret id>`, mode 0600.
//!
//! Secrets are cached in memory after the first read so a delivery doesn't hit the Keychain
//! for every candidate trigger.
use std::collections::HashMap;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::Mutex;

pub const KEYCHAIN_SERVICE: &str = "com.mrgnhnt.midna.webhook";
pub const VAULT_SERVICE: &str = "com.mrgnhnt.midna.secret";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Keychain,
    File,
}

impl Mode {
    /// `MIDNA_SECRETS=file` selects the file store; anything else is the Keychain.
    pub fn from_env() -> Mode {
        match std::env::var("MIDNA_SECRETS").as_deref() {
            Ok("file") => Mode::File,
            _ => Mode::Keychain,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Mode::Keychain => "keychain",
            Mode::File => "file",
        }
    }
}

pub struct Store {
    pub mode: Mode,
    service: &'static str,
    dir: PathBuf,
    cache: Mutex<HashMap<String, Vec<u8>>>,
}

fn valid_account(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

impl Store {
    pub fn new(mode: Mode, home: &std::path::Path) -> Store {
        Store { mode, service: KEYCHAIN_SERVICE, dir: home.join("secrets"), cache: Mutex::new(HashMap::new()) }
    }

    /// The human's pasted secrets (`secret.*`).
    pub fn vault(mode: Mode, home: &std::path::Path) -> Store {
        Store { mode, service: VAULT_SERVICE, dir: home.join("vault"), cache: Mutex::new(HashMap::new()) }
    }

    fn cache(&self) -> std::sync::MutexGuard<'_, HashMap<String, Vec<u8>>> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set(&self, id: &str, secret: &[u8]) -> Result<(), String> {
        if !valid_account(id) {
            return Err("bad id".into());
        }
        match self.mode {
            Mode::Keychain => security_framework::passwords::set_generic_password(self.service, id, secret)
                .map_err(|e| format!("keychain: {e}"))?,
            Mode::File => {
                std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
                let _ = std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700));
                let path = self.dir.join(id);
                let tmp = self.dir.join(format!(".{id}.tmp"));
                let _ = std::fs::remove_file(&tmp);
                let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp).map_err(|e| e.to_string())?;
                f.write_all(secret).map_err(|e| e.to_string())?;
                f.sync_all().map_err(|e| e.to_string())?;
                std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
            }
        }
        self.cache().insert(id.to_string(), secret.to_vec());
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<Vec<u8>> {
        if !valid_account(id) {
            return None;
        }
        if let Some(s) = self.cache().get(id) {
            return Some(s.clone());
        }
        let v = match self.mode {
            Mode::Keychain => security_framework::passwords::get_generic_password(self.service, id).ok()?,
            Mode::File => std::fs::read(self.dir.join(id)).ok()?,
        };
        self.cache().insert(id.to_string(), v.clone());
        Some(v)
    }

    pub fn delete(&self, id: &str) {
        if !valid_account(id) {
            return;
        }
        self.cache().remove(id);
        match self.mode {
            Mode::Keychain => {
                let _ = security_framework::passwords::delete_generic_password(self.service, id);
            }
            Mode::File => {
                let _ = std::fs::remove_file(self.dir.join(id));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn file_store_roundtrip_is_0600() {
        let home = std::env::temp_dir().join(format!("midna-secrets-{}", std::process::id()));
        let s = Store::new(Mode::File, &home);
        s.set("t_abc123", b"s3cret").unwrap();
        let meta = std::fs::metadata(home.join("secrets/t_abc123")).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        // A fresh store (no cache) reads it back from disk.
        assert_eq!(Store::new(Mode::File, &home).get("t_abc123").as_deref(), Some(&b"s3cret"[..]));
        s.delete("t_abc123");
        assert!(Store::new(Mode::File, &home).get("t_abc123").is_none());
        assert!(s.set("../evil", b"x").is_err());
        let _ = std::fs::remove_dir_all(&home);
    }
}
