use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use anyhow::{Context, ensure};
use notify::{RecursiveMode, Watcher};
use regex::Regex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{Repository, WatchReport};
use crate::{
    Result,
    workspace::{BackupReport, BackupStore, UploadState},
};

/// A cooperative stop signal shared by an interface and a running watch task.
#[derive(Clone, Default)]
pub struct StopToken(Arc<AtomicBool>);

/// Timing and event-source settings, independent from either storage mode.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MonitorOptions {
    debounce: Duration,
    poll: Duration,
    max_wait: Duration,
    commit_on_start: bool,
    native: bool,
    events: Vec<String>,
}

/// A typed event shared by the CLI, TUI and desktop interface.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "event", content = "detail", rename_all = "snake_case")]
pub enum Event {
    /// The watch loop is ready. The message identifies its event source.
    Watching(String),
    /// Changes are waiting for the quiet interval.
    Pending,
    /// A direct-repository operation completed.
    Repository(WatchReport),
    /// A workspace snapshot completed.
    Backup(BackupReport),
    /// A previously pending upload was retried.
    Upload(UploadState),
    /// An operation failed; the loop retains pending work for retry.
    Error(String),
    /// The owning interface requested termination.
    Stopped,
}

enum Job {
    Repository(Box<Repository>),
    Workspace(BackupStore, Uuid),
}

// ===== impl StopToken =====

