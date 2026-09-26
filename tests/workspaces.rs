use std::{fs, path::Path, process::Command};

use gitwatch::workspace::{BackupStore, Change, UploadState, Workspace};
use tempfile::TempDir;

#[cfg(unix)]
#[test]
fn restore_checks_and_preserves_unix_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    let file = source.join("script.sh");
    fs::write(&file, "#!/bin/sh\necho test\n").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o700)).unwrap();
    let store = BackupStore::open(temp.path().join("data")).unwrap();
    let workspace = Workspace::builder("script", &source)
        .include("script.sh")
        .build()
        .unwrap();
    store.register(workspace.clone()).unwrap();
    let saved = store.backup(workspace.id()).unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    let plan = store
        .preview_restore(workspace.id(), saved.commit(), &[])
        .unwrap();
    assert_eq!(plan.entries()[0].change(), Change::Replace);
    fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
    assert!(store.apply_restore(plan.id()).is_err());
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    let report = store.apply_restore(plan.id()).unwrap();
    assert!(report.error().is_none());
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(report.recovery().join("files/script.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

fn git(directory: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("Git must be installed for integration tests");
    assert!(
        output.status.success(),
        "Git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn project(path: &Path, content: &str) {
    fs::create_dir_all(path.join(".agents")).unwrap();
    fs::write(path.join("AGENTS.md"), content).unwrap();
    fs::write(path.join(".agents/research.md"), "ignored research\r\n").unwrap();
}

fn workspace(root: &Path, name: &str) -> Workspace {
    Workspace::builder(name, root)
        .branch(name)
        .include("AGENTS.md")
        .include(".agents")
        .build()
        .unwrap()
}

#[test]
fn switching_remotes_invalidates_previously_fetched_workspace_branches() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    project(&root, "first remote");
    let remote = temp.path().join("remote.git");
    git(temp.path(), &["init", "--bare", remote.to_str().unwrap()]);
    let producer = BackupStore::open(temp.path().join("producer")).unwrap();
    let workspace = workspace(&root, "notes");
    producer.register(workspace.clone()).unwrap();
    producer.backup(workspace.id()).unwrap();
    producer
        .set_remote(Some(remote.to_str().unwrap()), false)
        .unwrap();
    producer.push(workspace.id()).unwrap();
    assert!(matches!(
        producer.status(workspace.id()).unwrap().unwrap().upload(),
        UploadState::Synced
    ));
    producer
        .set_remote(Some("https://example.invalid/new-remote.git"), true)
        .unwrap();
    assert!(matches!(
        producer.status(workspace.id()).unwrap().unwrap().upload(),
        UploadState::Pending
    ));
    producer.set_remote(None, false).unwrap();
    assert!(matches!(
        producer.status(workspace.id()).unwrap().unwrap().upload(),
        UploadState::Disabled
    ));
    let consumer = BackupStore::open(temp.path().join("consumer")).unwrap();
    consumer
        .set_remote(Some(remote.to_str().unwrap()), false)
        .unwrap();
    assert_eq!(consumer.fetch().unwrap().len(), 1);
    consumer
        .set_remote(Some("https://example.invalid/another.git"), false)
        .unwrap();
    assert!(consumer.remote_workspaces().unwrap().is_empty());
    assert!(consumer.import("notes", &root).is_err());
}

#[test]
fn workspace_fingerprint_separates_binary_contents_from_following_paths() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    fs::create_dir_all(root.join("notes")).unwrap();
    fs::write(root.join("notes/a"), b"onenotes/b\0\0two").unwrap();
    let store = BackupStore::open(temp.path().join("data")).unwrap();
    let workspace = Workspace::builder("binary notes", &root)
        .include("notes")
        .build()
        .unwrap();
    store.register(workspace.clone()).unwrap();
    let before = store.fingerprint(workspace.id()).unwrap();
    fs::write(root.join("notes/a"), b"one").unwrap();
    fs::write(root.join("notes/b"), b"two").unwrap();
    assert_ne!(before, store.fingerprint(workspace.id()).unwrap());
}

#[test]
fn resuming_a_paused_workspace_backs_up_changes_made_while_paused() {
    use std::{
        sync::mpsc,
        time::{Duration, Instant},
    };

    use gitwatch::watch::{self, Event, MonitorOptions, StopToken};
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    project(&root, "before pause");
    let store = BackupStore::open(temp.path().join("data")).unwrap();
    let workspace = workspace(&root, "paused")
        .edit()
        .paused(true)
        .build()
        .unwrap();
    let id = workspace.id();
    store.register(workspace.clone()).unwrap();
    store.backup(id).unwrap();
    let stop = StopToken::default();
    let token = stop.clone();
    let background = store.clone();
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        watch::watch_workspace(
            background,
            id,
            MonitorOptions::default()
                .native(false)
                .commit_on_start(true)
                .poll_interval(Duration::from_millis(50))
                .debounce(Duration::ZERO),
            token,
            |event| {
                let _ = tx.send(event);
            },
        )
    });
    assert!(matches!(
        rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        Event::Watching(_)
    ));
    assert!(matches!(
        rx.recv_timeout(Duration::from_millis(300)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    fs::write(root.join("AGENTS.md"), "edited while paused").unwrap();
    store
        .update(workspace.edit().paused(false).build().unwrap())
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut backed_up = false;
    while Instant::now() < deadline {
        if let Ok(Event::Backup(report)) = rx.recv_timeout(Duration::from_millis(100))
            && report.changed()
        {
            backed_up = true;
            break;
        }
    }
    stop.stop();
    worker.join().unwrap().unwrap();
    assert!(backed_up);
    assert_eq!(
        git(store.repository(), &["show", "paused:files/AGENTS.md"]),
        "edited while paused"
    );
}

#[test]
fn unavailable_sources_resume_without_losing_the_previous_snapshot() {
    use std::{
        sync::mpsc,
        time::{Duration, Instant},
    };

    use gitwatch::watch::{self, Event, MonitorOptions, StopToken};
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    project(&root, "before");
    let store = BackupStore::open(temp.path().join("data")).unwrap();
    let workspace = workspace(&root, "notes");
    store.register(workspace.clone()).unwrap();
    let saved = store.backup(workspace.id()).unwrap();
    let offline = temp.path().join("unmounted");
    fs::rename(&root, &offline).unwrap();
    let stop = StopToken::default();
    let token = stop.clone();
    let background = store.clone();
    let id = workspace.id();
    let (sender, receiver) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        watch::watch_workspace(
            background,
            id,
            MonitorOptions::default()
                .native(false)
                .commit_on_start(true)
                .poll_interval(Duration::from_millis(50))
                .debounce(Duration::ZERO),
            token,
            |event| {
                let _ = sender.send(event);
            },
        )
    });
    loop {
        if matches!(
            receiver.recv_timeout(Duration::from_secs(5)).unwrap(),
            Event::Watching(_)
        ) {
            break;
        }
    }
    assert_eq!(store.status(id).unwrap().unwrap().commit(), saved.commit());
    fs::rename(&offline, &root).unwrap();
    fs::write(root.join("AGENTS.md"), "after").unwrap();
    let deadline = Instant::now() + Duration::from_secs(12);
    let mut changed = false;
    while Instant::now() < deadline {
        if let Ok(Event::Backup(report)) = receiver.recv_timeout(Duration::from_millis(200))
            && report.changed()
        {
            changed = true;
            break;
        }
    }
    stop.stop();
    worker.join().unwrap().unwrap();
    assert!(changed);
    assert_eq!(
        git(store.repository(), &["show", "notes:files/AGENTS.md"]),
        "after"
    );
}

