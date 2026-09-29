use std::process::Command;

use gitwatch::{i18n::Language, workspace::BackupStore};

#[test]
fn missing_git_diagnostics_allow_initialization_to_retry() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "missing_git_child", "--nocapture"])
        .env("GITWATCH_TEST_STARTUP", &data)
        .env("GW_GIT_BIN", temp.path().join("missing-git-executable"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let store = BackupStore::open(data).unwrap();
    assert!(store.workspaces().unwrap().is_empty());
}

#[test]
fn missing_git_child() {
    let Some(data) = std::env::var_os("GITWATCH_TEST_STARTUP") else {
        return;
    };
    for language in [Language::English, Language::Chinese] {
        let error = match BackupStore::open(&data) {
            Ok(_) => panic!("Missing Git must prevent initialization"),
            Err(error) => language.error(&format!("{error:#}")),
        };
        for expected in [language.text("Cannot run Git; install Git, make sure it is on PATH, then restart gitwatch. If GW_GIT_BIN is set, check that path"), "missing-git-executable"] {
            assert!(error.contains(expected), "{error}");
        }
    }
}