impl StopToken {
    /// Requests shutdown at the next safe operation boundary.
    pub fn stop(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    /// Returns whether shutdown was requested.
    pub fn is_stopped(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

// ===== impl MonitorOptions =====

impl MonitorOptions {
    /// Sets the quiet period after the last relevant event.
    pub fn debounce(mut self, value: Duration) -> Self {
        self.debounce = value;
        self
    }
    /// Sets the periodic content-check interval, including with native events enabled.
    pub fn poll_interval(mut self, value: Duration) -> Self {
        self.poll = value;
        self
    }
    /// Bounds delays during continuous writing.
    pub fn max_wait(mut self, value: Duration) -> Self {
        self.max_wait = value;
        self
    }
    /// Performs an initial operation before waiting for a new change.
    pub fn commit_on_start(mut self, enabled: bool) -> Self {
        self.commit_on_start = enabled;
        self
    }
    /// Enables native events; false uses content polling alone.
    pub fn native(mut self, enabled: bool) -> Self {
        self.native = enabled;
        self
    }
    /// Filters native events using portable create/modify/delete/move names.
    /// Periodic reconciliation still detects missed content changes.
    pub fn events(mut self, events: Vec<String>) -> Self {
        self.events = events;
        self
    }
}

impl Default for MonitorOptions {
    fn default() -> Self {
        Self {
            debounce: Duration::from_secs(2),
            poll: Duration::from_secs(5),
            max_wait: Duration::from_secs(60),
            commit_on_start: false,
            native: true,
            events: Vec::new(),
        }
    }
}

/// Runs a repository watch until stopped, reporting local and upload results separately.
pub fn watch_repository(
    repository: Repository,
    options: MonitorOptions,
    stop: StopToken,
    report: impl FnMut(Event),
) -> Result<()> {
    run(Job::Repository(Box::new(repository)), options, stop, report)
}

/// Runs automatic backups for one bound workspace until stopped.
pub fn watch_workspace(
    store: BackupStore,
    id: Uuid,
    options: MonitorOptions,
    stop: StopToken,
    report: impl FnMut(Event),
) -> Result<()> {
    run(Job::Workspace(store, id), options, stop, report)
}

// ===== impl Job =====

impl Job {
    fn root(&self) -> Result<PathBuf> {
        match self {
            Self::Repository(repo) => Ok(repo.root().to_path_buf()),
            Self::Workspace(store, id) => Ok(store
                .workspaces()?
                .into_iter()
                .find(|w| w.id() == *id)
                .context("Unknown workspace")?
                .root()
                .to_path_buf()),
        }
    }

    fn fingerprint(&self) -> Result<String> {
        match self {
            Self::Repository(repo) => repo.fingerprint(),
            Self::Workspace(store, id) => store.fingerprint(*id),
        }
    }

    fn commit(&mut self) -> Result<Event> {
        match self {
            Self::Repository(repo) => Ok(Event::Repository(repo.commit()?)),
            Self::Workspace(store, id) => Ok(Event::Backup(store.backup(*id)?)),
        }
    }

    fn retry(&mut self) -> Result<Option<UploadState>> {
        match self {
            Self::Repository(repo) => Ok(Some(repo.retry_upload()?)),
            Self::Workspace(store, id) => {
                if !store.remote()?.1 {
                    return Ok(None);
                }
                let pending = store.status(*id)?.is_some_and(|s| {
                    matches!(
                        s.upload(),
                        UploadState::Pending | UploadState::Failed { .. }
                    )
                });
                if !pending {
                    return Ok(None);
                }
                Ok(Some(match store.push(*id) {
                    Ok(()) => UploadState::Synced,
                    Err(error) => UploadState::Failed {
                        message: error.to_string(),
                    },
                }))
            }
        }
    }

    fn paused(&self) -> Result<bool> {
        match self {
            Self::Repository(_) => Ok(false),
            Self::Workspace(store, id) => Ok(store
                .workspaces()?
                .into_iter()
                .find(|w| w.id() == *id)
                .context("Workspace was removed")?
                .is_paused()),
        }
    }
}

fn run(
    mut job: Job,
    options: MonitorOptions,
    stop: StopToken,
    mut report: impl FnMut(Event),
) -> Result<()> {
    ensure!(
        !options.poll.is_zero()
            && !options.max_wait.is_zero()
            && options.max_wait >= options.debounce,
        "Polling must be positive and max-wait must cover debounce"
    );
    ensure!(
        [options.poll, options.max_wait, options.debounce]
            .into_iter()
            .all(|duration| Instant::now().checked_add(duration).is_some()),
        "Watch duration exceeds the platform clock range"
    );
    for event in &options.events {
        ensure!(
            matches!(
                event.as_str(),
                "create" | "modify" | "delete" | "move" | "move_self" | "close_write"
            ),
            "Unsupported portable event: {event}"
        );
    }
    let mut root_error = String::new();
    let root = loop {
        if stop.is_stopped() {
            report(Event::Stopped);
            return Ok(());
        }
        match job.root() {
            Ok(root) => break root,
            Err(error) => {
                emit_error(&mut report, &mut root_error, error.to_string());
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    };
    let watched_scope = match &job {
        Job::Repository(repo) => Some(repo.target().to_path_buf()),
        _ => None,
    };
    let exclusion = match &job {
        Job::Repository(repo) => repo.options().exclusion().map(Regex::new).transpose()?,
        _ => None,
    };
    let event_filter = options.events.clone();
    let (tx, rx) = mpsc::sync_channel(1);
    let mut native = if options.native {
        match notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let relevant = match event {
                Ok(event) => {
                    event_matches(&event, &event_filter)
                        && event.paths.iter().any(|path| {
                            watched_scope.as_ref().is_none_or(|scope| {
                                path.starts_with(scope) || scope.starts_with(path)
                            }) && !path
                                .components()
                                .any(|p| p.as_os_str().eq_ignore_ascii_case(".git"))
                                && !exclusion
                                    .as_ref()
                                    .is_some_and(|r| r.is_match(&path.to_string_lossy()))
                        })
                }
                Err(_) => true,
            };
            if relevant {
                let _ = tx.try_send(());
            }
        }) {
            Ok(mut watcher) => match watcher.watch(&root, RecursiveMode::Recursive) {
                Ok(()) => Some(watcher),
                Err(error) => {
                    report(Event::Error(format!(
                        "Native watch unavailable; polling continues: {error}"
                    )));
                    None
                }
            },
            Err(error) => {
                report(Event::Error(format!(
                    "Native watch unavailable; polling continues: {error}"
                )));
                None
            }
        }
    } else {
        None
    };
    let mut observed = match job.fingerprint() {
        Ok(fingerprint) => Some(fingerprint),
        Err(error) => {
            report(Event::Error(error.to_string()));
            None
        }
    };
    report(Event::Watching(
        if native.is_some() {
            "native events with periodic content checks"
        } else {
            "content polling"
        }
        .into(),
    ));
    let now = Instant::now();
    let mut dirty = options.commit_on_start.then_some((now, now));
    let mut next_poll = now + options.poll;
    let mut next_retry = now + Duration::from_secs(30);
    let mut backoff = Duration::from_secs(5);
    let mut last_error = String::new();
    let mut paused = false;
    while !stop.is_stopped() {
        let now = Instant::now();
        let is_paused = match job.paused() {
            Ok(paused) => paused,
            Err(error) => {
                emit_error(&mut report, &mut last_error, error.to_string());
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        if is_paused {
            paused = true;
            dirty = None;
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        if paused {
            dirty = Some((now, now + options.debounce));
            observed = match job.fingerprint() {
                Ok(fingerprint) => Some(fingerprint),
                Err(error) => {
                    emit_error(&mut report, &mut last_error, error.to_string());
                    None
                }
            };
            paused = false;
        }
        if rx.try_recv().is_ok() {
            let first = dirty.map(|(first, _)| first).unwrap_or(now);
            dirty = Some((first, now + options.debounce));
            report(Event::Pending);
        }
        if now >= next_poll {
            next_poll = now + options.poll;
            match job.fingerprint() {
                Ok(current) if observed.as_ref() != Some(&current) => {
                    if observed.is_some() {
                        let first = dirty.map(|(first, _)| first).unwrap_or(now);
                        dirty = Some((first, now + options.debounce));
                    }
                    observed = Some(current);
                }
                Ok(_) => {}
                Err(error) => emit_error(&mut report, &mut last_error, error.to_string()),
            }
        }
        if dirty.is_some_and(|(first, deadline)| {
            now >= deadline || now.duration_since(first) >= options.max_wait
        }) {
            match job.commit() {
                Ok(event) => {
                    let deferred =
                        matches!(&event, Event::Repository(report) if report.skipped().is_some());
                    report(event);
                    let now = Instant::now();
                    dirty =
                        deferred.then_some((now, now + options.poll.max(Duration::from_secs(1))));
                    last_error.clear();
                    next_retry = Instant::now() + Duration::from_secs(30);
                }
                Err(error) => {
                    emit_error(&mut report, &mut last_error, error.to_string());
                    dirty = Some((
                        Instant::now(),
                        Instant::now() + options.poll.max(Duration::from_secs(5)),
                    ));
                }
            }
        }
        if now >= next_retry {
            let failed = match job.retry() {
                Ok(Some(state)) => {
                    let failed = matches!(state, UploadState::Failed { .. });
                    report(Event::Upload(state));
                    failed
                }
                Ok(None) => false,
                Err(error) => {
                    emit_error(&mut report, &mut last_error, error.to_string());
                    true
                }
            };
            backoff = if failed {
                (backoff * 2).min(Duration::from_secs(300))
            } else {
                Duration::from_secs(30)
            };
            next_retry = Instant::now() + backoff;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(native.take());
    report(Event::Stopped);
    Ok(())
}

fn emit_error(report: &mut impl FnMut(Event), last: &mut String, error: String) {
    if *last != error {
        report(Event::Error(error.clone()));
        *last = error;
    }
}

fn event_matches(event: &notify::Event, filter: &[String]) -> bool {
    use notify::event::{AccessKind, AccessMode, ModifyKind};
    let category = match event.kind {
        notify::EventKind::Create(_) => "create",
        notify::EventKind::Modify(ModifyKind::Name(_)) => "move",
        notify::EventKind::Modify(_) => "modify",
        notify::EventKind::Remove(_) => "delete",
        notify::EventKind::Access(AccessKind::Close(AccessMode::Write)) => "close_write",
        notify::EventKind::Any | notify::EventKind::Other => return true,
        _ => return false,
    };
    filter.is_empty()
        || filter
            .iter()
            .any(|name| name == category || (name == "move_self" && category == "move"))
}