#[test]
fn branches_are_isolated_and_backups_leave_the_source_repository_untouched() {
    let temp = TempDir::new().unwrap();
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    project(&first, "first project\r\n");
    project(&second, "second project\n");
    git(&first, &["init", "-b", "main"]);
    git(&first, &["config", "user.name", "Test"]);
    git(&first, &["config", "user.email", "test@example.invalid"]);
    fs::write(first.join(".gitignore"), ".agents/\nAGENTS.md\n").unwrap();
    git(&first, &["add", ".gitignore"]);
    git(
        &first,
        &["-c", "commit.gpgsign=false", "commit", "-m", "Initial"],
    );
    let source_head = git(&first, &["rev-parse", "HEAD"]);
    let source_index = fs::read(first.join(".git/index")).unwrap();
    let store = BackupStore::open(temp.path().join("store")).unwrap();
    let a = workspace(&first, "wreq");
    let b = workspace(&second, "btls");
    store.register(a.clone()).unwrap();
    store.register(b.clone()).unwrap();
    let initial = store.backup(a.id()).unwrap();
    let other = store.backup(b.id()).unwrap();
    assert!(initial.changed() && other.changed());
    assert_eq!(initial.files(), 2);
    assert!(!store.backup(a.id()).unwrap().changed());
    assert_eq!(
        git(store.repository(), &["show", "wreq:files/AGENTS.md"]),
        "first project"
    );
    assert_eq!(
        git(store.repository(), &["show", "btls:files/AGENTS.md"]),
        "second project"
    );
    assert_eq!(
        git(store.repository(), &["rev-list", "--count", "btls"]),
        "1"
    );
    assert_eq!(git(&first, &["rev-parse", "HEAD"]), source_head);
    assert_eq!(fs::read(first.join(".git/index")).unwrap(), source_index);
    fs::remove_file(first.join("AGENTS.md")).unwrap();
    let retained = store.backup(a.id()).unwrap();
    assert_eq!(retained.retained(), &["AGENTS.md"]);
    assert_eq!(
        git(store.repository(), &["show", "wreq:files/AGENTS.md"]),
        "first project"
    );
    fs::rename(&first, temp.path().join("offline")).unwrap();
    assert!(store.backup(a.id()).is_err());
    assert_eq!(store.history(a.id(), None, 20).unwrap().len(), 2);
}

