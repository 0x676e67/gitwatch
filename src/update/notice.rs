use std::{
    fs,
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use super::Release;
use crate::{Result, git::Lock, paths};

const INTERVAL: u64 = 24 * 60 * 60;

/// Non-blocking, cached release notifications. Dropping this stops future checks.
/// Set `GITWATCH_NO_UPDATE_CHECK=1` to disable automatic network requests.
pub struct Notifications {
    receiver: Receiver<Release>,
    stop: Sender<()>,
}

#[derive(Default, Serialize, Deserialize)]
struct Cache {
    checked: u64,
    release: Option<Release>,
}

impl Notifications {
    /// Starts a daily check without delaying startup or reporting network failures.
    /// Uses the system cache unless an explicit cache directory is supplied.
    pub fn start(cache: Option<PathBuf>) -> Self {
        let (sender, receiver) = mpsc::channel();
        let (stop, stopped) = mpsc::channel();
        if !cfg!(test)
            && std::env::var_os("GITWATCH_NO_UPDATE_CHECK").is_none_or(|value| value != "1")
        {
            thread::spawn(move || {
                let Ok(directory) = cache.map_or_else(paths::cache_directory, Ok) else {
                    return;
                };
                let mut shown = String::new();
                loop {
                    if let Ok(Some(release)) = cached(&directory)
                        && release.is_newer()
                        && release.version() != shown
                    {
                        shown = release.version().into();
                        if sender.send(release).is_err() {
                            break;
                        }
                    }
                    if stopped.recv_timeout(Duration::from_secs(3600))
                        != Err(mpsc::RecvTimeoutError::Timeout)
                    {
                        break;
                    }
                }
            });
        }
        Self { receiver, stop }
    }

    /// Takes a newly discovered version, if available, without waiting.
    pub fn poll(&self) -> Option<Release> {
        self.receiver.try_recv().ok()
    }
}

impl Drop for Notifications {
    fn drop(&mut self) {
        let _ = self.stop.send(());
    }
}

fn cached(directory: &std::path::Path) -> Result<Option<Release>> {
    fs::create_dir_all(directory)?;
    let _lock = Lock::acquire(&directory.join("update-check.lock"))?;
    let path = directory.join("update-check.json");
    let mut cache = paths::read_file(&path)
        .ok()
        .and_then(|data| serde_json::from_slice::<Cache>(&data).ok())
        .unwrap_or_default();
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    if cache.checked > now || now.saturating_sub(cache.checked) >= INTERVAL {
        // Persist the attempt first so offline launches do not repeatedly hit the API.
        cache.checked = now;
        paths::atomic_write(&path, &serde_json::to_vec(&cache)?)?;
        if let Ok(release) = Release::query(None, Duration::from_secs(3)) {
            cache.release = Some(release);
            paths::atomic_write(&path, &serde_json::to_vec(&cache)?)?;
        }
    }
    Ok(cache.release)
}
