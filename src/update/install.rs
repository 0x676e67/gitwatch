use std::{
    collections::BTreeMap,
    fs::{self, File},
    path::{Path, PathBuf},
};

use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};

use super::{
    Release,
    archive::{self, CLI, DESKTOP, RECEIPT, Receipt},
};
use crate::{Result, paths};

const RECOVERY: &str = ".gitwatch-recovery";

/// Holds a shared installation lock while a CLI or desktop task is running.
pub struct Running {
    _lock: File,
}

/// An exclusively locked installation, restricted to its own program files.
pub struct Installation {
    directory: PathBuf,
    files: BTreeMap<String, String>,
    _lock: File,
}

#[derive(Serialize, Deserialize)]
struct Recovery {
    files: BTreeMap<String, String>,
    receipt: bool,
    #[serde(default)]
    committed: bool,
}

// ===== impl Running =====

impl Running {
    /// Prevents this installation from being changed until the guard is dropped.
    pub fn acquire() -> Result<Self> {
        let directory = directory()?;
        let lock = lock(&directory, false)?;
        ensure!(
            !directory.join(RECOVERY).try_exists()?,
            "An interrupted operation needs recovery; run gitwatch self update --recover"
        );
        Ok(Self { _lock: lock })
    }
}

// ===== impl Installation =====

impl Installation {
    /// Opens the current CLI installation. Package-managed installations must use
    /// their package manager so that its installation records remain consistent.
    pub fn current() -> Result<Self> {
        let executable = std::env::current_exe()?;
        ensure!(
            executable.file_name().is_some_and(|name| name == CLI),
            "Run self management from the installed gitwatch executable"
        );
        let directory = directory()?;
        managed(&directory)?;
        let lock = lock(&directory, true)?;
        ensure!(
            !directory.join(RECOVERY).exists(),
            "An interrupted operation needs recovery; run gitwatch self update --recover"
        );
        let files = if directory.join(RECEIPT).try_exists()? {
            Receipt::read(&directory)?.files
        } else {
            // A standalone CLI can bootstrap itself. A pair requires a release receipt
            // so an unrelated same-name executable cannot be silently overwritten.
            ensure!(
                !directory.join(DESKTOP).try_exists()?,
                "This desktop installation has no receipt; extract a current official release before self management"
            );
            BTreeMap::from([(CLI.into(), archive::hash(&executable)?)])
        };
        ensure!(
            files.get(CLI) == Some(&archive::hash(&executable)?),
            "Current executable does not match the installation"
        );
        Ok(Self {
            directory,
            files,
            _lock: lock,
        })
    }

