use super::*;

struct Pair {
    _temp: tempfile::TempDir,
    a: BackupStore,
    b: BackupStore,
    id: Uuid,
    left: PathBuf,
    right: PathBuf,
}

impl Pair {
    fn new(strategy: PullStrategy) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let remote = temp.path().join("remote.git");
        git::init_bare(&remote).unwrap();
        let left = temp.path().join("left");
        let right = temp.path().join("right");
        fs::create_dir_all(left.join("notes")).unwrap();
        fs::create_dir(&right).unwrap();
        fs::write(left.join("notes/a.txt"), "base\n").unwrap();
        fs::write(left.join("notes/delete.txt"), "delete later\n").unwrap();
        let a = BackupStore::open(temp.path().join("a")).unwrap();
        let b = BackupStore::open(temp.path().join("b")).unwrap();
        for store in [&a, &b] {
            store
                .set_remote(Some(remote.to_str().unwrap()), false)
                .unwrap();
        }
        let workspace = Workspace::builder("notes", &left)
            .include("notes")
            .build()
            .unwrap();
        let id = workspace.id();
        a.register(workspace).unwrap();
        a.backup(id).unwrap();
        a.push(id).unwrap();
        b.fetch().unwrap();
        b.import("notes", &right).unwrap();
        for store in [&a, &b] {
            let plan = store
                .preview_sync(id, SyncOptions::default().strategy(strategy), false)
                .unwrap();
            store.confirm_sync(id, plan.token().unwrap()).unwrap();
        }
        assert_eq!(fs::read(right.join("notes/a.txt")).unwrap(), b"base\n");
        Self {
            _temp: temp,
            a,
            b,
            id,
            left,
            right,
        }
    }
}

#[test]
fn two_devices_integrate_edits_deletions_and_preserve_source_git() {
    for strategy in [PullStrategy::Rebase, PullStrategy::Merge] {
        let pair = Pair::new(strategy);
        fs::create_dir(pair.right.join(".git")).unwrap();
        fs::write(pair.right.join(".git/index"), b"source index").unwrap();
        fs::write(pair.left.join("notes/a.txt"), b"remote edit\n").unwrap();
        fs::remove_file(pair.left.join("notes/delete.txt")).unwrap();
        pair.a.synchronize(pair.id).unwrap();
        fs::write(pair.right.join("notes/local.txt"), b"local edit\n").unwrap();
        let report = pair.b.synchronize(pair.id).unwrap();
        assert!(matches!(report.upload(), UploadState::Synced));
        assert_eq!(
            fs::read(pair.right.join("notes/a.txt")).unwrap(),
            b"remote edit\n"
        );
        assert!(!pair.right.join("notes/delete.txt").exists());
        assert_eq!(
            fs::read(pair.right.join("notes/local.txt")).unwrap(),
            b"local edit\n"
        );
        assert_eq!(
            fs::read(pair.right.join(".git/index")).unwrap(),
            b"source index"
        );
        let parents = pair
            .b
            .git
            .text(["rev-list", "--parents", "-n", "1", report.commit()])
            .unwrap();
        assert_eq!(
            parents.split_whitespace().count(),
            if strategy == PullStrategy::Merge {
                3
            } else {
                2
            }
        );
        pair.a.synchronize(pair.id).unwrap();
        assert_eq!(
            fs::read(pair.left.join("notes/local.txt")).unwrap(),
            b"local edit\n"
        );
    }
}

