//! First-launch install: where midna runs from and how midnad gets started.
//!
//! - **Installed** (`Midna.app/Contents/MacOS/midna-app`, no `MIDNA_DEV`): copy the bundled
//!   midnad into `MIDNA_HOME/bin/<version>-<hash>/` (via `midnad --install-self`), register the
//!   LaunchAgent in `Contents/Library/LaunchAgents/` with SMAppService (macOS 13+; a plain
//!   `~/Library/LaunchAgents` plist + `launchctl bootstrap` below that), link the `midna` CLI
//!   into `~/.local/bin` when that is on the login shell's PATH, then make sure the running
//!   daemon is the installed build (`daemon.upgrade` in place otherwise; shells survive).
//! - **Dev** (`MIDNA_DEV=1`, or not inside a bundle): nothing is registered; if midnad isn't
//!   answering, the sibling `midnad` binary is started directly (opt out: `MIDNA_NO_SPAWN=1`).
//!
//! All functions here block; `lifecycle` runs them off the UI thread.
use objc2_foundation::NSString;
use objc2_service_management::{SMAppService, SMAppServiceStatus};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub enum Mode {
    Dev,
    Installed { bundle: PathBuf },
}

/// `MIDNA_DEV=1` forces dev mode; otherwise we're installed when running from a `.app`.
pub fn mode() -> Mode {
    if std::env::var("MIDNA_DEV").is_ok_and(|v| !v.is_empty() && v != "0") {
        return Mode::Dev;
    }
    match bundle_of(&std::env::current_exe().unwrap_or_default()) {
        Some(bundle) => Mode::Installed { bundle },
        None => Mode::Dev,
    }
}

/// `/x/Midna.app/Contents/MacOS/midna-app` -> `/x/Midna.app`.
pub fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    (macos.file_name()? == "MacOS" && contents.file_name()? == "Contents" && app.extension()? == "app").then(|| app.to_path_buf())
}

// ------------------------------------------------------------------ login item (launchd)

#[derive(Clone, Debug, PartialEq)]
pub enum LoginItem {
    /// Dev mode: midnad is started directly.
    Dev,
    Checking,
    Enabled,
    /// Registered, but the human must switch it on in System Settings ▸ Login Items.
    RequiresApproval,
    NotRegistered,
    /// macOS 12: a plain `~/Library/LaunchAgents` plist.
    Legacy,
    Failed(String),
}

/// The LaunchAgent plist shipped in the bundle (the first one in `Contents/Library/LaunchAgents`).
pub fn bundled_agent_plist(bundle: &Path) -> Option<PathBuf> {
    let dir = bundle.join("Contents/Library/LaunchAgents");
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "plist")).collect();
    v.sort();
    v.into_iter().next()
}

/// A plist file as JSON (`plutil -convert json`).
pub fn read_plist(path: &Path) -> Option<Value> {
    let o = Command::new("/usr/bin/plutil").args(["-convert", "json", "-o", "-"]).arg(path).output().ok()?;
    o.status.success().then(|| serde_json::from_slice(&o.stdout).ok()).flatten()
}

pub fn macos_version() -> (u32, u32) {
    let mut buf = [0u8; 64];
    let mut len = buf.len();
    let r = unsafe { libc::sysctlbyname(c"kern.osproductversion".as_ptr(), buf.as_mut_ptr() as *mut _, &mut len, std::ptr::null_mut(), 0) };
    if r != 0 {
        return (0, 0);
    }
    let s = String::from_utf8_lossy(&buf[..len.saturating_sub(1)]).to_string();
    let mut it = s.trim_end_matches('\0').split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0))
}

fn has_smappservice() -> bool {
    macos_version().0 >= 13
}

fn agent_service(plist: &Path) -> objc2::rc::Retained<SMAppService> {
    let name = plist.file_name().unwrap_or_default().to_string_lossy();
    unsafe { SMAppService::agentServiceWithPlistName(&NSString::from_str(&name)) }
}

fn from_status(s: SMAppServiceStatus) -> LoginItem {
    match s {
        SMAppServiceStatus::Enabled => LoginItem::Enabled,
        SMAppServiceStatus::RequiresApproval => LoginItem::RequiresApproval,
        SMAppServiceStatus::NotRegistered => LoginItem::NotRegistered,
        SMAppServiceStatus::NotFound => LoginItem::Failed("launch agent plist not found in the bundle".into()),
        _ => LoginItem::Failed("unknown SMAppService status".into()),
    }
}