    /// Lists exact program paths affected by update or uninstall.
    pub fn files(&self) -> impl Iterator<Item = PathBuf> + '_ {
        self.files.keys().map(|name| self.directory.join(name))
    }

    /// Downloads, verifies and installs a newer official release.
    /// A recovery copy is retained if replacement or rollback fails.
    pub fn update(self, release: &Release) -> Result<()> {
        ensure!(
            release.is_newer(),
            "The selected release is not newer than this build"
        );
        let staging = tempfile::tempdir_in(&self.directory)?;
        let archive = release.download(staging.path())?;
        let extracted = staging.path().join("extracted");
        fs::create_dir(&extracted)?;
        let mut receipt = archive::extract(&archive, &extracted, release.version())?;
        receipt
            .files
            .retain(|name, _| self.files.contains_key(name));
        ensure!(
            receipt.files.len() == self.files.len(),
            "Release is missing an installed program"
        );
        for name in receipt.files.keys() {
            runnable(&extracted.join(name), release.version())?;
        }
        self.install(&extracted, &receipt)
    }

    fn install(&self, extracted: &Path, receipt: &Receipt) -> Result<()> {
        // Finish all fallible license preparation before replacing either program.
        self.licenses(extracted, &receipt.version)?;
        self.verify()?;
        self.backup()?;
        let result = (|| -> Result<()> {
            if self.files.contains_key(DESKTOP) {
                replace(&extracted.join(DESKTOP), &self.directory.join(DESKTOP))?;
            }
            // self-replace handles Windows executable image locks without rebooting.
            // https://docs.rs/self-replace/1.5.0/self_replace/fn.self_replace.html
            self_replace::self_replace(extracted.join(CLI))?;
            paths::atomic_write(
                &self.directory.join(RECEIPT),
                &serde_json::to_vec_pretty(receipt)?,
            )?;
            Receipt::read(&self.directory)?;
            Ok(())
        })();
        if let Err(error) = result {
            return match restore(&self.directory) {
                Ok(()) => {
                    Err(error).context("Update failed; the previous installation was restored")
                }
                Err(recovery) => Err(error).context(format!(
                    "Update failed; recovery is still required: {recovery:#}"
                )),
            };
        }
        cleanup(&self.directory)
    }

    fn licenses(&self, extracted: &Path, version: &str) -> Result<()> {
        let licenses = self.directory.join("gitwatch-licenses");
        if !licenses.try_exists()? {
            fs::create_dir(&licenses)?;
        }
        ensure!(
            paths::no_link(&licenses)?.is_dir(),
            "Invalid license directory"
        );
        let licenses = licenses.join(format!("v{version}"));
        if !licenses.try_exists()? {
            fs::create_dir(&licenses)?;
        }
        ensure!(
            paths::no_link(&licenses)?.is_dir(),
            "Invalid license directory"
        );
        for name in ["LICENSE", "OFL.txt", "NOTICE"] {
            let destination = licenses.join(name);
            if destination.try_exists()? {
                ensure!(
                    paths::read_file(&destination)? == paths::read_file(&extracted.join(name))?,
                    "Existing license file differs from the release"
                );
            } else {
                let mut file = File::options()
                    .create_new(true)
                    .write(true)
                    .open(&destination)?;
                std::io::copy(&mut File::open(extracted.join(name))?, &mut file)?;
                file.sync_all()?;
            }
        }
        Ok(())
    }

    /// Removes the verified program files, retaining all user data and license notices.
    /// On Windows the running CLI disappears after this process exits.
    pub fn uninstall(self) -> Result<()> {
        self.verify()?;
        self.backup()?;
        let result = (|| -> Result<()> {
            if self.files.contains_key(DESKTOP) {
                fs::remove_file(self.directory.join(DESKTOP))?;
            }
            if self.directory.join(RECEIPT).try_exists()? {
                fs::remove_file(self.directory.join(RECEIPT))?;
            }
            self_replace::self_delete()?;
            Ok(())
        })();
        if let Err(error) = result {
            return match restore(&self.directory) {
                Ok(()) => Err(error).context("Uninstall failed; the installation was restored"),
                Err(recovery) => Err(error).context(format!(
                    "Uninstall failed; recovery is still required: {recovery:#}"
                )),
            };
        }
        cleanup(&self.directory)
    }

    /// Restores binaries saved by an interrupted operation in the current directory.
    pub fn recover() -> Result<()> {
        let directory = directory()?;
        managed(&directory)?;
        let _lock = lock(&directory, true)?;
        restore(&directory)
    }

    fn verify(&self) -> Result<()> {
        for (name, hash) in &self.files {
            ensure!(
                archive::hash(&self.directory.join(name))? == *hash,
                "Installation changed while preparing the operation"
            );
        }
        Ok(())
    }

    fn backup(&self) -> Result<()> {
        let staging = tempfile::tempdir_in(&self.directory)?;
        for name in self.files.keys() {
            fs::copy(self.directory.join(name), staging.path().join(name))?;
            File::options()
                .write(true)
                .open(staging.path().join(name))?
                .sync_all()?;
        }
        let receipt = self.directory.join(RECEIPT).try_exists()?;
        if receipt {
            fs::copy(self.directory.join(RECEIPT), staging.path().join(RECEIPT))?;
            File::options()
                .write(true)
                .open(staging.path().join(RECEIPT))?
                .sync_all()?;
        }
        paths::atomic_write(
            &staging.path().join("recovery.json"),
            &serde_json::to_vec(&Recovery {
                files: self.files.clone(),
                receipt,
                committed: false,
            })?,
        )?;
        fs::rename(staging.path(), self.directory.join(RECOVERY))?;
        Ok(())
    }
}

