# gitwatch

[![CI](https://github.com/0x676e67/gitwatch/actions/workflows/ci.yml/badge.svg)](https://github.com/0x676e67/gitwatch/actions/workflows/ci.yml)
![Crates.io Version](https://img.shields.io/crates/v/gitwatch)

gitwatch watches your files and commits changes to Git as you work. It can also back up selected files to a separate repository or pull updates on a schedule.

## Install

Install Git and Rust 1.96 or newer, then choose the interfaces you need:

```sh
cargo install gitwatch --locked                     # CLI
cargo install gitwatch --locked --features tui      # CLI + TUI
cargo install gitwatch --locked --all-features      # CLI + TUI + desktop
```

Prebuilt archives are available on [GitHub Releases](https://github.com/0x676e67/gitwatch/releases). See [installation](https://gitwatch.dpdns.org/en/install.html) for source builds and Linux desktop dependencies.

## Quick start

Watch a directory inside an existing Git repository:

```sh
gitwatch ./notes
```

Edit a file and gitwatch commits it after the changes settle. Add `-f` to commit existing changes at startup, or `-r origin -b main` to push commits as well.

To keep your source project's Git history untouched, use a workspace backup:

```sh
gitwatch workspace add ./project --name project --include notes.md
gitwatch workspace backup project
```

Prefer an interactive interface? Run `gitwatch tui` or `gitwatch desktop` after installing the corresponding features.

Use the desktop workspace menu or press **F4** in the TUI to keep separate task lists. Your existing tasks stay in the default local workspace. New workspaces require a backup remote; switching workspaces leaves started tasks running in the background.

## Documentation

- [Desktop app](https://gitwatch.dpdns.org/en/desktop.html) and [terminal UI](https://gitwatch.dpdns.org/en/tui.html)
- [Watching repositories](https://gitwatch.dpdns.org/en/watch.html), [workspace backups and restore](https://gitwatch.dpdns.org/en/backup.html), [scheduled pulls](https://gitwatch.dpdns.org/en/pull.html)
- [Language, local data and updates](https://gitwatch.dpdns.org/en/settings.html)
- [Building and contributing](https://gitwatch.dpdns.org/en/development.html)

## License

Licensed under either of Apache License, Version 2.0 ([LICENSE](./LICENSE) or http://www.apache.org/licenses/LICENSE-2.0).

The bundled desktop font uses [OFL-1.1](src/fonts/OFL.txt); see its [notice](src/fonts/NOTICE).

## Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the [Apache-2.0](./LICENSE) license, shall be licensed as above, without any additional terms or conditions.