#[test]
fn conflicts_survive_reopen_and_require_explicit_resolution() {
    let pair = Pair::new(PullStrategy::Rebase);
    fs::write(pair.left.join("notes/a.txt"), b"remote\n").unwrap();
    pair.a.synchronize(pair.id).unwrap();
    fs::write(pair.right.join("notes/a.txt"), b"local\n").unwrap();
    assert!(pair.b.synchronize(pair.id).is_err());
    assert_eq!(
        fs::read(pair.right.join("notes/a.txt")).unwrap(),
        b"local\n"
    );
    let reopened = BackupStore::open(pair.b.directory()).unwrap();
    let status = reopened.sync_status(pair.id).unwrap();
    assert_eq!(status.phase(), SyncPhase::Conflict);
    assert_eq!(status.conflicts(), &["notes/a.txt"]);
    let contents = reopened
        .sync_conflict_contents(pair.id, "notes/a.txt")
        .unwrap();
    assert_eq!(contents.local().unwrap(), b"local\n");
    assert_eq!(contents.remote().unwrap(), b"remote\n");
    assert_eq!(contents.base().unwrap(), b"base\n");
    assert!(reopened.continue_sync(pair.id).is_err());
    reopened
        .resolve_sync_file(pair.id, "notes/a.txt", Some(b"combined\n"))
        .unwrap();
    let preview = reopened.continue_sync(pair.id).unwrap();
    assert_eq!(preview.phase(), SyncPhase::Preview);
    assert_eq!(
        fs::read(pair.right.join("notes/a.txt")).unwrap(),
        b"local\n"
    );
    reopened
        .confirm_sync(pair.id, preview.token().unwrap())
        .unwrap();
    reopened.synchronize(pair.id).unwrap();
    assert_eq!(
        fs::read(pair.right.join("notes/a.txt")).unwrap(),
        b"combined\n"
    );
}

#[test]
fn stale_previews_and_ff_only_never_overwrite_source() {
    let pair = Pair::new(PullStrategy::FastForwardOnly);
    fs::write(pair.left.join("notes/a.txt"), b"remote\n").unwrap();
    pair.a.synchronize(pair.id).unwrap();
    fs::write(pair.right.join("notes/a.txt"), b"local\n").unwrap();
    assert!(pair.b.synchronize(pair.id).is_err());
    assert_eq!(
        pair.b.sync_status(pair.id).unwrap().phase(),
        SyncPhase::Conflict
    );
    let preview = pair
        .b
        .preview_sync(pair.id, SyncOptions::default(), true)
        .unwrap();
    assert!(
        preview
            .entries()
            .iter()
            .any(|entry| entry.path() == "notes/a.txt")
    );
    fs::write(pair.right.join("notes/a.txt"), b"new edit\n").unwrap();
    assert!(
        pair.b
            .confirm_sync(pair.id, preview.token().unwrap())
            .is_err()
    );
    assert_eq!(
        fs::read(pair.right.join("notes/a.txt")).unwrap(),
        b"new edit\n"
    );
    let preview = pair
        .b
        .preview_sync(pair.id, SyncOptions::default(), true)
        .unwrap();
    pair.b
        .confirm_sync(pair.id, preview.token().unwrap())
        .unwrap();
    assert_eq!(
        fs::read(pair.right.join("notes/a.txt")).unwrap(),
        b"remote\n"
    );
    let status = pair.b.sync_status(pair.id).unwrap();
    assert_eq!(
        fs::read(status.recovery().unwrap().join("files/notes/a.txt")).unwrap(),
        b"new edit\n"
    );
    let undo = pair.b.preview_sync_undo(pair.id).unwrap();
    pair.b.confirm_sync(pair.id, undo.token().unwrap()).unwrap();
    assert_eq!(
        fs::read(pair.right.join("notes/a.txt")).unwrap(),
        b"new edit\n"
    );
}

