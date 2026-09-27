use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, ensure};
use uuid::Uuid;

use super::{
    BackupReport, HistoryEntry, Manifest, RemoteWorkspace, UploadState, Workspace, model::Config,
    scan,
};
use crate::{
    Result,
    git::{self, Git, Lock},
    paths,
};

/// Local bindings and a bare Git repository containing independent workspace branches.
/// Data operations wait for concurrent store operations to finish.
#[derive(Clone, Debug)]
pub struct BackupStore {
    pub(crate) data: PathBuf,
    pub(crate) git: Git,
}

#[derive(Clone)]
pub(crate) struct Blob {
    pub oid: String,
    pub executable: bool,
}

impl BackupStore {
    /// Opens an existing store or initializes an empty directory.
    /// Refuses unrelated contents; local language preferences may already exist.
    pub fn open(data: impl AsRef<Path>) -> Result<Self> {
        fs::create_dir_all(data.as_ref())?;
        let data = paths::root(data.as_ref())?;
        let _lock = Lock::wait(&data.join("store.lock"))?;
        let config_path = data.join("config.json");
        if !config_path.exists() {
            ensure!(
                fs::read_dir(&data)?.all(|entry| entry.is_ok_and(|e| {
                    [
                        "store.lock",
                        "preferences.json",
                        "update-check.json",
                        "update-check.lock",
                    ]
                    .iter()
                    .any(|name| e.file_name() == *name)
                        && e.file_type().is_ok_and(|kind| kind.is_file())
                })),
                "Directory is not an empty gitwatch store"
            );
            let staging = tempfile::tempdir_in(&data)?;
            git::init_bare(&staging.path().join("backup.git"))?;
            fs::rename(staging.path().join("backup.git"), data.join("backup.git"))?;
            paths::atomic_write(
                &config_path,
                &serde_json::to_vec_pretty(&Config::default())?,
            )?;
        }
        ensure!(
            paths::no_link(&data.join("backup.git"))?.is_dir(),
            "Backup repository is unavailable"
        );
        let store = Self {
            git: Git::bare(data.join("backup.git")),
            data,
        };
        ensure!(
            store.git.text(["rev-parse", "--is-bare-repository"])? == "true",
            "Expected an application-owned bare repository"
        );
        store.config()?;
        Ok(store)
    }

    /// Returns the platform-specific default store directory.
    pub fn default_directory() -> Result<PathBuf> {
        Ok(directories::ProjectDirs::from("", "", "gitwatch")
            .context("Cannot locate user data directory")?
            .data_local_dir()
            .to_path_buf())
    }

    /// Returns the local data directory, including recovery copies.
    pub fn directory(&self) -> &Path {
        &self.data
    }

    /// Computes a content fingerprint for periodic reconciliation without writing Git objects.
    pub fn fingerprint(&self, id: Uuid) -> Result<String> {
        use sha2::{Digest, Sha256};
        let _lock = self.lock()?;
        let config = self.config()?;
        let workspace = self.workspace(&config, id)?;
        self.validate(workspace)?;
        let mut hash = Sha256::new();
        hash.update(serde_json::to_vec(workspace)?);
        for (path, file) in scan::collect(workspace, &self.data)? {
            hash.update((path.len() as u64).to_le_bytes());
            hash.update(path.as_bytes());
            hash.update([0, u8::from(file.executable)]);
            hash.update((file.bytes.len() as u64).to_le_bytes());
            hash.update(file.bytes);
        }
        Ok(format!("{:x}", hash.finalize()))
    }
    /// Returns the bare repository path for explicit Git inspection.
    pub fn repository(&self) -> &Path {
        self.git.dir()
    }

    /// Lists local workspace bindings without accessing source directories.
    pub fn workspaces(&self) -> Result<Vec<Workspace>> {
        let _lock = self.lock()?;
        Ok(self.config()?.workspaces)
    }

    /// Registers a new workspace without copying or uploading project files.
    pub fn register(&self, workspace: Workspace) -> Result<()> {
        let _tasks = Lock::wait(&self.data.join("tasks.lock"))?;
        let _lock = self.lock()?;
        self.validate(&workspace)?;
        let mut config = self.config()?;
        self.ensure_unique_name(&config, workspace.id, &workspace.name)?;
        self.check_branch(&config, &workspace)?;
        ensure!(
            !config
                .workspaces
                .iter()
                .any(|w| w.id == workspace.id || w.branch.eq_ignore_ascii_case(&workspace.branch)),
            "Workspace identity or branch is already registered"
        );
        ensure!(
            self.git.reference(&workspace.reference())?.is_none(),
            "Branch already exists; import it instead"
        );
        config.workspaces.push(workspace);
        self.save_config(&config)
    }

