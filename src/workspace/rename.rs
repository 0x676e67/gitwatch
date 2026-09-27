use anyhow::{Context, ensure};

use super::{BackupReport, BackupStore, UploadState, Workspace};
use crate::Result;

impl BackupStore {
    pub(super) fn rename_branch(
        &self,
        old: &Workspace,
        workspace: &Workspace,
        remote: bool,
    ) -> Result<Option<BackupReport>> {
        ensure!(
            old.branch.to_lowercase() != workspace.branch.to_lowercase(),
            "Backup branches cannot differ only by letter case"
        );
        let from = old.reference();
        let to = workspace.reference();
        let parent = self.git.reference(&from)?;
        let target = self.git.reference(&to)?;
        let Some(parent) = parent else {
            ensure!(target.is_none(), "Branch already exists; import it instead");
            if remote {
                ensure!(
                    self.git
                        .text(["ls-remote", "--refs", "origin", &from, &to])?
                        .is_empty(),
                    "Remote branch already exists; fetch and import it before renaming"
                );
            }
            return Ok(None);
        };
        let mut manifest = self.manifest(&parent, Some(workspace.id))?;
        manifest.name.clone_from(&workspace.name);
        let files = self.blobs(&parent)?;
        let manifest_oid = String::from_utf8(self.git.input(
            ["hash-object", "-w", "--stdin", "--no-filters"],
            Some(&serde_json::to_vec_pretty(&manifest)?),
            None,
        )?)?
        .trim()
        .to_owned();
        let tree = self.write_tree(&files, &manifest_oid)?;
        let pending = format!("refs/gitwatch/renames/{}", workspace.id);
        let staged = self.git.reference(&pending)?;
        let matches = |target: &str| -> Result<bool> {
            Ok(self
                .git
                .text(["rev-parse", &format!("{target}^{{tree}}")])?
                == tree
                && self
                    .git
                    .text(["rev-list", "--parents", "-n", "1", target])?
                    == format!("{target} {parent}"))
        };
        let commit = if let Some(target) = &target {
            // Reuse an interrupted migration only when both its parent and tree match.
            ensure!(matches(target)?, "Branch already exists; import it instead");
            target.clone()
        } else if let Some(staged) = &staged
            && matches(staged)?
        {
            staged.clone()
        } else {
            let commit = String::from_utf8(self.git.input(
                ["commit-tree", &tree, "-p", &parent],
                Some(b"Rename backup task\n"),
                None,
            )?)?
            .trim()
            .to_owned();
            // Stage privately so a failed migration does not reserve the requested branch name.
            self.git.update_ref(&pending, &commit, staged.as_deref())?;
            commit
        };
        let mut uploaded = false;
        if remote {
            let refs = self
                .git
                .text(["ls-remote", "--refs", "origin", &from, &to])?;
            let tip = |reference: &str| {
                refs.lines()
                    .filter_map(|line| line.split_once('\t'))
                    .find_map(|(oid, name)| (name == reference).then_some(oid))
            };
            let previous = tip(&from);
            let next = tip(&to);
            ensure!(
                next.is_none() || (previous.is_none() && next == Some(commit.as_str())),
                "Remote branch already exists; choose another task name"
            );
            if let Some(previous) = previous {
                ensure!(
                    self.git.ancestor(previous, &parent).unwrap_or(false),
                    "Remote history changed; fetch and reconcile it before renaming"
                );
            }
            // Atomic push and explicit leases prevent partial renames and concurrent overwrites.
            // https://git-scm.com/docs/git-push#Documentation/git-push.txt---atomic
            if previous.is_some() && next.is_none() {
                let mut args = vec![
                    "push".to_owned(),
                    "--atomic".to_owned(),
                    format!("--force-with-lease={to}:"),
                    format!("--force-with-lease={from}:{}", previous.unwrap_or("")),
                    "origin".to_owned(),
                    format!("{commit}:{to}"),
                ];
                if previous.is_some() {
                    args.push(format!(":{from}"));
                }
                self.git.run(args).context(
                    "Could not rename remote branch; check connectivity, branch protection and the repository default branch"
                )?;
            }
            uploaded = previous.is_some() || next.is_some();
        }
        if target.is_none() {
            self.git.update_ref(&to, &commit, None)?;
        }
        Ok(Some(BackupReport {
            workspace: workspace.id,
            commit,
            changed: true,
            files: files.len(),
            retained: manifest.retained,
            upload: if uploaded {
                UploadState::Synced
            } else if remote {
                UploadState::Pending
            } else {
                UploadState::Disabled
            },
        }))
    }
}