#[test]
fn restore_is_reviewable_rejects_stale_previews_and_keeps_recovery_copies() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    project(&source, "old\r\n");
    let store = BackupStore::open(temp.path().join("store")).unwrap();
    let workspace = workspace(&source, "notes");
    store.register(workspace.clone()).unwrap();
    let old = store.backup(workspace.id()).unwrap();
    fs::write(source.join("AGENTS.md"), "new\n").unwrap();
    let new = store.backup(workspace.id()).unwrap();
    assert!(
        store
            .diff(workspace.id(), old.commit(), new.commit())
            .unwrap()
            .contains("new")
    );
    let stale = store
        .preview_restore(workspace.id(), old.commit(), &["AGENTS.md".into()])
        .unwrap();
    assert_eq!(stale.entries()[0].change(), Change::Replace);
    fs::write(source.join("AGENTS.md"), "edited after preview").unwrap();
    assert!(store.apply_restore(stale.id()).is_err());
    fs::write(source.join("extra.txt"), "keep this").unwrap();
    let plan = store
        .preview_restore(workspace.id(), old.commit(), &["AGENTS.md".into()])
        .unwrap();
    let (before, after) = store.restore_contents(plan.id(), "AGENTS.md").unwrap();
    assert_eq!(before.unwrap(), b"edited after preview");
    assert_eq!(after, b"old\r\n");
    let report = store.apply_restore(plan.id()).unwrap();
    assert!(report.error().is_none());
    assert_eq!(fs::read(source.join("AGENTS.md")).unwrap(), b"old\r\n");
    assert_eq!(
        fs::read(report.recovery().join("files/AGENTS.md")).unwrap(),
        b"edited after preview"
    );
    assert_eq!(fs::read(source.join("extra.txt")).unwrap(), b"keep this");
    assert_eq!(
        git(store.repository(), &["rev-parse", "notes"]),
        new.commit()
    );
    assert!(store.apply_restore(plan.id()).is_err());
}

