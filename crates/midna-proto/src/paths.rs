//! Where midna lives on disk. Socket: `MIDNA_SOCKET`, else `$MIDNA_HOME/midnad.sock`.
//! Home: `MIDNA_HOME`, else `~/Library/Application Support/com.mrgnhnt.midna`.
//!
//! Midna Dev (`scripts/dev-app.sh`) is built with its home baked in ([`DEV_HOME`]). It ignores
//! `MIDNA_HOME` / `MIDNA_SOCKET`, which every Midna terminal exports pointing at production,
//! and [`guard_write`] refuses production's files.
use std::path::{Component, Path, PathBuf};

pub const BUNDLE_ID: &str = "com.mrgnhnt.midna";
pub const DAEMON_LABEL: &str = "com.mrgnhnt.midna.daemon";
/// System Settings ▸ Notifications opened on midna's own page (not the list of every app).
pub const NOTIFICATIONS_PANE: &str = "x-apple.systempreferences:com.apple.Notifications-Settings.extension?id=com.mrgnhnt.midna";

/// Midna Dev's home, set at build time (`packaging/build-app.sh --dev-home DIR`). Some = this
/// build is Midna Dev.
pub const DEV_HOME: Option<&str> = match option_env!("MIDNA_DEV_HOME") {
    Some(h) if !h.is_empty() => Some(h),
    _ => None,
};

/// Is this build Midna Dev? It never touches the installed Midna (see [`guard_write`]).
pub fn is_dev() -> bool {
    DEV_HOME.is_some()
}

fn user_home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// The installed Midna's home, whatever this build is.
pub fn production_home() -> PathBuf {
    user_home().join("Library/Application Support").join(BUNDLE_ID)
}

pub fn midna_home() -> PathBuf {
    resolve_home(DEV_HOME, std::env::var_os("MIDNA_HOME"))
}

pub fn socket_path() -> PathBuf {
    resolve_socket(DEV_HOME, std::env::var_os("MIDNA_SOCKET"), midna_home())
}

fn resolve_home(dev: Option<&str>, env: Option<std::ffi::OsString>) -> PathBuf {
    if let Some(h) = dev {
        return PathBuf::from(h);
    }
    match env.filter(|h| !h.is_empty()) {
        Some(h) => PathBuf::from(h),
        None => production_home(),
    }
}

fn resolve_socket(dev: Option<&str>, env: Option<std::ffi::OsString>, home: PathBuf) -> PathBuf {
    match env.filter(|s| !s.is_empty()) {
        Some(s) if dev.is_none() => PathBuf::from(s),
        _ => home.join("midnad.sock"),
    }
}

/// What Midna Dev must never write: production's home, CLI link, Quick Action and app bundle.
pub fn production_paths() -> Vec<PathBuf> {
    let home = user_home();
    vec![
        production_home(),
        home.join(".local/bin/midna"),
        home.join("Library/Services/Open in Midna.workflow"),
        home.join("Library/LaunchAgents").join(format!("{DAEMON_LABEL}.plist")),
        PathBuf::from("/Applications/Midna.app"),
    ]
}

/// Midna Dev refuses to write `path` when it is, or is inside, one of production's
/// ([`production_paths`]). Always Ok in every other build.
pub fn guard_write(path: &Path) -> std::io::Result<()> {
    if is_dev() { check_write(path, &production_paths()) } else { Ok(()) }
}

fn check_write(path: &Path, protected: &[PathBuf]) -> std::io::Result<()> {
    let p = resolved(path);
    match protected.iter().find(|q| p.starts_with(resolved(q))) {
        Some(q) => Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("Midna Dev never writes the installed Midna's files: {} is under {}", path.display(), q.display()),
        )),
        None => Ok(()),
    }
}

/// `path` with `.`/`..` folded and symlinks resolved as far as it exists, so a link into
/// production (or `~/x/../Library/…`) is caught too.
fn resolved(path: &Path) -> PathBuf {
    let mut lexical = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                lexical.pop();
            }
            Component::CurDir => {}
            c => lexical.push(c),
        }
    }
    let mut rest = Vec::new();
    let mut base = lexical.as_path();
    loop {
        if let Ok(real) = std::fs::canonicalize(base) {
            return rest.iter().rev().fold(real, |acc: PathBuf, c| acc.join(c));
        }
        match (base.parent(), base.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                base = parent;
            }
            _ => return lexical,
        }
    }
}

pub fn state_path(home: &std::path::Path) -> PathBuf {
    home.join("state.json")
}

pub fn events_path(home: &std::path::Path) -> PathBuf {
    home.join("events.jsonl")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_build_ignores_the_inherited_home_and_socket() {
        let prod = Some("/Users/me/Library/Application Support/com.mrgnhnt.midna".into());
        let home = resolve_home(Some("/dev/home"), prod);
        assert_eq!(home, PathBuf::from("/dev/home"));
        let sock = resolve_socket(Some("/dev/home"), Some("/prod/midnad.sock".into()), home);
        assert_eq!(sock, PathBuf::from("/dev/home/midnad.sock"));
    }

    #[test]
    fn other_builds_honour_the_env() {
        assert_eq!(resolve_home(None, Some("/tmp/h".into())), PathBuf::from("/tmp/h"));
        assert_eq!(resolve_home(None, Some("".into())), production_home());
        assert_eq!(resolve_socket(None, Some("/tmp/s.sock".into()), "/tmp/h".into()), PathBuf::from("/tmp/s.sock"));
        assert_eq!(resolve_socket(None, None, "/tmp/h".into()), PathBuf::from("/tmp/h/midnad.sock"));
    }

    #[test]
    fn check_write_refuses_production_paths_however_spelled() {
        let tmp = std::env::temp_dir().join(format!("midna-guard-{}", std::process::id()));
        let prod = tmp.join("prod");
        let dev = tmp.join("dev");
        std::fs::create_dir_all(prod.join("bin")).unwrap();
        std::fs::create_dir_all(&dev).unwrap();
        std::os::unix::fs::symlink(&prod, dev.join("sneaky")).unwrap();
        let protected = [prod.clone()];
        assert!(check_write(&prod, &protected).is_err());
        assert!(check_write(&prod.join("bin/current"), &protected).is_err(), "inside, not yet existing");
        assert!(check_write(&dev.join("../prod/bin"), &protected).is_err(), "via ..");
        assert!(check_write(&dev.join("sneaky/bin/current"), &protected).is_err(), "via a symlink");
        assert!(check_write(&dev.join("bin/current"), &protected).is_ok());
        assert!(check_write(&tmp.join("prod-other"), &protected).is_ok(), "a sibling sharing the prefix");
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
