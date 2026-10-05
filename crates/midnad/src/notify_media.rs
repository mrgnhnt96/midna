//! Notification sounds and images: the macOS sounds plus what the human imported into
//! `MIDNA_HOME/notify/{sounds,images}` (`notify.import`). Settings name a file by its file name
//! (`notify.sound.<kind>`, `notify.image[.<kind>]`); the notifier resolves names to paths.
use crate::daemon::Daemon;
use midna_proto::notify::{IMAGE_EXTS, SOUND_EXTS, SYSTEM_SOUNDS, system_sound_path};
use midna_proto::settings::SETTINGS;
use midna_proto::*;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Imports larger than this are refused (a notification sound is a second or two).
const MAX_BYTES: u64 = 10 * 1024 * 1024;

pub fn dir(home: &Path) -> PathBuf {
    home.join("notify")
}

fn kind_dir(home: &Path, kind: &str) -> PathBuf {
    dir(home).join(if kind == "sound" { "sounds" } else { "images" })
}

/// `sound` or `image`, from the file extension.
fn kind_of(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if SOUND_EXTS.contains(&ext.as_str()) {
        Some("sound")
    } else if IMAGE_EXTS.contains(&ext.as_str()) {
        Some("image")
    } else {
        None
    }
}

/// The file really is the format its extension says (a renamed text file plays nothing).
fn sniff(path: &Path, b: &[u8]) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let at = |i: usize, m: &[u8]| b.get(i..i + m.len()) == Some(m);
    match ext.as_str() {
        "png" => at(0, b"\x89PNG"),
        "jpg" | "jpeg" => at(0, b"\xFF\xD8\xFF"),
        "gif" => at(0, b"GIF8"),
        "wav" => at(0, b"RIFF") && at(8, b"WAVE"),
        "aiff" | "aif" => at(0, b"FORM") && (at(8, b"AIFF") || at(8, b"AIFC")),
        "caf" => at(0, b"caff"),
        "m4a" => at(4, b"ftyp"),
        "mp3" => at(0, b"ID3") || (b.len() > 1 && b[0] == 0xFF && b[1] & 0xE0 == 0xE0),
        _ => false,
    }
}

/// An imported file's name: a plain file name, nothing that walks out of its folder.
fn valid_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('.') && !name.contains(['/', '\\', '\0']) && kind_of(Path::new(name)).is_some()
}

/// The file a sound setting's value plays (None: silent, or missing).
pub fn sound_path(home: &Path, value: &str) -> Option<PathBuf> {
    if let Some(p) = system_sound_path(value) {
        return Some(PathBuf::from(p));
    }
    (valid_name(value) && kind_of(Path::new(value)) == Some("sound")).then(|| kind_dir(home, "sound").join(value)).filter(|p| p.is_file())
}

/// The file an image setting's value shows (None: no image, or missing).
pub fn image_path(home: &Path, value: &str) -> Option<PathBuf> {
    (valid_name(value) && kind_of(Path::new(value)) == Some("image")).then(|| kind_dir(home, "image").join(value)).filter(|p| p.is_file())
}

/// Longer notification sounds make macOS play its default sound instead.
const MAX_SECS: usize = 30;
/// Rendered sounds unused for this long are deleted (rendering one again is cheap).
const CACHE_DAYS: u64 = 30;