#[test]
fn interrupted_application_blocks_backup_and_resumes_without_losing_edits() {
    let pair = Pair::new(PullStrategy::Rebase);
    fs::write(pair.left.join("notes/a.txt"), b"remote\n").unwrap();
    fs::write(pair.left.join("notes/new.txt"), b"new remote\n").unwrap();
    pair.a.synchronize(pair.id).unwrap();
    let preview = pair
        .b
        .preview_sync(pair.id, SyncOptions::default(), true)
        .unwrap();
    let mut state = pair.b.sync_state(pair.id).unwrap();
    let pending = state.pending.as_mut().unwrap();
    let recovery = pair.b.session_path(pair.id, pending.id).join("recovery");
    fs::create_dir_all(recovery.join("files/notes")).unwrap();
    fs::write(recovery.join("files/notes/a.txt"), b"base\n").unwrap();
    paths::atomic_write(
        &recovery.join("operation.json"),
        &serde_json::to_vec(pending).unwrap(),
    )
    .unwrap();
    pending.phase = SyncPhase::Applying;
    pair.b.save_sync(pair.id, &state).unwrap();
    // Simulate a process exit after one of two journaled writes.
    fs::write(pair.right.join("notes/a.txt"), b"remote\n").unwrap();
    let reopened = BackupStore::open(pair.b.directory()).unwrap();
    assert!(reopened.backup(pair.id).is_err());
    assert!(reopened.cancel_sync(pair.id).is_err());
    fs::write(pair.right.join("notes/a.txt"), b"edit after interruption\n").unwrap();
    assert!(
        reopened
            .confirm_sync(pair.id, preview.token().unwrap())
            .is_err()
    );
    assert_eq!(
        fs::read(pair.right.join("notes/a.txt")).unwrap(),
        b"edit after interruption\n"
    );
    fs::write(pair.right.join("notes/a.txt"), b"remote\n").unwrap();
    fs::write(pair.right.join("notes/new.txt"), b"concurrent edit\n").unwrap();
    assert!(
        reopened
            .confirm_sync(pair.id, preview.token().unwrap())
            .is_err()
    );
    assert_eq!(
        fs::read(pair.right.join("notes/new.txt")).unwrap(),
        b"concurrent edit\n"
    );
    fs::remove_file(pair.right.join("notes/new.txt")).unwrap();
    reopened
        .confirm_sync(pair.id, preview.token().unwrap())
        .unwrap();
    assert_eq!(
        fs::read(pair.right.join("notes/new.txt")).unwrap(),
        b"new remote\n"
    );
    assert_eq!(
        reopened.sync_status(pair.id).unwrap().phase(),
        SyncPhase::Ready
    );
}

#[test]
fn offline_commits_binary_conflicts_and_abort_keep_both_histories() {
    let pair = Pair::new(PullStrategy::Merge);
    fs::write(pair.left.join("notes/a.txt"), b"\0remote binary").unwrap();
    pair.a.synchronize(pair.id).unwrap();
    fs::write(pair.right.join("notes/a.txt"), b"\0local binary").unwrap();
    assert!(pair.b.synchronize(pair.id).is_err());
    let contents = pair
        .b
        .sync_conflict_contents(pair.id, "notes/a.txt")
        .unwrap();
    assert_eq!(contents.local().unwrap(), b"\0local binary");
    assert_eq!(contents.remote().unwrap(), b"\0remote binary");
    let before = pair.b.git.reference("refs/heads/notes").unwrap().unwrap();
    pair.b.cancel_sync(pair.id).unwrap();
    assert_eq!(
        pair.b.git.reference("refs/heads/notes").unwrap().unwrap(),
        before
    );
    assert_eq!(
        fs::read(pair.right.join("notes/a.txt")).unwrap(),
        b"\0local binary"
    );
    // Break only this isolated fixture's remote; local saves must still survive.
    pair.b
        .git
        .run([
            "config",
            "remote.origin.url",
            pair._temp.path().join("missing.git").to_str().unwrap(),
        ])
        .unwrap();
    fs::write(pair.right.join("notes/offline.txt"), b"offline work").unwrap();
    assert!(pair.b.synchronize(pair.id).is_err());
    let saved = pair.b.git.resolve("refs/heads/notes").unwrap();
    assert!(pair.b.git.ancestor(&before, &saved).unwrap());
    assert_eq!(
        pair.b
            .git
            .run(["show", &format!("{saved}:files/notes/offline.txt")])
            .unwrap(),
        b"offline work"
    );
    assert!(pair.b.sync_status(pair.id).unwrap().next_check().is_some());
}

