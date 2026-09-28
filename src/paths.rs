use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, ensure};
use sha2::{Digest, Sha256};

use crate::Result;

pub(crate) const MAX_FILE: u64 = 16 * 1024 * 1024;
pub(crate) const MAX_SNAPSHOT: usize = 128 * 1024 * 1024;

pub(crate) fn data_directory() -> Result<PathBuf> {
    let root = dirs::data_local_dir().context("Cannot locate user data directory")?;
    // Preserve the application's existing platform layout when switching to dirs.
    // https://docs.rs/directories/6.0.0/directories/struct.ProjectDirs.html
    Ok(root.join(if cfg!(windows) {
        "gitwatch/data"
    } else {
        "gitwatch"
    }))
}

pub(crate) fn cache_directory() -> Result<PathBuf> {
    let root = dirs::cache_dir().context("Cannot locate application cache")?;
    Ok(root.join(if cfg!(windows) {
        "gitwatch/cache"
    } else {
        "gitwatch"
    }))
}

pub(crate) fn relative(path: &str) -> Result<()> {
    ensure!(!path.is_empty(), "Select a relative file or directory");
    for part in path.split('/') {
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        ensure!(
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.ends_with(['.', ' '])
                && !part.eq_ignore_ascii_case(".git")
                && !part
                    .chars()
                    .any(|c| c.is_control() || "\\:*?\"<>|".contains(c))
                && !matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                && !(stem.len() == 4
                    && (stem.starts_with("COM") || stem.starts_with("LPT"))
                    && stem.as_bytes()[3].is_ascii_digit()),
            "Unsafe or non-portable relative path: {path}"
        );
    }
    Ok(())
}

pub(crate) fn no_link(path: &Path) -> Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        !metadata.file_type().is_symlink(),
        "Symbolic link is not allowed: {}",
        path.display()
    );
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        ensure!(
            metadata.file_attributes() & 0x400 == 0,
            "Reparse point is not allowed: {}",
            path.display()
        );
    }
    Ok(metadata)
}

pub(crate) fn root(path: &Path) -> Result<PathBuf> {
    ensure!(
        no_link(path)?.is_dir(),
        "Expected a directory: {}",
        path.display()
    );
    // Git for Windows does not accept Rust's verbatim \\?\ path prefix.
    Ok(dunce::canonicalize(path)?)
}

pub(crate) fn joined(root: &Path, relative_path: &str) -> Result<PathBuf> {
    relative(relative_path)?;
    ensure!(no_link(root)?.is_dir(), "Root directory is unavailable");
    let mut path = root.to_path_buf();
    for part in relative_path.split('/') {
        path.push(part);
        match no_link(&path) {
            Ok(_) => {}
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(path)
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("Path has no parent")?;
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|e| e.error)?;
    Ok(())
}

pub(crate) fn read_file(path: &Path) -> Result<Vec<u8>> {
    let before = no_link(path)?;
    ensure!(
        before.is_file() && before.len() <= MAX_FILE,
        "Not a regular file or larger than 16 MiB: {}",
        path.display()
    );
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_FILE + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= MAX_FILE, "File grew beyond 16 MiB");
    let after = no_link(path)?;
    ensure!(
        before.len() == after.len() && before.modified()? == after.modified()?,
        "File changed while reading: {}",
        path.display()
    );
    Ok(bytes)
}

pub(crate) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn disjoint(source: &Path, data: &Path) -> Result<()> {
    ensure!(
        !source.starts_with(data) && !data.starts_with(source),
        "Source and backup data directories must not contain one another"
    );
    Ok(())
}
