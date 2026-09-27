//! Periodic repository updates with an explicit history integration strategy.

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

/// How a pull integrates remote history with local commits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum PullStrategy {
    /// Refuse divergent history without rewriting or merging local commits.
    #[default]
    #[serde(rename = "ff-only")]
    #[value(name = "ff-only")]
    FastForwardOnly,
    /// Fast-forward when possible, otherwise create a merge commit.
    Merge,
    /// Replay local commits on the fetched history, changing their commit IDs.
    Rebase,
}

/// A local repository and optional clone source for a periodic update task.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PullOptions {
    path: PathBuf,
    url: Option<String>,
    remote: String,
    branch: Option<String>,
    interval: Duration,
    #[serde(default)]
    strategy: PullStrategy,
}

/// The result of cloning or updating a local repository.
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
    blocked: bool,
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
            strategy: PullStrategy::default(),
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
    /// Selects a branch for cloning and subsequent updates.
    pub fn branch(mut self, branch: impl Into<String>) -> Self {
        self.branch = Some(branch.into());
        self
    }
    /// Sets a positive update interval.
    pub fn interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }
    /// Selects how remote history is integrated. Defaults to fast-forward only.
    pub fn strategy(mut self, strategy: PullStrategy) -> Self {
        self.strategy = strategy;
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
            blocked: false,
        })
    }

    /// Clones when needed, otherwise updates the pinned branch using the selected strategy.
    /// Refuses dirty worktrees; conflicts are left for manual resolution or abort.
    pub fn update(&mut self) -> Result<PullReport> {
        self.blocked = false;
        if self.git.is_none() {
            self.prepare()?;
        }
        let git = self.git.as_ref().context("Repository was not prepared")?;
        let common = git.text(["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
        let _lock = Lock::acquire(&PathBuf::from(common).join("gitwatch.lock"))?;
        self.blocked = has_operation(git)?;
        ensure!(
            !self.blocked,
            "Pull task stopped; resolve or abort the active Git operation, then restart the task"
        );
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
        let before = git.resolve("HEAD")?;
        if !self.initial_clone {
            // Use the merge backend for interruption recovery; leave other local refs unchanged.
            // https://git-scm.com/docs/git-rebase#_interruptability
            let mut args = vec![
                "-c",
                "rebase.backend=merge",
                "-c",
                "rebase.updateRefs=false",
                "pull",
                "--no-autostash",
                "--no-squash",
            ];
            args.extend_from_slice(match self.options.strategy {
                PullStrategy::FastForwardOnly => &["--ff-only", "--no-rebase"],
                PullStrategy::Merge => &["--ff", "--no-rebase", "--no-edit", "--commit"],
                PullStrategy::Rebase => &["--ff", "--rebase"],
            });
            args.push(&self.options.remote);
            if let Some(branch) = &self.options.branch {
                args.push(branch);
            }
            if let Err(error) = git
                .output(args, None, None)
                .and_then(|output| output.check("pull"))
            {
                self.blocked = has_operation(git)?;
                return if self.blocked {
                    Err(error).context("Pull task stopped; resolve or abort the active Git operation, then restart the task")
                } else {
                    Err(error)
                };
            }
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
    /// Retries ordinary failures; an unfinished Git operation stops the task with an error.
    pub fn run(self, stop: StopToken, report: impl FnMut(Result<PullReport>)) -> Result<()> {
        self.run_scheduled(stop, None, report, |_| Ok(()))
    }

    /// Reports the actual monotonic deadline, or none while an update is executing.
    pub(crate) fn run_scheduled(
        mut self,
        stop: StopToken,
        next: Option<Instant>,
        mut report: impl FnMut(Result<PullReport>),
        mut schedule: impl FnMut(Option<Instant>) -> Result<()>,
    ) -> Result<()> {
        let mut next = next.unwrap_or_else(Instant::now);
        if next > Instant::now() {
            schedule(Some(next))?;
        }
        while !stop.is_stopped() {
            if Instant::now() >= next {
                schedule(None)?;
                let result = self.update();
                if self.blocked {
                    return result.map(|_| ());
                }
                report(result);
                next = Instant::now() + self.options.interval;
                schedule(Some(next))?;
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
        self.blocked = has_operation(&git)?;
        ensure!(
            !self.blocked,
            "Pull task stopped; resolve or abort the active Git operation, then restart the task"
        );
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

fn has_operation(git: &Git) -> Result<bool> {
    for marker in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-merge",
        "rebase-apply",
        "sequencer",
    ] {
        let path = git.text(["rev-parse", "--path-format=absolute", "--git-path", marker])?;
        if Path::new(&path).try_exists()? {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;

    #[test]
    fn schedule_reports_execution_then_the_real_retry_deadline() {
        let temp = tempfile::tempdir().unwrap();
        for delay in [None, Some(Duration::from_millis(200))] {
            let stop = StopToken::default();
            let events = RefCell::new(Vec::new());
            let finished = Cell::new(Instant::now());
            let resume_at = delay.map(|delay| Instant::now() + delay);
            PullTask::new(
                PullOptions::new(temp.path().join("missing")).interval(Duration::from_secs(600)),
            )
            .unwrap()
            .run_scheduled(
                stop.clone(),
                resume_at,
                |result| {
                    assert!(result.is_err());
                    assert!(resume_at.is_none_or(|next| Instant::now() >= next));
                    events.borrow_mut().push("result");
                    finished.set(Instant::now());
                },
                |next| {
                    if let Some(next) = next {
                        if Some(next) == resume_at {
                            events.borrow_mut().push("resuming");
                            return Ok(());
                        }
                        assert!(next >= finished.get() + Duration::from_secs(600));
                        events.borrow_mut().push("waiting");
                        stop.stop();
                    } else {
                        events.borrow_mut().push("pulling");
                    }
                    Ok(())
                },
            )
            .unwrap();
            let expected: &[&str] = if resume_at.is_some() {
                &["resuming", "pulling", "result", "waiting"]
            } else {
                &["pulling", "result", "waiting"]
            };
            assert_eq!(*events.borrow(), expected);
        }
    }
}
