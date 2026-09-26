use std::{fs, path::Path, process::Command, sync::mpsc, time::Duration};

use gitwatch::watch::{self, Event, MonitorOptions, Repository, StopToken, WatchOptions};
use tempfile::TempDir;

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn repository() -> TempDir {
    let temp = TempDir::new().unwrap();
    git(temp.path(), &["init", "-b", "main"]);
    for (key, value) in [
        ("user.name", "Test"),
        ("user.email", "test@example.invalid"),
        ("commit.gpgsign", "false"),
        ("core.autocrlf", "false"),
        ("core.hooksPath", ".test-hooks"),
    ] {
        git(temp.path(), &["config", key, value]);
    }
    fs::write(temp.path().join("watched.md"), "initial").unwrap();
    fs::write(temp.path().join("other.md"), "untouched").unwrap();
    git(temp.path(), &["add", "."]);
    git(temp.path(), &["commit", "-m", "Initial"]);
    temp
}

#[test]
fn selected_commits_preserve_other_staged_files_and_refuse_partial_staging() {
    let temp = repository();
    let root = temp.path();
    let mut watcher = Repository::open(
        root.join("watched.md"),
        None,
        WatchOptions::default()
            .message("Auto %d / %d")
            .date_format("+%Y"),
    )
    .unwrap();
    fs::write(root.join("other.md"), "staged independently").unwrap();
    git(root, &["add", "other.md"]);
    fs::write(root.join("watched.md"), "edited").unwrap();
    assert!(watcher.commit().unwrap().commit().is_some());
    assert_eq!(
        git(root, &["show", "--format=", "--name-only", "HEAD"]),
        "watched.md"
    );
    assert_eq!(git(root, &["diff", "--cached", "--name-only"]), "other.md");
    assert!(!git(root, &["log", "-1", "--format=%s"]).contains("%d"));
    assert!(watcher.commit().unwrap().commit().is_none());
    fs::write(root.join("watched.md"), "staged target").unwrap();
    git(root, &["add", "watched.md"]);
    fs::write(root.join("watched.md"), "unstaged target").unwrap();
    assert!(watcher.commit().unwrap_err().to_string().contains("staged"));
    assert_eq!(git(root, &["show", ":watched.md"]), "staged target");
    assert_eq!(
        fs::read(root.join("watched.md")).unwrap(),
        b"unstaged target"
    );
}

#[test]
fn directory_scope_handles_new_files_deletions_and_literal_names() {
    let temp = repository();
    let root = temp.path();
    fs::create_dir(root.join("notes")).unwrap();
    let mut watcher = Repository::open(root.join("notes"), None, WatchOptions::default()).unwrap();
    fs::write(root.join("notes/[draft] file.md"), "new").unwrap();
    assert!(watcher.commit().unwrap().commit().is_some());
    fs::remove_file(root.join("notes/[draft] file.md")).unwrap();
    assert!(watcher.commit().unwrap().commit().is_some());
    assert!(git(root, &["ls-tree", "-r", "--name-only", "HEAD", "notes"]).is_empty());
    fs::write(root.join("[exact].md"), "literal").unwrap();
    let mut file =
        Repository::open(root.join("[exact].md"), None, WatchOptions::default()).unwrap();
    assert!(file.commit().unwrap().commit().is_some());
    fs::remove_file(root.join("[exact].md")).unwrap();
    assert!(file.commit().unwrap().commit().is_some());
}

#[test]
fn branch_changes_and_repository_operations_stop_automatic_commits() {
    let temp = repository();
    let root = temp.path();
    let mut watcher = Repository::open(root, None, WatchOptions::default()).unwrap();
    fs::write(root.join("watched.md"), "pending").unwrap();
    fs::write(
        root.join(".git/MERGE_HEAD"),
        git(root, &["rev-parse", "HEAD"]),
    )
    .unwrap();
    assert!(watcher.commit().unwrap().skipped().is_some());
    fs::remove_file(root.join(".git/MERGE_HEAD")).unwrap();
    git(root, &["checkout", "-b", "another"]);
    assert!(
        watcher
            .commit()
            .unwrap_err()
            .to_string()
            .contains("branch changed")
    );
    assert_eq!(git(root, &["rev-list", "--count", "HEAD"]), "1");
}