fn runnable(path: &Path, version: &str) -> Result<()> {
    let mut command = std::process::Command::new(path);
    command.args(["--lang", "en", "--version"]);
    let output = crate::git::execute(command, None, std::time::Duration::from_secs(5))
        .context("The downloaded program cannot run on this system")?;
    let stdout = std::str::from_utf8(&output.stdout)?;
    let reported = stdout
        .trim()
        .strip_prefix("gitwatch ")
        .or_else(|| stdout.trim().strip_prefix("gitwatch-desktop "));
    ensure!(
        output.code == 0 && reported == Some(version),
        "The downloaded program cannot run or reports an unexpected version"
    );
    Ok(())
}

fn directory() -> Result<PathBuf> {
    let executable = dunce::canonicalize(std::env::current_exe()?)?;
    Ok(executable
        .parent()
        .context("Executable has no parent directory")?
        .to_owned())
}

fn lock(directory: &Path, exclusive: bool) -> Result<File> {
    let cache = paths::cache_directory()?.join("installations");
    fs::create_dir_all(&cache)?;
    let name = paths::digest(directory.as_os_str().as_encoded_bytes());
    let path = cache.join(format!("{name}.lock"));
    if path.try_exists()? {
        paths::no_link(&path)?;
    }
    let file = File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    if exclusive {
        fs2::FileExt::try_lock_exclusive(&file)
    } else {
        fs2::FileExt::try_lock_shared(&file)
    }
    .context(
        "Another gitwatch process is using this installation; stop it before self management",
    )?;
    Ok(file)
}

fn managed(directory: &Path) -> Result<()> {
    if let Some(root) = directory.parent()
        && directory.file_name().is_some_and(|name| name == "bin")
        && cargo_manages(root)?
    {
        anyhow::bail!(
            "Cargo manages this installation; use cargo install gitwatch --force or cargo uninstall gitwatch with the same --root"
        );
    }
    let path = directory
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    ensure!(
        ![
            "/cellar/",
            "/homebrew/",
            "/scoop/",
            "/chocolatey/",
            "/nix/store/",
            "/windowsapps/"
        ]
        .iter()
        .any(|part| path.contains(part))
            && !matches!(path.as_str(), "/usr/bin" | "/bin"),
        "Use the package manager to update or uninstall this installation"
    );
    Ok(())
}

fn cargo_manages(root: &Path) -> Result<bool> {
    let path = root.join(".crates2.json");
    if path.try_exists()? {
        let value: serde_json::Value = serde_json::from_slice(&paths::read_file(&path)?)?;
        let installs = value
            .get("installs")
            .and_then(serde_json::Value::as_object)
            .context("Invalid Cargo installation metadata")?;
        return Ok(installs.values().any(|value| {
            value
                .get("bins")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|bins| {
                    bins.iter()
                        .any(|bin| bin == "gitwatch" || bin == "gitwatch.exe")
                })
        }));
    }
    let legacy = root.join(".crates.toml");
    if !legacy.try_exists()? {
        return Ok(false);
    }
    let bytes = paths::read_file(&legacy)?;
    Ok(std::str::from_utf8(&bytes)?.lines().any(|line| {
        line.split_once('=').is_some_and(|(_, bins)| {
            bins.contains("\"gitwatch\"") || bins.contains("\"gitwatch.exe\"")
        })
    }))
}

