//! Explicit two-way synchronization in an isolated Git worktree.
//!
//! Source writes require a durable plan. A failed or interrupted application stays
//! pending, so a restart cannot mistake partially applied files for new local edits.

use std::{collections::BTreeMap, fs, path::PathBuf};

use anyhow::{Context, bail, ensure};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{BackupReport, BackupStore, Manifest, UploadState, Workspace, scan, store::Blob};
use crate::{
    Result,
    git::{self, Git},
    paths,
    pull::PullStrategy,
};

#[cfg(test)]
mod tests;

/// Integration policy and remote polling interval for an explicitly enabled task.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyncOptions {
    pub(crate) strategy: PullStrategy,
    pub(crate) interval: u64,
}

/// The durable stage of a synchronization operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncPhase {
    /// The task still uses the original backup behavior.
    Disabled,
    /// Alignment is confirmed and another synchronization can begin.
    Ready,
    /// File changes await explicit confirmation.
    Preview,
    /// History integration needs user action; source files remain unchanged.
    Conflict,
    /// A durable file application is in progress or was interrupted.
    Applying,
}

/// A proposed change inside the task's selected paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncChange {
    /// Create a selected file.
    Add,
    /// Replace the contents or executable mode of a selected file.
    Replace,
    /// Delete a previously tracked, selected file.
    Delete,
}

/// One source change, including the objects needed to recover the previous bytes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyncEntry {
    path: String,
    before: Option<Blob>,
    after: Option<Blob>,
    before_mode: Option<u32>,
}

/// Original device versions and their shared ancestor for explicit conflict resolution.
#[derive(Clone, Debug)]
pub struct ConflictContents {
    pub(crate) local: Option<Vec<u8>>,
    pub(crate) remote: Option<Vec<u8>>,
    pub(crate) base: Option<Vec<u8>>,
}

// ===== impl ConflictContents =====

impl ConflictContents {
    /// Returns the saved local file, or `None` for deletion.
    pub fn local(&self) -> Option<&[u8]> {
        self.local.as_deref()
    }
    /// Returns the fetched remote file, or `None` for deletion.
    pub fn remote(&self) -> Option<&[u8]> {
        self.remote.as_deref()
    }
    /// Returns the common ancestor's file, when one exists.
    pub fn base(&self) -> Option<&[u8]> {
        self.base.as_deref()
    }
}

/// A synchronization status or confirmation preview; reading it never fetches or writes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyncStatus {
    pub(crate) workspace: Uuid,
    pub(crate) enabled: bool,
    pub(crate) options: SyncOptions,
    pub(crate) phase: SyncPhase,
    pub(crate) token: Option<Uuid>,
    pub(crate) entries: Vec<SyncEntry>,
    pub(crate) conflicts: Vec<String>,
    pub(crate) recovery: Option<PathBuf>,
    pub(crate) next_check: Option<u64>,
}

#[derive(Clone, Serialize, Deserialize)]
struct State {
    enabled: bool,
    options: SyncOptions,
    configuration: String,
    applied: Option<String>,
    pending: Option<Pending>,
    recovery: Option<Uuid>,
    #[serde(default)]
    next_check: Option<u64>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Pending {
    id: Uuid,
    phase: SyncPhase,
    local: Option<String>,
    remote: Option<String>,
    commit: Option<String>,
    source: BTreeMap<String, Blob>,
    entries: Vec<SyncEntry>,
    session: bool,
    conflicts: Vec<String>,
    enable: bool,
}

// ===== impl SyncOptions =====

impl Default for SyncOptions {
    fn default() -> Self {
        Self {
            strategy: PullStrategy::Rebase,
            interval: 60,
        }
    }
}

impl SyncOptions {
    /// Selects how divergent history is integrated. The default is Rebase.
    pub fn strategy(mut self, strategy: PullStrategy) -> Self {
        self.strategy = strategy;
        self
    }
    /// Sets remote checks in seconds, between 10 seconds and one day.
    pub fn interval(mut self, seconds: u64) -> Self {
        self.interval = seconds;
        self
    }
    /// Returns the history integration policy.
    pub fn integration(&self) -> PullStrategy {
        self.strategy
    }
    /// Returns the remote polling interval in seconds.
    pub fn interval_seconds(&self) -> u64 {
        self.interval
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            (10..=86400).contains(&self.interval),
            "Sync interval must be between 10 and 86400 seconds"
        );
        Ok(())
    }
}

// ===== impl SyncEntry =====

impl SyncEntry {
    /// Returns the source-relative path.
    pub fn path(&self) -> &str {
        &self.path
    }
    /// Returns the proposed file operation.
    pub fn change(&self) -> SyncChange {
        if self.after.is_none() {
            SyncChange::Delete
        } else if self.before.is_none() {
            SyncChange::Add
        } else {
            SyncChange::Replace
        }
    }
}

// ===== impl SyncStatus =====