fn label_of(plist: &Path) -> String {
    read_plist(plist).and_then(|v| v.get("Label").and_then(Value::as_str).map(str::to_string)).unwrap_or_else(|| midna_proto::paths::DAEMON_LABEL.into())
}

fn legacy_plist_path(label: &str) -> PathBuf {
    home_dir().join("Library/LaunchAgents").join(format!("{label}.plist"))
}

pub fn login_status(bundle: &Path) -> LoginItem {
    let Some(plist) = bundled_agent_plist(bundle) else {
        return LoginItem::Failed("no launch agent plist in the bundle".into());
    };
    if has_smappservice() {
        from_status(unsafe { agent_service(&plist).status() })
    } else if legacy_plist_path(&label_of(&plist)).exists() {
        LoginItem::Legacy
    } else {
        LoginItem::NotRegistered
    }
}

/// Register the bundled LaunchAgent (idempotent). Returns the resulting state.
pub fn register(bundle: &Path) -> LoginItem {
    let Some(plist) = bundled_agent_plist(bundle) else {
        return LoginItem::Failed("no launch agent plist in the bundle".into());
    };
    if !has_smappservice() {
        return legacy_register(&plist);
    }
    let svc = agent_service(&plist);
    match unsafe { svc.status() } {
        SMAppServiceStatus::Enabled | SMAppServiceStatus::RequiresApproval => {}
        _ => {
            if let Err(e) = unsafe { svc.registerAndReturnError() } {
                let state = from_status(unsafe { svc.status() });
                // "Operation not permitted" = the human must approve it; status says so.
                if state != LoginItem::RequiresApproval {
                    return LoginItem::Failed(e.localizedDescription().to_string());
                }
            }
        }
    }
    from_status(unsafe { svc.status() })
}

/// Unregister (the Settings "Remove" path and test cleanup). Stops the daemon first so shells
/// are hung up deliberately rather than by launchd's SIGTERM.
pub fn unregister(bundle: &Path) -> Result<(), String> {
    let plist = bundled_agent_plist(bundle).ok_or("no launch agent plist in the bundle")?;
    if has_smappservice() {
        unsafe { agent_service(&plist).unregisterAndReturnError() }.map_err(|e| e.localizedDescription().to_string())
    } else {
        let label = label_of(&plist);
        let _ = Command::new("/bin/launchctl").args(["bootout", &format!("gui/{}/{label}", uid())]).status();
        std::fs::remove_file(legacy_plist_path(&label)).map_err(|e| e.to_string())
    }
}

/// macOS 12: copy the bundled plist to ~/Library/LaunchAgents with an absolute program path
/// (the stable `bin/current/midnad`) and bootstrap it.
fn legacy_register(plist: &Path) -> LoginItem {
    let Some(mut v) = read_plist(plist) else {
        return LoginItem::Failed("unreadable launch agent plist".into());
    };
    let label = label_of(plist);
    let home = midna_proto::paths::midna_home();
    let program = home.join("bin/current/midnad");
    if let Some(o) = v.as_object_mut() {
        o.remove("BundleProgram");
        o.remove("AssociatedBundleIdentifiers");
        o.insert("ProgramArguments".into(), serde_json::json!([program, "--launchd"]));
    }
    let dest = legacy_plist_path(&label);
    if let Err(e) = midna_proto::paths::guard_write(&dest) {
        return LoginItem::Failed(e.to_string());
    }
    let _ = std::fs::create_dir_all(dest.parent().unwrap());
    let tmp = dest.with_extension("json.tmp");
    if std::fs::write(&tmp, serde_json::to_vec(&v).unwrap_or_default()).is_err() {
        return LoginItem::Failed(format!("can't write {}", dest.display()));
    }
    let ok = Command::new("/usr/bin/plutil").args(["-convert", "xml1", "-o"]).arg(&dest).arg(&tmp).status().is_ok_and(|s| s.success());
    let _ = std::fs::remove_file(&tmp);
    if !ok {
        return LoginItem::Failed("plutil failed".into());
    }
    let _ = Command::new("/bin/launchctl").args(["bootstrap", &format!("gui/{}", uid())]).arg(&dest).status();
    LoginItem::Legacy
}

/// Open System Settings ▸ General ▸ Login Items (where a RequiresApproval agent is switched on).
pub fn open_login_items() {
    if has_smappservice() {
        unsafe { SMAppService::openSystemSettingsLoginItems() };
    } else {
        let _ = Command::new("/usr/bin/open").arg("x-apple.systempreferences:com.apple.LoginItems-Settings.extension").status();
    }
}

