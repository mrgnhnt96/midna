//! midnad library: the daemon's modules, kept in a lib so they're testable.
//! See docs/ARCHITECTURE.md. Entry point: [`server::start`].
pub mod agent_state;
pub mod agent_work;
pub mod conn;
pub mod daemon;
pub mod engine;
pub mod eventlog;
pub mod git;
pub mod hooks;
pub mod images;
pub mod insights;
pub mod install;
pub mod links;
pub mod local;
pub mod queue;
pub mod notify;
pub mod notify_media;
pub mod peer;
pub mod policy;
pub mod procs;
pub mod prompts;
pub mod pty;
pub mod restart;
pub mod rpc;
pub mod server;
pub mod state;
pub mod stream;
pub mod term;
pub mod upgrade;
pub mod webhooks;

pub use daemon::Config;
pub use server::{Handle, start, start_with};
