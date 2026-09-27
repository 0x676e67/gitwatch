use std::{path::PathBuf, time::Duration};

use anyhow::{Context, ensure};
use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};
use gitwatch::{
    Result,
    i18n::Language,
    watch::{self, Event, MonitorOptions, Repository, StopToken, WatchOptions},
    workspace::{BackupStore, UploadState, Workspace},
};
use serde::Serialize;
use uuid::Uuid;

#[derive(Parser)]
#[command(
    name = "gitwatch",
    version,
    about = "Watch Git repositories or back up projects to workspace branches",
    subcommand_negates_reqs = true
)]
struct Cli {
    /// Language (en or zh-CN).
    #[arg(long, global = true, env = "GITWATCH_LANG")]
    lang: Option<String>,
    #[command(flatten)]
    watch: DirectArgs,
    #[command(subcommand)]
    command: Option<Action>,
    /// Store directory for workspace backups and local bindings.
    #[arg(long, global = true, env = "GITWATCH_DATA_DIR")]
    data_dir: Option<PathBuf>,
    /// Emit versioned JSON records on stdout.
    #[arg(long, global = true)]
    json: bool,
    /// Show pending events and detailed status.
    #[arg(short = 'v', long, global = true)]
    verbose: bool,
}

#[derive(Args, Default)]
struct DirectArgs {
    /// Existing file or directory within a Git repository.
    target: Option<PathBuf>,
    #[arg(short = 's', long = "delay", default_value_t = 2.0)]
    delay: f64,
    #[arg(short = 'm', long = "message")]
    message: Option<String>,
    #[arg(short = 'd', long = "date-format")]
    date_format: Option<String>,
    #[arg(short = 'r', short_alias = 'p', long = "remote")]
    remote: Option<String>,
    #[arg(short = 'b', long = "branch", requires = "remote")]
    branch: Option<String>,
    #[arg(short = 'R', long = "pull-rebase", requires = "remote")]
    pull_rebase: bool,
    #[arg(short = 'g', long = "git-dir")]
    git_dir: Option<PathBuf>,
    /// Filter change events with a regex; this does not exclude files from commits.
    #[arg(short = 'x', long = "exclude")]
    exclude: Option<String>,
    /// Portable native-event filter: create,modify,delete,move,move_self,close_write.
    #[arg(short = 'e', long = "events", value_delimiter = ',')]
    events: Vec<String>,
    #[arg(short = 'f', long = "commit-on-start")]
    commit_on_start: bool,
    /// Compatibility flag; active Git operations are always deferred safely.
    #[arg(short = 'M', long = "skip-if-merging")]
    skip_if_merging: bool,
    #[arg(short = 'l', long = "diff-lines", conflicts_with = "plain_diff_lines")]
    diff_lines: Option<usize>,
    #[arg(short = 'L', long = "plain-diff-lines")]
    plain_diff_lines: Option<usize>,
    #[arg(short = 'c', long = "message-command")]
    message_command: Option<String>,
    #[arg(short = 'C', long = "pass-files", requires = "message_command")]
    pass_files: bool,
    /// Commit once and exit instead of starting a watch loop.
    #[arg(long)]
    once: bool,
    #[arg(long, default_value_t = 5.0)]
    poll_interval: f64,
    #[arg(long, default_value_t = 60.0)]
    max_wait: f64,
    /// Use content polling without native filesystem events.
    #[arg(long)]
    polling: bool,
}

#[derive(Subcommand)]
enum Action {
    /// Update or uninstall this gitwatch installation.
    #[command(name = "self")]
    Manage {
        #[command(subcommand)]
        command: SelfAction,
    },
    /// Clone or periodically update a local repository.
    Pull {
        path: PathBuf,
        #[arg(long)]
        url: Option<String>,
        #[arg(long, default_value = "origin")]
        remote: String,
        #[arg(short = 'b', long)]
        branch: Option<String>,
        #[arg(long, default_value_t = 3600.0)]
        every: f64,
        /// How to integrate remote history: ff-only, merge or rebase.
        #[arg(
            long,
            value_enum,
            default_value = "ff-only",
            hide_possible_values = true
        )]
        strategy: gitwatch::pull::PullStrategy,
        #[arg(long)]
        once: bool,
    },
    /// Explicit form of the legacy positional watch command.
    Watch(DirectArgs),
    /// Manage projects backed up on independent branches of one repository.
    Workspace {
        #[command(subcommand)]
        command: WorkspaceAction,
    },
    /// Open the terminal interface (requires the tui feature).
    Tui,
    /// Open the desktop interface (requires the desktop feature).
    Desktop,
}