/// `launchctl kickstart` the agent (enabled but not answering, e.g. after `daemon.stop`).
pub fn kickstart(bundle: &Path) {
    if let Some(plist) = bundled_agent_plist(bundle) {
        let _ = Command::new("/bin/launchctl").args(["kickstart", &format!("gui/{}/{}", uid(), label_of(&plist))]).output();
    }
}

fn uid() -> u32 {
    unsafe { libc::getuid() }
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"))
}

// ------------------------------------------------------------------ stable daemon binary

/// `bin/current/midnad` under MIDNA_HOME (what launchd's trampoline and upgrades run).
pub fn current_daemon(home: &Path) -> PathBuf {
    home.join("bin/current/midnad")
}

/// Make `bin/current` the bundled midnad (and CLI). Skips the copy when it already is.
pub fn install_daemon(bundle: &Path, home: &Path) -> Result<PathBuf, String> {
    midna_proto::paths::guard_write(home).map_err(|e| e.to_string())?;
    let bundled = bundle.join("Contents/MacOS/midnad");
    if !bundled.is_file() {
        return Err(format!("{} is missing", bundled.display()));
    }
    let cur = current_daemon(home);
    if same_bytes(&bundled, &cur) {
        return Ok(cur);
    }
    let o = Command::new(&bundled).arg("--install-self").env("MIDNA_HOME", home).output().map_err(|e| e.to_string())?;
    if !o.status.success() {
        return Err(format!("midnad --install-self: {}", String::from_utf8_lossy(&o.stderr).trim()));
    }
    prune_old_installs(home);
    Ok(cur)
}

fn same_bytes(a: &Path, b: &Path) -> bool {
    let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
        return false;
    };
    ma.len() == mb.len() && std::fs::read(a).ok() == std::fs::read(b).ok()
}

/// Keep `current` plus the two most recent other versions (an upgrade watchdog may fall back
/// to the previous binary); delete the rest.
pub fn prune_old_installs(home: &Path) {
    let root = home.join("bin");
    let current = std::fs::canonicalize(root.join("current")).ok();
    let mut dirs: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(&root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .filter(|e| std::fs::canonicalize(e.path()).ok() != current)
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    dirs.sort();
    let n = dirs.len();
    for (_, d) in dirs.into_iter().take(n.saturating_sub(2)) {
        let _ = std::fs::remove_dir_all(d);
    }
}

pub fn socket_alive(socket: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(socket).is_ok()
}

/// Dev mode: start the `midnad` next to this executable (it detaches itself).
pub fn spawn_dev_daemon() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let midnad = exe.with_file_name("midnad");
    if !midnad.is_file() {
        return Err(format!("no midnad next to {}", exe.display()));
    }
    let o = Command::new(&midnad).stdin(std::process::Stdio::null()).output().map_err(|e| e.to_string())?;
    if o.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&o.stderr).trim().to_string()) }
}

// ------------------------------------------------------------------ midna CLI for agents

#[derive(Clone, Debug, PartialEq)]
pub enum CliLink {
    Dev,
    /// `link` -> `target`, and `link`'s directory is on PATH.
    Linked {
        link: PathBuf,
    },
    /// The link dir isn't on the login shell's PATH; Settings offers to install it anyway.
    NotOnPath {
        dir: PathBuf,
    },
    /// Something that isn't ours already lives at `link`; left alone.
    Conflict {
        link: PathBuf,
    },
    Failed(String),
}

/// Midna Dev: `<its home>/cli`. Else `MIDNA_CLI_LINK_DIR` (tests), else `~/.local/bin`.
pub fn cli_link_dir() -> PathBuf {
    if let Some(h) = midna_proto::paths::DEV_HOME {
        return Path::new(h).join("cli");
    }
    std::env::var_os("MIDNA_CLI_LINK_DIR").map(PathBuf::from).unwrap_or_else(|| home_dir().join(".local/bin"))
}

/// The link dir was chosen for us (Midna Dev, tests): link there whether or not it's on PATH.
pub fn cli_link_dir_pinned() -> bool {
    midna_proto::paths::is_dev() || std::env::var_os("MIDNA_CLI_LINK_DIR").is_some()
}

