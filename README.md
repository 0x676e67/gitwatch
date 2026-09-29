# gitwatch

[![CI](https://github.com/0x676e67/gitwatch/actions/workflows/ci.yml/badge.svg)](https://github.com/0x676e67/gitwatch/actions/workflows/ci.yml)
![Crates.io Version](https://img.shields.io/crates/v/gitwatch)

gitwatch is a desktop app for keeping a history of your work with Git. Watch a repository for changes, back up selected files to a separate repository, or pull updates on a schedule. Enable two-way sync to share backed-up files between computers, with a preview before the first sync and controls for resolving conflicts.

Available on Windows, macOS and Linux, in English and Simplified Chinese.

## Install

Install [Git](https://git-scm.com/downloads), then download an archive from [GitHub Releases](https://github.com/0x676e67/gitwatch/releases). Extract the whole archive and open `gitwatch` (`gitwatch.exe` on Windows).

With Rust 1.96 or newer, you can also install from crates.io:

```sh
cargo install gitwatch --locked
gitwatch
```

See the [installation guide](https://gitwatch.dpdns.org/en/install.html) for Linux dependencies and source builds. Upgrading from 0.3.x? [Reinstall with Cargo or download a fresh archive](https://gitwatch.dpdns.org/en/upgrade.html); the old updater cannot install the new single-program package. CLI and TUI interfaces have been removed.

## Get started

Choose **Add task**, select what you want to do, and pick a local directory. Tasks stay paused until you start them. File backups keep their history in a separate repository; direct watching commits to the source repository itself.

To bring a backup to another computer, create a workspace with the same backup remote and choose **Restore remote backup**. Review the files and confirm the restore. You can then enable **Two-way sync** for ongoing updates.

[English documentation](https://gitwatch.dpdns.org/en/index.html) · [简体中文文档](https://gitwatch.dpdns.org/zh-CN/index.html)

## License

The code is licensed under [Apache-2.0](./LICENSE). Bundled fonts are covered by [OFL-1.1](./src/fonts/OFL.txt); see the [font notices](./src/fonts/NOTICE).

## Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you shall be licensed under Apache-2.0, without any additional terms or conditions.
