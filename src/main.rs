#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use gitwatch::i18n::Language;

fn main() {
    // Release verification does not initialize Git, preferences or a window.
    if std::env::args_os().skip(1).eq(["--version"]) {
        println!("gitwatch {}", gitwatch::update::VERSION);
        return;
    }
    let data = std::env::var_os("GITWATCH_DATA_DIR").map(std::path::PathBuf::from);
    let language = Language::resolve(None, data.as_deref());
    let result = language
        .as_ref()
        .map_err(|error| anyhow::anyhow!("{error:#}"))
        .and_then(|language| {
            anyhow::ensure!(
                std::env::args_os().len() == 1,
                "gitwatch is a desktop application. Open it without command-line arguments."
            );
            gitwatch::desktop::run_with_language(data, *language)
        });
    if let Err(error) = result {
        let error = language.unwrap_or_default().error(&format!("{error:#}"));
        eprintln!("gitwatch: {error}");
        rfd::MessageDialog::new()
            .set_title("gitwatch")
            .set_level(rfd::MessageLevel::Error)
            .set_description(error)
            .set_buttons(rfd::MessageButtons::Ok)
            .show();
        std::process::exit(1);
    }
}
