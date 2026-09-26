//! Direct repository watching, with explicit commit boundaries and upload policy.

mod monitor;
mod repository;

pub use monitor::{Event, MonitorOptions, StopToken, watch_repository, watch_workspace};
pub use repository::{Repository, WatchOptions, WatchReport};
