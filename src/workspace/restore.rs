use std::{
    fs,
    io::ErrorKind,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::BackupStore;
use crate::{Result, paths};

/// A source-file change proposed by a restore preview.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    /// The file does not exist locally.
    Add,
    /// Local contents differ from the selected version.
    Replace,
    /// The selected version already matches the file.
    Unchanged,
}

/// One validated relative path and its expected contents before restoration.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RestoreEntry {
    path: String,
    blob: String,
    executable: bool,
    before: Option<String>,
    #[serde(default)]
    before_mode: Option<u32>,
    change: Change,
}

/// A saved preview bound to an immutable commit and the current local contents.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RestorePlan {
    id: Uuid,
    workspace: Uuid,
    root: PathBuf,
    commit: String,
    created: u64,
    entries: Vec<RestoreEntry>,
}

/// An applied restore. Partial failures retain recovery copies and completed paths.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RestoreReport {
    recovery: PathBuf,
    written: Vec<String>,
    error: Option<String>,
}

// ===== impl RestoreEntry =====

impl RestoreEntry {
    /// Returns the project-relative file path.
    pub fn path(&self) -> &str {
        &self.path
    }
    /// Returns the proposed operation; restoration never deletes extra local files.
    pub fn change(&self) -> Change {
        self.change
    }
}

// ===== impl RestorePlan =====

impl RestorePlan {
    /// Returns the workspace whose destination binding was previewed.
    pub fn workspace(&self) -> Uuid {
        self.workspace
    }
    /// Returns the token required to confirm this exact preview.
    pub fn id(&self) -> Uuid {
        self.id
    }
    /// Returns the immutable version being restored.
    pub fn commit(&self) -> &str {
        &self.commit
    }
    /// Returns the proposed file operations.
    pub fn entries(&self) -> &[RestoreEntry] {
        &self.entries
    }
    /// Returns the destination directory shown during confirmation.
    pub fn root(&self) -> &std::path::Path {
        &self.root
    }
}

// ===== impl RestoreReport =====

impl RestoreReport {
    /// Returns the directory containing pre-restore copies and the operation record.
    pub fn recovery(&self) -> &std::path::Path {
        &self.recovery
    }
    /// Returns paths that were actually written.
    pub fn written(&self) -> &[String] {
        &self.written
    }
    /// Returns a partial failure, if any; already written files are not rolled back silently.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

// ===== impl BackupStore =====

impl BackupStore {
    /// Loads and revalidates a saved restore preview without writing source files.
    pub fn restore_plan(&self, id: Uuid) -> Result<RestorePlan> {
        let _lock = self.lock()?;
        let plan = self.load_plan(id)?;
        self.validate_plan(&plan)?;
        Ok(plan)
    }
    /// Saves a restore preview. An empty selection means all files in that version.
    /// No source file is changed, and extra local files are never scheduled for deletion.
    pub fn preview_restore(
        &self,
        workspace: Uuid,
        revision: &str,
        selected: &[String],
    ) -> Result<RestorePlan> {
        let _lock = self.lock()?;
        let config = self.config()?;
        let binding = self.workspace(&config, workspace)?;
        let root = paths::root(&binding.root)?;
        paths::disjoint(&root, &self.data)?;
        let commit = self.git.resolve(revision)?;
        self.manifest(&commit, Some(workspace))?;
        let blobs = self.blobs(&commit)?;
        for path in selected {
            ensure!(
                blobs.contains_key(path),
                "Selected file is not in this version: {path}"
            );
        }
        let mut entries = Vec::new();
        let mut total = 0;
        for (path, blob) in blobs {
            if !selected.is_empty() && !selected.contains(&path) {
                continue;
            }
            let destination = paths::joined(&root, &path)?;
            let current = local_contents(&destination)?;
            let before_mode = local_mode(&destination)?;
            let bytes = self.git.run(["cat-file", "blob", &blob.oid])?;
            total += bytes.len();
            ensure!(
                bytes.len() as u64 <= paths::MAX_FILE && total <= paths::MAX_SNAPSHOT,
                "Restore exceeds file or snapshot size limit"
            );
            let change = match &current {
                None => Change::Add,
                Some(current)
                    if *current == bytes
                        && before_mode
                            .is_none_or(|mode| (mode & 0o111 != 0) == blob.executable) =>
                {
                    Change::Unchanged
                }
                Some(_) => Change::Replace,
            };
            entries.push(RestoreEntry {
                path,
                blob: blob.oid,
                executable: blob.executable,
                before: current.as_deref().map(paths::digest),
                before_mode,
                change,
            });
        }
        let plan = RestorePlan {
            id: Uuid::new_v4(),
            workspace,
            root,
            commit,
            created: timestamp()?,
            entries,
        };
        paths::atomic_write(&self.plan_path(plan.id), &serde_json::to_vec_pretty(&plan)?)?;
        Ok(plan)
    }

    /// Reads one file's current and proposed bytes for a side-by-side preview.
    pub fn restore_contents(
        &self,
        plan: Uuid,
        relative: &str,
    ) -> Result<(Option<Vec<u8>>, Vec<u8>)> {
        let _lock = self.lock()?;
        let plan = self.load_plan(plan)?;
        self.validate_plan(&plan)?;
        let entry = plan
            .entries
            .iter()
            .find(|entry| entry.path == relative)
            .context("File is not in this restore plan")?;
        let current = local_contents(&paths::joined(&plan.root, relative)?)?;
        Ok((current, self.git.run(["cat-file", "blob", &entry.blob])?))
    }