/// `file` at `volume` (1–100) as the 16-bit WAV a notification names, so macOS plays it with
/// the banner (`UNNotificationSound` has no volume of its own). Converted with `afconvert`,
/// scaled, cut to 30 s, and kept in `notify/cache` by file and volume. None if that fails.
pub fn rendered(home: &Path, file: &str, volume: u8) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    let meta = std::fs::metadata(file).ok()?;
    let mut h = std::hash::DefaultHasher::new();
    (file, meta.len(), meta.modified().ok()).hash(&mut h);
    let cache = dir(home).join("cache");
    let out = cache.join(format!("{:016x}-{volume}.wav", h.finish()));
    if out.is_file() {
        // Touched so pruning keeps what's in use.
        let _ = std::fs::File::options().append(true).open(&out).and_then(|f| f.set_modified(std::time::SystemTime::now()));
        return Some(out);
    }
    std::fs::create_dir_all(&cache).ok()?;
    prune(&cache);
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp = cache.join(format!(".{}-{}.wav", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    let converted = std::process::Command::new("/usr/bin/afconvert")
        .args(["-f", "WAVE", "-d", "LEI16"])
        .arg(file)
        .arg(&tmp)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    let done = converted
        .then(|| std::fs::read(&tmp).ok())
        .flatten()
        .and_then(|b| scale_wav(b, volume))
        .and_then(|b| std::fs::write(&tmp, b).ok())
        .and_then(|_| std::fs::rename(&tmp, &out).ok());
    if done.is_none() {
        let _ = std::fs::remove_file(&tmp);
    }
    done.map(|_| out)
}

fn prune(cache: &Path) {
    let old = std::time::Duration::from_secs(CACHE_DAYS * 24 * 3600);
    for e in std::fs::read_dir(cache).into_iter().flatten().flatten() {
        if e.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()).is_some_and(|age| age > old) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// A 16-bit PCM WAV with its samples scaled to `volume` percent and cut to `MAX_SECS`.
fn scale_wav(mut b: Vec<u8>, volume: u8) -> Option<Vec<u8>> {
    if !(b.starts_with(b"RIFF") && b.get(8..12) == Some(b"WAVE")) {
        return None;
    }
    let u16_at = |b: &[u8], i: usize| Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?));
    let u32_at = |b: &[u8], i: usize| Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?));
    let (mut i, mut fmt) = (12, None);
    while i + 8 <= b.len() {
        let len = u32_at(&b, i + 4)? as usize;
        let body = i + 8;
        match &b[i..i + 4] {
            b"fmt " => fmt = Some((u16_at(&b, body + 2)?, u32_at(&b, body + 4)?, u16_at(&b, body + 14)?)),
            b"data" => {
                let (channels, rate, bits) = fmt?;
                if bits != 16 {
                    return None;
                }
                let n = len.min(b.len() - body).min(MAX_SECS * rate as usize * channels as usize * 2) & !1;
                let gain = volume.min(100) as i32;
                for s in b[body..body + n].chunks_exact_mut(2) {
                    let v = i16::from_le_bytes([s[0], s[1]]) as i32 * gain / 100;
                    s.copy_from_slice(&(v as i16).to_le_bytes());
                }
                b.truncate(body + n);
                b[i + 4..i + 8].copy_from_slice(&(n as u32).to_le_bytes());
                let riff = (b.len() - 8) as u32;
                b[4..8].copy_from_slice(&riff.to_le_bytes());
                return Some(b);
            }
            _ => {}
        }
        i = body + len + (len & 1);
    }
    None
}

/// Sound and image settings only take what exists (`settings.set` calls this).
pub fn check_setting(home: &Path, key: &str, value: &Value) -> Result<(), String> {
    let v = value.as_str().unwrap_or("");
    if key.starts_with("notify.sound.") {
        if v == "none" || SYSTEM_SOUNDS.contains(&v) || sound_path(home, v).is_some() {
            return Ok(());
        }
        return Err(format!("no sound `{v}`: use none, a macOS sound ({}) or a sound from `midna notify media` (import one with `midna notify import <file>`)", SYSTEM_SOUNDS.join(", ")));
    }
    if key == "notify.image" || key.starts_with("notify.image.") {
        if v.is_empty() || v == "none" || image_path(home, v).is_some() {
            return Ok(());
        }
        return Err(format!("no image `{v}`: use an image from `midna notify media` (import one with `midna notify import <file>`), none, or empty"));
    }
    Ok(())
}

/// Settings whose value is `name`.
pub fn used_by(d: &Daemon, kind: &str, name: &str) -> Vec<String> {
    let core = d.core();
    SETTINGS
        .iter()
        .filter(|s| if kind == "sound" { s.key.starts_with("notify.sound.") } else { s.key == "notify.image" || s.key.starts_with("notify.image.") })
        .filter(|s| core.state.setting(s.key).as_str() == Some(name))
        .map(|s| s.key.to_string())
        .collect()
}

fn imported(home: &Path, kind: &str) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = std::fs::read_dir(kind_dir(home, kind))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            (valid_name(&name) && kind_of(Path::new(&name)) == Some(kind) && e.path().is_file()).then(|| (name, e.path()))
        })
        .collect();
    out.sort_by_key(|(n, _)| n.to_lowercase());
    out
}

