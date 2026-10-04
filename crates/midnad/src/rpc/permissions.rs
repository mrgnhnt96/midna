//! `permissions.status`: the macOS permissions midna uses, as far as midnad can see them.
//! Only the human can grant them; each entry carries the `midna permissions open <name>`
//! command that opens the right System Settings pane.
use super::{R, ok};
use crate::daemon::Daemon;
use midna_proto::*;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

/// The System Settings URL for a permission name (shared with the CLI's `permissions open`).
pub fn pane_url(name: &str) -> Option<&'static str> {
    Some(match name {
        "accessibility" => "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
        "notifications" => "x-apple.systempreferences:com.apple.preference.notifications",
        "login-items" => "x-apple.systempreferences:com.apple.LoginItems-Settings.extension",
        _ => return None,
    })
}

pub fn status(_d: &Daemon) -> R {
    let ax = unsafe { AXIsProcessTrusted() };
    // launchd sets XPC_SERVICE_NAME to the job label for processes it starts.
    let label = std::env::var("XPC_SERVICE_NAME").ok().filter(|l| l.contains("midna"));
    let plist = std::env::var_os("HOME")
        .map(|h| std::path::PathBuf::from(h).join("Library/LaunchAgents/com.mrgnhnt.midna.daemon.plist"))
        .filter(|p| p.exists());
    let login = match (&label, &plist) {
        (Some(l), _) => ("enabled", format!("midnad runs as launchd job {l}, so it starts at login")),
        (None, Some(p)) => ("enabled", format!("legacy LaunchAgent {} exists", p.display())),
        (None, None) => ("disabled", "midnad was not started by launchd (dev run or not installed); the app registers the login item".into()),
    };
    let entry = |name: &str, state: &str, detail: String| PermissionEntry {
        name: name.into(),
        state: state.into(),
        detail,
        open_cli: format!("midna permissions open {name}"),
    };
    ok(PermissionsStatus {
        permissions: vec![
            entry(
                "accessibility",
                if ax { "granted" } else { "unknown" },
                if ax {
                    "midnad itself is trusted for Accessibility".into()
                } else {
                    "midnad is not trusted; the grant that matters belongs to Midna.app, which only the app can check (Settings → Permissions)".into()
                },
            ),
            entry("notifications", "not_requested", "midna posts no notifications yet, so there is nothing to grant".into()),
            entry("login-items", login.0, login.1),
        ],
    })
}