    /// Updates a binding, migrating local and remote history when its branch changes.
    /// A conflicting or unavailable remote leaves the binding unchanged.
    pub fn update(&self, workspace: Workspace) -> Result<()> {
        let _tasks = Lock::wait(&self.data.join("tasks.lock"))?;
        let _lock = self.lock()?;
        self.validate(&workspace)?;
        let mut config = self.config()?;
        self.ensure_unique_name(&config, workspace.id, &workspace.name)?;
        self.check_branch(&config, &workspace)?;
        ensure!(
            !config
                .workspaces
                .iter()
                .any(|w| w.id != workspace.id && w.branch.eq_ignore_ascii_case(&workspace.branch)),
            "Workspace identity or branch is already registered"
        );
        let existing = config
            .workspaces
            .iter()
            .position(|w| w.id == workspace.id)
            .context("Unknown workspace")?;
        let old = &config.workspaces[existing];
        let renamed = old.branch != workspace.branch;
        let old_reference = old.reference();
        let old_commit = self.git.reference(&old_reference)?;
        let report = if renamed {
            self.rename_branch(old, &workspace, config.remote.is_some())?
        } else {
            None
        };
        config.workspaces[existing] = workspace;
        self.save_config(&config)?;
        if let Some(report) = report {
            self.save_report(&report)?;
        }
        if renamed && let Some(commit) = old_commit {
            self.git
                .run(["update-ref", "--no-deref", "-d", &old_reference, &commit])?;
        }
        if renamed {
            self.git.run([
                "update-ref",
                "--no-deref",
                "-d",
                &format!("refs/gitwatch/renames/{}", config.workspaces[existing].id),
            ])?;
        }
        Ok(())
    }

    /// Removes a local binding. Its branch and history remain available.
    pub fn remove(&self, id: Uuid) -> Result<()> {
        let _lock = self.lock()?;
        let mut config = self.config()?;
        let count = config.workspaces.len();
        config.workspaces.retain(|w| w.id != id);
        ensure!(config.workspaces.len() != count, "Unknown workspace");
        self.save_config(&config)
    }

    /// Configures a remote and whether subsequent backups should upload automatically.
    /// No connection is made until fetch, push or an enabled backup is requested.
    pub fn set_remote(&self, remote: Option<&str>, auto_push: bool) -> Result<()> {
        let _lock = self.lock()?;
        let mut config = self.config()?;
        if let Some(remote) = remote {
            ensure!(
                !remote.is_empty() && !remote.starts_with('-') && !remote.contains(['\n', '\r']),
                "Invalid remote"
            );
            self.git
                .run(["config", "--local", "remote.origin.url", remote])?;
            self.git.run([
                "config",
                "--local",
                "remote.origin.fetch",
                "+refs/heads/*:refs/remotes/origin/*",
            ])?;
        } else if config.remote.is_some() {
            self.git.run(["remote", "remove", "origin"])?;
        }
        let changed = config.remote.as_deref() != remote;
        if changed {
            let references = self.git.text([
                "for-each-ref",
                "--format=%(refname)",
                "refs/remotes/origin/",
            ])?;
            for reference in references.lines() {
                self.git
                    .run(["update-ref", "--no-deref", "-d", reference])?;
            }
        }
        config.remote = remote.map(str::to_owned);
        config.auto_push = auto_push && remote.is_some();
        self.save_config(&config)?;
        if changed {
            for workspace in &config.workspaces {
                let path = self
                    .data
                    .join("state")
                    .join(format!("{}.json", workspace.id));
                if !path.try_exists()? {
                    continue;
                }
                let mut report: BackupReport = serde_json::from_slice(&paths::read_file(&path)?)?;
                report.upload = if remote.is_some() {
                    UploadState::Pending
                } else {
                    UploadState::Disabled
                };
                self.save_report(&report)?;
            }
        }
        Ok(())
    }

