use std::{
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, bail, ensure};

use crate::Result;

const OUTPUT_LIMIT: u64 = 128 * 1024 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct Git {
    dir: PathBuf,
    work_tree: Option<PathBuf>,
    owned: bool,
    isolated: bool,
}

pub(crate) struct Output {
    pub code: i32,
    pub stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// An OS lock, released when its file handle is closed, including after a crash.
pub(crate) struct Lock {
    _file: File,
}

// ===== impl Git =====

impl Git {
    pub(crate) fn bare(dir: PathBuf) -> Self {
        Self {
            dir,
            work_tree: None,
            owned: true,
            isolated: false,
        }
    }

    pub(crate) fn work_tree(dir: PathBuf, work_tree: PathBuf) -> Self {
        Self {
            dir,
            work_tree: Some(work_tree),
            owned: false,
            isolated: false,
        }
    }

    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    /// Uses a private integration worktree without user filters, hooks or editors.
    pub(crate) fn isolated(mut self, root: PathBuf) -> Self {
        self.work_tree = Some(root);
        self.isolated = true;
        self
    }

    /// Finds an operation that must finish before automatic repository writes.
    pub(crate) fn operation(&self) -> Result<Option<&'static str>> {
        for marker in [
            "MERGE_HEAD",
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "rebase-merge",
            "rebase-apply",
            "sequencer",
            "BISECT_LOG",
        ] {
            // Git resolves per-worktree metadata, including linked worktrees.
            // https://git-scm.com/docs/git-rev-parse#Documentation/git-rev-parse.txt---git-pathltpathgt
            let path = self.text(["rev-parse", "--path-format=absolute", "--git-path", marker])?;
            if Path::new(&path).try_exists()? {
                return Ok(Some(marker));
            }
        }
        Ok(None)
    }

    pub(crate) fn run<I, S>(&self, args: I) -> Result<Vec<u8>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.input(args, None, None)
    }

    pub(crate) fn text<I, S>(&self, args: I) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        Ok(String::from_utf8(self.run(args)?)?
            .trim_end_matches(['\r', '\n'])
            .to_owned())
    }

    pub(crate) fn input<I, S>(
        &self,
        args: I,
        input: Option<&[u8]>,
        index: Option<&Path>,
    ) -> Result<Vec<u8>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let args: Vec<OsString> = args.into_iter().map(|s| s.as_ref().to_owned()).collect();
        let output = self.output(&args, input, index)?;
        output.check(args.first().and_then(|s| s.to_str()).unwrap_or("operation"))?;
        Ok(output.stdout)
    }

    pub(crate) fn output<I, S>(
        &self,
        args: I,
        input: Option<&[u8]>,
        index: Option<&Path>,
    ) -> Result<Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = base_command();
        if self.isolated {
            command
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env(
                    "GIT_CONFIG_GLOBAL",
                    if cfg!(windows) { "NUL" } else { "/dev/null" },
                )
                .env("GIT_EDITOR", "true")
                .env("GIT_SEQUENCE_EDITOR", "true");
            command.args([
                "-c",
                "core.autocrlf=false",
                "-c",
                "core.attributesFile=",
                "-c",
                "rerere.enabled=false",
            ]);
        }
        command
            .arg("--literal-pathspecs")
            .arg("--git-dir")
            .arg(&self.dir);
        if let Some(root) = &self.work_tree {
            command.arg("--work-tree").arg(root).current_dir(root);
        } else {
            command.current_dir(&self.dir);
        }
        if self.owned {
            command.args([
                "-c",
                "user.name=Gitwatch",
                "-c",
                "user.email=gitwatch@localhost",
                "-c",
                "commit.gpgsign=false",
            ]);
            command.arg("-c").arg(format!(
                "core.hooksPath={}",
                self.dir.join("disabled-hooks").display()
            ));
        }
        if let Some(index) = index {
            command.env("GIT_INDEX_FILE", index);
        }
        command.args(args);
        execute(command, input, Duration::from_secs(60))
    }

    pub(crate) fn resolve(&self, revision: &str) -> Result<String> {
        ensure!(
            !revision.is_empty() && !revision.starts_with('-') && !revision.contains(['\n', '\r']),
            "Invalid revision"
        );
        let oid = self.text([
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{revision}^{{commit}}"),
        ])?;
        ensure!(
            matches!(oid.len(), 40 | 64) && oid.bytes().all(|c| c.is_ascii_hexdigit()),
            "Invalid commit ID"
        );
        Ok(oid)
    }

    pub(crate) fn reference(&self, reference: &str) -> Result<Option<String>> {
        let result = self.output(["show-ref", "--verify", "--hash", reference], None, None)?;
        if result.code == 1 {
            return Ok(None);
        }
        // show-ref uses 128 for an absent ref in some Git versions without --quiet.
        if result.code == 128 && result.stderr.windows(15).any(|s| s == b"not a valid ref") {
            return Ok(None);
        }
        result.check("show-ref")?;
        Ok(Some(String::from_utf8(result.stdout)?.trim().to_owned()))
    }

    pub(crate) fn ancestor(&self, old: &str, new: &str) -> Result<bool> {
        let result = self.output(["merge-base", "--is-ancestor", old, new], None, None)?;
        ensure!(
            result.code == 0 || result.code == 1,
            "Cannot compare backup histories"
        );
        Ok(result.code == 0)
    }

    pub(crate) fn update_ref(&self, reference: &str, new: &str, old: Option<&str>) -> Result<()> {
        let absent = "0".repeat(new.len());
        self.run(["update-ref", reference, new, old.unwrap_or(&absent)])?;
        Ok(())
    }
}