impl SyncStatus {
    /// Returns whether automatic source writes were explicitly enabled.
    pub fn enabled(&self) -> bool {
        self.enabled
    }
    /// Returns the durable operation stage.
    pub fn phase(&self) -> SyncPhase {
        self.phase
    }
    /// Returns the token required to confirm this exact preview.
    pub fn token(&self) -> Option<Uuid> {
        self.token
    }
    /// Returns pending source changes.
    pub fn entries(&self) -> &[SyncEntry] {
        &self.entries
    }
    /// Returns unresolved paths in the isolated integration worktree.
    pub fn conflicts(&self) -> &[String] {
        &self.conflicts
    }
    /// Returns the saved integration policy and polling interval.
    pub fn options(&self) -> &SyncOptions {
        &self.options
    }
    /// Returns recovery storage for the last applied operation.
    pub fn recovery(&self) -> Option<&std::path::Path> {
        self.recovery.as_deref()
    }
    /// Returns the next scheduled check as Unix seconds, preserved across restarts.
    pub fn next_check(&self) -> Option<u64> {
        self.next_check
    }
}

// ===== impl BackupStore =====

impl BackupStore {
    pub(super) fn sync_active(&self, id: Uuid) -> Result<bool> {
        let state = self.sync_state(id)?;
        ensure!(
            state.pending.is_none(),
            "Synchronization needs attention; resolve or cancel its pending operation"
        );
        Ok(state.enabled)
    }

    pub(super) fn check_sync_edit(
        &self,
        before: &Workspace,
        after: &Workspace,
        remote: Option<&str>,
    ) -> Result<()> {
        let state = self.sync_state(before.id)?;
        if state.enabled || state.pending.is_some() {
            ensure!(
                sync_configuration(before, remote.unwrap_or(""))?
                    == sync_configuration(after, remote.unwrap_or(""))?,
                "Disable sync and cancel pending operations before changing task paths, rules or branch"
            );
        }
        Ok(())
    }

    /// Reads durable sync state, including conflicts left by an earlier process.
    pub fn sync_status(&self, id: Uuid) -> Result<SyncStatus> {
        let _lock = self.lock()?;
        Ok(self.sync_view(id, &self.sync_state(id)?))
    }

    /// Previews first-time alignment, or an explicit replacement with the remote tree.
    /// This never enables automatic source writes until the returned token is confirmed.
    pub fn preview_sync(
        &self,
        id: Uuid,
        options: SyncOptions,
        overwrite: bool,
    ) -> Result<SyncStatus> {
        options.validate()?;
        let _lock = self.lock()?;
        let config = self.config()?;
        let workspace = self.workspace(&config, id)?;
        ensure!(
            !workspace.follow_links,
            "Disable symbolic-link following before enabling two-way sync"
        );
        let remote_url = config
            .remote
            .as_deref()
            .context("Configure a backup remote first")?;
        let mut state = self.sync_state(id)?;
        ensure!(
            state
                .pending
                .as_ref()
                .is_none_or(|p| p.phase != SyncPhase::Applying),
            "Finish the interrupted application before creating another preview"
        );
        ensure!(
            !state.enabled || overwrite,
            "Sync is already enabled; change its settings instead"
        );
        let source = self.sync_files(workspace)?;
        self.git.run(["fetch", "--prune", "origin"])?;
        let remote = self
            .git
            .reference(&format!("refs/remotes/origin/{}", workspace.branch))?;
        let local = self.git.reference(&workspace.reference())?;
        let commit = if let Some(remote) = &remote {
            self.sync_tree(workspace, remote)?;
            remote.clone()
        } else {
            ensure!(
                !overwrite,
                "No remote backup for this task; upload it from the other computer first"
            );
            self.sync_commit(workspace, &source, local.as_deref())?
        };
        let mut target = self.sync_tree(workspace, &commit)?;
        // Initial alignment does not interpret an empty destination as remote deletions.
        // Explicit replacement only deletes files previously tracked by this task.
        let tracked = local
            .as_deref()
            .map(|commit| self.blobs(commit))
            .transpose()?
            .unwrap_or_default();
        for (path, blob) in &source {
            if !target.contains_key(path) && (!overwrite || !tracked.contains_key(path)) {
                target.insert(path.clone(), blob.clone());
            }
        }
        let entries = self.sync_entries(workspace, &source, &target)?;
        state.options = options;
        state.configuration = sync_configuration(workspace, remote_url)?;
        state.pending = Some(Pending {
            id: Uuid::new_v4(),
            phase: SyncPhase::Preview,
            local,
            remote,
            commit: Some(commit),
            source,
            entries,
            session: false,
            conflicts: Vec::new(),
            enable: true,
        });
        self.save_sync(id, &state)?;
        Ok(self.sync_view(id, &state))
    }

    /// Changes the policy without discarding pending work. Disabling keeps saved history.
    pub fn configure_sync(&self, id: Uuid, options: SyncOptions, enabled: bool) -> Result<()> {
        options.validate()?;
        let _lock = self.lock()?;
        let mut state = self.sync_state(id)?;
        ensure!(
            state.pending.is_none(),
            "Resolve or cancel the pending synchronization first"
        );
        ensure!(
            !enabled || state.enabled,
            "Preview and confirm first-time alignment before enabling sync"
        );
        state.options = options;
        state.enabled = enabled;
        self.save_sync(id, &state)
    }

