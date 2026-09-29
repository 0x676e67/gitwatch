#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! Watch Git repositories and keep selected project files on workspace branches.
//!
//! Direct watching operates on an existing repository. Workspace backups read a
//! project and save to a separate, application-owned repository. Explicitly enabled
//! two-way sync also applies selected files, leaving the source Git repository alone.

mod git;
mod paths;
mod preferences;
mod repository;
#[cfg(test)]
#[path = "../tests/support/git.rs"]
mod test_git;

pub mod desktop;
mod interface;

pub mod i18n;
pub mod pull;
pub mod update;
pub mod watch;
pub mod workspace;

/// An operation that can fail without terminating the application.
pub type Result<T> = anyhow::Result<T>;