#[derive(Subcommand)]
enum SelfAction {
    /// Download and install the latest stable GitHub release.
    Update {
        /// Check for a newer version without changing the installation.
        #[arg(long, conflicts_with = "recover")]
        check: bool,
        /// Select a stable release version.
        #[arg(long, conflicts_with = "recover")]
        version: Option<String>,
        /// Restore an installation interrupted during replacement.
        #[arg(long)]
        recover: bool,
        /// Confirm replacement with the official release build.
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Remove program binaries while keeping settings and backups.
    Uninstall {
        /// Confirm removal of the listed program files.
        #[arg(long, short = 'y')]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum WorkspaceAction {
    Add {
        root: PathBuf,
        #[arg(long)]
        name: String,
        #[arg(long)]
        branch: Option<String>,
        #[arg(long = "include", required = true)]
        includes: Vec<String>,
        #[arg(long = "exclude")]
        excludes: Vec<String>,
        /// Back up symbolic-link target contents, including outside the source directory.
        #[arg(long)]
        follow_links: bool,
    },
    List,
    Edit {
        workspace: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(long = "include")]
        includes: Vec<String>,
        #[arg(long = "exclude")]
        excludes: Vec<String>,
        #[arg(long)]
        clear_excludes: bool,
        /// Enable or disable reading symbolic-link targets during backup.
        #[arg(long, action = clap::ArgAction::Set)]
        follow_links: Option<bool>,
        #[arg(long, conflicts_with = "resume")]
        pause: bool,
        #[arg(long)]
        resume: bool,
    },
    Bind {
        workspace: String,
        root: PathBuf,
    },
    /// Remove a local binding while retaining its backup branch.
    Remove {
        workspace: String,
    },
    Backup {
        workspace: String,
        #[arg(long)]
        push: bool,
    },
    Status {
        workspace: String,
    },
    Push {
        workspace: String,
    },
    /// Configure a remote. Omitting the URL displays whether one is configured.
    Remote {
        url: Option<String>,
        #[arg(long)]
        auto_push: bool,
        #[arg(long, conflicts_with = "url")]
        clear: bool,
    },
    Fetch,
    Branches,
    Import {
        branch: String,
        root: PathBuf,
    },
    History {
        workspace: String,
        #[arg(long)]
        revision: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    Diff {
        workspace: String,
        from: String,
        to: String,
    },
    Restore {
        workspace: String,
        #[arg(long, conflicts_with = "plan")]
        revision: Option<String>,
        #[arg(long)]
        plan: Option<Uuid>,
        #[arg(long, requires = "plan")]
        confirm: bool,
        #[arg(long)]
        preview: bool,
        #[arg(long = "file")]
        files: Vec<String>,
        #[arg(long, requires = "plan", conflicts_with = "confirm")]
        show: Option<String>,
    },
    Watch {
        workspace: Option<String>,
        #[arg(long, conflicts_with = "workspace")]
        all: bool,
        #[arg(long, default_value_t = 2.0)]
        delay: f64,
        #[arg(long, default_value_t = 5.0)]
        poll_interval: f64,
        #[arg(long)]
        no_initial: bool,
        #[arg(long)]
        polling: bool,
    },
}

pub fn run() -> Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
    let language = Language::from_args(&args)?;
    run_localized(args, language)
        .map_err(|error| anyhow::anyhow!(language.error(&format!("{error:#}"))))
}

fn run_localized(args: Vec<std::ffi::OsString>, language: Language) -> Result<()> {
    let matches = match language.command(Cli::command()).try_get_matches_from(args) {
        Ok(matches) => matches,
        Err(error) if error.use_stderr() => {
            eprint!("{}", language.argument_error(&error));
            std::process::exit(error.exit_code());
        }
        Err(error) => {
            error.print()?;
            return Ok(());
        }
    };
    let cli = Cli::from_arg_matches(&matches)?;
    if cli.command.is_some() {
        for id in [
            "target",
            "delay",
            "message",
            "date_format",
            "remote",
            "branch",
            "pull_rebase",
            "git_dir",
            "exclude",
            "events",
            "commit_on_start",
            "skip_if_merging",
            "diff_lines",
            "plain_diff_lines",
            "message_command",
            "pass_files",
            "once",
            "poll_interval",
            "max_wait",
            "polling",
        ] {
            ensure!(
                matches.value_source(id) != Some(clap::parser::ValueSource::CommandLine),
                "Direct watch arguments must follow the watch subcommand"
            );
        }
    }
    let _running = if matches!(cli.command, Some(Action::Manage { .. })) {
        None
    } else {
        Some(gitwatch::update::Running::acquire()?)
    };
    let _notice = if !cli.json
        && !matches!(
            cli.command,
            Some(Action::Manage { .. } | Action::Tui | Action::Desktop)
        ) {
        cli_notifications(cli.data_dir.clone(), language)
    } else {
        None
    };
    match cli.command {
        Some(Action::Manage { command }) => self_command(command, cli.json, language),
        Some(Action::Pull {
            path,
            url,
            remote,
            branch,
            every,
            strategy,
            once,
        }) => {
            let mut options = gitwatch::pull::PullOptions::new(path)
                .remote(remote)
                .strategy(strategy)
                .interval(duration(every)?);
            if let Some(url) = url {
                options = options.url(url);
            }
            if let Some(branch) = branch {
                options = options.branch(branch);
            }
            let mut task = gitwatch::pull::PullTask::new(options)?;
            if once {
                let report = task.update()?;
                return emit(
                    &report,
                    cli.json,
                    language.format(
                        "{0} {1} at {2}",
                        &[
                            language.text(if report.cloned() {
                                "Cloned"
                            } else if report.changed() {
                                "Updated"
                            } else {
                                "Unchanged"
                            }),
                            &report.path().display().to_string(),
                            report.after(),
                        ],
                    ),
                );
            }
            task.run(signal()?, |result| match result {
                Ok(report) => {
                    let _ = emit(
                        &report,
                        cli.json,
                        language.format(
                            "{0} at {1}",
                            &[&report.path().display().to_string(), report.after()],
                        ),
                    );
                }
                Err(error) => event(
                    Event::Error(error.to_string()),
                    cli.json,
                    cli.verbose,
                    language,
                ),
            })
        }
        Some(Action::Watch(args)) => direct(args, cli.json, cli.verbose, language),
        Some(Action::Workspace { command }) => {
            let directory = cli
                .data_dir
                .map_or_else(BackupStore::default_directory, Ok)?;
            let store = BackupStore::open(directory)?;
            workspace_command(store, command, cli.json, cli.verbose, language)
        }
        Some(Action::Tui) => {
            #[cfg(feature = "tui")]
            {
                gitwatch::tui::run_with_language(cli.data_dir, language)
            }
            #[cfg(not(feature = "tui"))]
            anyhow::bail!("TUI was not compiled; install gitwatch with --features tui")
        }
        Some(Action::Desktop) => launch_desktop(cli.data_dir, language),
        None if cli.watch.target.is_some() => direct(cli.watch, cli.json, cli.verbose, language),
        None => {
            language.command(Cli::command()).print_help()?;
            println!();
            Ok(())
        }
    }
}

fn self_command(command: SelfAction, json: bool, language: Language) -> Result<()> {
    use gitwatch::update::{Installation, Release, VERSION};
    match command {
        SelfAction::Update {
            check,
            version,
            recover,
            yes,
        } => {
            if recover {
                Installation::recover()?;
                return emit(
                    &serde_json::json!({"status":"recovered"}),
                    json,
                    language.text("Installation restored.").into(),
                );
            }
            let release = Release::check(version.as_deref())?;
            if check || !release.is_newer() {
                return emit(
                    &serde_json::json!({"current":VERSION,"latest":release.version(),"available":release.is_newer(),"url":release.url()}),
                    json,
                    if release.is_newer() {
                        language.format(
                            "gitwatch {0} is available. Run gitwatch self update.",
                            &[release.version()],
                        )
                    } else {
                        language.text("gitwatch is up to date.").into()
                    },
                );
            }
            let installation = Installation::current()?;
            confirm_installation(
                &installation,
                yes,
                json,
                language,
                "Replace these programs with the official release build?",
            )?;
            if !json {
                eprintln!("{}", language.text("Downloading and verifying the update…"));
            }
            installation.update(&release)?;
            emit(
                &serde_json::json!({"status":"updated","version":release.version()}),
                json,
                language.format("Updated to gitwatch {0}.", &[release.version()]),
            )
        }
        SelfAction::Uninstall { yes } => {
            let installation = Installation::current()?;
            confirm_installation(
                &installation,
                yes,
                json,
                language,
                "Remove these programs? Settings and backups will be kept.",
            )?;
            installation.uninstall()?;
            emit(
                &serde_json::json!({"status":"uninstalled","data_retained":true}),
                json,
                language
                    .text("Uninstalled. Settings and backups were kept.")
                    .into(),
            )
        }
    }
}

fn confirm_installation(
    installation: &gitwatch::update::Installation,
    yes: bool,
    json: bool,
    language: Language,
    question: &str,
) -> Result<()> {
    use std::io::{IsTerminal, Write};
    if !json {
        for path in installation.files() {
            eprintln!("{}", path.display());
        }
    }
    if yes {
        return Ok(());
    }
    ensure!(
        !json && std::io::stdin().is_terminal(),
        "Use --yes to confirm this operation non-interactively"
    );
    eprint!("{} [y/N] ", language.text(question));
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    ensure!(
        matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes"),
        "Operation cancelled"
    );
    Ok(())
}

fn cli_notifications(
    data: Option<PathBuf>,
    language: Language,
) -> Option<std::sync::mpsc::Sender<()>> {
    use std::io::IsTerminal;
    if !std::io::stderr().is_terminal() {
        return None;
    }
    let (stop, stopped) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let notices = gitwatch::update::Notifications::start(data);
        loop {
            if let Some(release) = notices.poll() {
                eprintln!(
                    "{}",
                    language.format(
                        "gitwatch {0} is available. Run gitwatch self update.",
                        &[release.version()]
                    )
                );
            }
            if stopped.recv_timeout(Duration::from_millis(100))
                != Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            {
                break;
            }
        }
    });
    Some(stop)
}