    /// Saves local changes, fetches, integrates in a private worktree, applies and pushes.
    /// Conflicts remain pending without changing source files or pushing conflict markers.
    pub fn synchronize(&self, id: Uuid) -> Result<BackupReport> {
        let _lock = self.lock()?;
        let config = self.config()?;
        let workspace = self.workspace(&config, id)?;
        let mut state = self.sync_state(id)?;
        ensure!(state.enabled, "Two-way sync is not enabled");
        self.check_sync_configuration(workspace, &state)?;
        ensure!(
            state.pending.is_none(),
            "Synchronization needs attention; resolve or cancel its pending operation"
        );
        state.next_check = Some(sync_time()?.saturating_add(state.options.interval));
        self.save_sync(id, &state)?;
        let source = self.sync_files(workspace)?;
        let old = self.git.reference(&workspace.reference())?;
        let local = self.sync_commit(workspace, &source, old.as_deref())?;
        self.git
            .update_ref(&workspace.reference(), &local, old.as_deref())?;
        self.save_report(&self.sync_report(
            workspace,
            &local,
            old.as_deref() != Some(&local),
            UploadState::Pending,
        )?)?;
        self.git.run(["fetch", "--prune", "origin"])?;
        let remote = self
            .git
            .reference(&format!("refs/remotes/origin/{}", workspace.branch))?;
        let mut pending = Pending {
            id: Uuid::new_v4(),
            phase: SyncPhase::Conflict,
            local: Some(local.clone()),
            remote: remote.clone(),
            commit: None,
            source,
            entries: Vec::new(),
            session: false,
            conflicts: Vec::new(),
            enable: true,
        };
        let commit = match remote {
            None => local.clone(),
            Some(remote) => {
                self.sync_tree(workspace, &remote)?;
                if self.git.ancestor(&remote, &local)? {
                    local.clone()
                } else if self.git.ancestor(&local, &remote)? {
                    remote
                } else {
                    state.pending = Some(pending.clone());
                    self.save_sync(id, &state)?;
                    ensure!(
                        state.options.strategy != PullStrategy::FastForwardOnly,
                        "Histories diverged; choose Rebase or Merge, or explicitly use the remote version"
                    );
                    // Validate every replayed tree before Git checks it out, including modes
                    // and metadata. Integration never uses the source repository or its index.
                    let commits = self.git.text(["rev-list", &local, &format!("^{remote}")])?;
                    for commit in commits.lines() {
                        self.sync_tree(workspace, commit)?;
                    }
                    let integration = self.create_sync_session(workspace, &pending)?;
                    pending.session = true;
                    state.pending = Some(pending.clone());
                    self.save_sync(id, &state)?;
                    let output = match state.options.strategy {
                        PullStrategy::Rebase => integration.output(
                            ["rebase", "--merge", "--no-autostash", &remote],
                            None,
                            None,
                        )?,
                        PullStrategy::Merge => integration.output(
                            ["merge", "--no-edit", "--no-autostash", &remote],
                            None,
                            None,
                        )?,
                        PullStrategy::FastForwardOnly => unreachable_strategy()?,
                    };
                    if output.code != 0 {
                        pending.conflicts = conflict_paths(&integration)?;
                        state.pending = Some(pending);
                        self.save_sync(id, &state)?;
                        if state
                            .pending
                            .as_ref()
                            .is_some_and(|p| p.conflicts.is_empty())
                        {
                            output.check("integrate")?;
                        }
                        bail!(
                            "Synchronization conflict; source files are unchanged. Resolve the listed files or use the remote version"
                        );
                    }
                    self.import_sync_result(workspace, &pending, &integration)?
                }
            }
        };
        pending.commit = Some(commit.clone());
        let changed = old.as_deref() != Some(&commit);
        pending.entries = self.sync_entries(
            workspace,
            &pending.source,
            &self.sync_tree(workspace, &commit)?,
        )?;
        if pending.entries.is_empty() {
            if local != commit {
                if !self.git.ancestor(&local, &commit)? {
                    self.git.run([
                        "update-ref",
                        &format!("refs/gitwatch/recovery/{}", pending.id),
                        &local,
                    ])?;
                }
                self.git
                    .update_ref(&workspace.reference(), &commit, Some(&local))?;
            }
            state.applied = Some(commit);
            state.pending = None;
            self.save_sync(id, &state)?;
            return self.push_sync(workspace, changed);
        }
        pending.phase = SyncPhase::Preview;
        state.pending = Some(pending);
        self.save_sync(id, &state)?;
        self.apply_sync_locked(workspace, &mut state)?;
        self.push_sync(workspace, changed)
    }

