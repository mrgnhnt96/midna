//! midna wire types, method catalog, OpenRPC generation, frame codec and a blocking client.
//! See docs/ARCHITECTURE.md.
pub mod catalog;
pub mod cleanup;
pub mod agent_cli;
pub mod client;
pub mod cron;
pub mod error;
pub mod frame;
pub mod keep_awake;
pub mod link_patterns;
pub mod methods;
pub mod notify;
pub mod openrpc;
pub mod paths;
pub mod prompts;
pub mod secrets;
pub mod settings;
pub mod system;
pub mod themes;
pub mod time;
pub mod types;

pub use catalog::{MethodSpec, catalog, method};
pub use cleanup::{
    CleanupGetParams, CleanupPlan, CleanupPreviewParams, CleanupRun, CleanupRunParams, CleanupRunsParams, CleanupRunsResult, CleanupState, CleanupTarget,
};
pub use client::{AttachStream, Client, ClientError, Subscription};
pub use error::RpcError;
pub use frame::{Cell, Frame, RowData};
pub use methods::*;
pub use openrpc::openrpc;
pub use system::{
    HeavyTerminal, SessionPauseParams, SessionPauseResult, StopProcessesParams, StopProcessesResult, StoppedProcess, SystemLoad,
    WorktreeFailure, WorktreeInfo, WorktreesCleanParams, WorktreesCleanResult, WorktreesList, WorktreesListParams,
};
pub use types::*;

/// The midna release version: `MIDNA_BUILD_VERSION` at build time (set by
/// `packaging/build-app.sh --version`), else the workspace Cargo version. Every binary
/// reports this one, so a packaged build is consistent end to end.
pub const VERSION: &str = match option_env!("MIDNA_BUILD_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};
