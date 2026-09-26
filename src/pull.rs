//! Periodic, fast-forward-only updates of repositories without overwriting local work.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    Result,
    git::{self, Git, Lock},
    watch::StopToken,
};

/// A local repository and optional clone source for a periodic update task.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PullOptions {
    path: PathBuf,
    url: Option<String>,
    remote: String,
    branch: Option<String>,
    interval: Duration,
}

/// The result of cloning or fast-forwarding a local repository.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PullReport {
    path: PathBuf,
    before: Option<String>,
    after: String,
    cloned: bool,
}

/// A prepared update task pinned to the local branch selected at startup.
pub struct PullTask {
    options: PullOptions,
    git: Option<Git>,
    branch: Option<String>,
    initial_clone: bool,
}

// ===== impl PullOptions =====

impl PullOptions {
    /// Configures an existing repository, with hourly updates by default.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            url: None,
            remote: "origin".into(),
            branch: None,
            interval: Duration::from_secs(3600),
        }
    }
    /// Sets the remote URL used only when cloning into an absent or empty destination.
    pub fn url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }
    /// Selects a configured remote for subsequent updates.
    pub fn remote(mut self, remote: impl Into<String>) -> Self {
        self.remote = remote.into();
        self
    }
    /// Selects a branch for cloning and subsequent fast-forward updates.
    pub fn branch(mut self, branch: impl Into<String>) -> Self {
        self.branch = Some(branch.into());
        self
    }
    /// Sets a positive update interval.
    pub fn interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }
    /// Returns the local destination shown in task interfaces.
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Returns the update interval.
    pub fn every(&self) -> Duration {
        self.interval
    }
}

// ===== impl PullReport =====

impl PullReport {
    /// Returns the local repository directory.
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Returns the commit before updating, or none for a fresh clone.
    pub fn before(&self) -> Option<&str> {
        self.before.as_deref()
    }
    /// Returns the resulting local HEAD.
    pub fn after(&self) -> &str {
        &self.after
    }
    /// Returns whether a new repository was cloned.
    pub fn cloned(&self) -> bool {
        self.cloned
    }
    /// Returns whether this operation changed the local checkout.
    pub fn changed(&self) -> bool {
        self.before.as_deref() != Some(&self.after)
    }
}

// ===== impl PullTask =====

impl PullTask {
    /// Validates settings without accessing the network or changing a repository.
    pub fn new(mut options: PullOptions) -> Result<Self> {
        ensure!(
            !options.interval.is_zero() && Instant::now().checked_add(options.interval).is_some(),
            "Pull interval must be positive and within the platform clock range"
        );
        ensure!(
            !options.remote.is_empty()
                && !options.remote.starts_with('-')
                && !options.remote.contains(['\n', '\r']),
            "Invalid remote name"
        );
        if let Some(url) = &options.url {
            ensure!(
                !url.is_empty() && !url.starts_with('-') && !url.contains(['\n', '\r']),
                "Invalid clone URL"
            );
        }
        if !options.path.is_absolute() {
            options.path = std::env::current_dir()?.join(&options.path);
        }
        Ok(Self {
            options,
            git: None,
            branch: None,
            initial_clone: false,
        })
    }

