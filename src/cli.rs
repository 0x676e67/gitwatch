use std::{path::PathBuf, time::Duration};

use anyhow::{Context, ensure};
use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};
use gitwatch::{
    Result,
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
    /// Clone or periodically fast-forward a local repository.
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
    let matches = Cli::command().get_matches();
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
    match cli.command {
        Some(Action::Pull {
            path,
            url,
            remote,
            branch,
            every,
            once,
        }) => {
            let mut options = gitwatch::pull::PullOptions::new(path)
                .remote(remote)
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
                    format!(
                        "{} {} at {}",
                        if report.cloned() {
                            "Cloned"
                        } else if report.changed() {
                            "Updated"
                        } else {
                            "Unchanged"
                        },
                        report.path().display(),
                        report.after()
                    ),
                );
            }
            task.run(signal()?, |result| match result {
                Ok(report) => {
                    let _ = emit(
                        &report,
                        cli.json,
                        format!("{} at {}", report.path().display(), report.after()),
                    );
                }
                Err(error) => event(Event::Error(error.to_string()), cli.json, cli.verbose),
            })
        }
        Some(Action::Watch(args)) => direct(args, cli.json, cli.verbose),
        Some(Action::Workspace { command }) => {
            let store =
                BackupStore::open(cli.data_dir.unwrap_or(BackupStore::default_directory()?))?;
            workspace_command(store, command, cli.json, cli.verbose)
        }
        Some(Action::Tui) => {
            #[cfg(feature = "tui")]
            {
                gitwatch::tui::run(cli.data_dir)
            }
            #[cfg(not(feature = "tui"))]
            anyhow::bail!("TUI was not compiled; install gitwatch with --features tui")
        }
        Some(Action::Desktop) => launch_desktop(cli.data_dir),
        None if cli.watch.target.is_some() => direct(cli.watch, cli.json, cli.verbose),
        None => {
            Cli::command().print_help()?;
            println!();
            Ok(())
        }
    }
}

fn direct(args: DirectArgs, json: bool, verbose: bool) -> Result<()> {
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
        event(Event::Repository(report), json, verbose);
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
        event(value, json, verbose)
    })
}

fn workspace_command(
    store: BackupStore,
    command: WorkspaceAction,
    json: bool,
    verbose: bool,
) -> Result<()> {
    match command {
        WorkspaceAction::Add {
            root,
            name,
            branch,
            includes,
            excludes,
        } => {
            let mut builder = Workspace::builder(name, root)
                .includes(includes)
                .excludes(excludes);
            if let Some(branch) = branch {
                builder = builder.branch(branch);
            }
            let workspace = builder.build()?;
            store.register(workspace.clone())?;
            emit(
                &workspace,
                json,
                format!(
                    "Registered {} ({}) on branch {}",
                    workspace.name(),
                    workspace.id(),
                    workspace.branch()
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
                        format!(
                            "{}  {}  branch={}  {}{}",
                            w.id(),
                            w.name(),
                            w.branch(),
                            w.root().display(),
                            if w.is_paused() { " [paused]" } else { "" }
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
            if pause || resume {
                builder = builder.paused(pause);
            }
            let workspace = builder.build()?;
            store.update(workspace.clone())?;
            emit(&workspace, json, format!("Updated {}", workspace.name()))?;
        }
        WorkspaceAction::Bind { workspace, root } => {
            let workspace = find(&store, &workspace)?.edit().root(root).build()?;
            store.update(workspace.clone())?;
            emit(
                &workspace,
                json,
                format!(
                    "Bound {} to {}",
                    workspace.name(),
                    workspace.root().display()
                ),
            )?;
        }
        WorkspaceAction::Remove { workspace } => {
            store.remove(find(&store, &workspace)?.id())?;
            emit(
                &true,
                json,
                "Binding removed; branch history retained".into(),
            )?;
        }
        WorkspaceAction::Backup { workspace, push } => {
            let workspace = find(&store, &workspace)?;
            let report = store.backup(workspace.id())?;
            let failed = matches!(report.upload(), UploadState::Failed { .. });
            let uploaded = matches!(report.upload(), UploadState::Synced);
            event(Event::Backup(report), json, verbose);
            if push && !uploaded {
                store.push(workspace.id())?;
                event(Event::Upload(UploadState::Synced), json, verbose);
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
                        format!(
                            "Local {} ({} files); upload {:?}",
                            s.commit(),
                            s.files(),
                            s.upload()
                        )
                    })
                    .unwrap_or("No backup yet".into()),
            )?;
        }
        WorkspaceAction::Push { workspace } => {
            store.push(find(&store, &workspace)?.id())?;
            event(Event::Upload(UploadState::Synced), json, verbose);
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
                format!(
                    "Remote configured: {}; automatic upload: {auto}",
                    url.is_some()
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
                format!(
                    "Imported {} (paused). Preview a restore before resuming.",
                    workspace.name()
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
                    let (before, after) = store.restore_contents(plan, &path)?;
                    let before = before.as_deref().map(display_bytes);
                    let after = display_bytes(&after);
                    emit(
                        &serde_json::json!({"before":before,"after":after}),
                        json,
                        format!(
                            "--- current ---\n{}\n--- selected ---\n{after}",
                            before.as_deref().unwrap_or("(missing)")
                        ),
                    )?;
                } else {
                    ensure!(confirm, "Use --confirm to apply the saved preview");
                    let report = store.apply_restore(plan)?;
                    emit(
                        &report,
                        json,
                        format!(
                            "Restored {} files. Recovery copies: {}",
                            report.written().len(),
                            report.recovery().display()
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
                    .map(|entry| format!("{:?}  {}", entry.change(), entry.path()))
                    .collect::<Vec<_>>()
                    .join("\n");
                emit(
                    &plan,
                    json,
                    format!(
                        "{lines}\nPlan: {}\nConfirm with: gitwatch workspace restore {} --plan {} --confirm",
                        plan.id(),
                        workspace.id(),
                        plan.id()
                    ),
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
                            event(value, json, verbose)
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

fn event(value: Event, json: bool, verbose: bool) {
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
                println!("Committed {commit}; upload {:?}", report.upload());
            }
            if let Some(reason) = report.skipped() {
                eprintln!("Paused: {reason}");
            }
        }
        Event::Backup(report) => println!(
            "{} {} ({} files; {} retained); upload {:?}",
            if report.changed() {
                "Saved"
            } else {
                "Unchanged"
            },
            report.commit(),
            report.files(),
            report.retained().len(),
            report.upload()
        ),
        Event::Error(error) => eprintln!("{error}"),
        Event::Upload(UploadState::Failed { message }) => eprintln!("Upload pending: {message}"),
        Event::Upload(UploadState::Synced) => println!("Upload synchronized"),
        Event::Stopped => println!("Stopped"),
        value if verbose => eprintln!("{value:?}"),
        _ => {}
    }
}

fn launch_desktop(data: Option<PathBuf>) -> Result<()> {
    #[cfg(not(feature = "desktop"))]
    {
        let _ = data;
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
        if let Some(data) = data {
            command.arg("--data-dir").arg(data);
        }
        command.spawn()?;
        Ok(())
    }
}
