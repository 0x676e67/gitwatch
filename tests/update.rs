use std::{
    fs,
    process::Command,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use gitwatch::update::Notifications;

#[test]
fn notifications_use_cached_versions_without_network_or_git() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("update-check.json");
    let bytes = serde_json::to_vec(&serde_json::json!({"checked":SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),"release":{"version":"99.0.0"}})).unwrap();
    fs::write(&path, &bytes).unwrap();
    let notifications = Notifications::start(Some(temp.path().to_owned()));
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
    assert!(!temp.path().join("backup.git").exists());
    gitwatch::workspace::BackupStore::open(temp.path()).unwrap();
}

#[test]
fn self_help_and_uninstall_confirmation_are_localized() {
    let temp = tempfile::tempdir().unwrap();
    for language in ["en", "zh-CN"] {
        let output = Command::new(env!("CARGO_BIN_EXE_gitwatch"))
            .args(["--lang", language, "self", "--help"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(help.contains("update") && help.contains("uninstall"));
        assert!(help.contains(if language == "en" {
            "Update or uninstall"
        } else {
            "更新或卸载"
        }));
    }
    let cli = temp.path().join(if cfg!(windows) {
        "gitwatch.exe"
    } else {
        "gitwatch"
    });
    fs::copy(env!("CARGO_BIN_EXE_gitwatch"), &cli).unwrap();
    let data = temp.path().join("data");
    fs::create_dir(&data).unwrap();
    fs::write(data.join("keep"), "backup").unwrap();
    let output = Command::new(&cli)
        .args(["--lang", "zh-CN", "self", "uninstall"])
        .env("GITWATCH_DATA_DIR", &data)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--yes"));
    assert!(cli.exists());
    assert_eq!(fs::read_to_string(data.join("keep")).unwrap(), "backup");
}
