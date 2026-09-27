use std::path::{Path, PathBuf};

use anyhow::ensure;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Result, paths};

/// A project binding and the branch that stores its selected files.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Workspace {
    pub(crate) id: Uuid,
    pub(crate) name: String,
    pub(crate) root: PathBuf,
    pub(crate) branch: String,
    pub(crate) include: Vec<String>,
    pub(crate) exclude: Vec<String>,
    #[serde(default)]
    pub(crate) follow_links: bool,
    pub(crate) paused: bool,
}

/// Builds or edits a workspace without writing configuration or creating a branch.
pub struct WorkspaceBuilder {
    workspace: Workspace,
}

/// Portable metadata stored at the root of a workspace branch.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub(crate) version: u32,
    pub(crate) id: Uuid,
    pub(crate) name: String,
    pub(crate) include: Vec<String>,
    pub(crate) exclude: Vec<String>,
    pub(crate) retained: Vec<String>,
}

/// The last observed upload result, separate from the local commit result.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum UploadState {
    /// Uploading is not configured for this operation.
    Disabled,
    /// A local commit has not been confirmed on the remote.
    Pending,
    /// The remote accepted the local branch tip.
    Synced,
    /// Local data remains saved; uploading needs attention or a retry.
    Failed {
        /// A diagnostic that does not expose remote credentials.
        message: String,
    },
}

/// A completed local backup, including any independent upload failure.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BackupReport {
    pub(crate) workspace: Uuid,
    pub(crate) commit: String,
    pub(crate) changed: bool,
    pub(crate) files: usize,
    pub(crate) retained: Vec<String>,
    pub(crate) upload: UploadState,
}

/// One immutable version in a workspace's history.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub(crate) commit: String,
    pub(crate) timestamp: i64,
    pub(crate) summary: String,
}

/// A remotely discovered workspace that can be bound to a local directory.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemoteWorkspace {
    pub(crate) branch: String,
    pub(crate) commit: String,
    pub(crate) manifest: Manifest,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Config {
    pub version: u32,
    pub remote: Option<String>,
    pub auto_push: bool,
    pub workspaces: Vec<Workspace>,
}

// ===== impl Workspace =====

impl Workspace {
    /// Creates a workspace builder with a stable ID and an independent branch.
    pub fn builder(name: impl Into<String>, root: impl Into<PathBuf>) -> WorkspaceBuilder {
        let id = Uuid::new_v4();
        let name = name.into().trim().to_owned();
        WorkspaceBuilder {
            workspace: Self {
                id,
                branch: branch_name(&name),
                name,
                root: root.into(),
                include: Vec::new(),
                exclude: Vec::new(),
                follow_links: false,
                paused: false,
            },
        }
    }

    /// Edits settings while preserving the workspace identity.
    pub fn edit(&self) -> WorkspaceBuilder {
        WorkspaceBuilder {
            workspace: self.clone(),
        }
    }
    /// Returns the identity shared by bindings on different machines.
    pub fn id(&self) -> Uuid {
        self.id
    }
    /// Returns the user-facing name.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Returns the source directory on this machine.
    pub fn root(&self) -> &Path {
        &self.root
    }
    /// Returns the branch in the backup repository.
    pub fn branch(&self) -> &str {
        &self.branch
    }
    /// Returns the explicitly selected relative files and directories.
    pub fn includes(&self) -> &[String] {
        &self.include
    }
    /// Returns relative-path exclusion globs.
    pub fn excludes(&self) -> &[String] {
        &self.exclude
    }
    /// Returns whether backups read the contents of symbolic-link targets.
    pub fn follows_links(&self) -> bool {
        self.follow_links
    }
    /// Returns whether automatic backup is paused.
    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub(crate) fn reference(&self) -> String {
        format!("refs/heads/{}", self.branch)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            !self.name.trim().is_empty(),
            "Workspace name cannot be empty"
        );
        ensure!(
            !self.include.is_empty(),
            "Select at least one file or directory"
        );
        for path in &self.include {
            paths::relative(path)?;
        }
        super::scan::exclusions(&self.exclude)?;
        ensure!(
            !self.branch.is_empty() && !self.branch.starts_with('-'),
            "Invalid workspace branch"
        );
        Ok(())
    }
}