// ===== impl Output =====

impl Output {
    pub(crate) fn check(&self, operation: &str) -> Result<()> {
        if self.code == 0 {
            return Ok(());
        }
        let detail = String::from_utf8_lossy(&self.stderr).to_lowercase();
        // Git diagnostics can echo credential-bearing URLs and file contents.
        let reason = if detail.contains("authentication")
            || detail.contains("permission denied")
            || detail.contains("could not read username")
        {
            "authentication or permissions; check your Git credentials"
        } else if detail.contains("non-fast-forward")
            || detail.contains("fetch first")
            || detail.contains("rejected")
        {
            "remote history changed; fetch and inspect the branches"
        } else if detail.contains("lock") {
            "repository is locked or changed concurrently"
        } else if detail.contains("conflict") {
            "conflict requires manual resolution"
        } else if detail.contains("identity") || detail.contains("email") {
            "configure Git author name and email"
        } else {
            "inspect repository state and Git configuration"
        };
        bail!("Git {operation} failed (exit {}): {reason}", self.code)
    }
}

// ===== impl Lock =====

impl Lock {
    pub(crate) fn acquire(path: &Path) -> Result<Self> {
        let file = Self::open_file(path)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match fs2::FileExt::try_lock_exclusive(&file) {
                Ok(()) => break,
                Err(error)
                    if error.raw_os_error() == fs2::lock_contended_error().raw_os_error()
                        && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(error) => {
                    return Err(error).context("Another gitwatch operation owns this repository");
                }
            }
        }
        Ok(Self { _file: file })
    }

    /// Waits for a data operation; instance ownership still uses bounded acquisition.
    pub(crate) fn wait(path: &Path) -> Result<Self> {
        let file = Self::open_file(path)?;
        // The OS queues contenders and releases the lock when the handle closes.
        // https://docs.rs/fs2/latest/fs2/trait.FileExt.html#tymethod.lock_exclusive
        fs2::FileExt::lock_exclusive(&file).context("Cannot lock the gitwatch data store")?;
        Ok(Self { _file: file })
    }

    fn open_file(path: &Path) -> Result<File> {
        Ok(OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?)
    }
}

pub(crate) fn base_command() -> Command {
    let mut command = Command::new(std::env::var_os("GW_GIT_BIN").unwrap_or_else(|| "git".into()));
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
        "GIT_NAMESPACE",
    ] {
        command.env_remove(key);
    }
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "Never");
    if std::env::var_os("GIT_SSH_COMMAND").is_none() {
        command.env("GIT_SSH_COMMAND", "ssh -oBatchMode=yes -oConnectTimeout=15");
    }
    command
}

pub(crate) fn execute(
    mut command: Command,
    input: Option<&[u8]>,
    timeout: Duration,
) -> Result<Output> {
    let mut stdin = tempfile::tempfile()?;
    if let Some(bytes) = input {
        stdin.write_all(bytes)?;
    }
    stdin.seek(SeekFrom::Start(0))?;
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    command
        .stdin(stdin)
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = command.spawn().with_context(|| {
        format!(
            "Could not start executable '{}'",
            command.get_program().to_string_lossy()
        )
    });
    let mut child = if command.get_program()
        == std::env::var_os("GW_GIT_BIN").unwrap_or_else(|| "git".into())
    {
        child.context(
            "Cannot run Git; install Git, make sure it is on PATH, then restart gitwatch. If GW_GIT_BIN is set, check that path",
        )?
    } else {
        child?
    };
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Output {
                code: status.code().unwrap_or(-1),
                stdout: read_output(&mut stdout)?,
                stderr: read_output(&mut stderr)?,
            });
        }
        if start.elapsed() >= timeout {
            terminate_tree(child.id());
            let _ = child.kill();
            let _ = child.wait();
            bail!("Process exceeded its {} second timeout", timeout.as_secs());
        }
        if stdout.metadata()?.len() > OUTPUT_LIMIT || stderr.metadata()?.len() > OUTPUT_LIMIT {
            terminate_tree(child.id());
            let _ = child.kill();
            let _ = child.wait();
            bail!("Process output exceeded the size limit");
        }
        thread::sleep(Duration::from_millis(15));
    }
}