#[test]
fn remote_import_and_divergent_upload_preserve_both_devices() {
    let temp = TempDir::new().unwrap();
    let remote = temp.path().join("remote.git");
    fs::create_dir(&remote).unwrap();
    git(&remote, &["init", "--bare"]);
    let source_a = temp.path().join("a");
    let source_b = temp.path().join("b");
    project(&source_a, "shared baseline");
    fs::create_dir(&source_b).unwrap();
    let a = BackupStore::open(temp.path().join("store-a")).unwrap();
    let b = BackupStore::open(temp.path().join("store-b")).unwrap();
    let workspace = workspace(&source_a, "project");
    a.register(workspace.clone()).unwrap();
    a.set_remote(Some(remote.to_str().unwrap()), true).unwrap();
    let first_upload = a.backup(workspace.id()).unwrap();
    assert!(
        matches!(first_upload.upload(), UploadState::Synced),
        "{first_upload:?}"
    );
    b.set_remote(Some(remote.to_str().unwrap()), true).unwrap();
    assert_eq!(b.fetch().unwrap().len(), 1);
    let imported = b.import("project", &source_b).unwrap();
    assert_eq!(imported.id(), workspace.id());
    assert!(imported.is_paused());
    assert!(!source_b.join("AGENTS.md").exists());
    let plan = b.preview_restore(imported.id(), "project", &[]).unwrap();
    assert!(b.apply_restore(plan.id()).unwrap().error().is_none());
    fs::write(source_a.join("AGENTS.md"), "device A").unwrap();
    let a_commit = a.backup(workspace.id()).unwrap();
    fs::write(source_b.join("AGENTS.md"), "device B").unwrap();
    let b_commit = b.backup(imported.id()).unwrap();
    assert!(matches!(b_commit.upload(), UploadState::Failed { .. }));
    assert_eq!(git(&remote, &["rev-parse", "project"]), a_commit.commit());
    assert_eq!(
        git(b.repository(), &["rev-parse", "project"]),
        b_commit.commit()
    );
    assert_eq!(fs::read(source_b.join("AGENTS.md")).unwrap(), b"device B");
    assert_eq!(b.fetch().unwrap()[0].commit(), a_commit.commit());
}

#[test]
fn selection_and_store_boundaries_reject_unsafe_paths_and_branch_aliases() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    project(&source, "notes");
    for invalid in [
        "../outside",
        "/absolute",
        "dir/.git/config",
        "C:/secret",
        "file:stream",
        "CON.txt",
        "dir\\file",
    ] {
        assert!(
            Workspace::builder("bad", &source)
                .include(invalid)
                .build()
                .is_err(),
            "{invalid}"
        );
    }
    let store = BackupStore::open(temp.path().join("store")).unwrap();
    let first = workspace(&source, "first");
    store.register(first.clone()).unwrap();
    let same_branch = workspace(&source, "FIRST");
    assert!(store.register(same_branch).is_err());
    let overlapping = Workspace::builder("overlap", temp.path())
        .include("source")
        .build()
        .unwrap();
    assert!(store.register(overlapping).is_err());
    fs::write(source.join(".env"), "secret").unwrap();
    let secret = Workspace::builder("secret", &source)
        .include(".env")
        .build()
        .unwrap();
    store.register(secret.clone()).unwrap();
    assert!(store.backup(secret.id()).is_err());
    assert!(store.history(first.id(), None, 10).unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn symlinks_are_never_followed_during_backup_or_restore() {
    use std::os::unix::fs::symlink;
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    project(&source, "original");
    let store = BackupStore::open(temp.path().join("store")).unwrap();
    let workspace = workspace(&source, "notes");
    store.register(workspace.clone()).unwrap();
    let snapshot = store.backup(workspace.id()).unwrap();
    let outside = temp.path().join("outside");
    fs::write(&outside, "outside").unwrap();
    fs::remove_file(source.join("AGENTS.md")).unwrap();
    symlink(&outside, source.join("AGENTS.md")).unwrap();
    assert!(store.backup(workspace.id()).is_err());
    assert!(
        store
            .preview_restore(workspace.id(), snapshot.commit(), &[])
            .is_err()
    );
    assert_eq!(fs::read(&outside).unwrap(), b"outside");
}
