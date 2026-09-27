//! Read-only repository summaries. History is scoped to a captured local HEAD;
//! line counts describe committed files, including data and prose.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, ensure};

use crate::{Result, git, watch::StopToken};

pub(crate) struct Summary {
    pub root: PathBuf,
    pub remote: Option<Remote>,
    pub branch: String,
    pub shallow: bool,
    pub commits: u64,
    pub branches: usize,
    pub tags: usize,
    pub contributors: Vec<(String, u64)>,
    pub latest: Option<Commit>,
    pub files: usize,
    pub lines: Vec<Lines>,
    pub skipped: usize,
}

pub(crate) struct Commit {
    pub id: String,
    pub author: String,
    pub time: String,
    pub subject: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Remote {
    pub url: String,
    pub github: Option<(String, String)>,
}

#[derive(Default)]
pub(crate) struct Lines {
    pub file_type: String,
    pub files: usize,
    pub text: usize,
}

// ===== impl Summary =====

impl Summary {
    pub fn read(path: &Path, remote: &str, stop: &StopToken) -> Result<Self> {
        let directory = if path.is_file() {
            path.parent().context("Missing parent directory")?
        } else {
            path
        };
        let root = PathBuf::from(query(directory, &["rev-parse", "--show-toplevel"], stop)?);
        let head = output(&root, &["rev-parse", "--verify", "--quiet", "HEAD"], stop)?;
        let head = if head.code == 1 {
            None
        } else {
            head.check("read HEAD")?;
            Some(String::from_utf8(head.stdout)?.trim().to_owned())
        };
        let mut summary = Self {
            remote: query(
                &root,
                &[
                    "remote",
                    "get-url",
                    "--",
                    if remote.is_empty() { "origin" } else { remote },
                ],
                stop,
            )
            .ok()
            .and_then(|url| Remote::parse(&url)),
            branch: query(&root, &["symbolic-ref", "--quiet", "--short", "HEAD"], stop)
                .unwrap_or_default(),
            shallow: query(&root, &["rev-parse", "--is-shallow-repository"], stop)? == "true",
            branches: query(
                &root,
                &["for-each-ref", "--format=%(refname)", "refs/heads"],
                stop,
            )?
            .lines()
            .count(),
            tags: query(
                &root,
                &["for-each-ref", "--format=%(refname)", "refs/tags"],
                stop,
            )?
            .lines()
            .count(),
            root,
            commits: 0,
            contributors: Vec::new(),
            latest: None,
            files: 0,
            lines: Vec::new(),
            skipped: 0,
        };
        if let Some(head) = head {
            summary.commits =
                query(&summary.root, &["rev-list", "--count", &head, "--"], stop)?.parse()?;
            // Uppercase author placeholders honor .mailmap: https://git-scm.com/docs/pretty-formats
            let log = query(
                &summary.root,
                &["log", "-1", "--format=%H%x00%aN%x00%cI%x00%s", &head, "--"],
                stop,
            )?;
            let fields: Vec<_> = log.splitn(4, '\0').collect();
            if let [id, author, time, subject] = fields.as_slice() {
                summary.latest = Some(Commit {
                    id: (*id).into(),
                    author: (*author).into(),
                    time: (*time).into(),
                    subject: (*subject).into(),
                });
            }
            // Git groups mailmapped author identities, including merge commits.
            // https://git-scm.com/docs/git-shortlog
            summary.contributors = query(&summary.root, &["shortlog", "-sne", &head, "--"], stop)?
                .lines()
                .filter_map(|line| {
                    let (count, author) = line.trim().split_once('\t')?;
                    Some((author.to_owned(), count.parse().ok()?))
                })
                .collect();
            summary.count_lines(&head, stop)?;
        }
        Ok(summary)
    }

