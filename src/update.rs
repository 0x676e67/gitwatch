//! Version checks and explicit management of the current gitwatch installation.
//!
//! Downloads come from the official release repository. Updating and uninstalling
//! never open workspace repositories or remove user configuration and backups.

mod archive;
mod install;
mod notice;
mod release;

pub use self::{
    install::{Installation, Running},
    notice::Notifications,
    release::Release,
};

/// Version of this build, independent of the selected display language.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Returns the release target for this build, or an error for unsupported targets.
pub fn target() -> crate::Result<&'static str> {
    if cfg!(all(
        target_os = "windows",
        target_arch = "x86_64",
        target_env = "msvc"
    )) {
        Ok("x86_64-pc-windows-msvc")
    } else if cfg!(all(
        target_os = "linux",
        target_arch = "x86_64",
        target_env = "gnu"
    )) {
        Ok("x86_64-unknown-linux-gnu")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Ok("aarch64-apple-darwin")
    } else {
        anyhow::bail!("No official release is available for this platform")
    }
}
