use std::{
    collections::BTreeMap,
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail, ensure};
use globset::{Glob, GlobSet, GlobSetBuilder};

use super::Workspace;
use crate::{Result, paths};

pub(crate) struct FileData {
    pub bytes: Vec<u8>,
    pub executable: bool,
}

pub(crate) fn exclusions(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder
            .add(Glob::new(pattern).with_context(|| format!("Invalid exclusion glob: {pattern}"))?);
    }
    Ok(builder.build()?)
}

fn protected(path: &str) -> bool {
    path.split('/').any(|part| {
        let part = part.to_ascii_lowercase();
        matches!(
            part.as_str(),
            ".git"
                | ".ssh"
                | ".aws"
                | ".codex"
                | "node_modules"
                | "target"
                | "id_rsa"
                | "id_ed25519"
                | "auth.json"
                | "credentials.json"
        ) || part == ".env"
            || part.starts_with(".env.")
    })
}

pub(crate) fn selected(workspace: &Workspace, excludes: &GlobSet, path: &str) -> bool {
    workspace.include.iter().any(|include| {
        path == include
            || path
                .strip_prefix(include)
                .is_some_and(|rest| rest.starts_with('/'))
    }) && !excluded(excludes, path)
}

fn excluded(excludes: &GlobSet, path: &str) -> bool {
    protected(path)
        || std::iter::once(path)
            .chain(path.match_indices('/').map(|(index, _)| &path[..index]))
            .any(|path| excludes.is_match(path))
}

pub(crate) fn collect(
    workspace: &Workspace,
    data: &Path,
    excludes: &GlobSet,
) -> Result<BTreeMap<String, FileData>> {
    ensure!(
        paths::no_link(&workspace.root)?.is_dir(),
        "Source directory is unavailable; backup preserved"
    );
    let mut scan = Scan {
        workspace,
        data,
        excludes,
        files: BTreeMap::new(),
        total: 0,
        ancestors: Vec::new(),
    };
    for path in &workspace.include {
        paths::relative(path)?;
        ensure!(
            !protected(path),
            "Credential, cache or Git metadata path cannot be selected: {path}"
        );
        scan.visit(path)?;
    }
    Ok(scan.files)
}

struct Scan<'a> {
    workspace: &'a Workspace,
    data: &'a Path,
    excludes: &'a GlobSet,
    files: BTreeMap<String, FileData>,
    total: usize,
    ancestors: Vec<PathBuf>,
}

impl Scan<'_> {
    fn visit(&mut self, relative: &str) -> Result<()> {
        if excluded(self.excludes, relative) {
            return Ok(());
        }
        let Some(path) = self.resolve(relative)? else {
            return Ok(());
        };
        let metadata = paths::no_link(&path)?;
        if metadata.is_dir() {
            ensure!(
                !self.ancestors.contains(&path),
                "Symbolic link cycle: {relative}"
            );
            ensure!(
                self.ancestors.len() < 128,
                "Selection exceeds 128 directory levels"
            );
            self.ancestors.push(path.clone());
            for entry in fs::read_dir(&path)? {
                let entry = entry?;
                let name = entry.file_name().into_string().map_err(|_| {
                    anyhow::anyhow!("Non UTF-8 file names cannot be backed up portably")
                })?;
                self.visit(&format!("{relative}/{name}"))?;
            }
            self.ancestors.pop();
        } else if metadata.is_file() {
            if self.files.contains_key(relative) {
                return Ok(());
            }
            let bytes = paths::read_file(&path)?;
            self.total += bytes.len();
            ensure!(
                self.total <= paths::MAX_SNAPSHOT && self.files.len() < 10_000,
                "Selection exceeds 128 MiB or 10,000 files"
            );
            #[cfg(unix)]
            let executable = {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode() & 0o111 != 0
            };
            #[cfg(not(unix))]
            let executable = false;
            self.files
                .insert(relative.to_owned(), FileData { bytes, executable });
        } else {
            bail!("Unsupported file type: {relative}");
        }
        Ok(())
    }

    fn resolve(&self, relative: &str) -> Result<Option<PathBuf>> {
        paths::relative(relative)?;
        let mut path = self.workspace.root.clone();
        paths::no_link(&path)?;
        for part in relative.split('/') {
            path.push(part);
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error.into()),
            };
            if metadata.file_type().is_symlink() && self.workspace.follow_links {
                // canonicalize resolves link chains, including directory links:
                // https://doc.rust-lang.org/std/fs/fn.canonicalize.html
                path = match dunce::canonicalize(&path) {
                    Ok(target) => target,
                    Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
                    Err(error) => bail!("Cannot resolve symbolic link: {relative}: {error}"),
                };
                paths::disjoint(&path, self.data)?;
                ensure!(
                    !path
                        .components()
                        .any(|part| protected(&part.as_os_str().to_string_lossy())),
                    "Symbolic link targets a protected path: {relative}"
                );
            }
            paths::no_link(&path)?;
        }
        Ok(Some(path))
    }
}
