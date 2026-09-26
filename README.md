# gitwatch

gitwatch watches a file or directory and commits your changes to Git as you work. It's a Rust implementation inspired by [gitwatch/gitwatch](https://github.com/gitwatch/gitwatch), with a CLI, a terminal UI and a desktop app for Windows, macOS and Linux.

You can use it to keep a history of your notes, back up files from several projects, or pull updates into a local repository on a schedule.

- `watch` commits in the repository you're watching and can push to a remote.
- `workspace` copies selected files into a separate backup repository, with one branch per workspace. It doesn't stage or commit anything in your source projects.
- `pull` keeps a local checkout up to date, cloning it first if needed.

## Build and run

The first release isn't on crates.io yet. You'll need Git and Rust 1.96 or newer to install from source:

```sh
git clone https://github.com/0x676e67/gitwatch.git
cd gitwatch
```

Choose the interfaces you need:

```sh
cargo install --path . --locked                     # CLI
cargo install --path . --locked --features tui      # CLI + terminal interface
cargo install --path . --locked --all-features      # CLI + TUI + desktop
```

Then run `gitwatch --help`, or open an interface you included in the build:

```sh
gitwatch --help
gitwatch tui
gitwatch desktop
```

On Linux, building the desktop app also needs the development packages for X11/Wayland, OpenGL and xkbcommon. See the Ubuntu package list in the [CI workflow](.github/workflows/ci.yml). The CLI build doesn't need these desktop dependencies.

## Watch an existing repository

Point gitwatch at a file or directory inside a Git repository:

```sh
gitwatch ./notes
gitwatch -s 2 -r origin -b main ./notes
gitwatch -f -m "Notes: %d" ./notes
gitwatch watch ./notes --once
```

The first command watches `./notes`. The second waits two seconds after changes settle, then commits and pushes to `origin` on branch `main`. Use `-f` to commit existing changes at startup, or `--once` to make a single pass and exit. `%d` in a commit message expands to the date and time.

Only the selected file or directory is committed. Git ignore rules still apply, and deletions are recorded. If you've already staged changes within that path, gitwatch waits for you to handle them. Staged changes elsewhere are left alone.

Without `-f`, existing changes become the starting baseline; a later change triggers a commit. gitwatch listens for filesystem events and also checks file contents periodically. Use `--polling` if filesystem notifications aren't reliable on your setup.

A few options to know if you're coming from the shell version:

- `-R` enables pull/rebase before pushing. It isn't enabled by default.
- `-x` filters the changes that trigger a commit; it doesn't exclude files from that commit.
- `-c` runs a command to generate the commit message. It doesn't run through a shell unless you explicitly invoke one.
- `-h` and `-C` take no argument. Failed commands return a nonzero exit code, and gitwatch waits while another Git operation is in progress.

## Back up selected workspace files

Use workspace backups when you want to save a few files from a project without adding commits to its repository. Each workspace gets a branch in the same dedicated backup repository. The source directory doesn't have to be a Git repository at all.

```sh
gitwatch workspace add ./project-a --name project-a --include notes.md --include notes
gitwatch workspace add ./project-b --name project-b --include notes.md --include notes
gitwatch workspace list
gitwatch workspace backup project-a
gitwatch workspace remote https://github.com/you/private-backup.git --auto-push
gitwatch workspace watch --all
```

This example selects `notes.md` and the `notes` directory from two projects, takes a backup of `project-a`, and starts watching both. Replace the paths and file selections with your own. You can leave out the `remote` command if you only need local backups.

Each workspace has a stable UUID. Its branch defaults to `workspaces/<uuid>` and contains a `manifest.json` alongside `files/<project-relative-path>`. Absolute paths stay in your local configuration, so you can bind the same backup to a different directory on another machine.

You can select files that the source project's Git ignore rules exclude. Git metadata and known credential or cache paths are still excluded. Workspace backups reject symlinks and Windows reparse points, and are limited to 16 MiB per file, 128 MiB of scanned content and 10,000 files.

If a selected file disappears, its last backup is kept and reported as retained. If the whole source directory is unavailable, gitwatch reports an error instead of taking a backup.