/// The user's PATH as their login shell sets it (the app's own PATH from launchd is minimal).
/// Reads only; never edits shell files.
pub fn login_shell_path() -> Option<String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let mut child = Command::new(&shell)
        .args(["-l", "-i", "-c", "printf '\\n__MIDNA_PATH__%s\\n' \"$PATH\""])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let t0 = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if t0.elapsed() < Duration::from_secs(5) => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = String::new();
    std::io::Read::read_to_string(child.stdout.as_mut()?, &mut out).ok()?;
    out.lines().find_map(|l| l.strip_prefix("__MIDNA_PATH__")).map(str::to_string)
}

fn dir_on_path(dir: &Path, path: &str) -> bool {
    let want = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    path.split(':').any(|p| !p.is_empty() && std::fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p)) == want)
}

/// Is `link` a symlink we made (pointing at some midna home's `bin/current/midna`)?
fn is_our_link(link: &Path) -> bool {
    std::fs::read_link(link).is_ok_and(|t| t.ends_with("bin/current/midna"))
}

/// Link `<dir>/midna` -> `MIDNA_HOME/bin/current/midna`. `force_dir`: install even when `dir`
/// isn't on PATH (the human asked in Settings).
pub fn link_cli(home: &Path, shell_path: Option<&str>, force_dir: bool) -> CliLink {
    let dir = cli_link_dir();
    let link = dir.join("midna");
    let target = home.join("bin/current/midna");
    let on_path = cli_link_dir_pinned() || shell_path.is_some_and(|p| dir_on_path(&dir, p));
    if std::fs::symlink_metadata(&link).is_ok() {
        if !is_our_link(&link) {
            return CliLink::Conflict { link };
        }
        if std::fs::read_link(&link).ok().as_deref() == Some(target.as_path()) {
            return if on_path { CliLink::Linked { link } } else { CliLink::NotOnPath { dir } };
        }
    }
    if !on_path && !force_dir {
        return CliLink::NotOnPath { dir };
    }
    if let Err(e) = midna_proto::paths::guard_write(&link).and_then(|()| std::fs::create_dir_all(&dir)) {
        return CliLink::Failed(format!("{}: {e}", dir.display()));
    }
    let tmp = dir.join(format!(".midna.{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    if let Err(e) = std::os::unix::fs::symlink(&target, &tmp).and_then(|_| std::fs::rename(&tmp, &link)) {
        let _ = std::fs::remove_file(&tmp);
        return CliLink::Failed(e.to_string());
    }
    if on_path { CliLink::Linked { link } } else { CliLink::NotOnPath { dir } }
}

// ------------------------------------------------------------------ relaunch

/// Start `app` again once this process has exited (LaunchServices `open`), e.g. after an
/// update or an Accessibility grant (a running process never sees a new grant). The caller
/// quits right after.
pub fn relaunch_after_exit(app: &Path) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let pid = std::process::id().to_string();
    let mut cmd = Command::new("/bin/sh");
    cmd.args(["-c", "i=0; while kill -0 \"$1\" 2>/dev/null && [ $i -lt 300 ]; do sleep 0.1; i=$((i+1)); done; exec /usr/bin/open \"$2\"", "sh", &pid])
        .arg(app)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_detection() {
        assert_eq!(bundle_of(Path::new("/A/Midna.app/Contents/MacOS/midna-app")), Some(PathBuf::from("/A/Midna.app")));
        assert_eq!(bundle_of(Path::new("/x/target/debug/midna-app")), None);
        assert_eq!(bundle_of(Path::new("/x/Foo/Contents/MacOS/midna-app")), None);
    }

    #[test]
    fn path_membership() {
        assert!(dir_on_path(Path::new("/usr/bin"), "/opt/x:/usr/bin"));
        assert!(!dir_on_path(Path::new("/usr/sbin"), "/opt/x:/usr/bin"));
    }

    #[test]
    fn cli_link_respects_foreign_files() {
        let tmp = std::env::temp_dir().join(format!("midna-cli-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        unsafe { std::env::set_var("MIDNA_CLI_LINK_DIR", &tmp) };
        let home = tmp.join("home");
        assert_eq!(link_cli(&home, None, false), CliLink::Linked { link: tmp.join("midna") });
        assert_eq!(std::fs::read_link(tmp.join("midna")).unwrap(), home.join("bin/current/midna"));
        // a file that isn't our link is never replaced
        std::fs::remove_file(tmp.join("midna")).unwrap();
        std::fs::write(tmp.join("midna"), "#!/bin/sh\n").unwrap();
        assert!(matches!(link_cli(&home, None, false), CliLink::Conflict { .. }));
        unsafe { std::env::remove_var("MIDNA_CLI_LINK_DIR") };
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
