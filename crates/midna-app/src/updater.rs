//! Auto-update: feed, verification, download, staging and the atomic app swap.
//! The feed format is documented in docs/RELEASING.md; `packaging/make-update.sh` writes it.
//!
//! ```json
//! { "version": "0.1.1", "pub_date": "2026-10-03T21:00:00Z", "notes": "…",
//!   "url": "https://…/Midna-0.1.1.app.tar.gz", "sha256": "<hex>", "size": 12345678,
//!   "minimum_macos": "12.0", "arch": "aarch64", "signature": "<base64 ed25519>" }
//! ```
//!
//! The ed25519 signature covers [`signed_message`] (version, sha256, minimum macOS, arch), so
//! a feed can't pair a signed archive with a different version, and the archive is checked
//! against `sha256` before anything is extracted. The public key is embedded at build time
//! (`MIDNA_UPDATE_PUBKEY`, base64 of the raw 32-byte key); a build without one never updates.
//!
//! The swap is ours (not cargo-packager-updater's, which isn't atomic): the new bundle is
//! staged next to the installed one and exchanged with `renamex_np(RENAME_SWAP)`, so
//! `Midna.app` is always either the complete old or the complete new bundle.
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Midna Dev never updates itself, whatever key it was built with.
pub const PUBKEY: Option<&str> = match midna_proto::paths::DEV_HOME {
    Some(_) => None,
    None => option_env!("MIDNA_UPDATE_PUBKEY"),
};
pub const CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FeedEntry {
    pub version: String,
    #[serde(default)]
    pub pub_date: String,
    #[serde(default)]
    pub notes: String,
    pub url: String,
    pub sha256: String,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default = "default_min_macos")]
    pub minimum_macos: String,
    /// `aarch64`, `x86_64` or `universal` (default).
    #[serde(default = "default_arch")]
    pub arch: String,
    pub signature: String,
}

fn default_min_macos() -> String {
    "12.0".into()
}
fn default_arch() -> String {
    "universal".into()
}

/// The exact bytes the release key signs. Keep in sync with `packaging/release-tool`.
pub fn signed_message(e: &FeedEntry) -> String {
    format!("midna-update-v1\nversion={}\nsha256={}\nminimum_macos={}\narch={}\n", e.version, e.sha256.to_ascii_lowercase(), e.minimum_macos, e.arch)
}

pub fn feed_url(template: &str, channel: &str) -> String {
    template.replace("{channel}", channel)
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(15))
        .timeout_read(std::time::Duration::from_secs(60))
        .user_agent(&format!("midna/{}", midna_proto::VERSION))
        .build()
}

pub fn fetch_feed(url: &str) -> Result<FeedEntry, String> {
    let resp = agent().get(url).call().map_err(|e| format!("feed {url}: {e}"))?;
    let mut body = String::new();
    resp.into_reader().take(1 << 20).read_to_string(&mut body).map_err(|e| e.to_string())?;
    serde_json::from_str(&body).map_err(|e| format!("feed {url}: {e}"))
}

pub fn verify_signature(e: &FeedEntry, pubkey_b64: &str) -> Result<(), String> {
    let b64 = base64::engine::general_purpose::STANDARD;
    let key = b64.decode(pubkey_b64.trim()).map_err(|_| "embedded update key isn't base64")?;
    let sig = b64.decode(e.signature.trim()).map_err(|_| "feed signature isn't base64")?;
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
        .verify(signed_message(e).as_bytes(), &sig)
        .map_err(|_| "update signature doesn't verify with midna's key".to_string())
}

/// Strictly newer, by semver (pre-releases sort before their release).
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (semver::Version::parse(candidate), semver::Version::parse(current)) {
        (Ok(a), Ok(b)) => a > b,
        _ => false,
    }
}

fn version_tuple(v: &str) -> (u32, u32) {
    let mut it = v.split('.').map(|p| p.parse().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0))
}

pub fn arch_ok(arch: &str) -> bool {
    arch == "universal" || arch == std::env::consts::ARCH
}

/// Why `e` can't be installed here (signature, version, OS, arch), or Ok.
pub fn check_entry(e: &FeedEntry, current: &str, pubkey: &str) -> Result<bool, String> {
    verify_signature(e, pubkey)?;
    if !is_newer(&e.version, current) {
        return Ok(false);
    }
    if !arch_ok(&e.arch) {
        return Err(format!("midna {} is built for {}, this Mac is {}", e.version, e.arch, std::env::consts::ARCH));
    }
    if crate::install::macos_version() < version_tuple(&e.minimum_macos) {
        return Err(format!("midna {} needs macOS {}", e.version, e.minimum_macos));
    }
    Ok(true)
}

