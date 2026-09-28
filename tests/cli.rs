use std::{fs, process::Command};

use serde_json::Value;
use tempfile::TempDir;

#[path = "support/git.rs"]
mod support;

fn cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_gitwatch"))
        .args(args)
        .output()
        .unwrap()
}

fn json(args: &[&str]) -> Value {
    let output = cli(args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema_version"], 1);
    value["data"].clone()
}

#[test]
fn help_and_invalid_inputs_have_predictable_exit_codes() {
    for args in [
        &["-h"][..],
        &["--version"],
        &["watch", "--help"],
        &["workspace", "--help"],
        &["pull", "--help"],
    ] {
        assert!(cli(args).status.success(), "{args:?}");
    }
    assert!(cli(&[]).status.success());
    assert!(!cli(&["--does-not-exist"]).status.success());
    assert!(
        !cli(&["pull", ".", "--every", "NaN", "--once"])
            .status
            .success()
    );
    assert!(!cli(&["-C"]).status.success());
    assert_eq!(
        cli(&["pull", ".", "--strategy", "squash", "--once"])
            .status
            .code(),
        Some(2)
    );
    let help = cli(&["--lang", "zh-CN", "pull", "--help"]);
    assert!(String::from_utf8_lossy(&help.stdout).contains("拉取策略"));
}

#[test]
fn store_initialization_can_retry_after_git_is_unavailable() {
    let temp = TempDir::new().unwrap();
    let data = temp.path().join("data");
    let output = Command::new(env!("CARGO_BIN_EXE_gitwatch"))
        .args(["--lang", "en", "--data-dir"])
        .arg(&data)
        .args(["workspace", "list"])
        .env("GW_GIT_BIN", temp.path().join("missing-git-executable"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    let output = cli(&[
        "--lang",
        "en",
        "--data-dir",
        data.to_str().unwrap(),
        "workspace",
        "list",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn workspace_json_backup_and_confirmed_restore_round_trip() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("project");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("notes.md"), "saved").unwrap();
    let data = temp.path().join("store");
    let prefix = ["--data-dir", data.to_str().unwrap(), "--json", "workspace"];
    let run = |args: &[&str]| json(&[&prefix[..], args].concat());
    let workspace = run(&[
        "add",
        source.to_str().unwrap(),
        "--name",
        "notes",
        "--include",
        "notes.md",
        "--follow-links",
    ]);
    let id = workspace["id"].as_str().unwrap();
    assert_eq!(workspace["follow_links"], true);
    assert_eq!(
        run(&["edit", id, "--follow-links", "false"])["follow_links"],
        false
    );
    assert_eq!(
        run(&["edit", id, "--follow-links", "true"])["follow_links"],
        true
    );
    let backup = run(&["backup", id]);
    assert_eq!(backup["event"], "backup");
    fs::write(source.join("notes.md"), "local edit").unwrap();
    let preview = run(&["restore", id, "--preview"]);
    assert_eq!(preview["entries"][0]["change"], "replace");
    let plan = preview["id"].as_str().unwrap();
    let shown = run(&["restore", id, "--plan", plan, "--show", "notes.md"]);
    assert_eq!(shown["before"], "local edit");
    assert_eq!(shown["after"], "saved");
    assert!(
        !cli(&[&prefix[..], &["restore", id, "--plan", plan]].concat())
            .status
            .success()
    );
    let restored = run(&["restore", id, "--plan", plan, "--confirm"]);
    assert_eq!(restored["written"][0], "notes.md");
    assert_eq!(
        fs::read_to_string(source.join("notes.md")).unwrap(),
        "saved"
    );
    let remote = temp.path().join("remote.git");
    support::git(temp.path(), &["init", "--bare", remote.to_str().unwrap()]);
    run(&["remote", remote.to_str().unwrap()]);
    run(&["push", id]);
    fs::write(source.join("notes.md"), "changed locally").unwrap();
    let preview = run(&["restore", id, "--from-remote"]);
    assert!(preview["remote"].is_object());
    assert_eq!(preview["commit"], backup["detail"]["commit"]);
    assert_eq!(
        fs::read_to_string(source.join("notes.md")).unwrap(),
        "changed locally"
    );
    let plan = preview["id"].as_str().unwrap();
    run(&["restore", id, "--plan", plan, "--confirm"]);
    assert_eq!(
        fs::read_to_string(source.join("notes.md")).unwrap(),
        "saved"
    );
    assert_eq!(
        cli(&[
            &prefix[..],
            &["restore", id, "--from-remote", "--file", "notes.md"]
        ]
        .concat())
        .status
        .code(),
        Some(2)
    );
}