fn direct(args: DirectArgs, json: bool, verbose: bool, language: Language) -> Result<()> {
    let target = args
        .target
        .context("Provide a file or directory to watch")?;
    let mut options = WatchOptions::default().pull_rebase(args.pull_rebase);
    if let Some(value) = args.message {
        options = options.message(value);
    }
    if let Some(value) = args.date_format {
        options = options.date_format(value);
    }
    if let Some(value) = args.remote {
        options = options.remote(value);
    }
    if let Some(value) = args.branch {
        options = options.branch(value);
    }
    if let Some(value) = args.exclude {
        options = options.exclude(value);
    }
    if let Some(value) = args.message_command {
        options = options.message_command(value, args.pass_files);
    }
    if let Some(value) = args.diff_lines {
        options = options.diff_message(value, true);
    }
    if let Some(value) = args.plain_diff_lines {
        options = options.diff_message(value, false);
    }
    let mut repository = Repository::open(target, args.git_dir.as_deref(), options)?;
    if args.once {
        let report = repository.commit()?;
        let failed = matches!(report.upload(), UploadState::Failed { .. });
        event(Event::Repository(report), json, verbose, language);
        ensure!(!failed, "Local operation completed but upload failed");
        return Ok(());
    }
    let monitor = MonitorOptions::default()
        .debounce(duration(args.delay)?)
        .poll_interval(duration(args.poll_interval)?)
        .max_wait(duration(args.max_wait.max(args.delay))?)
        .commit_on_start(args.commit_on_start)
        .native(!args.polling)
        .events(args.events);
    let stop = signal()?;
    watch::watch_repository(repository, monitor, stop, |value| {
        event(value, json, verbose, language)
    })
}

