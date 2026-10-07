//! "Open in Midna" on folders in Finder: a Finder Sync extension in the bundle
//! (`Contents/PlugIns/MidnaFinderSync.appex`, packaging/finder-sync) that hands the selected
//! folders to this app, switched on and off by the setting `finder.quick_action` (on by
//! default), and the folders macOS then hands the app (`on_open_urls`; also folders dropped on
//! the Dock icon).
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use gpui_kit::*;
use serde_json::json;

use crate::app::{MainWindow, refresh};

const APPEX: &str = "Contents/PlugIns/MidnaFinderSync.appex";
/// The Quick Action older builds kept in ~/Library/Services, replaced by the extension.
const WORKFLOW: &str = "Open in Midna.workflow";
/// In that workflow's script, so only a Quick Action midna wrote is removed.
const MARKER: &str = "# Added by Midna (setting finder.quick_action).";

/// Turn the extension on or off to match `finder.quick_action`. Cheap to call on every settings
/// change: it only acts when the value changed. Each flavor (Midna, Midna Dev) has its own
/// extension (`<bundle id>.finder-sync`); dev builds outside a bundle have none.
pub fn sync(enabled: bool) {
    static LAST: Mutex<Option<bool>> = Mutex::new(None);
    let crate::install::Mode::Installed { bundle } = crate::install::mode() else {
        return;
    };
    if std::env::var("MIDNA_BACKEND").as_deref() == Ok("fake") || LAST.lock().unwrap().replace(enabled) == Some(enabled) {
        return;
    }
    std::thread::spawn(move || {
        let id = bundle_id(&bundle);
        // Only the real app owns the old Quick Action.
        if id == "com.mrgnhnt.midna" && !midna_proto::paths::is_dev() {
            let dir = std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Services").join(WORKFLOW));
            match dir.map(|d| uninstall(&d)) {
                // Refresh the Services menus now rather than at next login.
                Some(Ok(true)) => drop(std::process::Command::new("/System/Library/CoreServices/pbs").arg("-update").status()),
                Some(Err(e)) => crate::lifecycle::log(&format!("finder quick action: {e}")),
                _ => {}
            }
        }
        let appex = bundle.join(APPEX);
        if !appex.exists() {
            return;
        }
        // Register it (LaunchServices may not have scanned a freshly installed bundle yet), then
        // elect it, as the switch in System Settings › Login Items & Extensions › Finder does.
        let pluginkit = |args: &[&str], path: Option<&Path>| {
            let mut c = std::process::Command::new("/usr/bin/pluginkit");
            c.args(args);
            if let Some(p) = path {
                c.arg(p);
            }
            match c.output() {
                Ok(o) if o.status.success() => {}
                Ok(o) => crate::lifecycle::log(&format!("finder extension: pluginkit {args:?}: {}", String::from_utf8_lossy(&o.stderr).trim())),
                Err(e) => crate::lifecycle::log(&format!("finder extension: pluginkit: {e}")),
            }
        };
        pluginkit(&["-a"], Some(&appex));
        pluginkit(&["-e", if enabled { "use" } else { "ignore" }, "-i", &format!("{id}.finder-sync")], None);
    });
}

fn bundle_id(bundle: &Path) -> String {
    std::process::Command::new("/usr/bin/plutil")
        .args(["-extract", "CFBundleIdentifier", "raw", "-o", "-"])
        .arg(bundle.join("Contents/Info.plist"))
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "com.mrgnhnt.midna".into())
}

/// Remove the old workflow if it's ours. Ok(true) = it was there.
fn uninstall(dir: &Path) -> std::io::Result<bool> {
    if !std::fs::read_to_string(dir.join("Contents/document.wflow")).is_ok_and(|b| b.contains(MARKER)) {
        return Ok(false);
    }
    std::fs::remove_dir_all(dir)?;
    Ok(true)
}

// ------------------------------------------------------------------ folders handed to the app

