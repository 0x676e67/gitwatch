#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::PathBuf;

use clap::Parser;

#[derive(Parser)]
#[command(version, about = "gitwatch desktop")]
struct Args {
    #[arg(long, env = "GITWATCH_DATA_DIR")]
    data_dir: Option<PathBuf>,
}

fn main() {
    if let Err(error) = gitwatch::desktop::run(Args::parse().data_dir) {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}
