use std::sync::mpsc::{self, Receiver};

use eframe::egui;

use crate::{
    i18n::Language,
    update::{Installation, Release, Running, VERSION},
};

enum Action {
    Update(Release),
    Uninstall,
}

enum State {
    Idle,
    Checking(Receiver<Result<Release, String>>),
    Confirm(Action),
    Stopping(Action),
    Working(Receiver<Result<(), String>>),
    Finished(Result<(), String>),
}

/// Keeps the installation lock until all task workers have stopped.
pub(super) struct Panel {
    running: Option<Running>,
    state: State,
    pub open: bool,
    allowed: Result<(), String>,
    message: Option<String>,
    confirmed: bool,
    exit: bool,
}

impl Panel {
    pub fn new(running: Option<Running>) -> Self {
        Self {
            running,
            state: State::Idle,
            open: false,
            allowed: Ok(()),
            message: None,
            confirmed: false,
            exit: false,
        }
    }

    pub fn open(&mut self) {
        self.open = true;
        self.allowed = Installation::check().map_err(|error| format!("{error:#}"));
    }

    pub fn stopping(&self) -> bool {
        matches!(
            self.state,
            State::Stopping(_) | State::Working(_) | State::Finished(_)
        )
    }

    pub fn exiting(&self) -> bool {
        self.exit
    }