/// Download to `dir/<file>` while hashing; fails unless the sha256 matches the (signed) feed.
pub fn download(e: &FeedEntry, dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|err| err.to_string())?;
    let name = e.url.rsplit('/').next().filter(|n| !n.is_empty() && !n.contains("..")).unwrap_or("Midna.app.tar.gz");
    let dest = dir.join(name);
    let part = dir.join(format!("{name}.part"));
    let resp = agent().get(&e.url).call().map_err(|err| format!("download {}: {err}", e.url))?;
    let mut r = resp.into_reader();
    let mut f = std::fs::File::create(&part).map_err(|err| err.to_string())?;
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = vec![0u8; 1 << 16];
    let mut total = 0u64;
    loop {
        let n = r.read(&mut buf).map_err(|err| format!("download: {err}"))?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > 2 << 30 {
            return Err("download: larger than 2 GiB".into());
        }
        ctx.update(&buf[..n]);
        std::io::Write::write_all(&mut f, &buf[..n]).map_err(|err| err.to_string())?;
    }
    drop(f);
    let got: String = ctx.finish().as_ref().iter().map(|b| format!("{b:02x}")).collect();
    if got != e.sha256.to_ascii_lowercase() {
        let _ = std::fs::remove_file(&part);
        return Err(format!("download: sha256 mismatch (got {got})"));
    }
    std::fs::rename(&part, &dest).map_err(|err| err.to_string())?;
    Ok(dest)
}

/// Extract `archive` (`.tar.gz` / `.tgz` / `.zip`) into `dir` and validate the `Midna.app` in
/// it: bundle id, version, a valid code signature, and the same signing team as this app.
pub fn stage(archive: &Path, dir: &Path, expect_version: &str, installed: &Path) -> Result<PathBuf, String> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let name = archive.to_string_lossy();
    let st = if name.ends_with(".zip") {
        Command::new("/usr/bin/ditto").args(["-x", "-k"]).arg(archive).arg(dir).status()
    } else {
        Command::new("/usr/bin/tar").arg("-xzf").arg(archive).arg("-C").arg(dir).status()
    };
    if !st.is_ok_and(|s| s.success()) {
        return Err("couldn't extract the update".into());
    }
    let app = std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "app"))
        .ok_or("the update has no .app inside")?;
    let info = crate::install::read_plist(&app.join("Contents/Info.plist")).ok_or("the update has no Info.plist")?;
    let mine = crate::install::read_plist(&installed.join("Contents/Info.plist")).unwrap_or_default();
    let id = |v: &serde_json::Value| v.get("CFBundleIdentifier").and_then(|s| s.as_str()).unwrap_or("").to_string();
    if id(&info) != id(&mine) {
        return Err(format!("the update is {}, not {}", id(&info), id(&mine)));
    }
    let v = info.get("CFBundleShortVersionString").and_then(|s| s.as_str()).unwrap_or("");
    if v != expect_version {
        return Err(format!("the update says it is {v}, the feed said {expect_version}"));
    }
    let o = Command::new("/usr/bin/codesign").args(["--verify", "--deep", "--strict"]).arg(&app).output().map_err(|e| e.to_string())?;
    if !o.status.success() {
        return Err(format!("the update's code signature is invalid: {}", String::from_utf8_lossy(&o.stderr).trim()));
    }
    let (want, got) = (team_id(installed), team_id(&app));
    if want.is_some() && want != got {
        return Err(format!("the update is signed by {:?}, this app by {:?}", got, want));
    }
    Ok(app)
}

/// The Developer ID team of a signed bundle (`None` for ad-hoc / unsigned).
pub fn team_id(app: &Path) -> Option<String> {
    let o = Command::new("/usr/bin/codesign").args(["-dv", "--verbose=2"]).arg(app).output().ok()?;
    let text = String::from_utf8_lossy(&o.stderr).to_string();
    text.lines().find_map(|l| l.strip_prefix("TeamIdentifier=")).filter(|t| *t != "not set").map(str::to_string)
}