    /// Clones when needed, otherwise updates the pinned branch with `--ff-only`.
    /// Refuses dirty worktrees and never stashes, resets, rebases or resolves conflicts.
    pub fn update(&mut self) -> Result<PullReport> {
        if self.git.is_none() {
            self.prepare()?;
        }
        let git = self.git.as_ref().context("Repository was not prepared")?;
        let common = git.text(["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
        let _lock = Lock::acquire(&PathBuf::from(common).join("gitwatch.lock"))?;
        let current = git.text(["symbolic-ref", "--quiet", "HEAD"])?;
        ensure!(
            self.branch.as_deref() == Some(&current),
            "Local branch changed; restart the pull task explicitly"
        );
        ensure!(
            git.run(["status", "--porcelain=v1", "-z", "--untracked-files=all"])?
                .is_empty(),
            "Local repository has uncommitted files; update skipped"
        );
        for marker in [
            "MERGE_HEAD",
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "rebase-merge",
            "rebase-apply",
        ] {
            let path = git.text(["rev-parse", "--path-format=absolute", "--git-path", marker])?;
            ensure!(
                !Path::new(&path).try_exists()?,
                "Resolve the active Git operation before pulling"
            );
        }
        let before = git.resolve("HEAD")?;
        if !self.initial_clone {
            let mut args = vec!["pull", "--ff-only", "--no-rebase", &self.options.remote];
            if let Some(branch) = &self.options.branch {
                args.push(branch);
            }
            git.run(args)?;
        }
        let cloned = std::mem::take(&mut self.initial_clone);
        Ok(PullReport {
            path: self.options.path.clone(),
            before: (!cloned).then_some(before),
            after: git.resolve("HEAD")?,
            cloned,
        })
    }

    /// Updates immediately, then repeats at the configured interval until stopped.
    /// Failures are reported without discarding the task or local changes.
    pub fn run(self, stop: StopToken, report: impl FnMut(Result<PullReport>)) -> Result<()> {
        self.run_scheduled(stop, report, |_| {})
    }

    /// Reports the actual monotonic deadline, or none while an update is executing.
    pub(crate) fn run_scheduled(
        mut self,
        stop: StopToken,
        mut report: impl FnMut(Result<PullReport>),
        mut schedule: impl FnMut(Option<Instant>),
    ) -> Result<()> {
        let mut next = Instant::now();
        while !stop.is_stopped() {
            if Instant::now() >= next {
                schedule(None);
                report(self.update());
                next = Instant::now() + self.options.interval;
                schedule(Some(next));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    }

    fn prepare(&mut self) -> Result<()> {
        let destination = &self.options.path;
        let absent = !destination.try_exists()?;
        let empty = !absent && destination.is_dir() && fs::read_dir(destination)?.next().is_none();
        if absent || empty {
            let url = self
                .options
                .url
                .as_deref()
                .context("Destination is empty; provide a repository URL to clone")?;
            let parent = destination.parent().context("Destination has no parent")?;
            fs::create_dir_all(parent)?;
            let parent = dunce::canonicalize(parent)?;
            let staging = tempfile::tempdir_in(&parent)?;
            let checkout = staging.path().join("checkout");
            let mut command = git::base_command();
            command
                .current_dir(&parent)
                .arg("clone")
                .arg("--origin")
                .arg(&self.options.remote);
            if let Some(branch) = &self.options.branch {
                command.arg("--branch").arg(branch);
            }
            command.arg("--").arg(url).arg(&checkout);
            git::execute(command, None, Duration::from_secs(300))?.check("clone")?;
            if empty {
                fs::remove_dir(destination)
                    .context("Destination changed while cloning; checkout was not installed")?;
            }
            fs::rename(&checkout, destination).context("Cannot install cloned repository")?;
            self.initial_clone = true;
        }
        let path = dunce::canonicalize(destination)?;
        let mut command = git::base_command();
        command
            .current_dir(&path)
            .args(["rev-parse", "--absolute-git-dir", "--show-toplevel"]);
        let output = git::execute(command, None, Duration::from_secs(30))?;
        output.check("discover")?;
        let text = String::from_utf8(output.stdout)?;
        let mut lines = text.lines();
        let dir = PathBuf::from(lines.next().context("Missing Git directory")?);
        let root = dunce::canonicalize(lines.next().context("Missing worktree")?)?;
        ensure!(path == root, "Pull destination must be the repository root");
        let git = Git::work_tree(dir, root);
        if let Some(branch) = &self.options.branch {
            git.run(["check-ref-format", "--branch", branch])?;
        }
        let branch = git.text(["symbolic-ref", "--quiet", "HEAD"])?;
        if let Some(expected) = &self.options.branch {
            ensure!(
                branch == format!("refs/heads/{expected}"),
                "Requested branch is not checked out; switch it explicitly before starting this task"
            );
        }
        if let Some(url) = &self.options.url {
            ensure!(
                git.text(["remote", "get-url", &self.options.remote])? == *url,
                "Existing repository remote differs from the configured clone URL"
            );
        }
        self.branch = Some(branch);
        self.git = Some(git);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;

    #[test]
    fn schedule_reports_execution_then_the_real_retry_deadline() {
        let temp = tempfile::tempdir().unwrap();
        let stop = StopToken::default();
        let events = RefCell::new(Vec::new());
        let finished = Cell::new(Instant::now());
        PullTask::new(
            PullOptions::new(temp.path().join("missing")).interval(Duration::from_secs(600)),
        )
        .unwrap()
        .run_scheduled(
            stop.clone(),
            |result| {
                assert!(result.is_err());
                events.borrow_mut().push("result");
                finished.set(Instant::now());
            },
            |next| {
                if let Some(next) = next {
                    assert!(next >= finished.get() + Duration::from_secs(600));
                    events.borrow_mut().push("waiting");
                    stop.stop();
                } else {
                    events.borrow_mut().push("pulling");
                }
            },
        )
        .unwrap();
        assert_eq!(*events.borrow(), ["pulling", "result", "waiting"]);
    }
}