    /// Applies a previously confirmed preview after checking every destination again.
    /// Copies all overwritten files to local recovery storage before the first write.
    /// Multi-file restoration is not atomic; inspect the returned partial-failure field.
    pub fn apply_restore(&self, id: Uuid) -> Result<RestoreReport> {
        let _lock = self.lock()?;
        let plan = self.load_plan(id)?;
        self.validate_plan(&plan)?;
        let recovery = self.data.join("recovery").join(id.to_string());
        ensure!(
            !recovery.exists(),
            "Restore plan was already applied; create a fresh preview"
        );
        fs::create_dir_all(&recovery)?;
        let recovery_files = recovery.join("files");
        fs::create_dir_all(&recovery_files)?;
        let mut pending = Vec::new();
        let mut total = 0;
        for entry in &plan.entries {
            if entry.change == Change::Unchanged {
                continue;
            }
            let destination = paths::joined(&plan.root, &entry.path)?;
            if let Some(bytes) = local_contents(&destination)? {
                let copy = paths::joined(&recovery_files, &entry.path)?;
                paths::atomic_write(&copy, &bytes)?;
                fs::set_permissions(&copy, fs::metadata(&destination)?.permissions())?;
            }
            let bytes = self.git.run(["cat-file", "blob", &entry.blob])?;
            total += bytes.len();
            ensure!(
                bytes.len() as u64 <= paths::MAX_FILE && total <= paths::MAX_SNAPSHOT,
                "Restore exceeds size limit; source was not modified"
            );
            pending.push((entry, bytes));
        }
        paths::atomic_write(
            &recovery.join("restore-plan.json"),
            &serde_json::to_vec_pretty(&plan)?,
        )?;
        let mut report = RestoreReport {
            recovery,
            written: Vec::new(),
            error: None,
        };
        for (entry, bytes) in pending {
            let result = (|| -> Result<()> {
                let destination = paths::joined(&plan.root, &entry.path)?;
                ensure!(
                    local_contents(&destination)?.as_deref().map(paths::digest) == entry.before,
                    "File changed after preview: {}",
                    entry.path
                );
                ensure!(
                    local_mode(&destination)? == entry.before_mode,
                    "File permissions changed after preview: {}",
                    entry.path
                );
                paths::atomic_write(&destination, &bytes)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = entry.before_mode.unwrap_or(0o644);
                    let mode = if entry.executable {
                        mode | ((mode & 0o444) >> 2) | 0o100
                    } else {
                        mode & !0o111
                    };
                    fs::set_permissions(&destination, fs::Permissions::from_mode(mode))?;
                }
                Ok(())
            })();
            if let Err(error) = result {
                report.error = Some(error.to_string());
                break;
            }
            report.written.push(entry.path.clone());
        }
        if report.error.is_none() {
            let config = self.config()?;
            let workspace = self.workspace(&config, plan.workspace)?;
            let reference = workspace.reference();
            let old = self.git.reference(&reference)?;
            // A full restore can acknowledge a fetched fast-forward. Older or divergent
            // versions are restored as working files without discarding local history.
            if plan.entries.len() == self.blobs(&plan.commit)?.len()
                && old
                    .as_deref()
                    .map(|old| self.git.ancestor(old, &plan.commit))
                    .transpose()?
                    .unwrap_or(true)
                && let Err(error) = self
                    .git
                    .update_ref(&reference, &plan.commit, old.as_deref())
            {
                report.error = Some(format!("Files restored, but branch update failed: {error}"));
            }
        }
        paths::atomic_write(
            &report.recovery.join("result.json"),
            &serde_json::to_vec_pretty(&report)?,
        )?;
        Ok(report)
    }

    fn plan_path(&self, id: Uuid) -> PathBuf {
        self.data.join("plans").join(format!("{id}.json"))
    }

    fn load_plan(&self, id: Uuid) -> Result<RestorePlan> {
        let plan: RestorePlan = serde_json::from_slice(&paths::read_file(&self.plan_path(id))?)?;
        ensure!(plan.id == id, "Restore plan identity changed");
        Ok(plan)
    }

    fn validate_plan(&self, plan: &RestorePlan) -> Result<()> {
        ensure!(
            timestamp()?.saturating_sub(plan.created) < 24 * 60 * 60,
            "Restore preview expired; create a new preview"
        );
        let config = self.config()?;
        let workspace = self.workspace(&config, plan.workspace)?;
        ensure!(
            paths::root(&workspace.root)? == plan.root,
            "Workspace binding changed after preview"
        );
        paths::disjoint(&plan.root, &self.data)?;
        self.manifest(&plan.commit, Some(plan.workspace))?;
        let blobs = self.blobs(&plan.commit)?;
        let mut seen = std::collections::HashSet::new();
        for entry in &plan.entries {
            ensure!(seen.insert(&entry.path), "Duplicate restore path");
            let blob = blobs
                .get(&entry.path)
                .context("Restore plan does not match its commit")?;
            ensure!(
                blob.oid == entry.blob && blob.executable == entry.executable,
                "Restore plan object changed"
            );
            let current = local_contents(&paths::joined(&plan.root, &entry.path)?)?;
            ensure!(
                local_mode(&paths::joined(&plan.root, &entry.path)?)? == entry.before_mode,
                "File permissions changed after preview: {}",
                entry.path
            );
            ensure!(
                current.as_deref().map(paths::digest) == entry.before,
                "File changed after preview: {}",
                entry.path
            );
        }
        Ok(())
    }
}

fn local_contents(path: &std::path::Path) -> Result<Option<Vec<u8>>> {
    match paths::read_file(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn local_mode(path: &std::path::Path) -> Result<Option<u32>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match fs::symlink_metadata(path) {
            Ok(metadata) => Ok(Some(metadata.permissions().mode() & 0o777)),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(None)
    }
}

fn timestamp() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}