/// Swap `staged` into `installed` atomically. The staged copy is moved next to `installed`
/// first (same volume; a copy if it was elsewhere), then exchanged with `renamex_np`
/// `RENAME_SWAP`; the old bundle ends up at the staging path and is deleted.
pub fn apply(staged: &Path, installed: &Path) -> Result<(), String> {
    let parent = installed.parent().ok_or("app has no parent directory")?;
    let side = parent.join(format!(".{}.update-{}", installed.file_name().unwrap_or_default().to_string_lossy(), std::process::id()));
    let _ = std::fs::remove_dir_all(&side);
    if std::fs::rename(staged, &side).is_err() {
        // different volume: copy, preserving signatures/xattrs
        let st = Command::new("/usr/bin/ditto").arg(staged).arg(&side).status().map_err(|e| e.to_string())?;
        if !st.success() {
            let _ = std::fs::remove_dir_all(&side);
            return Err(format!("can't write next to {} (is the folder writable?)", installed.display()));
        }
    }
    swap(&side, installed).inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&side);
    })?;
    // `side` now holds the old bundle.
    let _ = std::fs::remove_dir_all(&side);
    Ok(())
}

fn swap(a: &Path, b: &Path) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt;
    let ca = std::ffi::CString::new(a.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let cb = std::ffi::CString::new(b.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    if unsafe { libc::renamex_np(ca.as_ptr(), cb.as_ptr(), libc::RENAME_SWAP) } == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    // Volumes without RENAME_SWAP (non-APFS): two renames with rollback. Not atomic, but the
    // window is one rename long and a failure restores the old bundle.
    if matches!(err.raw_os_error(), Some(libc::ENOTSUP) | Some(libc::EINVAL)) {
        let old = b.with_extension("app.old");
        let _ = std::fs::remove_dir_all(&old);
        std::fs::rename(b, &old).map_err(|e| e.to_string())?;
        if let Err(e) = std::fs::rename(a, b) {
            let _ = std::fs::rename(&old, b);
            return Err(e.to_string());
        }
        return std::fs::rename(&old, a).map_err(|e| e.to_string());
    }
    Err(format!("swap failed: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> FeedEntry {
        FeedEntry {
            version: "0.1.1".into(),
            pub_date: String::new(),
            notes: "n".into(),
            url: "http://x/Midna.app.tar.gz".into(),
            sha256: "AB".repeat(32),
            size: None,
            minimum_macos: "12.0".into(),
            arch: "universal".into(),
            signature: String::new(),
        }
    }

    #[test]
    fn signature_roundtrip_and_tamper() {
        use ring::signature::KeyPair;
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let kp = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let b64 = base64::engine::general_purpose::STANDARD;
        let pubkey = b64.encode(kp.public_key().as_ref());
        let mut e = entry();
        e.signature = b64.encode(kp.sign(signed_message(&e).as_bytes()).as_ref());
        assert!(verify_signature(&e, &pubkey).is_ok());
        assert_eq!(check_entry(&e, "0.1.0", &pubkey), Ok(true));
        assert_eq!(check_entry(&e, "0.1.1", &pubkey), Ok(false));
        let mut bumped = e.clone();
        bumped.version = "9.9.9".into();
        assert!(verify_signature(&bumped, &pubkey).is_err());
        let mut other = e.clone();
        other.sha256 = "cd".repeat(32);
        assert!(verify_signature(&other, &pubkey).is_err());
        // the url and notes aren't signed (the sha256 pins the bytes)
        let mut moved = e.clone();
        moved.url = "https://mirror/x.tar.gz".into();
        assert!(verify_signature(&moved, &pubkey).is_ok());
    }

    #[test]
    fn versions() {
        assert!(is_newer("0.1.1", "0.1.0"));
        assert!(is_newer("0.2.0-beta.1", "0.1.9"));
        assert!(!is_newer("0.2.0-beta.1", "0.2.0"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("garbage", "0.1.0"));
        assert_eq!(feed_url("https://h/{channel}.json", "beta"), "https://h/beta.json");
    }

    #[test]
    fn swap_exchanges_directories() {
        let tmp = std::env::temp_dir().join(format!("midna-swap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let (a, b) = (tmp.join("staged/Midna.app"), tmp.join("Apps/Midna.app"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("v"), "new").unwrap();
        std::fs::write(b.join("v"), "old").unwrap();
        apply(&a, &b).unwrap();
        assert_eq!(std::fs::read_to_string(b.join("v")).unwrap(), "new");
        assert!(!a.exists());
        let leftovers: Vec<_> = std::fs::read_dir(tmp.join("Apps")).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(leftovers.len(), 1, "{leftovers:?}");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
