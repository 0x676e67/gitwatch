use std::{
    ffi::OsString,
    fs,
    io::{ErrorKind, Read},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    Result,
    git::{self, Git, Lock},
    workspace::UploadState,
};

/// Commit and remote settings for an existing repository.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WatchOptions {
    remote: Option<String>,
    branch: Option<String>,
}

/// A direct-watch result. Upload failures do not invalidate a successful local commit.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WatchReport {
    commit: Option<String>,
    upload: UploadState,
    skipped: Option<String>,
}

/// A selected file or directory within an existing Git worktree.
pub struct Repository {
    git: Git,
    root: PathBuf,
    target: PathBuf,
    scope: OsString,
    branch: Option<String>,
    lock_path: PathBuf,
    options: WatchOptions,
    pending_push: bool,
}

// ===== impl WatchOptions =====

impl WatchOptions {
    /// Enables pushes to a remote name, path or URL.
    pub fn remote(mut self, remote: impl Into<String>) -> Self {
        self.remote = Some(remote.into());
        self
    }
    /// Sets the remote destination branch without checking out a local branch.
    pub fn branch(mut self, branch: impl Into<String>) -> Self {
        self.branch = Some(branch.into());
        self
    }
}

// ===== impl WatchReport =====

impl WatchReport {
    /// Returns a new local commit, if this operation created one.
    pub fn commit(&self) -> Option<&str> {
        self.commit.as_deref()
    }
    /// Returns the independently observed upload outcome.
    pub fn upload(&self) -> &UploadState {
        &self.upload
    }
    /// Returns why a repository operation was deferred.
    pub fn skipped(&self) -> Option<&str> {
        self.skipped.as_deref()
    }
}

// ===== impl Repository =====

impl Repository {
    /// Discovers an existing worktree. `git_dir` supports separated Git metadata.
    /// Does not initialize a repository or change branches.
    pub fn open(
        target: impl AsRef<Path>,
        git_dir: Option<&Path>,
        options: WatchOptions,
    ) -> Result<Self> {
        let target = target.as_ref();
        let metadata = fs::symlink_metadata(target).context("Watch target does not exist")?;
        ensure!(
            metadata.is_dir() || metadata.is_file() || metadata.file_type().is_symlink(),
            "Expected a file or directory"
        );
        let absolute = if metadata.is_dir() {
            dunce::canonicalize(target)?
        } else {
            let parent = target
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            dunce::canonicalize(parent)?.join(target.file_name().context("Missing filename")?)
        };
        let directory = if metadata.is_dir() {
            absolute.clone()
        } else {
            absolute.parent().context("Missing parent")?.to_path_buf()
        };
        let (dir, root) = if let Some(dir) = git_dir {
            (dunce::canonicalize(dir)?, directory)
        } else {
            let mut discover = git::base_command();
            discover.current_dir(&directory).args([
                "rev-parse",
                "--absolute-git-dir",
                "--show-toplevel",
            ]);
            let output = git::execute(discover, None, Duration::from_secs(30))?;
            output.check("discover")?;
            let output = String::from_utf8(output.stdout)?;
            let mut lines = output.lines();
            (
                dunce::canonicalize(lines.next().context("Missing Git directory")?)?,
                dunce::canonicalize(lines.next().context("Missing worktree")?)?,
            )
        };
        let scope = absolute
            .strip_prefix(&root)
            .context("Target is outside the Git worktree")?;
        ensure!(
            !scope
                .components()
                .any(|c| c.as_os_str().eq_ignore_ascii_case(".git")),
            "Cannot watch Git metadata"
        );
        let scope = if scope.as_os_str().is_empty() {
            OsString::from(".")
        } else {
            scope.as_os_str().to_owned()
        };
        let git = Git::work_tree(dir, root.clone());
        ensure!(
            git.text(["rev-parse", "--is-inside-work-tree"])? == "true",
            "Target is not a Git worktree"
        );
        let common = git.text(["rev-parse", "--path-format=absolute", "--git-common-dir"])?;
        let branch = current_branch(&git)?;
        if let Some(remote) = &options.remote {
            ensure!(
                !remote.is_empty() && !remote.starts_with('-') && !remote.contains(['\n', '\r']),
                "Invalid remote"
            );
        }
        if let Some(branch) = &options.branch {
            git.run(["check-ref-format", "--branch", branch])?;
        }
        ensure!(
            options.branch.is_none() || options.remote.is_some(),
            "A destination branch requires a remote"
        );
        Ok(Self {
            git,
            root,
            target: absolute,
            scope,
            branch,
            lock_path: PathBuf::from(common).join("gitwatch.lock"),
            pending_push: options.remote.is_some(),
            options,
        })
    }

