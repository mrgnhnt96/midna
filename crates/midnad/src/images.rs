//! Images attached to `session.input`. Claude Code and Codex both turn a pasted image path into
//! an attachment (`[Image #1]`), so an image is delivered as its path, one bracketed paste
//! each, the same way the app's image sheet does it (`midna-app/src/annotate.rs`).
//!
//! Every image is copied to `$MIDNA_HOME/images/` first: the pasted path is then plain ASCII
//! with no spaces (which the agents' path detection can trip on), and it outlives a caller's
//! temp file. PNG, JPEG, GIF and WebP are copied as they are; anything else macOS can read
//! (HEIC, TIFF, …) is converted to PNG with `sips`. Copies older than a week are pruned.
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const KEEP: Duration = Duration::from_secs(7 * 24 * 3600);
/// Bigger than any screenshot; a guard against pasting a disk image by mistake.
const MAX_BYTES: u64 = 50 * 1024 * 1024;

/// The format the agents accept as-is, from the file's first bytes.
fn sniff(head: &[u8]) -> Option<&'static str> {
    match head {
        [0x89, b'P', b'N', b'G', ..] => Some("png"),
        [0xFF, 0xD8, 0xFF, ..] => Some("jpg"),
        [b'G', b'I', b'F', b'8', ..] => Some("gif"),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some("webp"),
        _ => None,
    }
}

/// `My Screenshot (2).png` -> `My-Screenshot-2`: the stem, safe to paste unquoted.
fn safe_stem(path: &Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("image");
    let mut out = String::new();
    for c in stem.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out: String = out.trim_matches(|c| c == '-' || c == '.').chars().take(48).collect();
    if out.is_empty() { "image".into() } else { out }
}

/// Copy (or convert) `src` into `dir`, returning the path to paste.
pub fn prepare(dir: &Path, src: &str) -> Result<PathBuf, String> {
    let path = Path::new(src);
    if !path.is_absolute() {
        return Err(format!("image path must be absolute: {src}"));
    }
    let meta = std::fs::metadata(path).map_err(|e| format!("can't read image {src}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("not a file: {src}"));
    }
    if meta.len() > MAX_BYTES {
        return Err(format!("image is too large ({} MB, limit {} MB): {src}", meta.len() / (1024 * 1024), MAX_BYTES / (1024 * 1024)));
    }
    let mut head = [0u8; 16];
    let n = std::io::Read::read(&mut std::fs::File::open(path).map_err(|e| format!("can't read image {src}: {e}"))?, &mut head).unwrap_or(0);
    std::fs::create_dir_all(dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    prune(dir);
    let stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_nanos() % 1_000_000_000_000;
    let base = format!("{stamp:012}-{}", safe_stem(path));
    match sniff(&head[..n]) {
        Some(ext) => {
            let dst = dir.join(format!("{base}.{ext}"));
            std::fs::copy(path, &dst).map_err(|e| format!("can't copy image {src}: {e}"))?;
            Ok(dst)
        }
        None => {
            let dst = dir.join(format!("{base}.png"));
            let ok = std::process::Command::new("/usr/bin/sips")
                .args(["-s", "format", "png"])
                .arg(path)
                .arg("--out")
                .arg(&dst)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if ok && dst.is_file() {
                Ok(dst)
            } else {
                let _ = std::fs::remove_file(&dst);
                Err(format!("not an image (PNG, JPEG, GIF, WebP, or something sips can convert): {src}"))
            }
        }
    }
}

/// Drop copies older than [`KEEP`].
fn prune(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let now = SystemTime::now();
    for e in rd.flatten() {
        let old = e.metadata().and_then(|m| m.modified()).is_ok_and(|t| now.duration_since(t).unwrap_or_default() > KEEP);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_formats() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n"), Some("png"));
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("jpg"));
        assert_eq!(sniff(b"GIF89a"), Some("gif"));
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), Some("webp"));
        assert_eq!(sniff(b"%PDF-1.7"), None);
    }

    #[test]
    fn stems_are_safe_to_paste() {
        assert_eq!(safe_stem(Path::new("/x/My Screenshot (2).png")), "My-Screenshot-2");
        assert_eq!(safe_stem(Path::new("/x/日本.png")), "image");
        assert_eq!(safe_stem(Path::new("/x/a.b.png")), "a.b");
    }

    #[test]
    fn copies_and_refuses() {
        let dir = std::env::temp_dir().join(format!("midna-images-test-{}", std::process::id()));
        let src = dir.join("in put.png");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&src, b"\x89PNG\r\n\x1a\nrest").unwrap();
        let out = prepare(&dir.join("out"), src.to_str().unwrap()).unwrap();
        assert!(out.to_str().unwrap().ends_with("-in-put.png"), "{}", out.display());
        assert_eq!(std::fs::read(&out).unwrap(), b"\x89PNG\r\n\x1a\nrest");
        assert!(prepare(&dir.join("out"), "relative.png").unwrap_err().contains("absolute"));
        assert!(prepare(&dir.join("out"), dir.join("missing.png").to_str().unwrap()).is_err());
        let txt = dir.join("notes.txt");
        std::fs::write(&txt, b"hello").unwrap();
        assert!(prepare(&dir.join("out"), txt.to_str().unwrap()).unwrap_err().contains("not an image"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
