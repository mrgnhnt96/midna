//! Where midna lives on disk. Socket: `MIDNA_SOCKET`, else `$MIDNA_HOME/midnad.sock`.
//! Home: `MIDNA_HOME`, else `~/Library/Application Support/com.mrgnhnt.midna`.
use std::path::PathBuf;

pub const BUNDLE_ID: &str = "com.mrgnhnt.midna";
pub const DAEMON_LABEL: &str = "com.mrgnhnt.midna.daemon";

pub fn midna_home() -> PathBuf {
    if let Some(h) = std::env::var_os("MIDNA_HOME").filter(|h| !h.is_empty()) {
        return PathBuf::from(h);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
    home.join("Library/Application Support").join(BUNDLE_ID)
}

pub fn socket_path() -> PathBuf {
    if let Some(s) = std::env::var_os("MIDNA_SOCKET").filter(|s| !s.is_empty()) {
        return PathBuf::from(s);
    }
    midna_home().join("midnad.sock")
}

pub fn state_path(home: &std::path::Path) -> PathBuf {
    home.join("state.json")
}

pub fn events_path(home: &std::path::Path) -> PathBuf {
    home.join("events.jsonl")
}