#[test]
fn failed_hook_preserves_staged_changes_and_never_claims_a_commit() {
    let temp = repository();
    let root = temp.path();
    fs::create_dir(root.join(".test-hooks")).unwrap();
    let hook = root.join(".test-hooks/pre-commit");
    fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::write(root.join("watched.md"), "pending").unwrap();
    let mut watcher =
        Repository::open(root.join("watched.md"), None, WatchOptions::default()).unwrap();
    assert!(watcher.commit().is_err());
    assert_eq!(git(root, &["rev-list", "--count", "HEAD"]), "1");
    assert_eq!(
        git(root, &["diff", "--cached", "--name-only"]),
        "watched.md"
    );
}

#[test]
fn polling_detects_content_edits_without_committing_the_startup_baseline() {
    let temp = repository();
    let root = temp.path();
    fs::write(root.join("watched.md"), "preexisting").unwrap();
    let watcher = Repository::open(root.join("watched.md"), None, WatchOptions::default()).unwrap();
    let options = MonitorOptions::default()
        .native(false)
        .poll_interval(Duration::from_millis(50))
        .debounce(Duration::from_millis(150));
    let stop = StopToken::default();
    let worker_stop = stop.clone();
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        watch::watch_repository(watcher, options, worker_stop, |event| {
            let _ = tx.send(event);
        })
    });
    assert!(matches!(
        rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        Event::Watching(_)
    ));
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(git(root, &["rev-list", "--count", "HEAD"]), "1");
    fs::write(root.join("watched.md"), "one").unwrap();
    std::thread::sleep(Duration::from_millis(60));
    fs::write(root.join("watched.md"), "two").unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut committed = false;
    while std::time::Instant::now() < deadline {
        if let Ok(Event::Repository(report)) = rx.recv_timeout(Duration::from_millis(300))
            && report.commit().is_some()
        {
            committed = true;
            break;
        }
    }
    stop.stop();
    worker.join().unwrap().unwrap();
    assert!(committed, "watch loop did not commit edited contents");
    assert_eq!(git(root, &["show", "HEAD:watched.md"]), "two");
    assert_eq!(git(root, &["rev-list", "--count", "HEAD"]), "2");
}

#[test]
fn upload_retries_without_new_changes_and_message_command_receives_selected_paths() {
    use gitwatch::workspace::UploadState;
    let temp = repository();
    let remote = TempDir::new().unwrap();
    let destination = remote.path().join("backup.git");
    git(
        temp.path(),
        &["remote", "add", "origin", destination.to_str().unwrap()],
    );
    let options = WatchOptions::default()
        .remote("origin")
        .branch("backup")
        .message_command("git hash-object --stdin", true);
    let mut watcher = Repository::open(temp.path().join("watched.md"), None, options).unwrap();
    fs::write(temp.path().join("watched.md"), "update").unwrap();
    let report = watcher.commit().unwrap();
    assert!(report.commit().is_some());
    assert!(matches!(report.upload(), UploadState::Failed { .. }));
    fs::write(temp.path().join("expected-message"), "watched.md\n").unwrap();
    let expected = git(temp.path(), &["hash-object", "expected-message"]);
    assert_eq!(git(temp.path(), &["log", "-1", "--format=%s"]), expected);
    git(remote.path(), &["init", "--bare", "backup.git"]);
    assert!(matches!(
        watcher.retry_upload().unwrap(),
        UploadState::Synced
    ));
    assert_eq!(
        git(&destination, &["rev-parse", "backup"]),
        report.commit().unwrap()
    );
    assert_eq!(git(temp.path(), &["rev-list", "--count", "HEAD"]), "2");
}