    /// Confirms the exact saved preview, or resumes its journal after interruption.
    pub fn confirm_sync(&self, id: Uuid, token: Uuid) -> Result<SyncStatus> {
        let _lock = self.lock()?;
        let config = self.config()?;
        let workspace = self.workspace(&config, id)?;
        let mut state = self.sync_state(id)?;
        ensure!(
            state.pending.as_ref().is_some_and(|p| p.id == token),
            "Sync preview changed; review the current preview"
        );
        self.apply_sync_locked(workspace, &mut state)?;
        Ok(self.sync_view(id, &state))
    }

    /// Cancels an unapplied operation. Local commits and isolated conflict files remain saved.
    pub fn cancel_sync(&self, id: Uuid) -> Result<()> {
        let _lock = self.lock()?;
        let mut state = self.sync_state(id)?;
        ensure!(
            state
                .pending
                .as_ref()
                .is_none_or(|p| p.phase != SyncPhase::Applying),
            "Finish the interrupted application before cancelling"
        );
        state.pending = None;
        self.save_sync(id, &state)
    }

    /// Previews undoing the most recent application without overwriting subsequent edits.
    /// Undo creates a normal local commit; it never rewrites the remote branch.
    pub fn preview_sync_undo(&self, id: Uuid) -> Result<SyncStatus> {
        let _lock = self.lock()?;
        let config = self.config()?;
        let workspace = self.workspace(&config, id)?;
        let mut state = self.sync_state(id)?;
        ensure!(
            state.pending.is_none(),
            "Resolve or cancel the pending synchronization first"
        );
        self.check_sync_configuration(workspace, &state)?;
        let recovery = state
            .recovery
            .context("No synchronization recovery is available")?;
        let previous: Pending = serde_json::from_slice(&paths::read_file(
            &self
                .session_path(id, recovery)
                .join("recovery/operation.json"),
        )?)?;
        let source = self.sync_files(workspace)?;
        let mut target = source.clone();
        for entry in previous.entries {
            ensure!(
                same_blob(source.get(&entry.path), entry.after.as_ref()),
                "File changed since synchronization; inspect recovery copies instead: {}",
                entry.path
            );
            if let Some(before) = entry.before {
                target.insert(entry.path, before);
            } else {
                target.remove(&entry.path);
            }
        }
        let local = self.git.reference(&workspace.reference())?;
        let commit = self.sync_commit(workspace, &target, local.as_deref())?;
        state.pending = Some(Pending {
            id: Uuid::new_v4(),
            phase: SyncPhase::Preview,
            local,
            remote: None,
            commit: Some(commit),
            entries: self.sync_entries(workspace, &source, &target)?,
            source,
            session: false,
            conflicts: Vec::new(),
            enable: state.enabled,
        });
        self.save_sync(id, &state)?;
        Ok(self.sync_view(id, &state))
    }

    /// Reads saved local and remote versions of a conflict, never Git's reversed rebase sides.
    pub fn sync_conflict_contents(&self, id: Uuid, path: &str) -> Result<ConflictContents> {
        let _lock = self.lock()?;
        let state = self.sync_state(id)?;
        let pending = state.pending.context("No pending synchronization")?;
        ensure!(
            pending.conflicts.iter().any(|p| p == path),
            "File is not an unresolved conflict"
        );
        let read = |commit: Option<&str>| -> Result<Option<Vec<u8>>> {
            let Some(commit) = commit else {
                return Ok(None);
            };
            let files = self.blobs(commit)?;
            files
                .get(path)
                .map(|blob| self.git.run(["cat-file", "blob", &blob.oid]))
                .transpose()
        };
        let base = if let (Some(local), Some(remote)) = (&pending.local, &pending.remote) {
            let output = self.git.output(["merge-base", local, remote], None, None)?;
            if output.code == 1 {
                None
            } else {
                output.check("merge-base")?;
                read(Some(String::from_utf8(output.stdout)?.trim()))?
            }
        } else {
            None
        };
        Ok(ConflictContents {
            local: read(pending.local.as_deref())?,
            remote: read(pending.remote.as_deref())?,
            base,
        })
    }

    /// Stages explicitly resolved bytes in the private worktree; `None` resolves to deletion.
    pub fn resolve_sync_file(
        &self,
        id: Uuid,
        path: &str,
        bytes: Option<&[u8]>,
    ) -> Result<SyncStatus> {
        let _lock = self.lock()?;
        let mut state = self.sync_state(id)?;
        let pending = state
            .pending
            .as_mut()
            .context("No pending synchronization")?;
        ensure!(
            pending.phase == SyncPhase::Conflict && pending.session,
            "No editable conflict session"
        );
        ensure!(
            pending.conflicts.iter().any(|p| p == path),
            "File is not an unresolved conflict"
        );
        paths::relative(path)?;
        let integration = self.sync_session(id, pending.id)?;
        let root = self.session_path(id, pending.id).join("files");
        let file = paths::joined(&root, &format!("files/{path}"))?;
        if let Some(bytes) = bytes {
            ensure!(
                bytes.len() as u64 <= paths::MAX_FILE,
                "Resolved file exceeds 16 MiB"
            );
            let permissions = fs::metadata(&file)
                .ok()
                .map(|metadata| metadata.permissions());
            paths::atomic_write(&file, bytes)?;
            if let Some(permissions) = permissions {
                fs::set_permissions(&file, permissions)?;
            }
            integration.run(["add", "--", &format!("files/{path}")])?;
        } else {
            integration.run(["rm", "-f", "--", &format!("files/{path}")])?;
        }
        pending.conflicts = conflict_paths(&integration)?;
        self.save_sync(id, &state)?;
        Ok(self.sync_view(id, &state))
    }

