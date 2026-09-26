use std::{fs, process::Command};

use serde_json::Value;
use tempfile::TempDir;

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
    ]);
    let id = workspace["id"].as_str().unwrap();
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
}