pub fn list(d: &Daemon, kind: Option<&str>) -> NotifyMediaResult {
    let home = &d.cfg.home;
    let media = |kind: &str, name: String, path: String, builtin: bool| NotifyMedia { kind: kind.into(), used_by: used_by(d, kind, &name), name, path, builtin };
    let sounds = if kind.is_none_or(|k| k == "sound") {
        let system = SYSTEM_SOUNDS.iter().filter_map(|n| system_sound_path(n).map(|p| media("sound", n.to_string(), p, true)));
        system.chain(imported(home, "sound").into_iter().map(|(n, p)| media("sound", n, p.to_string_lossy().into_owned(), false))).collect()
    } else {
        vec![]
    };
    let images = if kind.is_none_or(|k| k == "image") {
        imported(home, "image").into_iter().map(|(n, p)| media("image", n, p.to_string_lossy().into_owned(), false)).collect()
    } else {
        vec![]
    };
    NotifyMediaResult { sounds, images, dir: dir(home).to_string_lossy().into_owned() }
}

/// Copy a file in. The same file imported twice is one file; a different file with the same
/// name gets `-2`, `-3`, … before its extension.
pub fn import(d: &Daemon, actor: Actor, path: &str) -> Result<NotifyMedia, RpcError> {
    let src = match path.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME").map(|h| PathBuf::from(h).join(rest)).unwrap_or_else(|| PathBuf::from(path)),
        None => PathBuf::from(path),
    };
    if !src.is_absolute() {
        return Err(RpcError::bad_params(format!("`{path}` is not an absolute path")));
    }
    let Some(kind) = kind_of(&src) else {
        return Err(RpcError::bad_params(format!("`{path}`: sounds must be {} and images {}", SOUND_EXTS.join(", "), IMAGE_EXTS.join(", "))));
    };
    let meta = std::fs::metadata(&src).map_err(|e| RpcError::not_found(format!("{path}: {e}")))?;
    if !meta.is_file() {
        return Err(RpcError::bad_params(format!("`{path}` is not a file")));
    }
    if meta.len() > MAX_BYTES {
        return Err(RpcError::bad_params(format!("`{path}` is {} MB; notification {kind}s can be up to 10 MB", meta.len() / (1024 * 1024))));
    }
    let bytes = std::fs::read(&src).map_err(|e| RpcError::internal(format!("{path}: {e}")))?;
    if !sniff(&src, &bytes) {
        return Err(RpcError::bad_params(format!("`{path}` isn't a {} file", src.extension().and_then(|e| e.to_str()).unwrap_or("?"))));
    }
    let folder = kind_dir(&d.cfg.home, kind);
    std::fs::create_dir_all(&folder).map_err(|e| RpcError::internal(format!("{}: {e}", folder.display())))?;
    let file = src.file_name().map(|n| n.to_string_lossy().replace(['\\', '\0'], "_")).unwrap_or_default();
    let file = file.trim_start_matches('.').to_string();
    let (stem, ext) = file.rsplit_once('.').unwrap_or((&file, ""));
    let mut name = file.clone();
    for n in 2.. {
        let dest = folder.join(&name);
        match std::fs::read(&dest) {
            Ok(old) if old == bytes => break,
            Ok(_) => name = format!("{stem}-{n}.{ext}"),
            Err(_) => {
                std::fs::write(&dest, &bytes).map_err(|e| RpcError::internal(format!("{}: {e}", dest.display())))?;
                d.emit(kinds::NOTIFY_MEDIA, actor, None, None, json!({ "action": "imported", "kind": kind, "name": name }));
                break;
            }
        }
    }
    let path = folder.join(&name).to_string_lossy().into_owned();
    Ok(NotifyMedia { kind: kind.into(), used_by: used_by(d, kind, &name), name, path, builtin: false })
}

