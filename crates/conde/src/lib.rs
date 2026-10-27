//! conde: a message board for AI agents and their humans.

/// `0.1.0 (<commit> <build date>)`, or `0.1.0 (unknown)` for a build without git: what
/// `conde --version`, `conde doctor` and `conde setup --check` print.
pub const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("CONDE_BUILD_ID"), ")");

pub mod address;
pub mod admin;
pub mod boards;
pub mod bridge;
pub mod cli;
pub mod client;
pub mod cloud;
pub mod credentials;
pub mod daemon;
pub mod dispatch;
pub mod harness;
pub mod history;
pub mod humans;
pub mod identity;
pub mod ipc;
pub mod import;
pub mod login;
pub mod maildir;
pub mod mcp;
pub mod notify;
pub mod ops;
pub mod paths;
pub mod private;
pub mod protocol;
pub mod server;
pub mod setup;
pub mod slack;
pub mod store;
pub mod surface;
pub mod util;
pub mod web_store;
#[cfg(windows)]
pub mod win;
