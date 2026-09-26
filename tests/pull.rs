use std::{fs, path::Path, process::Command, time::Duration};

use gitwatch::pull::{PullOptions, PullTask};
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

fn commit(root: &Path, contents: &str) -> String {
    fs::write(root.join("notes.md"), contents).unwrap();
    git(root, &["add", "notes.md"]);
    git(
        root,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=.disabled-hooks",
            "commit",
            "-m",
            contents,
        ],
    );
    git(root, &["rev-parse", "HEAD"])
}

#[test]
fn clone_and_fast_forward_preserve_dirty_and_divergent_local_work() {
    let temp = TempDir::new().unwrap();
    let upstream = temp.path().join("upstream");
    fs::create_dir(&upstream).unwrap();
    git(&upstream, &["init", "-b", "main"]);
    let first = commit(&upstream, "first");
    let local = temp.path().join("local");
    let mut task = PullTask::new(
        PullOptions::new(&local)
            .url(upstream.to_str().unwrap())
            .branch("main"),
    )
    .unwrap();
    let cloned = task.update().unwrap();
    assert!(cloned.cloned());
    assert_eq!(cloned.after(), first);
    assert!(!task.update().unwrap().changed());
    let second = commit(&upstream, "second");
    fs::write(local.join("notes.md"), "local work").unwrap();
    assert!(
        task.update()
            .unwrap_err()
            .to_string()
            .contains("uncommitted")
    );
    assert_eq!(
        fs::read_to_string(local.join("notes.md")).unwrap(),
        "local work"
    );
    assert_eq!(git(&local, &["rev-parse", "HEAD"]), first);
    fs::write(local.join("notes.md"), "first").unwrap();
    let updated = task.update().unwrap();
    assert_eq!(updated.before(), Some(first.as_str()));
    assert_eq!(updated.after(), second);
    let ours = commit(&local, "ours");
    commit(&upstream, "theirs");
    assert!(task.update().is_err());
    assert_eq!(git(&local, &["rev-parse", "HEAD"]), ours);
    assert_eq!(fs::read_to_string(local.join("notes.md")).unwrap(), "ours");
    git(&local, &["checkout", "-b", "other"]);
    assert!(
        task.update()
            .unwrap_err()
            .to_string()
            .contains("branch changed")
    );
}

#[test]
fn failures_do_not_replace_existing_directories_or_leave_partial_clones() {
    let temp = TempDir::new().unwrap();
    let local = temp.path().join("local");
    fs::create_dir(&local).unwrap();
    fs::write(local.join("keep.txt"), "preserve").unwrap();
    let mut task = PullTask::new(PullOptions::new(&local).url("missing-local-repository")).unwrap();
    assert!(task.update().is_err());
    assert_eq!(
        fs::read_to_string(local.join("keep.txt")).unwrap(),
        "preserve"
    );
    let absent = temp.path().join("absent");
    let mut task =
        PullTask::new(PullOptions::new(&absent).url("missing-local-repository")).unwrap();
    assert!(task.update().is_err());
    assert!(!absent.exists());
    assert!(PullTask::new(PullOptions::new(&local).interval(Duration::ZERO)).is_err());
}

#[test]
fn scheduled_updates_repeat_and_stop_without_waiting_for_the_interval() {
    use std::sync::mpsc;

    use gitwatch::watch::StopToken;
    let temp = TempDir::new().unwrap();
    let upstream = temp.path().join("upstream");
    fs::create_dir(&upstream).unwrap();
    git(&upstream, &["init", "-b", "main"]);
    commit(&upstream, "first");
    let task = PullTask::new(
        PullOptions::new(temp.path().join("local"))
            .url(upstream.to_str().unwrap())
            .interval(Duration::from_millis(100)),
    )
    .unwrap();
    let stop = StopToken::default();
    let token = stop.clone();
    let (sender, receiver) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        task.run(token, |result| {
            let _ = sender.send(
                result
                    .map(|r| r.after().to_owned())
                    .map_err(|e| e.to_string()),
            );
        })
    });
    receiver
        .recv_timeout(Duration::from_secs(30))
        .unwrap()
        .unwrap();
    let expected = commit(&upstream, "second");
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut updated = false;
    while std::time::Instant::now() < deadline {
        match receiver.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())) {
            Ok(result) if result.as_ref().is_ok_and(|commit| commit == &expected) => {
                updated = true;
                break;
            }
            Ok(result) => {
                result.unwrap();
            }
            Err(mpsc::RecvTimeoutError::Timeout) => break,
            Err(error) => panic!("Pull worker disconnected: {error}"),
        }
    }
    stop.stop();
    worker.join().unwrap().unwrap();
    assert!(updated);
}