    pub fn poll(&mut self, workers_finished: bool) {
        match &self.state {
            State::Checking(receiver) => match receiver.try_recv() {
                Ok(Ok(release)) if release.is_newer() => {
                    self.confirmed = false;
                    self.state = State::Confirm(Action::Update(release));
                }
                Ok(Ok(_)) => {
                    self.message = Some("You are using the latest version.".into());
                    self.state = State::Idle;
                }
                Ok(Err(error)) => {
                    self.message = Some(error);
                    self.state = State::Idle;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.message = Some("The update worker stopped unexpectedly.".into());
                    self.state = State::Idle;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            },
            State::Working(receiver) => match receiver.try_recv() {
                Ok(result) => self.state = State::Finished(result),
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.state =
                        State::Finished(Err("The update worker stopped unexpectedly.".into()));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            },
            _ => {}
        }
        if workers_finished && matches!(self.state, State::Stopping(_)) {
            let State::Stopping(action) = std::mem::replace(&mut self.state, State::Idle) else {
                return;
            };
            // The exclusive installer lock must never overlap our shared running lock.
            drop(self.running.take());
            let (sender, receiver) = mpsc::channel();
            self.state = State::Working(receiver);
            std::thread::spawn(move || {
                let result = Installation::current().and_then(|installation| match action {
                    Action::Update(release) => installation.update(&release),
                    Action::Uninstall => installation.uninstall(),
                });
                let _ = sender.send(result.map_err(|error| format!("{error:#}")));
            });
        }
    }

    pub fn show(&mut self, context: &egui::Context, language: Language) {
        if !self.open {
            return;
        }
        egui::Modal::new(egui::Id::new("installation")).show(context, |ui| {
            ui.set_width((context.content_rect().width() - 64.0).clamp(280.0, 500.0));
            ui.heading(language.text("Manage installation"));
            ui.label(format!("gitwatch {VERSION}"));
            egui::ScrollArea::vertical().max_height((context.content_rect().height() - 230.0).max(80.0)).show(ui, |ui| {
                if let Err(error) = &self.allowed {
                    ui.label(language.error(error));
                }
                if let Some(message) = &self.message {
                    ui.label(language.error(message));
                }
                match &self.state {
                    State::Confirm(action) => {
                        match action {
                            Action::Update(release) => {
                                ui.label(language.format("Install gitwatch {0}?", &[release.version()]));
                                ui.hyperlink_to(language.text("Release notes"), release.url());
                            }
                            Action::Uninstall => { ui.label(language.text("Remove the installed application?")); }
                        }
                        ui.label(language.text("Running tasks will stop. Your task settings and backups will be kept."));
                    }
                    State::Checking(_) => { ui.spinner(); ui.label(language.text("Checking for updates…")); }
                    State::Stopping(_) => { ui.spinner(); ui.label(language.text("Stopping tasks…")); }
                    State::Working(_) => { ui.spinner(); ui.label(language.text("Updating the installation. Keep this window open.")); }
                    State::Finished(result) => {
                        match result {
                            Ok(()) => { ui.label(language.text("Installation changed. Close this window before opening gitwatch again.")); }
                            Err(error) => { ui.label(language.error(error)); ui.label(language.text("Tasks have stopped. Close and reopen gitwatch to continue.")); }
                        }
                    }
                    State::Idle => { ui.hyperlink_to(language.text("Downloads and release notes"), "https://github.com/0x676e67/gitwatch/releases"); }
                }
            });
            ui.separator();
            match &self.state {
                State::Idle => {
                    ui.horizontal_wrapped(|ui| {
                        if ui.add_enabled(self.allowed.is_ok(), egui::Button::new(language.text("Check for updates"))).clicked() {
                            self.message = None;
                            let (sender, receiver) = mpsc::channel();
                            self.state = State::Checking(receiver);
                            std::thread::spawn(move || { let _ = sender.send(Release::check(None).map_err(|error| format!("{error:#}"))); });
                        }
                        if ui.add_enabled(self.allowed.is_ok(), egui::Button::new(language.text("Uninstall application"))).clicked() {
                            self.confirmed = false;
                            self.state = State::Confirm(Action::Uninstall);
                        }
                        if ui.button(language.text("Close")).clicked() { self.open = false; }
                    });
                }
                State::Confirm(_) => {
                    ui.checkbox(&mut self.confirmed, language.text("I agree to stop tasks and change this installation."));
                    ui.horizontal(|ui| {
                        if ui.add_enabled(self.confirmed, egui::Button::new(language.text("Confirm"))).clicked()
                            && let State::Confirm(action) = std::mem::replace(&mut self.state, State::Idle) {
                            self.state = State::Stopping(action);
                        }
                        if ui.button(language.text("Cancel")).clicked() { self.state = State::Idle; }
                    });
                }
                State::Checking(_) => {
                    if ui.button(language.text("Cancel")).clicked() { self.state = State::Idle; }
                }
                State::Finished(_) => {
                    if ui.button(language.text("Close gitwatch")).clicked() { self.exit = true; }
                }
                State::Stopping(_) | State::Working(_) => {}
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        process::Command,
        time::{Duration, Instant},
    };

    use super::*;

    fn render(
        panel: &mut Panel,
        context: &egui::Context,
        language: Language,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let mut output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 560.0),
                )),
                events,
                ..Default::default()
            },
            |ui| panel.show(ui.ctx(), language),
        );
        output.textures_delta.clear();
        output
    }

    fn click(panel: &mut Panel, context: &egui::Context, language: Language, label: &str) {
        let mut point = None;
        for _ in 0..3 {
            let output = render(panel, context, language, vec![]);
            point = output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == language.text(label) => {
                    let point = text.pos + text.galley.size() / 2.0;
                    (shape.clip_rect.contains(point) && point.y < 560.0).then_some(point)
                }
                _ => None,
            });
            if point.is_some() {
                break;
            }
        }
        let point = point.unwrap_or_else(|| panic!("Missing visible action: {label}"));
        for pressed in [true, false] {
            render(
                panel,
                context,
                language,
                vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
    }

    #[test]
    fn confirmation_and_worker_shutdown_protect_the_installation() {
        let temp = tempfile::tempdir().unwrap();
        for language in ["en", "zh-CN"] {
            let directory = temp.path().join(language);
            fs::create_dir(&directory).unwrap();
            let executable = directory.join(if cfg!(windows) {
                "gitwatch.exe"
            } else {
                "gitwatch"
            });
            fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
            fs::write(directory.join("task-data"), "keep").unwrap();
            let output = Command::new(&executable)
                .args([
                    "--exact",
                    "desktop::installation::tests::copied_installation_child",
                    "--nocapture",
                ])
                .env("GITWATCH_TEST_DESKTOP_INSTALL", language)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let deadline = Instant::now() + Duration::from_secs(10);
            while executable.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(!executable.exists());
            assert_eq!(
                fs::read_to_string(directory.join("task-data")).unwrap(),
                "keep"
            );
        }
    }

    #[test]
    fn copied_installation_child() {
        let Ok(language) = std::env::var("GITWATCH_TEST_DESKTOP_INSTALL") else {
            return;
        };
        let language: Language = language.parse().unwrap();
        let executable = std::env::current_exe().unwrap();
        assert_eq!(executable.file_stem().unwrap(), "gitwatch");
        let mut panel = Panel::new(Some(Running::acquire().unwrap()));
        panel.open();
        assert!(panel.allowed.is_ok());
        assert!(Installation::current().is_err());
        let context = egui::Context::default();
        super::super::theme::apply(&context);
        super::super::configure_fonts(&context);
        click(&mut panel, &context, language, "Uninstall application");
        click(&mut panel, &context, language, "Confirm");
        assert!(
            matches!(panel.state, State::Confirm(_)),
            "Confirmation must require acknowledgement"
        );
        click(&mut panel, &context, language, "Cancel");
        assert!(matches!(panel.state, State::Idle));
        click(&mut panel, &context, language, "Uninstall application");
        click(
            &mut panel,
            &context,
            language,
            "I agree to stop tasks and change this installation.",
        );
        click(&mut panel, &context, language, "Confirm");
        assert!(panel.stopping());
        panel.poll(false);
        assert!(
            matches!(panel.state, State::Stopping(_)),
            "Do not modify the installation while task workers are active"
        );
        assert!(panel.running.is_some());
        assert!(executable.exists());
        panel.poll(true);
        assert!(panel.running.is_none());
        let deadline = Instant::now() + Duration::from_secs(10);
        while !matches!(panel.state, State::Finished(_)) {
            panel.poll(true);
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(matches!(panel.state, State::Finished(Ok(()))));
        assert!(!panel.exiting());
        click(&mut panel, &context, language, "Close gitwatch");
        assert!(panel.exiting());
    }
}
