//! "Open in Midna" on folders in Finder: a Quick Action in `~/Library/Services` (setting
//! `finder.quick_action`, on by default) that runs `open -a <this Midna.app> <folders>`, and the
//! folders macOS then hands the app (`on_open_urls`; also folders dropped on the Dock icon).
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use gpui_kit::*;
use serde_json::json;

use crate::app::{MainWindow, refresh};

const WORKFLOW: &str = "Open in Midna.workflow";
/// In the workflow's script, so `sync` only ever removes a Quick Action it wrote.
const MARKER: &str = "# Added by Midna (setting finder.quick_action).";

/// Install or remove the Quick Action to match `finder.quick_action`. Cheap to call on every
/// settings change: it only touches the disk when the value changed, and each launch rewrites the
/// workflow if the app has moved. Dev builds leave it alone
/// (the action would open the installed Midna, not this one).
pub fn sync(enabled: bool) {
    static LAST: Mutex<Option<bool>> = Mutex::new(None);
    let crate::install::Mode::Installed { bundle } = crate::install::mode() else {
        return;
    };
    if std::env::var("MIDNA_BACKEND").as_deref() == Ok("fake") || LAST.lock().unwrap().replace(enabled) == Some(enabled) {
        return;
    }
    std::thread::spawn(move || {
        let Some(dir) = std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Services").join(WORKFLOW)) else {
            return;
        };
        let changed = if enabled { install(&dir, &bundle, &bundle_id(&bundle)) } else { uninstall(&dir) };
        match changed {
            Ok(false) => {}
            // Refresh the Services / Quick Actions menus now rather than at next login.
            Ok(true) => drop(std::process::Command::new("/System/Library/CoreServices/pbs").arg("-update").status()),
            Err(e) => crate::lifecycle::log(&format!("finder quick action: {e}")),
        }
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

/// Write the workflow if it's missing or different. Ok(true) = it changed.
fn install(dir: &Path, bundle: &Path, id: &str) -> std::io::Result<bool> {
    let files = [("Contents/Info.plist", INFO_PLIST.to_string()), ("Contents/document.wflow", document(bundle, id))];
    if files.iter().all(|(f, body)| std::fs::read_to_string(dir.join(f)).is_ok_and(|b| &b == body)) {
        return Ok(false);
    }
    std::fs::create_dir_all(dir.join("Contents"))?;
    for (f, body) in files {
        std::fs::write(dir.join(f), body)?;
    }
    Ok(true)
}

/// Remove the workflow if it's ours. Ok(true) = it was there.
fn uninstall(dir: &Path) -> std::io::Result<bool> {
    if !std::fs::read_to_string(dir.join("Contents/document.wflow")).is_ok_and(|b| b.contains(MARKER)) {
        return Ok(false);
    }
    std::fs::remove_dir_all(dir)?;
    Ok(true)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The Automator document: one "Run Shell Script" action, folders passed as arguments. It opens
/// this bundle by path, else (moved or deleted since) whichever copy has our bundle id.
fn document(bundle: &Path, id: &str) -> String {
    let (app, id) = (sh_quote(&bundle.to_string_lossy()), sh_quote(id));
    let script = xml_escape(&format!("{MARKER}\nopen -a {app} \"$@\" 2>/dev/null || open -b {id} \"$@\""));
    DOCUMENT.replace("{SCRIPT}", &script)
}

const INFO_PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>NSServices</key>
	<array>
		<dict>
			<key>NSMenuItem</key>
			<dict><key>default</key><string>Open in Midna</string></dict>
			<key>NSMessage</key><string>runWorkflowAsService</string>
			<key>NSRequiredContext</key>
			<dict><key>NSApplicationIdentifier</key><string>com.apple.finder</string></dict>
			<key>NSSendFileTypes</key>
			<array><string>public.folder</string></array>
		</dict>
	</array>
</dict>
</plist>
"#;

const DOCUMENT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>AMApplicationBuild</key><string>523</string>
	<key>AMApplicationVersion</key><string>2.10</string>
	<key>AMDocumentVersion</key><string>2</string>
	<key>actions</key>
	<array>
		<dict>
			<key>action</key>
			<dict>
				<key>AMAccepts</key>
				<dict>
					<key>Container</key><string>List</string>
					<key>Optional</key><true/>
					<key>Types</key><array><string>com.apple.cocoa.string</string></array>
				</dict>
				<key>AMActionVersion</key><string>2.0.3</string>
				<key>AMApplication</key><array><string>Automator</string></array>
				<key>AMParameterProperties</key>
				<dict>
					<key>COMMAND_STRING</key><dict/>
					<key>CheckedForUserDefaultShell</key><dict/>
					<key>inputMethod</key><dict/>
					<key>shell</key><dict/>
					<key>source</key><dict/>
				</dict>
				<key>AMProvides</key>
				<dict>
					<key>Container</key><string>List</string>
					<key>Types</key><array><string>com.apple.cocoa.string</string></array>
				</dict>
				<key>ActionBundlePath</key><string>/System/Library/Automator/Run Shell Script.action</string>
				<key>ActionName</key><string>Run Shell Script</string>
				<key>ActionParameters</key>
				<dict>
					<key>COMMAND_STRING</key><string>{SCRIPT}</string>
					<key>CheckedForUserDefaultShell</key><true/>
					<key>inputMethod</key><integer>1</integer>
					<key>shell</key><string>/bin/sh</string>
					<key>source</key><string></string>
				</dict>
				<key>BundleIdentifier</key><string>com.apple.RunShellScript</string>
				<key>CFBundleVersion</key><string>2.0.3</string>
				<key>CanShowSelectedItemsWhenRun</key><false/>
				<key>CanShowWhenRun</key><true/>
				<key>Category</key><array><string>AMCategoryUtilities</string></array>
				<key>Class Name</key><string>RunShellScriptAction</string>
				<key>InputUUID</key><string>5A6C2E0B-6E1D-4C55-9C2B-1D2B5E1A0001</string>
				<key>Keywords</key><array><string>Shell</string><string>Script</string></array>
				<key>OutputUUID</key><string>5A6C2E0B-6E1D-4C55-9C2B-1D2B5E1A0002</string>
				<key>UUID</key><string>5A6C2E0B-6E1D-4C55-9C2B-1D2B5E1A0003</string>
				<key>UnlocalizedApplications</key><array><string>Automator</string></array>
				<key>arguments</key><dict/>
				<key>isViewVisible</key><integer>1</integer>
				<key>location</key><string>309.000000:253.000000</string>
				<key>nibPath</key><string>/System/Library/Automator/Run Shell Script.action/Contents/Resources/Base.lproj/main.nib</string>
			</dict>
			<key>isViewVisible</key><integer>1</integer>
		</dict>
	</array>
	<key>connectors</key><dict/>
	<key>workflowMetaData</key>
	<dict>
		<key>applicationBundleID</key><string>com.apple.finder</string>
		<key>applicationBundleIDsByPath</key>
		<dict><key>/System/Library/CoreServices/Finder.app</key><string>com.apple.finder</string></dict>
		<key>applicationPath</key><string>/System/Library/CoreServices/Finder.app</string>
		<key>applicationPaths</key><array><string>/System/Library/CoreServices/Finder.app</string></array>
		<key>inputTypeIdentifier</key><string>com.apple.Automator.fileSystemObject.folder</string>
		<key>outputTypeIdentifier</key><string>com.apple.Automator.nothing</string>
		<key>presentationMode</key><integer>15</integer>
		<key>processesInput</key><false/>
		<key>serviceApplicationBundleID</key><string>com.apple.finder</string>
		<key>serviceApplicationPath</key><string>/System/Library/CoreServices/Finder.app</string>
		<key>serviceInputTypeIdentifier</key><string>com.apple.Automator.fileSystemObject.folder</string>
		<key>serviceOutputTypeIdentifier</key><string>com.apple.Automator.nothing</string>
		<key>serviceProcessesInput</key><false/>
		<key>systemImageName</key><string>NSActionTemplate</string>
		<key>useAutomaticInputType</key><false/>
		<key>workflowTypeIdentifier</key><string>com.apple.Automator.servicesMenu</string>
	</dict>
</dict>
</plist>
"#;

// ------------------------------------------------------------------ folders handed to the app

/// The folder a `file://` URL from macOS names (a file: its folder). None for other URLs.
pub fn folder_of(url: &str) -> Option<String> {
    let path = PathBuf::from(percent_decode(url.strip_prefix("file://")?));
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
    use super::{document, folder_of, install, percent_decode, uninstall, MARKER};

    #[test]
    fn file_urls_become_folders() {
        assert_eq!(percent_decode("/a%20b/%E2%9C%93"), "/a b/✓");
        assert_eq!(percent_decode("/100%"), "/100%");
        let tmp = std::env::temp_dir().canonicalize().unwrap();
        let t = tmp.to_string_lossy().trim_end_matches('/').to_string();
        assert_eq!(folder_of(&format!("file://{t}/")).as_deref(), Some(t.as_str()));
        assert_eq!(folder_of(&format!("file://{t}/missing.txt")).as_deref(), Some(t.as_str()));
        assert_eq!(folder_of("https://example.com"), None);
    }

    #[test]
    fn workflow_script_opens_the_bundle() {
        let d = document(std::path::Path::new("/Applications/Mor'gan's Midna.app"), "com.mrgnhnt.midna");
        assert!(d.contains(MARKER));
        assert!(d.contains("open -a '/Applications/Mor'\\''gan'\\''s Midna.app' &quot;$@&quot; 2&gt;/dev/null || open -b 'com.mrgnhnt.midna' &quot;$@&quot;"));
    }

    #[test]
    fn install_is_idempotent_and_uninstall_only_removes_ours() {
        let dir = std::env::temp_dir().join(format!("midna-finder-{}", std::process::id())).join("Open in Midna.workflow");
        let app = std::path::Path::new("/Applications/Midna.app");
        assert_eq!(install(&dir, app, "com.mrgnhnt.midna").unwrap(), true);
        assert_eq!(install(&dir, app, "com.mrgnhnt.midna").unwrap(), false);
        for f in ["Contents/Info.plist", "Contents/document.wflow"] {
            assert!(std::process::Command::new("/usr/bin/plutil").arg("-lint").arg(dir.join(f)).status().unwrap().success(), "{f}");
        }
        assert_eq!(install(&dir, std::path::Path::new("/Applications/Moved.app"), "com.mrgnhnt.midna").unwrap(), true);
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
