#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::PathBuf;

use clap::{CommandFactory, FromArgMatches, Parser};
use gitwatch::i18n::Language;

#[derive(Parser)]
#[command(version, about = "gitwatch desktop")]
struct Args {
    /// Language (en or zh-CN).
    #[arg(long, env = "GITWATCH_LANG")]
    lang: Option<String>,
    /// Store directory for workspace backups and local bindings.
    #[arg(long, env = "GITWATCH_DATA_DIR")]
    data_dir: Option<PathBuf>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

fn run() -> gitwatch::Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
    let language = Language::from_args(&args)?;
    let matches = match language.command(Args::command()).try_get_matches_from(args) {
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
    let args = Args::from_arg_matches(&matches)?;
    gitwatch::desktop::run_with_language(args.data_dir, language)
        .map_err(|error| anyhow::anyhow!(language.error(&format!("{error:#}"))))
}
