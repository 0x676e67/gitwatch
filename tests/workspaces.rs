#[path = "support/git.rs"]
mod support;

use std::{fs, path::Path};

use gitwatch::workspace::{BackupStore, Change, UploadState, Workspace};
use support::git;
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
fn named_branches_rename_history_and_reject_conflicting_tasks() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    project(&root, "original");
    let store = BackupStore::open(temp.path().join("store")).unwrap();
    let task = Workspace::builder("quic", &root)
        .include("AGENTS.md")
        .build()
        .unwrap();
    assert_eq!(task.branch(), "quic");
    store.register(task.clone()).unwrap();
    let duplicate = Workspace::builder(" QuIc ", &root)
        .branch("different")
        .include("AGENTS.md")
        .build()
        .unwrap();
    assert!(
        store
            .register(duplicate)
            .unwrap_err()
            .to_string()
            .contains("name already exists")
    );
    assert!(
        store
            .register(
                Workspace::builder("quic", &root)
                    .branch("other")
                    .include("AGENTS.md")
                    .build()
                    .unwrap()
            )
            .unwrap_err()
            .to_string()
            .contains("name already exists")
    );
    let remote = temp.path().join("remote.git");
    git(temp.path(), &["init", "--bare", remote.to_str().unwrap()]);
    store
        .set_remote(Some(remote.to_str().unwrap()), true)
        .unwrap();
    store.push(task.id()).unwrap();
    let first = store
        .status(task.id())
        .unwrap()
        .unwrap()
        .commit()
        .to_owned();
    let config = fs::read(store.directory().join("config.json")).unwrap();
    let renamed = task.edit().name("Quic notes").build().unwrap();
    assert_eq!(renamed.branch(), "Quic-notes");
    git(&remote, &["config", "receive.denyDeletes", "true"]);
    assert!(store.update(renamed.clone()).is_err());
    assert_eq!(store.workspaces().unwrap()[0].name(), "quic");
    assert_eq!(git(&remote, &["rev-parse", "quic"]), first);
    assert_eq!(
        git(
            &remote,
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/heads/Quic-notes"
            ]
        ),
        ""
    );
    git(&remote, &["config", "receive.denyDeletes", "false"]);
    store.update(renamed.clone()).unwrap();
    let latest = store
        .status(task.id())
        .unwrap()
        .unwrap()
        .commit()
        .to_owned();
    assert_eq!(
        git(&remote, &["show", "Quic-notes:files/AGENTS.md"]),
        "original"
    );
    assert_eq!(git(&remote, &["rev-parse", "Quic-notes^"]), first);
    let manifest: serde_json::Value =
        serde_json::from_str(&git(&remote, &["show", "Quic-notes:manifest.json"])).unwrap();
    assert_eq!(manifest["name"], "Quic notes");
    assert_eq!(
        git(
            &remote,
            &["for-each-ref", "--format=%(refname)", "refs/heads/quic"]
        ),
        ""
    );
    // Simulate interruption after the remote accepted the rename but before config was saved.
    fs::write(store.directory().join("config.json"), &config).unwrap();
    git(
        store.repository(),
        &["update-ref", "refs/heads/quic", &first],
    );
    store.update(renamed.clone()).unwrap();
    assert_eq!(store.status(task.id()).unwrap().unwrap().commit(), latest);
    assert_eq!(store.workspaces().unwrap()[0].id(), task.id());
    // An unrelated destination, even with identical contents, must never be overwritten.
    git(&remote, &["update-ref", "refs/heads/taken", &first]);
    let failed = renamed.edit().name("taken").build().unwrap();
    assert!(
        store
            .update(failed)
            .unwrap_err()
            .to_string()
            .contains("already exists")
    );
    assert_eq!(store.workspaces().unwrap()[0].name(), "Quic notes");
    assert_eq!(git(&remote, &["rev-parse", "taken"]), first);
    assert_eq!(
        git(
            store.repository(),
            &["for-each-ref", "--format=%(refname)", "refs/heads/taken"]
        ),
        ""
    );
    // A failed rename must not block backing up or choosing another name.
    fs::write(root.join("AGENTS.md"), "new contents").unwrap();
    store.backup(task.id()).unwrap();
    let final_task = renamed.edit().name("final").build().unwrap();
    store.update(final_task).unwrap();
    assert_eq!(
        git(&remote, &["show", "final:files/AGENTS.md"]),
        "new contents"
    );
    // Existing UUID branch bindings remain usable and can be explicitly migrated.
    let legacy = Workspace::builder("legacy", &root)
        .branch("workspaces/old-id")
        .include("AGENTS.md")
        .build()
        .unwrap();
    store.register(legacy.clone()).unwrap();
    store.push(legacy.id()).unwrap();
    store
        .update(legacy.edit().branch("legacy").build().unwrap())
        .unwrap();
    assert_eq!(
        git(&remote, &["show", "legacy:files/AGENTS.md"]),
        "new contents"
    );
    // CLI/API backup creation shares the name namespace used by GUI pull/watch tasks.
    fs::write(
        store.directory().join("tasks.json"),
        serde_json::to_vec(&serde_json::json!([{
            "id": uuid::Uuid::new_v4(), "name": "Pull task"
        }]))
        .unwrap(),
    )
    .unwrap();
    let collision = Workspace::builder("pull TASK", &root)
        .include("AGENTS.md")
        .build()
        .unwrap();
    assert!(
        store
            .register(collision)
            .unwrap_err()
            .to_string()
            .contains("name already exists")
    );
}