// ===== impl WorkspaceBuilder =====

impl WorkspaceBuilder {
    /// Sets the task name and derives its branch when the name changes.
    /// Updating a registered task migrates its history to the new branch.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        let name = name.into().trim().to_owned();
        if self.workspace.name != name {
            self.workspace.branch = branch_name(&name);
            self.workspace.name = name;
        }
        self
    }
    /// Binds this workspace to a local source directory.
    pub fn root(mut self, root: impl Into<PathBuf>) -> Self {
        self.workspace.root = root.into();
        self
    }
    /// Sets the backup branch; updating an existing binding migrates its history.
    pub fn branch(mut self, branch: impl Into<String>) -> Self {
        self.workspace.branch = branch.into();
        self
    }
    /// Adds one relative file or directory to the selection.
    pub fn include(mut self, path: impl Into<String>) -> Self {
        self.workspace.include.push(path.into());
        self
    }
    /// Replaces the complete file selection.
    pub fn includes(mut self, paths: Vec<String>) -> Self {
        self.workspace.include = paths;
        self
    }
    /// Adds a relative-path exclusion glob.
    pub fn exclude(mut self, pattern: impl Into<String>) -> Self {
        self.workspace.exclude.push(pattern.into());
        self
    }
    /// Replaces exclusion globs.
    pub fn excludes(mut self, patterns: Vec<String>) -> Self {
        self.workspace.exclude = patterns;
        self
    }
    /// Reads symbolic-link targets as ordinary files and directories. Disabled by default.
    /// Targets may be outside the source; restoration still refuses linked destinations.
    pub fn follow_links(mut self, enabled: bool) -> Self {
        self.workspace.follow_links = enabled;
        self
    }
    /// Pauses or resumes automatic backup.
    pub fn paused(mut self, paused: bool) -> Self {
        self.workspace.paused = paused;
        self
    }
    /// Validates settings and resolves the source directory without modifying it.
    pub fn build(mut self) -> Result<Workspace> {
        self.workspace.validate()?;
        self.workspace.root = paths::root(&self.workspace.root)?;
        Ok(self.workspace)
    }
}

fn branch_name(name: &str) -> String {
    name.split_whitespace().collect::<Vec<_>>().join("-")
}

// ===== impl Manifest =====

impl Manifest {
    /// Returns the portable workspace ID.
    pub fn id(&self) -> Uuid {
        self.id
    }
    /// Returns the portable display name.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Returns files retained from an older backup because they were not collected.
    pub fn retained(&self) -> &[String] {
        &self.retained
    }
}

// ===== impl BackupReport =====

impl BackupReport {
    /// Returns the saved commit ID.
    pub fn commit(&self) -> &str {
        &self.commit
    }
    /// Returns whether a new commit was created.
    pub fn changed(&self) -> bool {
        self.changed
    }
    /// Returns the number of saved files, including retained older files.
    pub fn files(&self) -> usize {
        self.files
    }
    /// Returns files retained from previous versions.
    pub fn retained(&self) -> &[String] {
        &self.retained
    }
    /// Returns the upload outcome.
    pub fn upload(&self) -> &UploadState {
        &self.upload
    }
}

// ===== impl HistoryEntry =====

impl HistoryEntry {
    /// Returns the immutable commit ID.
    pub fn commit(&self) -> &str {
        &self.commit
    }
    /// Returns seconds since the Unix epoch.
    pub fn timestamp(&self) -> i64 {
        self.timestamp
    }
    /// Returns the commit subject.
    pub fn summary(&self) -> &str {
        &self.summary
    }
}

// ===== impl RemoteWorkspace =====

impl RemoteWorkspace {
    /// Returns the remote branch name.
    pub fn branch(&self) -> &str {
        &self.branch
    }
    /// Returns the remote commit observed during discovery.
    pub fn commit(&self) -> &str {
        &self.commit
    }
    /// Returns the workspace metadata at that commit.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            remote: None,
            auto_push: false,
            workspaces: Vec::new(),
        }
    }
}