/// Delete an imported file; returns the settings that used it (the caller resets them).
pub fn remove(d: &Daemon, actor: Actor, name: &str) -> Result<(String, Vec<String>), RpcError> {
    if SYSTEM_SOUNDS.contains(&name) {
        return Err(RpcError::bad_params(format!("`{name}` is a macOS sound; only imported files can be removed")));
    }
    let kind = kind_of(Path::new(name)).filter(|_| valid_name(name)).ok_or_else(|| RpcError::not_found(format!("no imported sound or image `{name}`")))?;
    let file = kind_dir(&d.cfg.home, kind).join(name);
    if !file.is_file() {
        return Err(RpcError::not_found(format!("no imported {kind} `{name}`; see `midna notify media`")));
    }
    let users = used_by(d, kind, name);
    std::fs::remove_file(&file).map_err(|e| RpcError::internal(format!("{}: {e}", file.display())))?;
    d.emit(kinds::NOTIFY_MEDIA, actor, None, None, json!({ "action": "removed", "kind": kind, "name": name }));
    Ok((kind.into(), users))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_formats() {
        assert!(valid_name("ding.mp3"));
        assert!(!valid_name("../ding.mp3"));
        assert!(!valid_name(".hidden.wav"));
        assert!(!valid_name("notes.txt"));
        assert!(sniff(Path::new("a.png"), b"\x89PNG\r\n"));
        assert!(!sniff(Path::new("a.png"), b"hello"));
        assert!(sniff(Path::new("a.wav"), b"RIFF\0\0\0\0WAVEfmt "));
        assert!(sniff(Path::new("a.mp3"), b"ID3\x04"));
        assert!(sniff(Path::new("a.m4a"), b"\0\0\0\x20ftypM4A "));
        let home = Path::new("/nonexistent");
        assert_eq!(sound_path(home, "Glass"), Some(PathBuf::from("/System/Library/Sounds/Glass.aiff")));
        assert_eq!(sound_path(home, "none"), None);
        assert!(check_setting(home, "notify.sound.approval", &json!("Ping")).is_ok());
        assert!(check_setting(home, "notify.sound.approval", &json!("/etc/passwd")).is_err());
        assert!(check_setting(home, "notify.image", &json!("")).is_ok());
        assert!(check_setting(home, "notify.image.failed", &json!("missing.png")).is_err());
    }

    /// A WAV with a chunk before `fmt ` and one after `data`, like afconvert can write.
    fn wav(samples: &[i16], rate: u32) -> Vec<u8> {
        let mut b = b"RIFF\0\0\0\0WAVEJUNK\x02\0\0\0xx".to_vec();
        b.extend_from_slice(b"fmt \x10\0\0\0\x01\0\x01\0");
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * 2).to_le_bytes());
        b.extend_from_slice(b"\x02\0\x10\0data");
        b.extend_from_slice(&((samples.len() * 2) as u32).to_le_bytes());
        samples.iter().for_each(|s| b.extend_from_slice(&s.to_le_bytes()));
        b.extend_from_slice(b"LIST\0\0\0\0");
        b
    }

    #[test]
    fn scales_and_cuts_sounds() {
        let out = scale_wav(wav(&[1000, -1000, 32767, -32768], 8000), 25).unwrap();
        let samples: Vec<i16> = out[out.len() - 8..].chunks(2).map(|s| i16::from_le_bytes([s[0], s[1]])).collect();
        assert_eq!(samples, [250, -250, 8191, -8192]);
        assert_eq!(u32::from_le_bytes(out[4..8].try_into().unwrap()) as usize, out.len() - 8);
        // 31 s at 1 Hz: cut to 30 samples.
        let long = scale_wav(wav(&[7; 31], 1), 100).unwrap();
        assert_eq!(long.len(), wav(&[7; 30], 1).len() - 8);
        assert!(scale_wav(b"RIFF\0\0\0\0WAVEdata\0\0\0\0".to_vec(), 50).is_none(), "no fmt chunk");
    }

    #[test]
    fn renders_system_sounds_once() {
        let home = std::env::temp_dir().join(format!("midna-render-{}", std::process::id()));
        let a = rendered(&home, "/System/Library/Sounds/Glass.aiff", 40).expect("afconvert");
        assert_eq!(rendered(&home, "/System/Library/Sounds/Glass.aiff", 40).as_ref(), Some(&a));
        assert!(std::fs::read(&a).unwrap().starts_with(b"RIFF"));
        assert_ne!(rendered(&home, "/System/Library/Sounds/Glass.aiff", 80), Some(a));
        assert_eq!(rendered(&home, "/nonexistent.wav", 40), None);
        let _ = std::fs::remove_dir_all(home);
    }
}