    /// Continues Git's merge or rebase after every conflict has been explicitly resolved.
    pub fn continue_sync(&self, id: Uuid) -> Result<SyncStatus> {
        let _lock = self.lock()?;
        let config = self.config()?;
        let workspace = self.workspace(&config, id)?;
        let mut state = self.sync_state(id)?;
        self.check_sync_configuration(workspace, &state)?;
        let pending = state
            .pending
            .as_mut()
            .context("No pending synchronization")?;
        ensure!(
            pending.phase == SyncPhase::Conflict && pending.session,
            "No integration to continue"
        );
        let integration = self.sync_session(id, pending.id)?;
        ensure!(
            conflict_paths(&integration)?.is_empty(),
            "Resolve all conflict files before continuing"
        );
        let output = match integration.operation()? {
            Some("rebase-merge" | "rebase-apply") => {
                integration.output(["rebase", "--continue"], None, None)?
            }
            Some("MERGE_HEAD") => integration.output(["commit", "--no-edit"], None, None)?,
            None => integration.output(["status", "--porcelain"], None, None)?,
            Some(_) => bail!("Unexpected operation in the integration worktree"),
        };
        pending.conflicts = conflict_paths(&integration)?;
        if output.code != 0 || !pending.conflicts.is_empty() {
            self.save_sync(id, &state)?;
            if state
                .pending
                .as_ref()
                .is_some_and(|p| p.conflicts.is_empty())
            {
                output.check("continue")?;
            }
            return Ok(self.sync_view(id, &state));
        }
        let commit = self.import_sync_result(workspace, pending, &integration)?;
        pending.entries = self.sync_entries(
            workspace,
            &pending.source,
            &self.sync_tree(workspace, &commit)?,
        )?;
        pending.commit = Some(commit);
        pending.phase = SyncPhase::Preview;
        self.save_sync(id, &state)?;
        Ok(self.sync_view(id, &state))
    }

