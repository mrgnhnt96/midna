//! Dev / test driver environment variables (`MIDNA_DEBUG_*`, the updater overrides, …).
//!
//! Some of them drive the UI: `MIDNA_DEBUG_KEYS` presses keys (⌘↩ approves), the Rules and
//! Triggers drivers click buttons, `MIDNA_DEBUG_UPDATE=apply` installs an update. The app's
//! connection is the *human* one, so if a release build honoured them, an agent could launch
//! the app binary with an environment that approves its own requests. They are compiled in
//! only for debug builds and the `dev-drivers` feature (used by `packaging/e2e`); a release
//! build always sees them as unset.

/// Whether driver variables are honoured in this build.
pub const ENABLED: bool = cfg!(any(debug_assertions, feature = "dev-drivers"));

/// `std::env::var`, or `NotPresent` in builds without dev drivers.
pub fn var(key: &str) -> Result<String, std::env::VarError> {
    if ENABLED { std::env::var(key) } else { Err(std::env::VarError::NotPresent) }
}

/// `std::env::var_os`, or `None` in builds without dev drivers.
pub fn var_os(key: &str) -> Option<std::ffi::OsString> {
    if ENABLED { std::env::var_os(key) } else { None }
}