fn workspace_command(
    store: BackupStore,
    command: WorkspaceAction,
    json: bool,
    verbose: bool,
    language: Language,
) -> Result<()> {
    match command {
        WorkspaceAction::Add {
            root,
            name,
            branch,
            includes,
            excludes,
            follow_links,
        } => {
            let mut builder = Workspace::builder(name, root)
                .includes(includes)
                .excludes(excludes)
                .follow_links(follow_links);
            if let Some(branch) = branch {
                builder = builder.branch(branch);
            }
            let workspace = builder.build()?;
            store.register(workspace.clone())?;
            emit(
                &workspace,
                json,
                language.format(
                    "Registered {0} ({1}) on branch {2}",
                    &[
                        workspace.name(),
                        &workspace.id().to_string(),
                        workspace.branch(),
                    ],
                ),
            )?;
        }
        WorkspaceAction::List => {
            let workspaces = store.workspaces()?;
            emit(
                &workspaces,
                json,
                workspaces
                    .iter()
                    .map(|w| {
                        language.format(
                            "{0}  {1}  branch={2}  {3}{4}",
                            &[
                                &w.id().to_string(),
                                w.name(),
                                w.branch(),
                                &w.root().display().to_string(),
                                language.text(if w.is_paused() { " [paused]" } else { "" }),
                            ],
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            )?;
        }
        WorkspaceAction::Edit {
            workspace,
            name,
            root,
            includes,
            excludes,
            clear_excludes,
            follow_links,
            pause,
            resume,
        } => {
            let mut builder = find(&store, &workspace)?.edit();
            if let Some(name) = name {
                builder = builder.name(name);
            }
            if let Some(root) = root {
                builder = builder.root(root);
            }
            if !includes.is_empty() {
                builder = builder.includes(includes);
            }
            if clear_excludes || !excludes.is_empty() {
                builder = builder.excludes(excludes);
            }
            if let Some(enabled) = follow_links {
                builder = builder.follow_links(enabled);
            }
            if pause || resume {
                builder = builder.paused(pause);
            }
            let workspace = builder.build()?;
            store.update(workspace.clone())?;
            emit(
                &workspace,
                json,
                language.format("Updated {0}", &[workspace.name()]),
            )?;
        }
        WorkspaceAction::Bind { workspace, root } => {
            let workspace = find(&store, &workspace)?.edit().root(root).build()?;
            store.update(workspace.clone())?;
            emit(
                &workspace,
                json,
                language.format(
                    "Bound {0} to {1}",
                    &[workspace.name(), &workspace.root().display().to_string()],
                ),
            )?;
        }
        WorkspaceAction::Remove { workspace } => {
            store.remove(find(&store, &workspace)?.id())?;
            emit(
                &true,
                json,
                language
                    .text("Binding removed; branch history retained")
                    .into(),
            )?;
        }
        WorkspaceAction::Backup { workspace, push } => {
            let workspace = find(&store, &workspace)?;
            let report = store.backup(workspace.id())?;
            let failed = matches!(report.upload(), UploadState::Failed { .. });
            let uploaded = matches!(report.upload(), UploadState::Synced);
            event(Event::Backup(report), json, verbose, language);
            if push && !uploaded {
                store.push(workspace.id())?;
                event(Event::Upload(UploadState::Synced), json, verbose, language);
            } else {
                ensure!(!failed, "Local backup saved; automatic upload failed");
            }
        }
        WorkspaceAction::Status { workspace } => {
            let status = store.status(find(&store, &workspace)?.id())?;
            emit(
                &status,
                json,
                status
                    .as_ref()
                    .map(|s| {
                        language.format(
                            "Local {0} ({1} files); upload {2}",
                            &[
                                s.commit(),
                                &s.files().to_string(),
                                &upload_text(s.upload(), language),
                            ],
                        )
                    })
                    .unwrap_or(language.text("No backup yet").into()),
            )?;
        }
        WorkspaceAction::Push { workspace } => {
            store.push(find(&store, &workspace)?.id())?;
            event(Event::Upload(UploadState::Synced), json, verbose, language);
        }
        WorkspaceAction::Remote {
            url,
            auto_push,
            clear,
        } => {
            if url.is_some() || clear {
                store.set_remote(url.as_deref(), auto_push)?;
            }
            let (url, auto) = store.remote()?;
            emit(
                &serde_json::json!({"configured":url.is_some(),"auto_push":auto}),
                json,
                language.format(
                    "Remote configured: {0}; automatic upload: {1}",
                    &[
                        language.text(if url.is_some() { "yes" } else { "no" }),
                        language.text(if auto { "yes" } else { "no" }),
                    ],
                ),
            )?;
        }
        WorkspaceAction::Fetch | WorkspaceAction::Branches => {
            let branches = if matches!(command, WorkspaceAction::Fetch) {
                store.fetch()?
            } else {
                store.remote_workspaces()?
            };
            emit(
                &branches,
                json,
                branches
                    .iter()
                    .map(|b| {
                        format!(
                            "{}  {}  {}",
                            b.branch(),
                            b.manifest().id(),
                            b.manifest().name()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            )?;
        }
        WorkspaceAction::Import { branch, root } => {
            let workspace = store.import(&branch, root)?;
            emit(
                &workspace,
                json,
                language.format(
                    "Imported {0} (paused). Preview a restore before resuming.",
                    &[workspace.name()],
                ),
            )?;
        }
        WorkspaceAction::History {
            workspace,
            revision,
            limit,
        } => {
            let history =
                store.history(find(&store, &workspace)?.id(), revision.as_deref(), limit)?;
            emit(
                &history,
                json,
                history
                    .iter()
                    .map(|e| format!("{}  {}  {}", e.commit(), e.timestamp(), e.summary()))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )?;
        }
        WorkspaceAction::Diff {
            workspace,
            from,
            to,
        } => {
            let diff = store.diff(find(&store, &workspace)?.id(), &from, &to)?;
            emit(&diff, json, diff.clone())?;
        }
        WorkspaceAction::Restore {
            workspace,
            revision,
            plan,
            confirm,
            preview: _,
            files,
            show,
        } => {
            let workspace = find(&store, &workspace)?;
            if let Some(plan) = plan {
                ensure!(
                    store.restore_plan(plan)?.workspace() == workspace.id(),
                    "Restore plan belongs to a different workspace"
                );
                if let Some(path) = show {
                    let (before_bytes, after_bytes) = store.restore_contents(plan, &path)?;
                    let before = before_bytes.as_deref().map(display_bytes);
                    let after = display_bytes(&after_bytes);
                    emit(
                        &serde_json::json!({"before":before,"after":after}),
                        json,
                        language.format(
                            "--- Current ---\n{0}\n--- Selected ---\n{1}",
                            &[
                                &before_bytes
                                    .as_deref()
                                    .map(|bytes| display_contents(bytes, language))
                                    .unwrap_or_else(|| language.text("(missing)").into()),
                                &display_contents(&after_bytes, language),
                            ],
                        ),
                    )?;
                } else {
                    ensure!(confirm, "Use --confirm to apply the saved preview");
                    let report = store.apply_restore(plan)?;
                    emit(
                        &report,
                        json,
                        language.format(
                            "Restored {0} files. Recovery copies: {1}",
                            &[
                                &report.written().len().to_string(),
                                &report.recovery().display().to_string(),
                            ],
                        ),
                    )?;
                    ensure!(
                        report.error().is_none(),
                        "Partial restore: {}",
                        report.error().unwrap_or("")
                    );
                }
            } else {
                ensure!(!confirm, "Confirmation requires a saved --plan");
                let plan = store.preview_restore(
                    workspace.id(),
                    revision.as_deref().unwrap_or(workspace.branch()),
                    &files,
                )?;
                let lines = plan
                    .entries()
                    .iter()
                    .map(|entry| {
                        format!(
                            "{}  {}",
                            language.text(&format!("{:?}", entry.change())),
                            entry.path()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                emit(
                    &plan,
                    json,
                    language.format("{0}\nPlan: {1}\nConfirm with: gitwatch workspace restore {2} --plan {3} --confirm", &[
                        &lines, &plan.id().to_string(), &workspace.id().to_string(), &plan.id().to_string(),
                    ]),
                )?;
            }
        }
        WorkspaceAction::Watch {
            workspace,
            all,
            delay,
            poll_interval,
            no_initial,
            polling,
        } => {
            let workspaces = if let Some(workspace) = workspace {
                vec![find(&store, &workspace)?]
            } else {
                ensure!(all, "Specify a workspace or --all");
                store.workspaces()?
            };
            ensure!(!workspaces.is_empty(), "No workspaces configured");
            let stop = signal()?;
            let monitor = MonitorOptions::default()
                .debounce(duration(delay)?)
                .poll_interval(duration(poll_interval)?)
                .max_wait(duration(delay.max(60.0))?)
                .commit_on_start(!no_initial)
                .native(!polling);
            std::thread::scope(|scope| -> Result<()> {
                let mut handles = Vec::new();
                for workspace in workspaces {
                    let store = store.clone();
                    let monitor = monitor.clone();
                    let stop = stop.clone();
                    handles.push(scope.spawn(move || {
                        watch::watch_workspace(store, workspace.id(), monitor, stop, |value| {
                            event(value, json, verbose, language)
                        })
                    }));
                }
                for handle in handles {
                    handle
                        .join()
                        .map_err(|_| anyhow::anyhow!("Watch thread terminated unexpectedly"))??;
                }
                Ok(())
            })?;
        }
    }
    Ok(())
}

fn find(store: &BackupStore, selector: &str) -> Result<Workspace> {
    let matches: Vec<_> = store
        .workspaces()?
        .into_iter()
        .filter(|w| {
            w.id().to_string() == selector || w.name() == selector || w.branch() == selector
        })
        .collect();
    ensure!(
        matches.len() == 1,
        "Workspace selector is unknown or ambiguous; use its UUID"
    );
    matches.into_iter().next().context("Unknown workspace")
}

fn signal() -> Result<StopToken> {
    let stop = StopToken::default();
    let token = stop.clone();
    ctrlc::set_handler(move || token.stop())?;
    Ok(stop)
}

fn duration(seconds: f64) -> Result<Duration> {
    Duration::try_from_secs_f64(seconds).context("Duration must be finite and nonnegative")
}

fn display_bytes(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) if !text.contains('\0') => text.to_owned(),
        _ => format!("(binary file, {} bytes)", bytes.len()),
    }
}

fn emit(value: &impl Serialize, json: bool, human: String) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({"schema_version":1,"data":value}))?
        );
    } else {
        println!("{human}");
    }
    Ok(())
}

fn event(value: Event, json: bool, verbose: bool, language: Language) {
    if json {
        if let Ok(text) =
            serde_json::to_string(&serde_json::json!({"schema_version":1,"data":value}))
        {
            println!("{text}");
        }
        return;
    }
    match value {
        Event::Repository(report) => {
            if let Some(commit) = report.commit() {
                println!(
                    "{}",
                    language.format(
                        "Committed {0}; upload {1}",
                        &[commit, &upload_text(report.upload(), language)]
                    )
                );
            }
            if let Some(reason) = report.skipped() {
                eprintln!(
                    "{}",
                    language.format("Paused: {0}", &[&language.error(reason)])
                );
            }
        }
        Event::Backup(report) => println!(
            "{}",
            language.format(
                "{0} {1} ({2} files; {3} retained); upload {4}",
                &[
                    language.text(if report.changed() {
                        "Saved"
                    } else {
                        "Unchanged"
                    }),
                    report.commit(),
                    &report.files().to_string(),
                    &report.retained().len().to_string(),
                    &upload_text(report.upload(), language),
                ]
            )
        ),
        Event::Error(error) => eprintln!("{}", language.error(&error)),
        Event::Upload(UploadState::Failed { message }) => eprintln!(
            "{}",
            language.format("Upload pending: {0}", &[&language.error(&message)])
        ),
        Event::Upload(UploadState::Synced) => println!("{}", language.text("Upload synchronized")),
        Event::Stopped => println!("{}", language.text("Stopped")),
        Event::Watching(source) if verbose => eprintln!(
            "{}",
            language.format("Watching: {0}", &[language.text(&source)])
        ),
        Event::Pending if verbose => eprintln!("{}", language.text("Changes pending")),
        _ => {}
    }
}

fn upload_text(upload: &UploadState, language: Language) -> String {
    match upload {
        UploadState::Disabled => language.text("Disabled").into(),
        UploadState::Pending => language.text("Pending").into(),
        UploadState::Synced => language.text("Synced").into(),
        UploadState::Failed { message } => {
            language.format("Failed: {0}", &[&language.error(message)])
        }
    }
}

fn display_contents(bytes: &[u8], language: Language) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) if !text.contains('\0') => text.into(),
        _ => language.format("(binary file, {0} bytes)", &[&bytes.len().to_string()]),
    }
}

fn launch_desktop(data: Option<PathBuf>, language: Language) -> Result<()> {
    #[cfg(not(feature = "desktop"))]
    {
        let _ = (data, language);
        anyhow::bail!("Desktop was not compiled; install gitwatch with --features desktop")
    }
    #[cfg(feature = "desktop")]
    {
        let executable = std::env::current_exe()?.with_file_name(if cfg!(windows) {
            "gitwatch-desktop.exe"
        } else {
            "gitwatch-desktop"
        });
        ensure!(
            executable.is_file(),
            "Desktop binary is not installed beside gitwatch"
        );
        let mut command = std::process::Command::new(executable);
        command.arg("--lang").arg(language.code());
        if let Some(data) = data {
            command.arg("--data-dir").arg(data);
        }
        command.spawn()?;
        Ok(())
    }
}