#[test]
fn remote_polling_needs_no_local_event_and_push_races_never_force_remote_history() {
    let pair = Pair::new(PullStrategy::Rebase);
    let workspace = pair.b.workspaces().unwrap().remove(0);
    let before = pair
        .b
        .git
        .reference(&workspace.reference())
        .unwrap()
        .unwrap();
    fs::write(pair.right.join("notes/local.txt"), b"pending local").unwrap();
    let local = pair
        .b
        .sync_commit(
            &workspace,
            &pair.b.sync_files(&workspace).unwrap(),
            Some(&before),
        )
        .unwrap();
    pair.b
        .git
        .update_ref(&workspace.reference(), &local, Some(&before))
        .unwrap();
    fs::rename(
        pair.left.join("notes/a.txt"),
        pair.left.join("notes/renamed.txt"),
    )
    .unwrap();
    let remote = pair.a.synchronize(pair.id).unwrap().commit().to_owned();
    let rejected = pair.b.push_sync(&workspace).unwrap();
    assert!(matches!(rejected.upload(), UploadState::Failed { .. }));
    assert_eq!(pair.a.git.resolve("refs/heads/notes").unwrap(), remote);
    pair.b.synchronize(pair.id).unwrap();
    assert!(!pair.right.join("notes/a.txt").exists());
    assert!(pair.right.join("notes/renamed.txt").exists());
    fs::write(pair.left.join("notes/remote-only.txt"), b"new remote file").unwrap();
    pair.a.synchronize(pair.id).unwrap();
    let mut state = pair.b.sync_state(pair.id).unwrap();
    state.next_check = Some(sync_time().unwrap() + 1);
    pair.b.save_sync(pair.id, &state).unwrap();
    let reopened = BackupStore::open(pair.b.directory()).unwrap();
    assert_eq!(
        reopened.sync_status(pair.id).unwrap().next_check(),
        state.next_check
    );
    let token = crate::watch::StopToken::default();
    let worker_token = token.clone();
    let id = pair.id;
    let (sender, receiver) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        crate::watch::watch_workspace(
            reopened,
            id,
            crate::watch::MonitorOptions::default(),
            worker_token,
            |event| {
                if matches!(event, crate::watch::Event::Backup(_)) {
                    let _ = sender.send(());
                }
            },
        )
    });
    receiver
        .recv_timeout(std::time::Duration::from_secs(20))
        .unwrap();
    token.stop();
    worker.join().unwrap().unwrap();
    assert_eq!(
        fs::read(pair.right.join("notes/remote-only.txt")).unwrap(),
        b"new remote file"
    );
}

#[test]
fn preview_is_bound_to_the_complete_selected_tree() {
    let pair = Pair::new(PullStrategy::Rebase);
    fs::write(pair.left.join("notes/a.txt"), b"remote\n").unwrap();
    pair.a.synchronize(pair.id).unwrap();
    let preview = pair
        .b
        .preview_sync(pair.id, SyncOptions::default(), true)
        .unwrap();
    fs::write(
        pair.right.join("notes/added-after-preview.txt"),
        b"new local work",
    )
    .unwrap();
    assert!(
        pair.b
            .confirm_sync(pair.id, preview.token().unwrap())
            .is_err()
    );
    assert_eq!(fs::read(pair.right.join("notes/a.txt")).unwrap(), b"base\n");
    assert_eq!(
        fs::read(pair.right.join("notes/added-after-preview.txt")).unwrap(),
        b"new local work"
    );
}

#[cfg(unix)]
#[test]
fn links_are_never_followed_during_sync_and_executable_modes_survive() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let pair = Pair::new(PullStrategy::Rebase);
    fs::set_permissions(
        pair.left.join("notes/a.txt"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    pair.a.synchronize(pair.id).unwrap();
    pair.b.synchronize(pair.id).unwrap();
    assert_ne!(
        fs::metadata(pair.right.join("notes/a.txt"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0
    );
    let outside = pair._temp.path().join("outside");
    fs::write(&outside, b"do not touch").unwrap();
    symlink(&outside, pair.right.join("notes/link")).unwrap();
    assert!(pair.b.synchronize(pair.id).is_err());
    assert_eq!(fs::read(outside).unwrap(), b"do not touch");
}