    fn count_lines(&mut self, head: &str, stop: &StopToken) -> Result<()> {
        // Compare the empty tree with the captured HEAD. Git handles
        // binary detection and attributes; external diff/text conversion is disabled.
        // https://git-scm.com/docs/git-diff#Documentation/git-diff.txt---numstat
        let empty_tree = query(&self.root, &["hash-object", "-t", "tree", "--stdin"], stop)?;
        let files = query(
            &self.root,
            &["ls-tree", "-r", "--format=%(objecttype)", head],
            stop,
        )?;
        self.files = files.lines().filter(|kind| *kind == "blob").count();
        let diff = query(
            &self.root,
            &[
                "diff",
                "--numstat",
                "--no-renames",
                "--no-ext-diff",
                "--no-textconv",
                "-z",
                "--ignore-submodules=all",
                &empty_tree,
                head,
                "--",
            ],
            stop,
        )?;
        let mut counts = BTreeMap::<String, Lines>::new();
        for record in diff.split('\0').filter(|record| !record.is_empty()) {
            let mut fields = record.splitn(3, '\t');
            let added = fields.next().context("Missing line count")?;
            let _removed = fields.next().context("Missing line count")?;
            let name = fields.next().context("Missing file name")?;
            let Ok(text) = added.parse::<usize>() else {
                self.skipped += 1;
                continue;
            };
            let file_type = file_type(name);
            let lines = counts.entry(file_type.clone()).or_insert_with(|| Lines {
                file_type,
                ..Lines::default()
            });
            lines.files += 1;
            lines.text = lines.text.saturating_add(text);
        }
        self.lines = counts.into_values().collect();
        self.lines
            .sort_by(|a, b| b.text.cmp(&a.text).then(a.file_type.cmp(&b.file_type)));
        Ok(())
    }
}

// ===== impl Remote =====

impl Remote {
    /// Converts common clone URLs into browser links without credentials.
    pub fn parse(value: &str) -> Option<Self> {
        let (address, ssh) = if let Some(address) = value
            .strip_prefix("https://")
            .or_else(|| value.strip_prefix("http://"))
        {
            (address.to_owned(), false)
        } else if let Some(address) = value
            .strip_prefix("ssh://")
            .or_else(|| value.strip_prefix("git://"))
        {
            (address.to_owned(), true)
        } else {
            let (host, path) = value.split_once(':')?;
            if !host.contains('@') || host.contains(['/', '\\']) {
                return None;
            }
            (format!("{host}/{path}"), true)
        };
        let (authority, path) = address.split_once('/')?;
        let authority = authority.rsplit('@').next()?;
        let uri: ureq::http::Uri = format!("https://{authority}/").parse().ok()?;
        let host = uri.host()?;
        let path = path
            .split(['?', '#'])
            .next()?
            .trim_end_matches('/')
            .strip_suffix(".git")
            .unwrap_or(path.split(['?', '#']).next()?.trim_end_matches('/'));
        if path.is_empty()
            || path.split('/').any(|part| {
                part.is_empty()
                    || part == "."
                    || part == ".."
                    || !part
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
            })
        {
            return None;
        }
        let authority = if ssh { host } else { authority };
        let parts: Vec<_> = path.split('/').collect();
        let github = (host.eq_ignore_ascii_case("github.com")
            && (ssh || uri.port_u16().is_none_or(|port| port == 443))
            && parts.len() == 2)
            .then(|| (parts[0].to_owned(), parts[1].to_owned()));
        Some(Self {
            url: format!("https://{authority}/{path}"),
            github,
        })
    }
}

fn file_type(name: &str) -> String {
    let extension = Path::new(name)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match extension.as_str() {
        "rs" => "Rust",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "ts" | "tsx" => "TypeScript",
        "py" => "Python",
        "go" => "Go",
        "c" | "h" => "C",
        "cc" | "cpp" | "hpp" => "C++",
        "cs" => "C#",
        "java" => "Java",
        "rb" => "Ruby",
        "sh" | "bash" => "Shell",
        "ps1" => "PowerShell",
        "md" | "mdx" => "Markdown",
        "toml" => "TOML",
        "yaml" | "yml" => "YAML",
        "json" => "JSON",
        "html" => "HTML",
        "css" | "scss" => "CSS",
        "" => "Other",
        _ => return format!(".{extension}"),
    }
    .into()
}

fn query(root: &Path, args: &[&str], stop: &StopToken) -> Result<String> {
    let output = output(root, args, stop)?;
    output.check("repository information")?;
    Ok(String::from_utf8(output.stdout)?
        .trim_end_matches(['\r', '\n'])
        .to_owned())
}

fn output(root: &Path, args: &[&str], stop: &StopToken) -> Result<git::Output> {
    ensure!(!stop.is_stopped(), "Repository scan cancelled");
    let mut command = git::base_command();
    // Read-only commands must not refresh the index or lazily fetch missing objects.
    // https://git-scm.com/docs/git#Documentation/git.txt---no-optional-locks
    command
        .current_dir(root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .args(args);
    git::execute(command, Some(&[]), Duration::from_secs(15))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::test_git::git;

    #[test]
    fn repository_counts_local_history_and_tracked_content_without_writes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        git(root, &["init", "-b", "main"]);
        git(root, &["config", "user.name", "Test Author"]);
        git(root, &["config", "user.email", "author@example.invalid"]);
        let stop = StopToken::default();
        let empty = Summary::read(root, "origin", &stop).unwrap();
        assert_eq!(empty.commits, 0);
        assert_eq!(empty.branch, "main");
        fs::write(root.join("main.rs"), "// note\nfn main() {}\n\n").unwrap();
        fs::write(root.join("binary.rs"), b"\0binary").unwrap();
        git(root, &["add", "."]);
        let commit = crate::test_git::command(root)
            .args(["commit", "-m", "First"])
            .env("GIT_AUTHOR_DATE", "2026-09-27T05:52:34+08:00")
            .env("GIT_COMMITTER_DATE", "2026-09-27T18:38:08+08:00")
            .output()
            .unwrap();
        assert!(commit.status.success());
        git(
            root,
            &[
                "remote",
                "add",
                "origin",
                "git@github.com:example/repository.git",
            ],
        );
        git(root, &["tag", "v1"]);
        git(root, &["branch", "other"]);
        fs::write(
            root.join("main.rs"),
            "// note\nfn main() {}\n\nfn second() {}\n",
        )
        .unwrap();
        fs::write(root.join("untracked.rs"), "fn ignored() {}\n").unwrap();
        let before = query(root, &["status", "--porcelain"], &stop).unwrap();
        let summary = Summary::read(root, "origin", &stop).unwrap();
        assert_eq!(summary.commits, 1);
        assert_eq!(summary.branches, 2);
        assert_eq!(summary.tags, 1);
        assert_eq!(summary.files, 2);
        assert_eq!(summary.skipped, 1);
        assert_eq!(summary.contributors.len(), 1);
        assert_eq!(summary.contributors[0].1, 1);
        let latest = summary.latest.unwrap();
        assert_eq!(latest.subject, "First");
        assert_eq!(latest.time, "2026-09-27T18:38:08+08:00");
        assert_eq!(
            summary.remote.unwrap().url,
            "https://github.com/example/repository"
        );
        assert_eq!(summary.lines[0].text, 3);
        assert_eq!(
            before,
            query(root, &["status", "--porcelain"], &stop).unwrap()
        );
        git(root, &["checkout", "--detach"]);
        assert!(
            Summary::read(root, "origin", &stop)
                .unwrap()
                .branch
                .is_empty()
        );
        stop.stop();
        assert!(Summary::read(root, "origin", &stop).is_err());
    }

    #[test]
    fn remote_links_remove_credentials_and_reject_local_or_executable_urls() {
        for input in [
            "git@github.com:owner/repo.git",
            "ssh://git@github.com/owner/repo.git",
            "https://user:secret@github.com/owner/repo.git?token=secret#part",
        ] {
            let remote = Remote::parse(input).unwrap();
            assert_eq!(remote.url, "https://github.com/owner/repo");
            assert_eq!(remote.github, Some(("owner".into(), "repo".into())));
        }
        assert_eq!(
            Remote::parse("https://git.example:8443/team/repo.git")
                .unwrap()
                .url,
            "https://git.example:8443/team/repo"
        );
        for input in [
            "file:///tmp/repo",
            "ext::evil",
            "javascript:alert(1)",
            "E:\\repo",
            "/tmp/repo",
        ] {
            assert!(Remote::parse(input).is_none(), "{input}");
        }
        assert!(
            Remote::parse("https://github.com.evil.test/owner/repo")
                .unwrap()
                .github
                .is_none()
        );
    }
}