    /// Returns remote settings; callers should not put credential-bearing URLs in logs.
    pub fn remote(&self) -> Result<(Option<String>, bool)> {
        let _lock = self.lock()?;
        let config = self.config()?;
        Ok((config.remote, config.auto_push))
    }

    /// Saves selected files on this workspace's branch, retaining missing older files.
    /// A failed upload is returned in the report after the local commit is saved.
    pub fn backup(&self, id: Uuid) -> Result<BackupReport> {
        let _lock = self.lock()?;
        let config = self.config()?;
        let workspace = self.workspace(&config, id)?;
        let mut report = self.snapshot(workspace)?;
        report.upload = if config.auto_push {
            match self.push_locked(workspace) {
                Ok(()) => UploadState::Synced,
                Err(error) => UploadState::Failed {
                    message: error.to_string(),
                },
            }
        } else if config.remote.is_some() {
            UploadState::Pending
        } else {
            UploadState::Disabled
        };
        self.save_report(&report)?;
        Ok(report)
    }

    fn snapshot(&self, workspace: &Workspace) -> Result<BackupReport> {
        let id = workspace.id;
        self.validate(workspace)?;
        let collected = scan::collect(workspace, &self.data)?;
        let reference = workspace.reference();
        let parent = self.git.reference(&reference)?;
        let mut blobs = if let Some(parent) = &parent {
            self.manifest(parent, Some(id))?;
            self.blobs(parent)?
        } else {
            BTreeMap::new()
        };
        let retained = blobs
            .keys()
            .filter(|p| !collected.contains_key(*p))
            .cloned()
            .collect::<Vec<_>>();
        for (path, file) in collected {
            let oid = String::from_utf8(self.git.input(
                ["hash-object", "-w", "--stdin", "--no-filters"],
                Some(&file.bytes),
                None,
            )?)?
            .trim()
            .to_owned();
            blobs.insert(
                path,
                Blob {
                    oid,
                    executable: file.executable,
                },
            );
        }
        validate_tree(&blobs)?;
        let manifest = Manifest {
            version: 1,
            id,
            name: workspace.name.clone(),
            include: workspace.include.clone(),
            exclude: workspace.exclude.clone(),
            retained: retained.clone(),
        };
        let manifest_oid = String::from_utf8(self.git.input(
            ["hash-object", "-w", "--stdin", "--no-filters"],
            Some(&serde_json::to_vec_pretty(&manifest)?),
            None,
        )?)?
        .trim()
        .to_owned();
        let tree = self.write_tree(&blobs, &manifest_oid)?;
        let unchanged = parent
            .as_ref()
            .map(|p| self.git.text(["rev-parse", &format!("{p}^{{tree}}")]))
            .transpose()?
            .is_some_and(|old| old == tree);
        let commit = if unchanged {
            parent.context("Missing parent commit")?
        } else {
            let mut args = vec!["commit-tree", tree.as_str()];
            if let Some(parent) = &parent {
                args.extend(["-p", parent.as_str()]);
            }
            let message = format!(
                "Backup {} ({})\n",
                workspace.name,
                chrono::Local::now().to_rfc3339()
            );
            let oid = String::from_utf8(self.git.input(args, Some(message.as_bytes()), None)?)?
                .trim()
                .to_owned();
            self.git.update_ref(&reference, &oid, parent.as_deref())?;
            oid
        };
        Ok(BackupReport {
            workspace: id,
            commit,
            changed: !unchanged,
            files: blobs.len(),
            retained,
            upload: UploadState::Pending,
        })
    }

