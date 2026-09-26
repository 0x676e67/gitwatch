use std::{fs, process::Command};

use gitwatch::i18n::Language;
use tempfile::TempDir;

#[test]
fn language_precedence_help_errors_and_json_preserve_user_data() {
    let temp = TempDir::new().unwrap();
    let data = temp.path().join("data");
    Language::Chinese.save(&data).unwrap();
    let run = |args: &[&str], language: Option<&str>| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_gitwatch"));
        command
            .arg("--data-dir")
            .arg(&data)
            .args(args)
            .env_remove("GITWATCH_LANG");
        if let Some(language) = language {
            command.env("GITWATCH_LANG", language);
        }
        command.output().unwrap()
    };
    let stdout = |args: &[&str], language| {
        let output = run(args, language);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    assert!(stdout(&["--help"], None).contains("用法："));
    assert!(stdout(&["--help"], Some("en")).contains("Usage:"));
    assert!(
        stdout(
            &["--lang=zh-CN", "workspace", "restore", "--help"],
            Some("en")
        )
        .contains("用法：")
    );
    assert!(stdout(&["--lang", "en", "--help"], Some("zh-CN")).contains("Usage:"));
    let invalid = run(&["--does-not-exist"], None);
    assert_eq!(invalid.status.code(), Some(2));
    let error = String::from_utf8(invalid.stderr).unwrap();
    assert!(error.contains("未知参数"), "{error}");
    assert!(error.contains("--does-not-exist"));
    assert!(!run(&["--lang", "fr", "--help"], None).status.success());

    let source = temp.path().join("Running 中文 {0}");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("notes.md"), "Saved 中文 {1}").unwrap();
    let workspace: serde_json::Value = serde_json::from_str(&stdout(
        &[
            "--json",
            "workspace",
            "add",
            source.to_str().unwrap(),
            "--name",
            "Running 中文 {0}",
            "--include",
            "notes.md",
        ],
        None,
    ))
    .unwrap();
    let id = workspace["data"]["id"].as_str().unwrap();
    stdout(&["workspace", "backup", id], None);
    assert_eq!(
        stdout(&["--lang", "en", "--json", "workspace", "list"], None),
        stdout(&["--lang", "zh-CN", "--json", "workspace", "list"], None),
    );
    assert!(stdout(&["workspace", "list"], None).contains("Running 中文 {0}"));
    fs::write(source.join("notes.md"), "local {0}").unwrap();
    let preview: serde_json::Value = serde_json::from_str(&stdout(
        &["--json", "workspace", "restore", id, "--preview"],
        None,
    ))
    .unwrap();
    let plan = preview["data"]["id"].as_str().unwrap();
    let args = [
        "--json",
        "workspace",
        "restore",
        id,
        "--plan",
        plan,
        "--show",
        "notes.md",
    ];
    let english = stdout(&args, Some("en"));
    assert_eq!(english, stdout(&args, Some("zh-CN")));
    let shown: serde_json::Value = serde_json::from_str(&english).unwrap();
    assert_eq!(shown["data"]["after"], "Saved 中文 {1}");
    assert_eq!(
        fs::read_to_string(source.join("notes.md")).unwrap(),
        "local {0}"
    );
}