    /// Returns the selected absolute path.
    pub fn target(&self) -> &Path {
        &self.target
    }
    /// Returns the containing Git worktree.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Commits the selected scope, preserving unrelated staged files.
    /// Pre-staged changes within the scope are refused to protect partial staging.
    pub fn commit(&mut self) -> Result<WatchReport> {
        let _lock = Lock::acquire(&self.lock_path)?;
        self.check_identity()?;
        if let Some(state) = self.operation()? {
            return Ok(WatchReport {
                commit: None,
                upload: if self.options.remote.is_some() {
                    UploadState::Pending
                } else {
                    UploadState::Disabled
                },
                skipped: Some(state),
            });
        }
        let changes = self.git.run(self.scoped(&[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
        ]))?;
        if changes.is_empty() {
            return Ok(WatchReport {
                commit: None,
                upload: self.upload(),
                skipped: None,
            });
        }
        let staged = self
            .git
            .run(self.scoped(&["diff", "--cached", "--name-only", "-z"]))?;
        ensure!(
            staged.is_empty(),
            "Selected paths contain staged changes; commit or unstage them before watching"
        );
        self.git.run(self.scoped(&["add", "--all"]))?;
        let difference =
            self.git
                .output(self.scoped(&["diff", "--cached", "--quiet"]), None, None)?;
        ensure!(
            matches!(difference.code, 0 | 1),
            "Could not inspect selected staged changes"
        );
        if difference.code == 0 {
            return Ok(WatchReport {
                commit: None,
                upload: self.upload(),
                skipped: None,
            });
        }
        let message = format!(
            "Scripted auto-commit on change ({}) by gitwatch",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
        );
        ensure!(
            !message.trim().is_empty(),
            "Commit message is empty; staged changes were preserved"
        );
        self.check_identity()?;
        let args = vec![
            OsString::from("commit"),
            "--only".into(),
            "-m".into(),
            message.into(),
            "--".into(),
            self.scope.clone(),
        ];
        self.git.run(args)?;
        let commit = self.git.resolve("HEAD")?;
        self.pending_push = self.options.remote.is_some();
        let upload = self.upload();
        Ok(WatchReport {
            commit: Some(commit),
            upload,
            skipped: None,
        })
    }

    /// Retries an outstanding upload independently from file changes.
    pub fn retry_upload(&mut self) -> Result<UploadState> {
        let _lock = Lock::acquire(&self.lock_path)?;
        self.check_identity()?;
        ensure!(
            self.operation()?.is_none(),
            "Resolve the active Git operation before uploading"
        );
        Ok(self.upload())
    }

    /// Hashes selected contents to reconcile missed events without relying on mtimes.
    pub fn fingerprint(&self) -> Result<String> {
        self.check_identity()?;
        ensure!(self.root.is_dir(), "Worktree is unavailable");
        let paths = self.git.run(self.scoped(&[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ]))?;
        let mut hash = Sha256::new();
        for relative in paths.split(|b| *b == 0).filter(|p| !p.is_empty()) {
            hash.update((relative.len() as u64).to_le_bytes());
            hash.update(relative);
            let path = self.root.join(os_path(relative)?);
            match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    hash.update(b"symlink\0");
                    hash.update(fs::read_link(&path)?.as_os_str().as_encoded_bytes());
                }
                Ok(metadata) if metadata.is_file() => {
                    hash.update(b"file\0");
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        hash.update((metadata.permissions().mode() & 0o111).to_le_bytes());
                    }
                    hash.update(metadata.len().to_le_bytes());
                    let mut file = fs::File::open(&path)?;
                    let mut buffer = [0; 64 * 1024];
                    loop {
                        let count = file.read(&mut buffer)?;
                        if count == 0 {
                            break;
                        }
                        hash.update(&buffer[..count]);
                    }
                }
                Ok(metadata) if metadata.is_dir() => {
                    hash.update(b"gitlink\0");
                    // A gitlink is one selected entry, never a recursive source tree.
                    let mut command = git::base_command();
                    command.current_dir(&path).args(["rev-parse", "HEAD"]);
                    let output = git::execute(command, None, Duration::from_secs(30))?;
                    output.check("submodule status")?;
                    hash.update(output.stdout);
                }
                Ok(_) => ensure!(false, "Unsupported source file type"),
                Err(error) if error.kind() == ErrorKind::NotFound => hash.update(b"missing"),
                Err(error) => return Err(error.into()),
            }
            hash.update([0]);
        }
        Ok(format!("{:x}", hash.finalize()))
    }

    fn scoped(&self, args: &[&str]) -> Vec<OsString> {
        args.iter()
            .map(OsString::from)
            .chain([OsString::from("--"), self.scope.clone()])
            .collect()
    }

    fn check_identity(&self) -> Result<()> {
        ensure!(
            current_branch(&self.git)? == self.branch,
            "Current branch changed; restart this watch explicitly"
        );
        Ok(())
    }

    fn operation(&self) -> Result<Option<String>> {
        Ok(self
            .git
            .operation()?
            .map(|marker| format!("Repository operation in progress: {marker}")))
    }

    fn upload(&mut self) -> UploadState {
        let Some(remote) = &self.options.remote else {
            return UploadState::Disabled;
        };
        if !self.pending_push {
            return UploadState::Synced;
        }
        let result = (|| -> Result<()> {
            let destination = self
                .options
                .branch
                .as_ref()
                .map(|branch| format!("HEAD:refs/heads/{branch}"));
            let mut args = vec!["push", remote];
            if let Some(destination) = &destination {
                args.push(destination);
            }
            self.git.run(args)?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.pending_push = false;
                UploadState::Synced
            }
            Err(error) => UploadState::Failed {
                message: error.to_string(),
            },
        }
    }
}

fn current_branch(git: &Git) -> Result<Option<String>> {
    let output = git.output(["symbolic-ref", "--quiet", "HEAD"], None, None)?;
    if output.code == 1 {
        return Ok(None);
    }
    output.check("symbolic-ref")?;
    Ok(Some(String::from_utf8(output.stdout)?.trim().to_owned()))
}

fn os_path(bytes: &[u8]) -> Result<OsString> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Ok(OsString::from_vec(bytes.to_vec()))
    }
    #[cfg(not(unix))]
    {
        Ok(std::str::from_utf8(bytes)?.into())
    }
}