    /// Returns the most recent local backup/upload report, if one exists.
    pub fn status(&self, id: Uuid) -> Result<Option<BackupReport>> {
        let _lock = self.lock()?;
        self.workspace(&self.config()?, id)?;
        let path = self.data.join("state").join(format!("{id}.json"));
        match fs::read(path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Pushes one branch without force, creating its first snapshot if necessary.
    /// Existing snapshots can be uploaded without accessing the source directory.
    pub fn push(&self, id: Uuid) -> Result<()> {
        let _lock = self.lock()?;
        let config = self.config()?;
        ensure!(config.remote.is_some(), "Configure a backup remote first");
        let workspace = self.workspace(&config, id)?;
        let mut report = if self.git.reference(&workspace.reference())?.is_none() {
            self.snapshot(workspace)?
        } else {
            let commit = self.git.resolve(&workspace.reference())?;
            let manifest = self.manifest(&commit, Some(id))?;
            BackupReport {
                workspace: id,
                files: self.blobs(&commit)?.len(),
                commit,
                changed: false,
                retained: manifest.retained,
                upload: UploadState::Pending,
            }
        };
        self.save_report(&report)?;
        let result = self.push_locked(workspace);
        report.upload = match &result {
            Ok(()) => UploadState::Synced,
            Err(error) => UploadState::Failed {
                message: error.to_string(),
            },
        };
        self.save_report(&report)?;
        result
    }

    /// Fetches remote branches without checking out files or merging local histories.
    pub fn fetch(&self) -> Result<Vec<RemoteWorkspace>> {
        let _lock = self.lock()?;
        ensure!(
            self.config()?.remote.is_some(),
            "Configure a backup remote first"
        );
        self.git.run(["fetch", "--prune", "origin"])?;
        self.remote_workspaces_locked()
    }

    /// Lists workspaces from the last fetch without contacting the remote.
    pub fn remote_workspaces(&self) -> Result<Vec<RemoteWorkspace>> {
        let _lock = self.lock()?;
        self.remote_workspaces_locked()
    }

    /// Binds a fetched branch to a project on this machine, without restoring files.
    pub fn import(&self, branch: &str, root: impl AsRef<Path>) -> Result<Workspace> {
        let _tasks = Lock::wait(&self.data.join("tasks.lock"))?;
        let _lock = self.lock()?;
        self.git.run(["check-ref-format", "--branch", branch])?;
        let commit = self.git.resolve(&format!("refs/remotes/origin/{branch}"))?;
        let manifest = self.manifest(&commit, None)?;
        let workspace = Workspace {
            id: manifest.id,
            name: manifest.name,
            root: paths::root(root.as_ref())?,
            branch: branch.to_owned(),
            include: manifest.include,
            exclude: manifest.exclude,
            follow_links: false,
            paused: true,
        };
        self.validate(&workspace)?;
        let mut config = self.config()?;
        self.ensure_unique_name(&config, workspace.id, &workspace.name)?;
        self.check_branch(&config, &workspace)?;
        ensure!(
            !config
                .workspaces
                .iter()
                .any(|w| w.id == workspace.id || w.branch.eq_ignore_ascii_case(branch)),
            "Workspace already bound; use restore or edit its binding"
        );
        let old = self.git.reference(&workspace.reference())?;
        if let Some(old) = &old {
            ensure!(
                self.git.ancestor(old, &commit)?,
                "Local and remote history diverged; local branch was preserved"
            );
        }
        self.git
            .update_ref(&workspace.reference(), &commit, old.as_deref())?;
        config.workspaces.push(workspace.clone());
        self.save_config(&config)?;
        Ok(workspace)
    }

    /// Lists commits belonging to this workspace, optionally from a fetched revision.
    pub fn history(
        &self,
        id: Uuid,
        revision: Option<&str>,
        limit: usize,
    ) -> Result<Vec<HistoryEntry>> {
        let _lock = self.lock()?;
        let config = self.config()?;
        let workspace = self.workspace(&config, id)?;
        let reference = workspace.reference();
        if revision.is_none() && self.git.reference(&reference)?.is_none() {
            return Ok(Vec::new());
        }
        let commit = self.git.resolve(revision.unwrap_or(&reference))?;
        self.manifest(&commit, Some(id))?;
        let output = self.git.text([
            "log",
            "--format=%H%x09%at%x09%s",
            &format!("-{}", limit.clamp(1, 1000)),
            &commit,
        ])?;
        output
            .lines()
            .map(|line| {
                let mut fields = line.splitn(3, '\t');
                Ok(HistoryEntry {
                    commit: fields.next().context("Missing commit ID")?.to_owned(),
                    timestamp: fields.next().context("Missing timestamp")?.parse()?,
                    summary: fields.next().unwrap_or("").to_owned(),
                })
            })
            .collect()
    }

    /// Shows a diff between two versions of the same workspace.
    pub fn diff(&self, id: Uuid, from: &str, to: &str) -> Result<String> {
        let _lock = self.lock()?;
        self.workspace(&self.config()?, id)?;
        let from = self.git.resolve(from)?;
        let to = self.git.resolve(to)?;
        self.manifest(&from, Some(id))?;
        self.manifest(&to, Some(id))?;
        Ok(String::from_utf8_lossy(&self.git.run([
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            &from,
            &to,
            "--",
            "files/",
        ])?)
        .into_owned())
    }

    pub(crate) fn lock(&self) -> Result<Lock> {
        Lock::wait(&self.data.join("store.lock"))
    }

    pub(crate) fn config(&self) -> Result<Config> {
        let config: Config = serde_json::from_slice(&fs::read(self.data.join("config.json"))?)?;
        ensure!(config.version == 1, "Unsupported configuration version");
        Ok(config)
    }

    fn save_config(&self, config: &Config) -> Result<()> {
        paths::atomic_write(
            &self.data.join("config.json"),
            &serde_json::to_vec_pretty(config)?,
        )
    }

    // Task writers acquire tasks.lock before store.lock so all task kinds share names.
    #[cfg(any(feature = "desktop", feature = "tui"))]
    pub(crate) fn check_task_name(&self, id: Uuid, name: &str) -> Result<()> {
        let _lock = self.lock()?;
        self.ensure_unique_name(&self.config()?, id, name)
    }

    fn ensure_unique_name(&self, config: &Config, id: Uuid, name: &str) -> Result<()> {
        #[derive(serde::Deserialize)]
        struct TaskName {
            id: Uuid,
            name: String,
        }
        let name = name.trim().to_lowercase();
        ensure!(
            !config
                .workspaces
                .iter()
                .any(|w| w.id != id && w.name.trim().to_lowercase() == name),
            "A task with this name already exists in this workspace"
        );
        let path = self.data.join("tasks.json");
        if path.try_exists()? {
            let tasks: Vec<TaskName> = serde_json::from_slice(&paths::read_file(&path)?)?;
            ensure!(
                !tasks
                    .iter()
                    .any(|task| task.id != id && task.name.trim().to_lowercase() == name),
                "A task with this name already exists in this workspace"
            );
        }
        Ok(())
    }

    fn check_branch(&self, config: &Config, workspace: &Workspace) -> Result<()> {
        let branches =
            self.git
                .text(["for-each-ref", "--format=%(refname:strip=2)", "refs/heads/"])?;
        let branch = workspace.branch.to_lowercase();
        for other in config
            .workspaces
            .iter()
            .filter(|w| w.id != workspace.id)
            .map(|w| w.branch.as_str())
            .chain(branches.lines())
        {
            if other == workspace.branch {
                continue;
            }
            let other = other.to_lowercase();
            ensure!(
                other != branch
                    && !other.starts_with(&format!("{branch}/"))
                    && !branch.starts_with(&format!("{other}/")),
                "Backup branch conflicts with an existing branch name"
            );
        }
        Ok(())
    }

    pub(crate) fn workspace<'a>(&self, config: &'a Config, id: Uuid) -> Result<&'a Workspace> {
        config
            .workspaces
            .iter()
            .find(|w| w.id == id)
            .context("Unknown workspace")
    }

