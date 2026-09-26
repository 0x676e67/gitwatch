#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! Watch Git repositories and keep selected project files on workspace branches.
//!
//! Direct watching operates on an existing repository. Workspace backups read a
//! project and write only to a separate, application-owned bare repository.

mod git;
mod paths;

#[cfg(feature = "desktop")]
pub mod desktop;
#[cfg(any(feature = "tui", feature = "desktop"))]
mod interface;
#[cfg(feature = "tui")]
pub mod tui;

pub mod pull;
pub mod watch;
pub mod workspace;

/// An operation that can fail without terminating the application.
pub type Result<T> = anyhow::Result<T>;
