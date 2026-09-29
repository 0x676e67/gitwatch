use std::{
    fs,
    process::Command,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use gitwatch::update::Notifications;

#[test]
fn notifications_use_cached_versions_without_network_or_git() {
    let temp = tempfile::tempdir().unwrap();
    let cache = temp.path().join("cache");
    fs::create_dir(&cache).unwrap();
    fs::write(cache.join(".tmpminQ1r"), "interrupted cache write").unwrap();
    let path = cache.join("update-check.json");
    let bytes = serde_json::to_vec(&serde_json::json!({"checked":SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),"release":{"version":"99.0.0"}})).unwrap();
    fs::write(&path, &bytes).unwrap();
    let notifications = Notifications::start(Some(cache.clone()));
    let deadline = Instant::now() + Duration::from_secs(3);
    let release = loop {
        if let Some(release) = notifications.poll() {
            break release;
        }
        assert!(
            Instant::now() < deadline,
            "Cached notification was not delivered"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(release.version(), "99.0.0");
    assert_eq!(
        release.url(),
        "https://github.com/0x676e67/gitwatch/releases/tag/v99.0.0"
    );
    assert!(notifications.poll().is_none());
    assert_eq!(fs::read(path).unwrap(), bytes);
    assert!(!cache.join("backup.git").exists());
    assert!(!cache.join("store.lock").exists());
}

#[test]
fn version_probe_does_not_open_git_or_initialize_user_data() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let output = Command::new(env!("CARGO_BIN_EXE_gitwatch"))
        .arg("--version")
        .env("GITWATCH_DATA_DIR", &data)
        .env("GW_GIT_BIN", temp.path().join("missing-git"))
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        format!("gitwatch {}", gitwatch::update::VERSION)
    );
    assert!(!data.exists());
}