    fn validate(&self, workspace: &Workspace) -> Result<()> {
        workspace.validate()?;
        ensure!(
            workspace.root.is_absolute(),
            "Workspace root must be absolute"
        );
        paths::disjoint(&paths::root(&workspace.root)?, &self.data)?;
        self.git
            .run(["check-ref-format", "--branch", &workspace.branch])
            .context(
                "Invalid backup branch; use a Git-compatible task name or set a branch explicitly",
            )?;
        Ok(())
    }

    pub(crate) fn manifest(&self, commit: &str, id: Option<Uuid>) -> Result<Manifest> {
        let bytes = self
            .git
            .run(["cat-file", "blob", &format!("{commit}:manifest.json")])?;
        ensure!(bytes.len() <= 1024 * 1024, "Manifest is too large");
        let manifest: Manifest = serde_json::from_slice(&bytes)?;
        ensure!(
            manifest.version == 1 && id.is_none_or(|id| id == manifest.id),
            "Commit does not belong to this workspace or format version"
        );
        for path in manifest.include.iter().chain(&manifest.retained) {
            paths::relative(path)?;
        }
        scan::exclusions(&manifest.exclude)?;
        Ok(manifest)
    }

    pub(crate) fn blobs(&self, commit: &str) -> Result<BTreeMap<String, Blob>> {
        let bytes = self
            .git
            .run(["ls-tree", "-rz", "--full-tree", commit, "--", "files/"])?;
        let mut files = BTreeMap::new();
        for entry in bytes.split(|b| *b == 0).filter(|e| !e.is_empty()) {
            let entry = std::str::from_utf8(entry)?;
            let (header, path) = entry.split_once('\t').context("Invalid tree entry")?;
            let parts: Vec<_> = header.split(' ').collect();
            ensure!(
                parts.len() == 3 && parts[1] == "blob" && matches!(parts[0], "100644" | "100755"),
                "Unsupported file mode in backup"
            );
            let relative = path.strip_prefix("files/").context("Invalid backup path")?;
            paths::relative(relative)?;
            files.insert(
                relative.to_owned(),
                Blob {
                    oid: parts[2].to_owned(),
                    executable: parts[0] == "100755",
                },
            );
        }
        validate_tree(&files)?;
        Ok(files)
    }