    fn sync_state(&self, id: Uuid) -> Result<State> {
        let path = self.data.join("sync").join(format!("{id}.json"));
        match paths::read_file(&path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                Ok(State {
                    enabled: false,
                    options: SyncOptions::default(),
                    configuration: String::new(),
                    applied: None,
                    pending: None,
                    recovery: None,
                    next_check: None,
                })
            }
            Err(error) => Err(error),
        }
    }

    fn save_sync(&self, id: Uuid, state: &State) -> Result<()> {
        if let Some(pending) = &state.pending {
            let source_ref = format!("refs/gitwatch/pending/{}/source", pending.id);
            if self.git.reference(&source_ref)?.is_none() {
                let config = self.config()?;
                let workspace = self.workspace(&config, id)?;
                let source =
                    self.sync_commit(workspace, &pending.source, pending.local.as_deref())?;
                self.git.update_ref(&source_ref, &source, None)?;
            }
            for (name, commit) in [
                ("local", &pending.local),
                ("remote", &pending.remote),
                ("result", &pending.commit),
            ] {
                if let Some(commit) = commit {
                    self.git.run([
                        "update-ref",
                        &format!("refs/gitwatch/pending/{}/{name}", pending.id),
                        commit,
                    ])?;
                }
            }
        }
        paths::atomic_write(
            &self.data.join("sync").join(format!("{id}.json")),
            &serde_json::to_vec_pretty(state)?,
        )
    }

    fn sync_view(&self, id: Uuid, state: &State) -> SyncStatus {
        SyncStatus {
            workspace: id,
            enabled: state.enabled,
            options: state.options.clone(),
            phase: state.pending.as_ref().map_or(
                if state.enabled {
                    SyncPhase::Ready
                } else {
                    SyncPhase::Disabled
                },
                |p| p.phase,
            ),
            token: state.pending.as_ref().map(|p| p.id),
            entries: state
                .pending
                .as_ref()
                .map(|p| p.entries.clone())
                .unwrap_or_default(),
            conflicts: state
                .pending
                .as_ref()
                .map(|p| p.conflicts.clone())
                .unwrap_or_default(),
            recovery: state
                .recovery
                .map(|token| self.session_path(id, token).join("recovery")),
            next_check: state.next_check,
        }
    }

    fn sync_files(&self, workspace: &Workspace) -> Result<BTreeMap<String, Blob>> {
        ensure!(
            !workspace.follow_links,
            "Two-way sync does not follow symbolic links"
        );
        paths::disjoint(&paths::root(&workspace.root)?, &self.data)?;
        #[cfg(windows)]
        let previous = self
            .git
            .reference(&workspace.reference())?
            .map(|commit| self.blobs(&commit))
            .transpose()?
            .unwrap_or_default();
        let collected = scan::collect(
            workspace,
            &self.data,
            &scan::exclusions(&workspace.exclude)?,
        )?;
        if collected.is_empty() {
            return Ok(BTreeMap::new());
        }
        let staging = tempfile::tempdir_in(self.git.dir())?;
        let name = staging
            .path()
            .file_name()
            .context("Missing staging directory name")?
            .to_str()
            .context("Invalid staging directory name")?;
        let mut input = String::new();
        for (index, file) in collected.values().enumerate() {
            fs::write(staging.path().join(index.to_string()), &file.bytes)?;
            input.push_str(&format!("{name}/{index}\n"));
        }
        // Hash captured bytes in one process, without attributes or newline conversion.
        // https://git-scm.com/docs/git-hash-object#Documentation/git-hash-object.txt---no-filters
        let output = self.git.input(
            ["hash-object", "-w", "--stdin-paths", "--no-filters"],
            Some(input.as_bytes()),
            None,
        )?;
        let oids: Vec<_> = std::str::from_utf8(&output)?.lines().collect();
        ensure!(
            oids.len() == collected.len(),
            "Incomplete snapshot object list"
        );
        Ok(collected
            .into_iter()
            .zip(oids)
            .map(|((path, file), oid)| {
                #[cfg(windows)]
                let executable = previous.get(&path).is_some_and(|blob| blob.executable);
                #[cfg(not(windows))]
                let executable = file.executable;
                #[cfg(windows)]
                let _ = file;
                (
                    path,
                    Blob {
                        oid: oid.to_owned(),
                        executable,
                    },
                )
            })
            .collect())
    }

    fn sync_tree(&self, workspace: &Workspace, commit: &str) -> Result<BTreeMap<String, Blob>> {
        let manifest = self.manifest(commit, Some(workspace.id))?;
        ensure!(
            manifest.include == workspace.include && manifest.exclude == workspace.exclude,
            "Remote selection rules differ; align the task rules before syncing"
        );
        let all = self.git.run(["ls-tree", "-rz", "--full-tree", commit])?;
        for entry in all.split(|b| *b == 0).filter(|e| !e.is_empty()) {
            let (header, path) = std::str::from_utf8(entry)?
                .split_once('\t')
                .context("Invalid tree entry")?;
            ensure!(
                header.starts_with("100644 blob ") || header.starts_with("100755 blob "),
                "Unsupported file mode in sync tree"
            );
            ensure!(
                path == "manifest.json" || path.starts_with("files/"),
                "Unexpected path in sync tree"
            );
        }
        let files = self.blobs(commit)?;
        let excludes = scan::exclusions(&workspace.exclude)?;
        ensure!(
            files
                .keys()
                .all(|p| scan::selected(workspace, &excludes, p)),
            "Remote files fall outside the task selection"
        );
        let input: String = files
            .values()
            .map(|blob| format!("{}\n", blob.oid))
            .collect();
        let output = self.git.input(
            ["cat-file", "--batch-check=%(objectsize)"],
            Some(input.as_bytes()),
            None,
        )?;
        let mut total = 0;
        for line in std::str::from_utf8(&output)?.lines() {
            let size: u64 = line.parse()?;
            total += size;
            ensure!(
                size <= paths::MAX_FILE && total <= paths::MAX_SNAPSHOT as u64,
                "Sync tree exceeds size limits"
            );
        }
        Ok(files)
    }

    fn sync_commit(
        &self,
        workspace: &Workspace,
        files: &BTreeMap<String, Blob>,
        parent: Option<&str>,
    ) -> Result<String> {
        let manifest = Manifest {
            version: 1,
            id: workspace.id,
            name: workspace.name.clone(),
            include: workspace.include.clone(),
            exclude: workspace.exclude.clone(),
            retained: Vec::new(),
        };
        let manifest = String::from_utf8(self.git.input(
            ["hash-object", "-w", "--stdin"],
            Some(&serde_json::to_vec_pretty(&manifest)?),
            None,
        )?)?
        .trim()
        .to_owned();
        let tree = self.write_tree(files, &manifest)?;
        if let Some(parent) = parent
            && self
                .git
                .text(["rev-parse", &format!("{parent}^{{tree}}")])?
                == tree
        {
            return Ok(parent.to_owned());
        }
        let mut args = vec!["commit-tree", &tree];
        if let Some(parent) = parent {
            args.extend(["-p", parent]);
        }
        Ok(String::from_utf8(
            self.git
                .input(args, Some(b"Save local sync changes\n"), None)?,
        )?
        .trim()
        .to_owned())
    }

    fn sync_entries(
        &self,
        workspace: &Workspace,
        source: &BTreeMap<String, Blob>,
        target: &BTreeMap<String, Blob>,
    ) -> Result<Vec<SyncEntry>> {
        let mut paths: Vec<_> = source.keys().chain(target.keys()).collect();
        paths.sort();
        paths.dedup();
        paths
            .into_iter()
            .filter(|path| !same_blob(source.get(*path), target.get(*path)))
            .map(|path| {
                let destination = paths::joined(&workspace.root, path)?;
                ensure!(
                    !destination.is_dir(),
                    "A directory blocks the incoming file; move it aside and preview again: {path}"
                );
                Ok(SyncEntry {
                    path: path.clone(),
                    before: source.get(path).cloned(),
                    after: target.get(path).cloned(),
                    before_mode: super::restore::local_mode(&destination)?,
                })
            })
            .collect()
    }

    fn check_sync_configuration(&self, workspace: &Workspace, state: &State) -> Result<()> {
        let config = self.config()?;
        ensure!(
            state.configuration
                == sync_configuration(
                    workspace,
                    config
                        .remote
                        .as_deref()
                        .context("Configure a backup remote first")?
                )?,
            "Sync configuration changed; disable sync and preview alignment again"
        );
        Ok(())
    }

    fn apply_sync_locked(&self, workspace: &Workspace, state: &mut State) -> Result<()> {
        self.check_sync_configuration(workspace, state)?;
        let pending = state
            .pending
            .as_ref()
            .context("No pending synchronization")?;
        ensure!(
            matches!(pending.phase, SyncPhase::Preview | SyncPhase::Applying),
            "Resolve conflicts before applying synchronization"
        );
        let commit = pending
            .commit
            .as_deref()
            .context("Sync result is missing")?;
        self.sync_tree(workspace, commit)?;
        let current_ref = self.git.reference(&workspace.reference())?;
        ensure!(
            current_ref == pending.local
                || (pending.phase == SyncPhase::Applying && current_ref.as_deref() == Some(commit)),
            "Local history changed after preview"
        );
        let recovery = self.session_path(workspace.id, pending.id).join("recovery");
        if pending.phase == SyncPhase::Preview {
            ensure!(
                self.sync_files(workspace)? == pending.source,
                "Source files changed after preview; cancel and preview again"
            );
            for entry in &pending.entries {
                let destination = paths::joined(&workspace.root, &entry.path)?;
                ensure!(
                    super::restore::local_mode(&destination)? == entry.before_mode,
                    "File permissions changed after preview"
                );
                if let Some(before) = &entry.before {
                    let bytes = self.git.run(["cat-file", "blob", &before.oid])?;
                    fs::create_dir_all(recovery.join("files"))?;
                    paths::atomic_write(
                        &paths::joined(&recovery.join("files"), &entry.path)?,
                        &bytes,
                    )?;
                }
            }
            fs::create_dir_all(&recovery)?;
            paths::atomic_write(
                &recovery.join("operation.json"),
                &serde_json::to_vec_pretty(pending)?,
            )?;
            if let Some(local) = &pending.local {
                self.git.run([
                    "update-ref",
                    &format!("refs/gitwatch/recovery/{}", pending.id),
                    local,
                ])?;
            }
            state.pending.as_mut().context("Missing sync plan")?.phase = SyncPhase::Applying;
            self.save_sync(workspace.id, state)?;
        }
        let pending = state.pending.as_ref().context("Missing sync plan")?;
        let journal: Pending =
            serde_json::from_slice(&paths::read_file(&recovery.join("operation.json"))?)?;
        ensure!(
            journal.id == pending.id && journal.commit == pending.commit,
            "Sync recovery journal does not match the pending operation"
        );
        // The journal precedes all writes. Replays only accept original or planned bytes,
        // so interruption never turns a half-applied tree into a new local snapshot.
        for entry in &pending.entries {
            let destination = paths::joined(&workspace.root, &entry.path)?;
            let current = self.sync_local_blob(&destination)?;
            if same_blob(current.as_ref(), entry.after.as_ref()) {
                continue;
            }
            ensure!(
                same_blob(current.as_ref(), entry.before.as_ref()),
                "File changed during synchronization: {}",
                entry.path
            );
            ensure!(
                super::restore::local_mode(&destination)? == entry.before_mode,
                "File permissions changed during synchronization: {}",
                entry.path
            );
            if let Some(after) = &entry.after {
                paths::atomic_write(
                    &destination,
                    &self.git.run(["cat-file", "blob", &after.oid])?,
                )?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = entry.before_mode.unwrap_or(0o644);
                    let mode = if after.executable {
                        mode | ((mode & 0o444) >> 2) | 0o100
                    } else {
                        mode & !0o111
                    };
                    fs::set_permissions(&destination, fs::Permissions::from_mode(mode))?;
                }
            } else if current.is_some() {
                fs::remove_file(&destination)?;
            }
        }
        let commit = pending.commit.as_deref().context("Missing sync result")?;
        for entry in &pending.entries {
            ensure!(
                same_blob(
                    self.sync_local_blob(&paths::joined(&workspace.root, &entry.path)?)?
                        .as_ref(),
                    entry.after.as_ref()
                ),
                "Source changed while finishing synchronization: {}",
                entry.path
            );
        }
        let current = self.git.reference(&workspace.reference())?;
        if current.as_deref() != Some(commit) {
            self.git
                .update_ref(&workspace.reference(), commit, pending.local.as_deref())?;
        }
        self.save_report(&self.sync_report(
            workspace,
            commit,
            pending.local.as_deref() != Some(commit),
            UploadState::Pending,
        )?)?;
        state.applied = Some(commit.to_owned());
        state.enabled = pending.enable;
        if !pending.entries.is_empty() {
            state.recovery = Some(pending.id);
        }
        state.pending = None;
        self.save_sync(workspace.id, state)
    }

    fn sync_local_blob(&self, path: &std::path::Path) -> Result<Option<Blob>> {
        let Some(bytes) = super::restore::local_contents(path)? else {
            return Ok(None);
        };
        let oid = String::from_utf8(self.git.input(
            ["hash-object", "-w", "--stdin"],
            Some(&bytes),
            None,
        )?)?
        .trim()
        .to_owned();
        Ok(Some(Blob {
            oid,
            executable: super::restore::local_mode(path)?.is_some_and(|mode| mode & 0o111 != 0),
        }))
    }

    fn session_path(&self, id: Uuid, token: Uuid) -> PathBuf {
        self.data
            .join("sync")
            .join(id.to_string())
            .join(token.to_string())
    }

    fn create_sync_session(&self, workspace: &Workspace, pending: &Pending) -> Result<Git> {
        let path = self.session_path(workspace.id, pending.id);
        fs::create_dir_all(path.join("files"))?;
        let integration = git::init_bare(&path.join("git"))?.isolated(path.join("files"));
        integration.run(["config", "core.bare", "false"])?;
        let local = pending.local.as_deref().context("Missing local history")?;
        let remote = pending
            .remote
            .as_deref()
            .context("Missing remote history")?;
        integration.run([
            "fetch",
            "--no-tags",
            &self.git.dir().to_string_lossy(),
            local,
            remote,
        ])?;
        integration.run(["checkout", "--detach", local])?;
        Ok(integration)
    }

    fn sync_session(&self, id: Uuid, token: Uuid) -> Result<Git> {
        let path = self.session_path(id, token);
        paths::no_link(&path.join("git"))?;
        paths::no_link(&path.join("files"))?;
        Ok(Git::bare(path.join("git")).isolated(path.join("files")))
    }

    fn import_sync_result(
        &self,
        workspace: &Workspace,
        pending: &Pending,
        integration: &Git,
    ) -> Result<String> {
        let commit = integration.resolve("HEAD")?;
        if let Some(remote) = &pending.remote {
            ensure!(
                integration.ancestor(remote, &commit)?,
                "Integration has not completed; cancel and retry synchronization"
            );
        }
        self.git.run([
            "fetch",
            "--no-tags",
            &self
                .session_path(workspace.id, pending.id)
                .join("git")
                .to_string_lossy(),
            &commit,
        ])?;
        self.sync_tree(workspace, &commit)?;
        Ok(commit)
    }

    fn sync_report(
        &self,
        workspace: &Workspace,
        commit: &str,
        changed: bool,
        upload: UploadState,
    ) -> Result<BackupReport> {
        Ok(BackupReport {
            workspace: workspace.id,
            commit: commit.into(),
            changed,
            files: self.blobs(commit)?.len(),
            retained: Vec::new(),
            upload,
        })
    }

    fn push_sync(&self, workspace: &Workspace, changed: bool) -> Result<BackupReport> {
        let reference = workspace.reference();
        let commit = self
            .git
            .reference(&reference)?
            .context("Missing sync history")?;
        let upload = match self
            .git
            .run(["push", "origin", &format!("{reference}:{reference}")])
        {
            Ok(_) => UploadState::Synced,
            Err(error) => UploadState::Failed {
                message: error.to_string(),
            },
        };
        let report = self.sync_report(workspace, &commit, changed, upload)?;
        self.save_report(&report)?;
        Ok(report)
    }
}

fn sync_configuration(workspace: &Workspace, remote: &str) -> Result<String> {
    Ok(paths::digest(&serde_json::to_vec(&(
        workspace.id,
        &workspace.root,
        &workspace.branch,
        &workspace.include,
        &workspace.exclude,
        workspace.follow_links,
        remote,
    ))?))
}

fn same_blob(a: Option<&Blob>, b: Option<&Blob>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.oid == b.oid && (cfg!(windows) || a.executable == b.executable),
        (None, None) => true,
        _ => false,
    }
}

fn conflict_paths(git: &Git) -> Result<Vec<String>> {
    let bytes = git.run(["diff", "--name-only", "--diff-filter=U", "-z"])?;
    bytes
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|path| {
            let path = std::str::from_utf8(path)?;
            Ok(path
                .strip_prefix("files/")
                .context("Backup metadata conflict; align task rules or use the remote version")?
                .to_owned())
        })
        .collect()
}

fn unreachable_strategy<T>() -> Result<T> {
    bail!("Fast-forward only cannot integrate divergent history")
}

fn sync_time() -> Result<u64> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs())
}