#[test]
fn first_upload_failures_keep_local_snapshots_for_retry() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("source");
    project(&root, "saved locally");
    let store = BackupStore::open(temp.path().join("store")).unwrap();
    let task = workspace(&root, "notes");
    store.register(task.clone()).unwrap();
    let remote = temp.path().join("missing.git");
    store
        .set_remote(Some(remote.to_str().unwrap()), true)
        .unwrap();
    fs::rename(&root, temp.path().join("offline-source")).unwrap();
    assert!(store.push(task.id()).is_err());
    assert!(store.status(task.id()).unwrap().is_none());
    fs::rename(temp.path().join("offline-source"), &root).unwrap();
    assert!(store.push(task.id()).is_err());
    let report = store.status(task.id()).unwrap().unwrap();
    assert_eq!(report.files(), 2);
    assert!(matches!(report.upload(), UploadState::Failed { .. }));
    git(temp.path(), &["init", "--bare", remote.to_str().unwrap()]);
    store.push(task.id()).unwrap();
    assert_eq!(git(&remote, &["rev-parse", "notes"]), report.commit());
    assert!(matches!(
        store.status(task.id()).unwrap().unwrap().upload(),
        UploadState::Synced
    ));
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
    producer
        .set_remote(Some(remote.to_str().unwrap()), false)
        .unwrap();
    assert!(producer.status(workspace.id()).unwrap().is_none());
    producer.push(workspace.id()).unwrap();
    let first = producer.status(workspace.id()).unwrap().unwrap();
    assert_eq!(first.files(), 2);
    fs::rename(&root, temp.path().join("offline-source")).unwrap();
    producer.push(workspace.id()).unwrap();
    assert_eq!(
        producer.status(workspace.id()).unwrap().unwrap().commit(),
        first.commit()
    );
    fs::rename(temp.path().join("offline-source"), &root).unwrap();
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