    pub(super) fn write_tree(
        &self,
        files: &BTreeMap<String, Blob>,
        manifest: &str,
    ) -> Result<String> {
        let directory = tempfile::tempdir_in(&self.data)?;
        let index = directory.path().join("index");
        self.git
            .input(["read-tree", "--empty"], None, Some(&index))?;
        let mut input = format!("100644 {manifest}\tmanifest.json\0").into_bytes();
        for (path, blob) in files {
            input.extend_from_slice(
                format!(
                    "{} {}\tfiles/{path}\0",
                    if blob.executable { "100755" } else { "100644" },
                    blob.oid
                )
                .as_bytes(),
            );
        }
        self.git.input(
            ["update-index", "-z", "--index-info"],
            Some(&input),
            Some(&index),
        )?;
        Ok(
            String::from_utf8(self.git.input(["write-tree"], None, Some(&index))?)?
                .trim()
                .to_owned(),
        )
    }

    fn push_locked(&self, workspace: &Workspace) -> Result<()> {
        let reference = workspace.reference();
        self.git
            .run(["push", "origin", &format!("{reference}:{reference}")])?;
        Ok(())
    }

    fn save_report(&self, report: &BackupReport) -> Result<()> {
        paths::atomic_write(
            &self
                .data
                .join("state")
                .join(format!("{}.json", report.workspace)),
            &serde_json::to_vec_pretty(report)?,
        )
    }

    fn remote_workspaces_locked(&self) -> Result<Vec<RemoteWorkspace>> {
        let output = self.git.text([
            "for-each-ref",
            "--format=%(refname:strip=3) %(objectname)",
            "refs/remotes/origin/",
        ])?;
        let mut workspaces = Vec::new();
        for line in output.lines() {
            let (branch, commit) = line.split_once(' ').context("Invalid remote reference")?;
            let exists = self.git.output(
                ["cat-file", "-e", &format!("{commit}:manifest.json")],
                None,
                None,
            )?;
            if exists.code != 0 {
                continue;
            }
            workspaces.push(RemoteWorkspace {
                branch: branch.to_owned(),
                commit: commit.to_owned(),
                manifest: self.manifest(commit, None)?,
            });
        }
        Ok(workspaces)
    }
}

fn validate_tree(files: &BTreeMap<String, Blob>) -> Result<()> {
    ensure!(files.len() <= 10_000, "Backup exceeds 10,000 files");
    let folded: HashSet<_> = files.keys().map(|path| path.to_lowercase()).collect();
    ensure!(
        folded.len() == files.len(),
        "Case-insensitive path collision"
    );
    let mut directories = BTreeMap::new();
    for path in files.keys() {
        paths::relative(path)?;
        let mut parent = Path::new(path).parent();
        while let Some(path) = parent {
            ensure!(
                !folded.contains(&path.to_string_lossy().replace('\\', "/").to_lowercase()),
                "File/directory collision in retained backup"
            );
            let directory = path.to_string_lossy().replace('\\', "/");
            if let Some(previous) = directories.insert(directory.to_lowercase(), directory.clone())
            {
                ensure!(
                    previous == directory,
                    "Case-insensitive directory collision"
                );
            }
            parent = path.parent();
        }
    }
    Ok(())
}
