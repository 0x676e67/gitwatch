mod cli;

fn main() {
    if let Err(error) = cli::run() {
        eprintln!("gitwatch: {error:#}");
        std::process::exit(1);
    }
}
