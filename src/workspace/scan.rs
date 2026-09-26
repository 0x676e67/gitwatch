use std::{collections::BTreeMap, fs, io::ErrorKind, path::Path};

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

pub(crate) fn collect(workspace: &Workspace) -> Result<BTreeMap<String, FileData>> {
    ensure!(
        paths::no_link(&workspace.root)?.is_dir(),
        "Source directory is unavailable; backup preserved"
    );
    let excludes = exclusions(&workspace.exclude)?;
    let mut files = BTreeMap::new();
    let mut total = 0;
    for path in &workspace.include {
        paths::relative(path)?;
        ensure!(
            !protected(path),
            "Credential, cache or Git metadata path cannot be selected: {path}"
        );
        visit(&workspace.root, path, &excludes, &mut files, &mut total)?;
    }
    Ok(files)
}

fn visit(
    root: &Path,
    relative: &str,
    excludes: &GlobSet,
    files: &mut BTreeMap<String, FileData>,
    total: &mut usize,
) -> Result<()> {
    if protected(relative) || excludes.is_match(relative) {
        return Ok(());
    }
    let path = paths::joined(root, relative)?;
    let metadata = match paths::no_link(&path) {
        Ok(metadata) => metadata,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == ErrorKind::NotFound) =>
        {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    if metadata.is_dir() {
        for entry in fs::read_dir(&path)? {
            let entry = entry?;
            let name = entry.file_name().into_string().map_err(|_| {
                anyhow::anyhow!("Non UTF-8 file names cannot be backed up portably")
            })?;
            visit(root, &format!("{relative}/{name}"), excludes, files, total)?;
        }
    } else if metadata.is_file() {
        if files.contains_key(relative) {
            return Ok(());
        }
        let bytes = paths::read_file(&path)?;
        *total += bytes.len();
        ensure!(
            *total <= paths::MAX_SNAPSHOT && files.len() < 10_000,
            "Selection exceeds 128 MiB or 10,000 files"
        );
        #[cfg(unix)]
        let executable = {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode() & 0o111 != 0
        };
        #[cfg(not(unix))]
        let executable = false;
        files.insert(relative.to_owned(), FileData { bytes, executable });
    } else {
        bail!("Unsupported file type: {relative}");
    }
    Ok(())
}