#[test]
fn following_links_backs_up_contents_without_changing_restore_boundaries() {
    fn link(target: &Path, path: &Path, directory: bool) {
        #[cfg(unix)]
        {
            let _ = directory;
            std::os::unix::fs::symlink(target, path).unwrap();
        }
        #[cfg(windows)]
        if directory {
            std::os::windows::fs::symlink_dir(target, path).unwrap();
        } else {
            std::os::windows::fs::symlink_file(target, path).unwrap();
        }
    }
    fn unlink_dir(path: &Path) {
        #[cfg(unix)]
        fs::remove_file(path).unwrap();
        #[cfg(windows)]
        fs::remove_dir(path).unwrap();
    }
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    let outside = temp.path().join("outside");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(source.join("notes.md"), "local").unwrap();
    fs::write(outside.join("SKILL.md"), "skill v1").unwrap();
    fs::write(outside.join(".env"), "secret").unwrap();
    link(&outside, &source.join("skills"), true);
    link(
        &Path::new("..").join("outside").join("SKILL.md"),
        &source.join("prompt.md"),
        false,
    );
    let store = BackupStore::open(temp.path().join("store")).unwrap();
    let workspace = Workspace::builder("agents", &source)
        .include("skills")
        .include("skills/SKILL.md")
        .include("prompt.md")
        .include("notes.md")
        .build()
        .unwrap();
    // Old configurations retain the opt-in boundary.
    let mut legacy = serde_json::to_value(&workspace).unwrap();
    legacy.as_object_mut().unwrap().remove("follow_links");
    assert!(
        !serde_json::from_value::<Workspace>(legacy)
            .unwrap()
            .follows_links()
    );
    store.register(workspace.clone()).unwrap();
    assert!(store.backup(workspace.id()).is_err());
    assert!(store.history(workspace.id(), None, 10).unwrap().is_empty());
    let workspace = workspace.edit().follow_links(true).build().unwrap();
    store.update(workspace.clone()).unwrap();
    assert!(store.workspaces().unwrap()[0].follows_links());
    let snapshot = store.backup(workspace.id()).unwrap();
    assert_eq!(snapshot.files(), 3);
    let fingerprint = store.fingerprint(workspace.id()).unwrap();
    fs::write(outside.join("SKILL.md"), "skill v2").unwrap();
    assert_ne!(fingerprint, store.fingerprint(workspace.id()).unwrap());
    assert!(
        store
            .preview_restore(workspace.id(), snapshot.commit(), &[])
            .is_err()
    );
    link(&outside, &outside.join("loop"), true);
    assert!(
        store
            .backup(workspace.id())
            .unwrap_err()
            .to_string()
            .contains("cycle")
    );
    unlink_dir(&outside.join("loop"));
    link(store.directory(), &outside.join("backup"), true);
    assert!(store.backup(workspace.id()).is_err());
    unlink_dir(&outside.join("backup"));
    link(&outside.join(".env"), &outside.join("secret.txt"), false);
    assert!(
        store
            .backup(workspace.id())
            .unwrap_err()
            .to_string()
            .contains("protected")
    );
    fs::remove_file(outside.join("secret.txt")).unwrap();
    link(&outside.join("missing"), &outside.join("broken"), false);
    assert_eq!(store.backup(workspace.id()).unwrap().files(), 3);
    // Exclusions refer to the visible source path, including linked children.
    fs::write(outside.join("extra.md"), "excluded").unwrap();
    store
        .update(workspace.edit().exclude("skills/extra.md").build().unwrap())
        .unwrap();
    assert_eq!(store.backup(workspace.id()).unwrap().files(), 3);
    fs::remove_file(outside.join("broken")).unwrap();
    let absent = temp.path().join("absent");
    fs::rename(&outside, &absent).unwrap();
    fs::write(source.join("notes.md"), "local v2").unwrap();
    let retained = store.backup(workspace.id()).unwrap();
    assert!(retained.changed());
    assert_eq!(retained.files(), 3);
    assert_eq!(retained.retained(), &["prompt.md", "skills/SKILL.md"]);
    fs::rename(&absent, &outside).unwrap();
    assert_eq!(fs::read(outside.join("SKILL.md")).unwrap(), b"skill v2");
    unlink_dir(&source.join("skills"));
    fs::remove_file(source.join("prompt.md")).unwrap();
    let plan = store
        .preview_restore(workspace.id(), snapshot.commit(), &[])
        .unwrap();
    assert!(store.apply_restore(plan.id()).unwrap().error().is_none());
    assert_eq!(
        fs::read(source.join("skills/SKILL.md")).unwrap(),
        b"skill v1"
    );
    assert_eq!(fs::read(source.join("prompt.md")).unwrap(), b"skill v1");
    assert!(
        !fs::symlink_metadata(source.join("skills"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(outside.join("SKILL.md")).unwrap(), b"skill v2");
}