/// The folder a URL from macOS names (a file: its folder): a `file://` URL, or the Finder
/// extension's `<bundle id>://open?path=<path>` (the sandbox keeps it from sending file URLs).
/// None for other URLs.
pub fn folder_of(url: &str) -> Option<String> {
    let raw = match url.strip_prefix("file://") {
        Some(p) => p,
        None => {
            let (scheme, rest) = url.split_once("://open?")?;
            if !scheme.starts_with("com.mrgnhnt.midna") {
                return None;
            }
            rest.split('&').find_map(|kv| kv.strip_prefix("path="))?
        }
    };
    let path = PathBuf::from(percent_decode(raw));
    let dir = if path.is_dir() { path } else { path.parent()?.to_path_buf() };
    let s = dir.to_string_lossy().trim_end_matches('/').to_string();
    Some(if s.is_empty() { "/".into() } else { s })
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        match (b[i], b.get(i + 1).and_then(|&c| hex(c)), b.get(i + 2).and_then(|&c| hex(c))) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (c, _, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Show a terminal in `dir`: a shell already there (not a background one), else a new one (in
/// the project containing `dir`, or a new project for it), in the main window you're using.
pub fn open(dir: String, cx: &mut App) {
    crate::lifecycle::log(&format!("open folder: {dir}"));
    cx.activate(true);
    crate::windows::with_active(cx, move |m, window, cx| {
        window.activate_window();
        let existing = m.sessions.iter().find(|s| !s.background && s.kind == crate::model::SessionKind::Shell && s.cwd.trim_end_matches('/') == dir).map(|s| s.id.clone());
        if let Some(id) = existing {
            cx.defer(move |cx| crate::windows::reveal(id, cx));
            return;
        }
        open_shell(m, dir, cx);
    });
}

fn open_shell(m: &mut MainWindow, dir: String, cx: &mut Context<MainWindow>) {
    m.rpc("session.open", json!({"kind": "shell", "cwd": dir}), cx, |m, v, window, cx| {
        let id = v.get("id").and_then(|x| x.as_str()).or_else(|| v.get("session").and_then(|s| s.get("id")).and_then(|x| x.as_str()));
        m.request_refresh(refresh::PROJECTS | refresh::SESSIONS, cx);
        if let Some(id) = id {
            m.select(id.to_string(), window, cx);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{folder_of, percent_decode, uninstall, MARKER};

    #[test]
    fn file_urls_become_folders() {
        assert_eq!(percent_decode("/a%20b/%E2%9C%93"), "/a b/✓");
        assert_eq!(percent_decode("/100%"), "/100%");
        let tmp = std::env::temp_dir().canonicalize().unwrap();
        let t = tmp.to_string_lossy().trim_end_matches('/').to_string();
        assert_eq!(folder_of(&format!("file://{t}/")).as_deref(), Some(t.as_str()));
        assert_eq!(folder_of(&format!("file://{t}/missing.txt")).as_deref(), Some(t.as_str()));
        assert_eq!(folder_of("https://example.com"), None);
        assert_eq!(folder_of(&format!("com.mrgnhnt.midna.dev://open?path={t}/missing%20file.txt")).as_deref(), Some(t.as_str()));
        assert_eq!(folder_of(&format!("com.mrgnhnt.midna://open?path={t}")).as_deref(), Some(t.as_str()));
        assert_eq!(folder_of(&format!("other://open?path={t}")), None);
    }

    #[test]
    fn uninstall_only_removes_our_old_workflow() {
        let dir = std::env::temp_dir().join(format!("midna-finder-{}", std::process::id())).join("Open in Midna.workflow");
        std::fs::create_dir_all(dir.join("Contents")).unwrap();
        std::fs::write(dir.join("Contents/document.wflow"), format!("<string>{MARKER}\nopen -a …</string>")).unwrap();
        assert_eq!(uninstall(&dir).unwrap(), true);
        assert!(!dir.exists());
        assert_eq!(uninstall(&dir).unwrap(), false);
        // Someone else's workflow of the same name stays.
        std::fs::create_dir_all(dir.join("Contents")).unwrap();
        std::fs::write(dir.join("Contents/document.wflow"), "theirs").unwrap();
        assert_eq!(uninstall(&dir).unwrap(), false);
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }
}