fn replace(source: &Path, destination: &Path) -> Result<()> {
    if destination.try_exists()? {
        paths::no_link(destination)?;
    }
    let parent = destination
        .parent()
        .context("Program has no installation directory")?;
    let temporary = tempfile::NamedTempFile::new_in(parent)?;
    fs::copy(source, temporary.path())?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(destination)
        .map_err(|error| error.error)?;
    Ok(())
}

fn recovery(directory: &Path) -> Result<Recovery> {
    let saved = directory.join(RECOVERY);
    ensure!(
        paths::no_link(&saved)?.is_dir(),
        "Recovery directory is unavailable"
    );
    let state: Recovery = serde_json::from_slice(&paths::read_file(&saved.join("recovery.json"))?)?;
    ensure!(
        state.files.contains_key(CLI)
            && state
                .files
                .keys()
                .all(|name| [CLI, DESKTOP].contains(&name.as_str())),
        "Invalid recovery manifest"
    );
    if !state.committed {
        for (name, digest) in &state.files {
            ensure!(
                archive::hash(&saved.join(name))? == *digest,
                "Recovery copy failed verification"
            );
        }
    }
    Ok(state)
}

fn restore(directory: &Path) -> Result<()> {
    let state = recovery(directory)?;
    if state.committed {
        return cleanup(directory);
    }
    let saved = directory.join(RECOVERY);
    for name in state.files.keys().filter(|name| name.as_str() != CLI) {
        replace(&saved.join(name), &directory.join(name))?;
    }
    if archive::hash(&directory.join(CLI)).ok().as_ref() != state.files.get(CLI)
        && let Err(error) = replace(&saved.join(CLI), &directory.join(CLI))
    {
        // Only the currently running image needs Windows self-replacement.
        // A previous replacement may already have moved this process aside.
        if dunce::canonicalize(std::env::current_exe()?).ok().as_ref() == Some(&directory.join(CLI))
        {
            self_replace::self_replace(saved.join(CLI))?;
        } else {
            return Err(error);
        }
    }
    if state.receipt {
        replace(&saved.join(RECEIPT), &directory.join(RECEIPT))?;
    } else if directory.join(RECEIPT).try_exists()? {
        fs::remove_file(directory.join(RECEIPT))?;
    }
    cleanup(directory)
}

