# gitwatch

Cross-platform Git watching, workspace backups and scheduled repository updates, written in Rust.

One package provides a CLI, an optional Ratatui terminal interface and an optional egui desktop application. Git must be installed separately.

- **Watch:** automatically commit changes in an existing repository, with optional upload.
- **Workspace backup:** save selected project files to separate branches of one dedicated backup repository. Source repositories are never staged or committed by this mode.
- **Scheduled pull:** clone a repository if needed, then update it at a fixed interval. Uncommitted changes and divergent history are preserved for manual resolution.

## Build and run

The first release is in development and has not been published to crates.io. Rust 1.96 or newer is required.

```sh
cargo install --path . --locked                     # CLI
cargo install --path . --locked --features tui      # CLI + terminal interface
cargo install --path . --locked --all-features      # CLI + TUI + desktop

gitwatch --help
gitwatch tui
gitwatch desktop
```

Desktop builds on Linux need development packages for X11/Wayland, OpenGL and xkbcommon. The CI workflow lists the Ubuntu packages. Default CLI builds do not require a desktop environment.

## Watch an existing repository

```sh
gitwatch ./notes
gitwatch -s 2 -r origin -b main ./notes
gitwatch -f -m "Notes: %d" ./notes
gitwatch watch ./notes --once
```

The positional interface follows [gitwatch/gitwatch](https://github.com/gitwatch/gitwatch). The target must belong to an existing Git worktree. Watching commits only the selected file or directory, respects Git ignore rules, and records deletions. Existing staged changes within the target require attention; staged changes outside it are preserved.

Native events are backed by content polling. Use `--polling` for filesystems with unreliable notifications. `-f` commits at startup; otherwise startup changes form the initial baseline. `-R` explicitly enables pull/rebase before upload. `-x` filters detected events, **not commit contents**.

Unlike the original script, failed operations return nonzero exit codes, `-h` and `-C` do not consume an extra argument, and active Git operations are always deferred. `-c` executes a parsed command directly; invoke a shell explicitly when you need shell syntax.

## Back up selected workspace files

```sh
gitwatch workspace add ./wreq --name wreq --include AGENTS.md --include .agents
gitwatch workspace add ./btls --name btls --include AGENTS.md --include .agents
gitwatch workspace list
gitwatch workspace backup wreq
gitwatch workspace remote https://github.com/you/private-backup.git --auto-push
gitwatch workspace watch --all
```

Each workspace has a stable UUID and an independent branch, defaulting to `workspaces/<uuid>`. A branch contains `manifest.json` and `files/<project-relative-path>`. Local paths stay in the local configuration. Ignored project files can be explicitly selected; Git metadata and known credential/cache paths are excluded.

Missing source files retain their previous backup, listed as retained in backup results. An unavailable source directory is an error. Backups are limited to 16 MiB per file, 128 MiB of scanned content and 10,000 files. Symlinks and Windows reparse points are rejected in workspace selections. Extra files are never deleted during restore.

To use a backup on another machine:

```sh
gitwatch workspace remote https://github.com/you/private-backup.git
gitwatch workspace fetch
gitwatch workspace import workspaces/<uuid> ./local-project
gitwatch workspace history <uuid>
gitwatch workspace restore <uuid> --preview
gitwatch workspace restore <uuid> --plan <preview-uuid> --show AGENTS.md
gitwatch workspace restore <uuid> --plan <preview-uuid> --confirm
```

Import creates a paused binding. Restore previews expire after 24 hours and are rejected if destination contents changed. Files being replaced are copied to the local `recovery/<preview-uuid>/files/` directory before the first source write. Multi-file restores can partially fail; the report lists completed files and the recovery location.

Uploads never force-push. A rejected upload leaves the local backup intact. Fetch only reads remote history; it does not overwrite project files. Remote history retrieval is currently explicit. Conflicting histories require manual resolution.

## Update a repository periodically

```sh
gitwatch pull ./upstream --url https://github.com/gitwatch/gitwatch.git --every 3600
gitwatch pull ./existing-repo --remote origin --once
```

The first command clones into an absent or empty destination, then updates hourly. Subsequent updates use `pull --ff-only --no-rebase`. A task refuses dirty worktrees, branch switches, in-progress merges and non-fast-forward histories. It never stashes, resets or force-updates local work.

## Interactive interfaces

The TUI and desktop share the same background controller and workspace store. Adding a task saves its settings; starting it is a separate action. Closing its owning interface stops the task at a safe operation boundary. There is no installed daemon or system tray integration in this version.

The desktop provides task forms, native file/folder pickers, workspace import, history, diffs and restore confirmation. In the TUI, `n` adds a task, `F2` selects its mode, `Tab` changes fields and `Ctrl+S` saves. `s` starts/stops, `b` runs once, `h` opens history, `v` prepares a restore, and `R` opens its confirmation. `q` or `Ctrl+C` exits. The footer shows available actions.

Use `--data-dir <directory>` or `GITWATCH_DATA_DIR` to select a store. The default is the platform's local application data directory. Keep this directory separate from source projects. CLI output supports `--json` with `schema_version: 1`.

## Development

```sh
cargo +nightly fmt --all
cargo test --all-features
cargo clippy --all-features --all-targets -- -D warnings
```

CI verifies default, TUI, desktop and combined builds on Windows, macOS and Linux. Tests use temporary repositories and local remotes. Tag builds produce CLI/desktop archives and a draft GitHub release; they do not publish to crates.io. Native installers, signing/notarization, tray support and automatic conflict resolution remain outside this first version.

Licensed under Apache-2.0. This is an independent Rust implementation inspired by gitwatch; upstream shell code is not included.
