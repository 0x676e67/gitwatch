# gitwatch

gitwatch watches a file or directory and commits your changes to Git as you work. It's a Rust implementation inspired by [gitwatch/gitwatch](https://github.com/gitwatch/gitwatch), with a CLI, a terminal UI and a desktop app for Windows, macOS and Linux.

You can use it to keep a history of your notes, back up files from several projects, or pull updates into a local repository on a schedule.

- `watch` commits in the repository you're watching and can push to a remote.
- `workspace` copies selected files into a separate backup repository, with one branch per workspace. It doesn't stage or commit anything in your source projects.
- `pull` keeps a local checkout up to date, cloning it first if needed.

## Install and run

Install Git and Rust 1.96 or newer, then choose the interfaces you need:

```sh
cargo install gitwatch --locked                     # CLI
cargo install gitwatch --locked --features tui      # CLI + terminal interface
cargo install gitwatch --locked --all-features      # CLI + TUI + desktop
```

To build from source, clone this repository and replace `gitwatch` in the install command with `--path .`.

Then run `gitwatch --help`, or open an interface you included in the build:

```sh
gitwatch --help
gitwatch tui
gitwatch desktop
```

On Linux, building the desktop app also needs the development packages for X11/Wayland, OpenGL and xkbcommon. See the Ubuntu package list in the [CI workflow](.github/workflows/ci.yml). The CLI build doesn't need these desktop dependencies.

In the desktop source build, closing or minimizing the window keeps the app and its running tasks in the system tray. Click the tray icon to reopen the window, or right-click it and choose **Quit** to stop tasks and exit. Under **Settings**, enable **Start minimized to tray** to hide the window on future launches. This setting is off by default and does not start tasks automatically.

On Linux, the tray uses D-Bus StatusNotifierItem without GTK or AppIndicator libraries. Your desktop needs a compatible tray host; GNOME may need a tray extension. If a tray cannot be created, gitwatch shows the window and closing it exits normally. Tray support will be included in the next release; v0.1.0 does not include it.

## Update or uninstall

The commands below are available in the source build and the next release; v0.1.0 does not include them.

```sh
gitwatch self update --check
gitwatch self update
gitwatch self update --version 0.2.0
gitwatch self uninstall
```

For an installation extracted from an official [GitHub Release](https://github.com/0x676e67/gitwatch/releases), keep `gitwatch-install.json` beside the programs. The updater verifies the download, checks that the new programs can start, then replaces the installed CLI and desktop together. A standalone CLI can also update itself. Updating uses the official build, including TUI support; it does not preserve custom Cargo feature selections or add a desktop program to a CLI-only installation. Older release folders without an installation receipt need a fresh download first.

Stop other gitwatch processes before updating or uninstalling. For the desktop app, choose **Quit** in the tray menu; closing its window leaves it running. Both commands list the affected programs and ask for confirmation; use `--yes` in scripts. Uninstall keeps your settings, backup history, recovery data and license notices. For Cargo installations, use `cargo install gitwatch --locked --force` with your original feature options, or `cargo uninstall gitwatch`. Other package-managed installations should use their package manager.

If an update is interrupted, run `gitwatch self update --recover`. The `.gitwatch-recovery` folder beside the executable holds the previous programs until the operation finishes. If the CLI itself is missing, copy its saved binary back to the installation directory, then run recovery. Updates never downgrade automatically.

The desktop and TUI show a notice when a newer stable version is available. Interactive CLI sessions print the update command to stderr. Checks run in the background, are cached for a day and never install anything automatically. Network failures leave your tasks running. Set `GITWATCH_NO_UPDATE_CHECK=1` to disable automatic checks; `self update --check` still works. JSON and redirected CLI output stay free of automatic notices.

The desktop uses one built-in dark theme with consistent spacing and task status colors.

The task list stays visible while you add tasks or change backup settings. Select a task to return to its details. Running pull tasks show the time remaining until their next attempt; a failed attempt waits the same interval before retrying.

Task details include a repository link, its GitHub owner avatar when available, local branches and tags, commit and contributor counts, and the latest commit. History counts cover the local HEAD, including merges; shallow clones show a warning. Git counts text lines in committed files at the same local HEAD, including comments and blank lines. The breakdown groups files by extension; it does not parse programming languages. Binary files are counted separately. Information refreshes in the background while the details are open, or when you click **Refresh information**. Avatar downloads are optional to the display: offline repositories still show their local statistics. These desktop additions are available in the source build and the next release.

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

## Language

Source builds support English and Simplified Chinese. This is not yet included in the v0.1.0 release.

```sh
gitwatch --lang en --help
gitwatch --lang zh-CN tui
gitwatch --lang zh-CN desktop
```

Use the language menu in the desktop app or press `F3` in the TUI to switch languages and save your choice. `--lang` takes priority over `GITWATCH_LANG`, your saved choice and the system language, in that order. Other system languages fall back to English.

Command names, option names and JSON output stay the same in both languages. File contents, paths and messages from Git are kept as they are. The desktop app includes a Chinese font; the TUI uses your terminal's font, which needs Chinese character support.

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

The code is licensed under Apache-2.0. The bundled desktop font is licensed under [OFL-1.1](assets/fonts/OFL.txt); see its [notice](assets/fonts/NOTICE). This is an independent Rust implementation inspired by gitwatch; upstream shell code is not included.