fn cleanup(directory: &Path) -> Result<()> {
    let mut state = recovery(directory)?;
    let saved = directory.join(RECOVERY);
    state.committed = true;
    paths::atomic_write(&saved.join("recovery.json"), &serde_json::to_vec(&state)?)?;
    for name in state.files.keys() {
        if saved.join(name).try_exists()? {
            fs::remove_file(saved.join(name))?;
        }
    }
    if state.receipt && saved.join(RECEIPT).try_exists()? {
        fs::remove_file(saved.join(RECEIPT))?;
    }
    fs::remove_file(saved.join("recovery.json"))?;
    fs::remove_dir(saved)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{io::Write, process::Command};

    use super::*;

    #[test]
    fn cargo_directory_alone_does_not_claim_a_manual_installation() {
        let temp = tempfile::tempdir().unwrap();
        let bin = temp.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let path = temp.path().join(".crates2.json");
        fs::write(&path, r#"{"installs":{"other 1.0.0":{"bins":["other"]}}}"#).unwrap();
        managed(&bin).unwrap();
        fs::write(
            &path,
            r#"{"installs":{"gitwatch 0.1.0":{"bins":["gitwatch"]}}}"#,
        )
        .unwrap();
        assert!(managed(&bin).is_err());
    }

    #[test]
    fn replacement_uninstall_and_recovery_run_only_in_copied_executables() {
        let temp = tempfile::tempdir().unwrap();
        for scenario in [
            "update",
            "rollback",
            "rollback_after",
            "recover",
            "cleanup",
            "uninstall",
        ] {
            let directory = temp.path().join(scenario);
            fs::create_dir(&directory).unwrap();
            fs::copy(std::env::current_exe().unwrap(), directory.join(CLI)).unwrap();
            fs::write(directory.join(DESKTOP), "old desktop").unwrap();
            fs::write(directory.join("user-data.txt"), "keep me").unwrap();
            let old = archive::hash(&directory.join(CLI)).unwrap();
            let output = Command::new(directory.join(CLI))
                .args([
                    "--exact",
                    "update::install::tests::child_operation",
                    "--nocapture",
                ])
                .env("GITWATCH_TEST_OPERATION", scenario)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{scenario}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                fs::read_to_string(directory.join("user-data.txt")).unwrap(),
                "keep me"
            );
            if scenario == "uninstall" {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                while directory.join(CLI).exists() && std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                assert!(!directory.join(CLI).exists());
                assert!(!directory.join(DESKTOP).exists());
            } else if scenario == "update" {
                assert_ne!(archive::hash(&directory.join(CLI)).unwrap(), old);
                assert_eq!(
                    fs::read_to_string(directory.join(DESKTOP)).unwrap(),
                    "new desktop"
                );
                Receipt::read(&directory).unwrap();
            } else {
                assert_eq!(archive::hash(&directory.join(CLI)).unwrap(), old);
                assert_eq!(
                    fs::read_to_string(directory.join(DESKTOP)).unwrap(),
                    "old desktop"
                );
            }
            assert!(!directory.join(RECOVERY).exists());
        }
    }

    #[test]
    fn child_operation() {
        let Ok(scenario) = std::env::var("GITWATCH_TEST_OPERATION") else {
            return;
        };
        let directory = directory().unwrap();
        // Only the parent-created copy is named gitwatch; cargo's test executable is not.
        assert_eq!(std::env::current_exe().unwrap().file_name().unwrap(), CLI);
        let installation = Installation {
            files: [CLI, DESKTOP]
                .into_iter()
                .map(|name| (name.into(), archive::hash(&directory.join(name)).unwrap()))
                .collect(),
            _lock: lock(&directory, true).unwrap(),
            directory: directory.clone(),
        };
        assert!(lock(&directory, false).is_err());
        if scenario == "uninstall" {
            installation.uninstall().unwrap();
            return;
        }
        if matches!(scenario.as_str(), "recover" | "cleanup") {
            installation.backup().unwrap();
            if scenario == "recover" {
                fs::write(directory.join(DESKTOP), "interrupted").unwrap();
            } else {
                let mut state = recovery(&directory).unwrap();
                state.committed = true;
                paths::atomic_write(
                    &directory.join(RECOVERY).join("recovery.json"),
                    &serde_json::to_vec(&state).unwrap(),
                )
                .unwrap();
                fs::remove_file(directory.join(RECOVERY).join(CLI)).unwrap();
            }
            restore(&directory).unwrap();
            return;
        }
        let staging = tempfile::tempdir_in(&directory).unwrap();
        fs::copy(directory.join(CLI), staging.path().join(CLI)).unwrap();
        File::options()
            .append(true)
            .open(staging.path().join(CLI))
            .unwrap()
            .write_all(b"new release")
            .unwrap();
        fs::write(staging.path().join(DESKTOP), "new desktop").unwrap();
        for name in ["LICENSE", "OFL.txt", "NOTICE"] {
            fs::write(staging.path().join(name), "notice").unwrap();
        }
        let mut receipt = Receipt {
            version: super::super::VERSION.into(),
            target: super::super::target().unwrap().into(),
            files: [CLI, DESKTOP]
                .into_iter()
                .map(|name| {
                    (
                        name.into(),
                        archive::hash(&staging.path().join(name)).unwrap(),
                    )
                })
                .collect(),
        };
        if scenario == "rollback" {
            fs::remove_file(staging.path().join(CLI)).unwrap();
        }
        if scenario == "rollback_after" {
            receipt.files.insert(CLI.into(), "00".repeat(32));
        }
        let result = installation.install(staging.path(), &receipt);
        assert_eq!(result.is_ok(), scenario == "update", "{result:?}");
    }
}