fn read_output(file: &mut File) -> Result<Vec<u8>> {
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.take(OUTPUT_LIMIT + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= OUTPUT_LIMIT,
        "Process output exceeded the size limit"
    );
    Ok(bytes)
}

fn terminate_tree(pid: u32) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .creation_flags(0x08000000)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{pid}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

pub(crate) fn init_bare(path: &Path) -> Result<Git> {
    fs::create_dir_all(path)?;
    let mut command = base_command();
    command
        .args(["init", "--bare", "--object-format=sha1"])
        .arg(path);
    execute(command, None, Duration::from_secs(30))?.check("init")?;
    Ok(Git::bare(path.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        pull::{PullOptions, PullTask},
        test_git::git,
        watch::{Repository, WatchOptions},
    };

    #[test]
    fn operations_protect_watch_and_pull_in_linked_worktrees() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("main");
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "-b", "main"]);
        git(
            &root,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-m",
                "Initial",
            ],
        );
        let linked = temp.path().join("linked");
        git(
            &root,
            &["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
        );
        for path in [&root, &linked] {
            let repository = Git::work_tree(
                PathBuf::from(git(path, &["rev-parse", "--absolute-git-dir"])),
                path.to_path_buf(),
            );
            let mut watch = Repository::open(path, None, WatchOptions::default()).unwrap();
            assert_eq!(repository.operation().unwrap(), None);
            for marker in [
                "MERGE_HEAD",
                "CHERRY_PICK_HEAD",
                "REVERT_HEAD",
                "rebase-merge",
                "rebase-apply",
                "sequencer",
                "BISECT_LOG",
            ] {
                let file = PathBuf::from(git(
                    path,
                    &["rev-parse", "--path-format=absolute", "--git-path", marker],
                ));
                let directory = matches!(marker, "rebase-merge" | "rebase-apply" | "sequencer");
                if directory {
                    fs::create_dir(&file).unwrap();
                } else {
                    fs::write(&file, "in progress").unwrap();
                }
                assert_eq!(repository.operation().unwrap(), Some(marker));
                let report = watch.commit().unwrap();
                assert!(
                    report
                        .skipped()
                        .is_some_and(|message| message.contains(marker))
                );
                let error = PullTask::new(PullOptions::new(path))
                    .unwrap()
                    .update()
                    .unwrap_err();
                assert!(
                    error.to_string().contains("active Git operation"),
                    "{marker}: {error:#}"
                );
                assert!(file.exists());
                if directory {
                    fs::remove_dir(&file).unwrap();
                } else {
                    fs::remove_file(&file).unwrap();
                }
                assert_eq!(repository.operation().unwrap(), None);
            }
            assert!(watch.commit().unwrap().skipped().is_none());
        }
    }

    #[test]
    fn test_git_ignores_inherited_repository_overrides() {
        const CHILD: &str = "GITWATCH_TEST_GIT_ENV";
        if std::env::var_os(CHILD).is_some() {
            let temp = tempfile::tempdir().unwrap();
            git(temp.path(), &["init", "-b", "main"]);
            assert_eq!(
                git(temp.path(), &["rev-parse", "--is-inside-work-tree"]),
                "true"
            );
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "git::tests::test_git_ignores_inherited_repository_overrides",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("GIT_DIR", temp.path().join("missing"))
            .env("GIT_WORK_TREE", temp.path().join("other"))
            .env("GIT_CONFIG_COUNT", "invalid")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn missing_executable_keeps_its_name_and_io_error() {
        let temp = tempfile::tempdir().unwrap();
        let command = Command::new(temp.path().join("missing-helper"));
        let error = execute(command, None, Duration::from_secs(1))
            .err()
            .unwrap();
        assert!(error.to_string().contains("missing-helper"));
        assert!(!error.to_string().contains("install Git"));
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::NotFound
        );
    }

    #[test]
    fn lock_contention_waits_for_the_current_operation() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("operation.lock");
        let first = Lock::acquire(&path).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            let _ = sender.send(Lock::acquire(&path).map(|_| ()));
        });
        assert!(matches!(
            receiver.recv_timeout(Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        drop(first);
        receiver
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .unwrap();
        worker.join().unwrap();
    }
}