### Restore on another machine

Fetch the backup branches and bind one to a local directory:

```sh
gitwatch workspace remote https://github.com/you/private-backup.git
gitwatch workspace fetch
gitwatch workspace import workspaces/<uuid> ./local-project
gitwatch workspace history <uuid>
gitwatch workspace restore <uuid> --preview
gitwatch workspace restore <uuid> --plan <preview-uuid> --show notes.md
gitwatch workspace restore <uuid> --plan <preview-uuid> --confirm
```

Use the workspace UUID from `workspace list` and the preview UUID returned by `restore --preview`. Import leaves the workspace paused so you can inspect its history and restore files before starting a watcher.

Restore doesn't write to your project until you pass `--confirm`. A preview expires after 24 hours; if destination contents have changed, create a new preview. Before replacing files, gitwatch copies them to `recovery/<preview-uuid>/files/` in the local store. Extra files in the destination are left alone. If a restore stops partway through, its report lists the files written and where to find the recovery copies.

Run `workspace fetch` when you want to retrieve remote history. Fetching doesn't restore files, and pushing never uses force. If a push is rejected, your local backup stays intact; conflicting histories need to be resolved manually.

## Update a repository periodically

```sh
gitwatch pull ./upstream --url https://github.com/gitwatch/gitwatch.git --every 3600
gitwatch pull ./existing-repo --remote origin --once
```

The first command clones into an absent or empty directory, then checks for updates every hour. The second updates an existing checkout once and exits.

Updates use `pull --ff-only --no-rebase`. If you have uncommitted changes, switch branches, or need to resolve a merge or diverged history, gitwatch leaves that work for you to handle. It won't stash, reset or force-update the checkout.

## Interactive interfaces

If you'd rather manage tasks interactively, use `gitwatch tui` or `gitwatch desktop`. Both use the same workspace store as the CLI. Adding a task saves its settings; start it when you're ready. Closing the interface stops its tasks once any operation already in progress finishes. This version doesn't install a daemon or run in the system tray.

In the desktop app, you can choose files and folders, configure tasks, import workspace branches and browse backup history. You can also review file changes before confirming a restore.

Common TUI shortcuts:

| Key | Action |
| --- | --- |
| `n` | Add a task |
| `F2` | Select the task mode |
| `Tab` / `Ctrl+S` | Move between fields / save |
| `s` / `b` | Start or stop / run once |
| `h` | Open history |
| `v` / `R` | Preview a restore / open confirmation |
| `q` / `Ctrl+C` | Exit |

The footer shows the actions available in the current view.

## Local data

gitwatch keeps its store in your system's local application data directory. Set `--data-dir <directory>` or `GITWATCH_DATA_DIR` to use another location, separate from the projects you're backing up.

For scripts, `--json` produces structured CLI output with `schema_version: 1`.

## Development

```sh
cargo +nightly fmt --all
cargo test --all-features
cargo clippy --all-features --all-targets -- -D warnings
```

CI checks CLI, TUI, desktop and combined builds on Windows, macOS and Linux. Tests use temporary repositories and local remotes. The terminal UI uses Ratatui; the desktop uses egui/eframe, with the same Rust core behind both.

The Release workflow builds versioned archives for Linux x86-64, Windows x86-64 and macOS Apple Silicon, with both binaries and SHA-256 checksums. Git must be installed separately. Native installers, signing, notarization and automatic conflict resolution aren't included in this first version.

Pushing a `v<version>` tag publishes a GitHub Release after the checks pass. The tag must match `Cargo.toml` and point to a commit on `main`. Crates.io uploads use [Trusted Publishing](https://crates.io/docs/trusted-publishing) through `release.yml` and the `crates-io` environment; an already published version is skipped. The first crate version needs to be published locally before configuring that trust.

To try the release build without publishing, run **Release** from the Actions tab with `publish` disabled. To publish an existing tag manually, select that tag and enable `publish`. Leave `publish_crate` disabled if you only want the GitHub Release.

Licensed under Apache-2.0. This is an independent Rust implementation inspired by gitwatch; upstream shell code is not included.
