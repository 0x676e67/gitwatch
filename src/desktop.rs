//! Native desktop task management, history and explicit restore previews.

mod repository;
mod theme;
mod tray;

use std::{path::PathBuf, time::Duration};

use eframe::egui::{self, Color32, RichText};
use uuid::Uuid;

use crate::{
    Result,
    i18n::Language,
    interface::{Command, Draft, Kind, Model, lines},
};

fn pull_countdown(next: Option<std::time::Instant>, language: Language) -> String {
    match next {
        Some(next) => {
            let remaining = next.saturating_duration_since(std::time::Instant::now());
            let seconds = remaining.as_secs() + u64::from(remaining.subsec_nanos() > 0);
            language.format(
                "Next pull in {0}",
                &[&format!(
                    "{:02}:{:02}:{:02}",
                    seconds / 3600,
                    seconds / 60 % 60,
                    seconds % 60
                )],
            )
        }
        None => language.text("Pulling…").into(),
    }
}

struct Desktop {
    model: Model,
    repository: repository::Panel,
    tray: Option<tray::Tray>,
    tray_error: Option<String>,
    hide_on_start: bool,
    desktop_settings: bool,
    selected: Option<Uuid>,
    draft: Option<Draft>,
    history: usize,
    restore_files: String,
    confirm: bool,
    remove: bool,
    remote: String,
    auto_push: bool,
    settings: bool,
    import_branch: usize,
    import_path: String,
}

/// Runs the native desktop interface; Git and filesystem work stays on workers.
pub fn run(data: Option<PathBuf>) -> Result<()> {
    let language = Language::resolve(None, data.as_deref())?;
    run_with_language(data, language)
}

/// Opens the desktop with an explicit initial language.
pub fn run_with_language(data: Option<PathBuf>, language: Language) -> Result<()> {
    let directory = data
        .clone()
        .map_or_else(crate::workspace::BackupStore::default_directory, Ok)?;
    let start_in_tray = crate::preferences::Preferences::load(&directory)?.start_in_tray;
    let mut model = Model::with_language(data, language);
    // Let eframe's screenshot harness capture a loaded fixture on its second frame.
    if cfg!(debug_assertions) && std::env::var_os("EFRAME_SCREENSHOT_TO").is_some() {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while model.busy && std::time::Instant::now() < deadline {
            model.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1140.0, 780.0])
            .with_min_inner_size([800.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native(
        "gitwatch",
        options,
        Box::new(move |context| {
            theme::apply(&context.egui_ctx);
            configure_fonts(&context.egui_ctx);
            let (tray, tray_error) = match tray::Tray::new(language) {
                Ok(tray) => (Some(tray), None),
                Err(error) => (None, Some(format!("{error:#}"))),
            };
            let hide_on_start = start_in_tray && tray.is_some();
            Ok(Box::new(Desktop {
                model,
                repository: repository::Panel::default(),
                tray,
                tray_error,
                hide_on_start,
                desktop_settings: false,
                selected: None,
                draft: None,
                history: 0,
                restore_files: String::new(),
                confirm: false,
                remove: false,
                remote: String::new(),
                auto_push: false,
                settings: false,
                import_branch: 0,
                import_path: String::new(),
            }))
        }),
    )
    .map_err(|error| anyhow::anyhow!("Desktop failed: {error}"))
}

impl eframe::App for Desktop {
    fn logic(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.model.poll();
        if let Some(tray) = &mut self.tray {
            tray.poll(context);
            tray.set_language(self.model.language);
            tray::close(context, tray.quitting());
            if std::mem::take(&mut self.hide_on_start)
                || context.input(|input| {
                    input.viewport().minimized == Some(true)
                        && input.viewport().visible() == Some(true)
                })
            {
                tray::hide(context);
            }
        }
        context.request_repaint_after(Duration::from_millis(250));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.paint(ui);
    }
}

impl Desktop {
    fn paint(&mut self, ui: &mut egui::Ui) {
        let language = self.model.language;
        self.model.poll();
        if !self
            .model
            .rows
            .iter()
            .any(|row| row.draft.id == self.selected)
        {
            self.selected = self.model.rows.first().and_then(|r| r.draft.id);
            self.model.history.clear();
            self.model.plan = None;
            self.model.text.clear();
            self.history = 0;
            self.confirm = false;
            self.remove = false;
        }
        ui.ctx().request_repaint_after(Duration::from_millis(100));
        egui::CentralPanel::default().frame(egui::Frame::new().fill(ui.visuals().panel_fill).inner_margin(24)).show(ui, |ui| {
            let mut selected_language = language;
            ui.horizontal(|ui| {
                ui.heading(RichText::new("gitwatch").size(28.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_enabled_ui(!self.model.busy, |ui| {
                        egui::ComboBox::from_id_salt("language")
                            .selected_text(language.name())
                            .show_ui(ui, |ui| {
                                for choice in [Language::English, Language::Chinese] {
                                    ui.selectable_value(&mut selected_language, choice, choice.name());
                                }
                            });
                    });
                    if ui.button(language.text("Settings")).clicked() { self.desktop_settings = !self.desktop_settings; }
                });
            });
            ui.weak(language.text("Keep a history of your work."));
            if selected_language != language { self.model.set_language(selected_language); }
            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                    if ui.button(language.text("Backup settings")).clicked() { self.settings = !self.settings; self.draft = None; }
                    if ui.button(language.text("+ Add task")).clicked() { self.draft = Some(Draft::default()); self.settings = false; }
                    if ui.add_enabled(!self.model.busy, egui::Button::new(language.text("Refresh"))).clicked() { self.model.send(Command::Refresh); self.repository.refresh(); }
                    if self.tray.is_some() && ui.button(language.text("Minimize to tray")).clicked() { tray::hide(ui.ctx()); }
            });
            ui.add_space(8.0);
            ui.separator();
            if let Some(release) = &self.model.update {
                egui::Frame::new().fill(theme::SELECTED).corner_radius(8).inner_margin(12).show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(language.format("gitwatch {0} is available.", &[release.version()]));
                        ui.monospace("gitwatch self update").on_hover_text(language.text("Choose Quit in the tray menu, then update from a terminal:"));
                        ui.hyperlink_to(language.text("Release notes"), release.url());
                    });
                });
            }
            if self.model.busy { ui.horizontal(|ui| { ui.spinner(); ui.label(language.text("Working in background…")); }); }
            if let Some(error) = &self.model.error { ui.colored_label(Color32::LIGHT_RED, language.error(error)); }
            if let Some(error) = &self.tray_error { ui.colored_label(Color32::YELLOW, language.text("System tray unavailable; closing this window will exit.")).on_hover_text(error); }
                ui.columns(2, |columns| {
                    columns[0].heading(language.text("Tasks"));
                    columns[0].label(language.text("Nothing runs until you start a task."));
                    egui::ScrollArea::vertical().id_salt("tasks").max_height((columns[0].available_height() - 100.0).max(100.0)).show(&mut columns[0], |ui| {
                        if self.model.rows.is_empty() { ui.add_space(24.0); ui.label(language.text("Add a workspace backup, watch a Git repository, or schedule repository updates.")); }
                        for row in &self.model.rows {
                            let id = row.draft.id;
                            egui::Frame::new().fill(if self.selected == id { theme::SELECTED } else { theme::SURFACE })
                                .stroke(egui::Stroke::new(1.0, if self.selected == id { theme::ACCENT } else { theme::BORDER }))
                                .corner_radius(10).inner_margin(14).show(ui, |ui| {
                                ui.set_min_width((ui.available_width() - 2.0).max(0.0));
                                if ui.selectable_label(self.selected == id, RichText::new(&row.draft.name).strong().size(18.0)).clicked() {
                                    self.selected = id; self.settings = false; self.draft = None; self.model.history.clear(); self.model.plan = None; self.model.text.clear(); self.history = 0; self.confirm = false; self.remove = false;
                                }
                                ui.horizontal_wrapped(|ui| {
                                    ui.weak(row.draft.kind.label(language));
                                    ui.colored_label(if row.running { theme::ACCENT } else { theme::MUTED }, language.text(if row.running { "Running" } else { "Stopped" }));
                                });
                                ui.add(egui::Label::new(RichText::new(&row.draft.path).small().color(theme::MUTED)).wrap());
                                ui.label(RichText::new(row.status.render(language)).small().color(theme::ACCENT));
                                if row.draft.kind == Kind::Pull && row.running {
                                    let next = id.and_then(|id| self.model.pull_schedule.get(&id)).copied().flatten();
                                    ui.small(pull_countdown(next, language));
                                }
                            });
                            ui.add_space(6.0);
                        }
                    });
                    if self.settings {
                        egui::ScrollArea::vertical().id_salt("backup-settings").max_height((columns[1].available_height() - 100.0).max(100.0)).show(&mut columns[1], |ui| self.settings(ui));
                    } else if self.draft.is_some() { self.form(&mut columns[1]); }
                    else { self.details(&mut columns[1]); }
                });
            ui.separator();
            egui::CollapsingHeader::new(language.text("Activity")).default_open(true).show(ui, |ui| {
                egui::ScrollArea::vertical().id_salt("activity").max_height(120.0).stick_to_bottom(true).show(ui, |ui| {
                    for message in &self.model.logs { ui.label(message.render(language)); }
                });
            });
        });
        egui::Window::new(language.text("Desktop settings"))
            .open(&mut self.desktop_settings)
            .collapsible(false)
            .resizable(false)
            .show(ui.ctx(), |ui| {
                ui.add_enabled_ui(!self.model.busy, |ui| {
                    let mut enabled = self.model.start_in_tray;
                    if ui
                        .checkbox(&mut enabled, language.text("Start minimized to tray"))
                        .changed()
                    {
                        self.model.send(Command::StartInTray(enabled));
                    }
                });
                ui.label(
                    language.text(
                        "Applies the next time you open gitwatch. Tasks still start manually.",
                    ),
                );
                ui.label(language.text("Closing the window keeps tasks running in the tray."));
                ui.label(language.text(
                    "Click the tray icon to show the window. Choose Quit in its menu to exit.",
                ));
            });
    }
}

impl Desktop {
    fn details(&mut self, ui: &mut egui::Ui) {
        let language = self.model.language;
        let Some(row) = self.model.rows.iter().find(|r| r.draft.id == self.selected) else {
            ui.heading(language.text("Choose a task"));
            return;
        };
        let Some(id) = row.draft.id else {
            return;
        };
        let running = row.running;
        let kind = row.draft.kind;
        let draft = row.draft.clone();
        let repository_path = draft.path.clone();
        let repository_remote = draft.remote.clone();
        ui.heading(&draft.name);
        ui.add_enabled_ui(!self.model.busy, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui
                    .button(language.text(if running { "Stop" } else { "Start" }))
                    .clicked()
                {
                    self.model.send(if running {
                        Command::Stop(id)
                    } else {
                        Command::Start(id)
                    });
                }
                if ui
                    .add_enabled(!running, egui::Button::new(language.text("Run once")))
                    .clicked()
                {
                    self.model.send(Command::Once(id));
                }
                if ui
                    .add_enabled(!running, egui::Button::new(language.text("Edit")))
                    .clicked()
                {
                    self.draft = Some(draft);
                }
                if ui
                    .add_enabled(!running, egui::Button::new(language.text("Remove")))
                    .clicked()
                {
                    self.remove = true;
                }
                if kind == Kind::Workspace {
                    if ui.button(language.text("History")).clicked() {
                        self.model.send(Command::History(id));
                    }
                    if ui.button(language.text("Upload")).clicked() {
                        self.model.send(Command::Push(id));
                    }
                }
            });
            if self.remove {
                ui.label(
                    language.text("Remove this local binding? Backup history will be retained."),
                );
                ui.horizontal(|ui| {
                    if ui.button(language.text("Remove binding")).clicked() {
                        self.model.send(Command::Remove(id));
                        self.remove = false;
                    }
                    if ui.button(language.text("Cancel")).clicked() {
                        self.remove = false;
                    }
                });
            }
        });
        if kind != Kind::Workspace {
            ui.add_space(16.0);
            egui::ScrollArea::vertical().id_salt("repository-details").max_height((ui.available_height() - 100.0).max(100.0)).show(ui, |ui| {
                self.repository.show(ui, &repository_path, &repository_remote, language);
                ui.add_space(12.0);
                ui.small(language.text(if kind == Kind::Pull { "Clones if needed, then pulls at the configured interval. Dirty or divergent repositories require your attention." } else { "Commits changes in the selected Git repository. The task stops automatic writes if the branch changes." }));
            });
            return;
        }
        egui::ScrollArea::vertical().id_salt("details").max_height((ui.available_height() - 100.0).max(100.0)).show(ui, |ui| {
            egui::CollapsingHeader::new(language.text("Source repository")).show(ui, |ui| self.repository.show(ui, &repository_path, "origin", language));
            if let Some(plan) = &self.model.plan {
                ui.separator(); ui.heading(language.text("Restore preview"));
                ui.label(language.format("Destination: {0}", &[&plan.root().display().to_string()]));
                ui.label(language.format("Version: {0}", &[plan.commit()]));
                let mut command = None;
                for entry in plan.entries() {
                    if ui.add_enabled(!self.model.busy, egui::Button::new(format!("{}  {}", language.text(&format!("{:?}", entry.change())), entry.path()))).clicked() { command = Some(Command::Contents(plan.id(), entry.path().into())); }
                }
                if self.confirm {
                    ui.colored_label(Color32::YELLOW, language.text("This replaces the previewed files. Current files are saved in the local recovery directory first. Extra files are kept."));
                    if ui.add_enabled(!self.model.busy && !running, egui::Button::new(language.text("Confirm restore"))).clicked() { command = Some(Command::Restore(plan.id())); self.confirm = false; }
                } else if ui.add_enabled(!running && !self.model.busy, egui::Button::new(language.text("Continue to confirmation"))).clicked() { self.confirm = true; }
                if let Some(command) = command { self.model.send(command); }
                if ui.button(language.text("Close preview")).clicked() { self.model.plan = None; self.confirm = false; }
            } else {
                ui.separator(); ui.heading(language.text("History"));
                for (index, entry) in self.model.history.iter().enumerate() {
                    if ui.selectable_label(self.history == index, format!("{}  {}", entry.commit().get(..12).unwrap_or(entry.commit()), entry.summary())).clicked() { self.history = index; }
                }
                if let Some(entry) = self.model.history.get(self.history) {
                    let revision = entry.commit().to_owned();
                    let older = self.model.history.get(self.history + 1).map(|e| e.commit().to_owned());
                    ui.label(language.text("Restore selected paths (one per line; empty means all)"));
                    ui.text_edit_multiline(&mut self.restore_files);
                    ui.horizontal(|ui| {
                        if ui.add_enabled(!self.model.busy && !running, egui::Button::new(language.text("Preview restore"))).clicked() { self.model.send(Command::Preview(id, revision.clone(), lines(&self.restore_files))); }
                        if let Some(older) = older && ui.add_enabled(!self.model.busy, egui::Button::new(language.text("Diff to previous"))).clicked() { self.model.send(Command::Diff(id, older, revision)); }
                    });
                    if running { ui.label(language.text("Stop this task before restoring.")); }
                }
            }
            if !self.model.text.is_empty() { ui.separator(); ui.add(egui::TextEdit::multiline(&mut self.model.text.render(language)).interactive(false).font(egui::TextStyle::Monospace).desired_rows(15).desired_width(f32::INFINITY)); }
        });
    }

    fn form(&mut self, ui: &mut egui::Ui) {
        let language = self.model.language;
        let Some(draft) = &mut self.draft else {
            return;
        };
        ui.heading(language.text(if draft.id.is_some() {
            "Edit task"
        } else {
            "New task"
        }));
        let mut save = false;
        let mut cancel = false;
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            save = ui
                .add_enabled(
                    !self.model.busy,
                    egui::Button::new(language.text("Save task")),
                )
                .clicked();
            cancel = ui.button(language.text("Cancel")).clicked();
            ui.label(language.text("Saving does not start a task."));
        });
        egui::ScrollArea::vertical()
            .id_salt("form")
            .max_height((ui.available_height() - 70.0).max(100.0))
            .show(ui, |ui| {
                ui.add_enabled_ui(draft.id.is_none(), |ui| {
                    ui.horizontal(|ui| {
                        ui.selectable_value(
                            &mut draft.kind,
                            Kind::Workspace,
                            language.text("Workspace backup"),
                        );
                        if ui
                            .selectable_value(
                                &mut draft.kind,
                                Kind::Watch,
                                language.text("Git watch"),
                            )
                            .changed()
                        {
                            draft.remote.clear();
                        }
                        ui.selectable_value(
                            &mut draft.kind,
                            Kind::Pull,
                            language.text("Scheduled pull"),
                        );
                    });
                });
                field(ui, language.text("Name"), &mut draft.name);
                field(ui, language.text("Local path"), &mut draft.path);
                ui.horizontal(|ui| {
                    if ui.button(language.text("Choose folder…")).clicked()
                        && let Some(path) = rfd::FileDialog::new().pick_folder()
                    {
                        draft.path = path.to_string_lossy().into_owned();
                    }
                    if draft.kind == Kind::Watch
                        && ui.button(language.text("Choose file…")).clicked()
                        && let Some(path) = rfd::FileDialog::new().pick_file()
                    {
                        draft.path = path.to_string_lossy().into_owned();
                    }
                });
                if draft.kind == Kind::Workspace {
                    ui.label(
                        language.text("Selected paths, relative to the workspace (one per line)"),
                    );
                    ui.text_edit_multiline(&mut draft.includes);
                    ui.horizontal(|ui| {
                        if ui.button(language.text("Add files…")).clicked()
                            && let Some(files) = rfd::FileDialog::new()
                                .set_directory(&draft.path)
                                .pick_files()
                        {
                            for file in files {
                                if let Ok(relative) = file.strip_prefix(&draft.path) {
                                    draft.includes.push('\n');
                                    draft
                                        .includes
                                        .push_str(&relative.to_string_lossy().replace('\\', "/"));
                                }
                            }
                        }
                        if ui.button(language.text("Add folder…")).clicked()
                            && let Some(file) = rfd::FileDialog::new()
                                .set_directory(&draft.path)
                                .pick_folder()
                            && let Ok(relative) = file.strip_prefix(&draft.path)
                        {
                            draft.includes.push('\n');
                            draft
                                .includes
                                .push_str(&relative.to_string_lossy().replace('\\', "/"));
                        }
                    });
                    ui.label(language.text("Excluded globs (one per line)"));
                    ui.text_edit_multiline(&mut draft.excludes);
                    ui.label(language.text(
                        "Source files are read only. Each workspace has its own backup branch.",
                    ));
                }
                field(ui, language.text("Branch (optional)"), &mut draft.branch);
                if draft.kind != Kind::Workspace {
                    field(
                        ui,
                        language.text("Remote (empty disables watch upload)"),
                        &mut draft.remote,
                    );
                }
                if draft.kind == Kind::Pull {
                    field(
                        ui,
                        language.text("Clone URL (for an absent or empty destination)"),
                        &mut draft.url,
                    );
                    field(
                        ui,
                        language.text("Update interval in seconds"),
                        &mut draft.interval,
                    );
                } else if draft.kind == Kind::Watch {
                    field(
                        ui,
                        language.text("Quiet period in seconds"),
                        &mut draft.delay,
                    );
                }
            });
        if save {
            self.model.send(Command::Save(draft.clone()));
            self.draft = None;
        }
        if cancel {
            self.draft = None;
        }
    }

    fn settings(&mut self, ui: &mut egui::Ui) {
        let language = self.model.language;
        ui.heading(language.text("Workspace backup remote"));
        ui.label(language.text(
            "Enter a URL to replace the configured remote. Fetch only reads remote history.",
        ));
        ui.add(
            egui::TextEdit::singleline(&mut self.remote)
                .password(true)
                .hint_text(language.text("Remote URL")),
        );
        ui.checkbox(
            &mut self.auto_push,
            language.text("Upload after each local backup"),
        );
        ui.add_enabled_ui(!self.model.busy, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !self.remote.is_empty(),
                        egui::Button::new(language.text("Save remote")),
                    )
                    .clicked()
                {
                    self.model
                        .send(Command::Remote(self.remote.clone(), self.auto_push));
                    self.remote.clear();
                }
                if ui.button(language.text("Fetch workspaces")).clicked() {
                    self.model.send(Command::Fetch);
                }
            });
        });
        if !self.model.branches.is_empty() {
            egui::ComboBox::from_label(language.text("Workspace branch"))
                .selected_text(
                    self.model
                        .branches
                        .get(self.import_branch)
                        .map(|b| b.branch())
                        .unwrap_or(language.text("Choose branch")),
                )
                .show_ui(ui, |ui| {
                    for (index, branch) in self.model.branches.iter().enumerate() {
                        ui.selectable_value(
                            &mut self.import_branch,
                            index,
                            format!("{} — {}", branch.branch(), branch.manifest().name()),
                        );
                    }
                });
            field(
                ui,
                language.text("Bind to local directory"),
                &mut self.import_path,
            );
            ui.horizontal(|ui| {
                if ui.button(language.text("Choose directory…")).clicked()
                    && let Some(path) = rfd::FileDialog::new().pick_folder()
                {
                    self.import_path = path.to_string_lossy().into_owned();
                }
                if ui
                    .add_enabled(
                        !self.model.busy && !self.import_path.is_empty(),
                        egui::Button::new(language.text("Import paused")),
                    )
                    .clicked()
                    && let Some(branch) = self.model.branches.get(self.import_branch)
                {
                    self.model.send(Command::Import(
                        branch.branch().into(),
                        self.import_path.clone().into(),
                    ));
                }
            });
            ui.label(language.text("Import creates a local binding. Preview a restore to copy files into your project."));
        }
    }
}

fn field(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.label(label);
    ui.add(egui::TextEdit::singleline(value).desired_width(f32::INFINITY));
}

fn configure_fonts(context: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "gitwatch-cjk".into(),
        egui::FontData::from_static(include_bytes!("../assets/fonts/GitwatchSansSC-Regular.ttf"))
            .into(),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("gitwatch-cjk".into());
    }
    context.set_fonts(fonts);
}

#[cfg(test)]
mod tests {
    use std::{fs, time::Instant};

    use super::*;

    fn settle(app: &mut Desktop) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.model.busy && Instant::now() < deadline {
            app.model.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!app.model.busy);
        assert!(app.model.error.is_none(), "{:?}", app.model.error);
    }

    fn render(
        app: &mut Desktop,
        context: &egui::Context,
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
            |ui| app.paint(ui),
        );
        output.textures_delta.clear();
        output
    }

    fn click(app: &mut Desktop, context: &egui::Context, label: &str) {
        let find = |output: &egui::FullOutput| {
            output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.job.text == label => {
                    Some(text.pos + text.galley.size() / 2.0)
                }
                _ => None,
            })
        };
        let mut output = render(app, context, vec![]);
        let mut position = find(&output);
        // At the minimum window size, history and restore controls are scrollable.
        for _ in 0..20 {
            if position.is_some() {
                break;
            }
            let pointer = output
                .shapes
                .iter()
                .find(|shape| {
                    matches!(&shape.shape, egui::epaint::Shape::Text(text) if text.pos.x > 400.0)
                        && shape.clip_rect.min.y > 200.0
                        && shape.clip_rect.height() > 50.0
                })
                .map(|shape| egui::pos2(600.0, shape.clip_rect.center().y))
                .unwrap_or(egui::pos2(600.0, 340.0));
            output = render(
                app,
                context,
                vec![
                    egui::Event::PointerMoved(pointer),
                    egui::Event::MouseWheel {
                        phase: egui::TouchPhase::Move,
                        unit: egui::MouseWheelUnit::Point,
                        delta: egui::vec2(0.0, -60.0),
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
            position = find(&output);
        }
        let position = position.unwrap_or_else(|| panic!("Missing button: {label}"));
        for pressed in [true, false] {
            render(
                app,
                context,
                vec![
                    egui::Event::PointerMoved(position),
                    egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
    }

    #[test]
    fn desktop_buttons_drive_backup_preview_and_explicit_restore() {
        let temp = tempfile::TempDir::new().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        let file = source.join("notes.md");
        fs::write(&file, "saved").unwrap();
        let mut app = Desktop {
            model: Model::new(Some(temp.path().join("data"))),
            repository: repository::Panel::default(),
            tray: None,
            tray_error: None,
            hide_on_start: false,
            desktop_settings: false,
            selected: None,
            draft: None,
            history: 0,
            restore_files: String::new(),
            confirm: false,
            remove: false,
            remote: String::new(),
            auto_push: false,
            settings: false,
            import_branch: 0,
            import_path: String::new(),
        };
        settle(&mut app);
        app.model.update =
            Some(serde_json::from_value(serde_json::json!({"version":"99.0.0"})).unwrap());
        let context = egui::Context::default();
        theme::apply(&context);
        context.all_styles_mut(|style| style.animation_time = 0.0);
        configure_fonts(&context);
        click(&mut app, &context, "English");
        click(&mut app, &context, "简体中文");
        settle(&mut app);
        assert_eq!(app.model.language, Language::Chinese);
        let preferences = fs::read_to_string(temp.path().join("data/preferences.json")).unwrap();
        assert!(preferences.contains("zh-CN"));
        let language = app.model.language;
        click(&mut app, &context, language.text("Settings"));
        click(&mut app, &context, language.text("Start minimized to tray"));
        settle(&mut app);
        assert!(app.model.start_in_tray);
        app.desktop_settings = false;
        click(&mut app, &context, language.text("+ Add task"));
        let draft = app.draft.as_mut().unwrap();
        draft.name = "Desktop notes".into();
        draft.path = source.to_string_lossy().into_owned();
        draft.includes = "notes.md".into();
        click(&mut app, &context, language.text("Save task"));
        settle(&mut app);
        assert_eq!(app.model.rows.len(), 1);
        let selected = app.model.rows[0].draft.id;
        for _ in 0..2 {
            for action in ["Backup settings", "+ Add task", "Refresh"] {
                click(&mut app, &context, language.text(action));
                settle(&mut app);
                let output = render(&mut app, &context, vec![]);
                assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::epaint::Shape::Text(text) if text.galley.job.text == "Desktop notes")), "Task must remain visible in {action}");
                assert_eq!(app.model.rows[0].draft.id, selected);
            }
            click(&mut app, &context, "Desktop notes");
            assert!(!app.settings && app.draft.is_none());
            assert_eq!(app.selected, selected);
        }
        click(&mut app, &context, language.text("Run once"));
        settle(&mut app);
        click(&mut app, &context, language.text("History"));
        settle(&mut app);
        assert_eq!(app.model.history.len(), 1);
        fs::write(&file, "local").unwrap();
        click(&mut app, &context, language.text("Preview restore"));
        settle(&mut app);
        assert!(app.model.plan.is_some());
        assert_eq!(fs::read_to_string(&file).unwrap(), "local");
        click(
            &mut app,
            &context,
            language.text("Continue to confirmation"),
        );
        assert!(app.confirm);
        assert_eq!(fs::read_to_string(&file).unwrap(), "local");
        click(&mut app, &context, language.text("Confirm restore"));
        settle(&mut app);
        assert_eq!(fs::read_to_string(&file).unwrap(), "saved");
        click(&mut app, &context, "简体中文");
        click(&mut app, &context, "English");
        settle(&mut app);
        assert_eq!(app.model.language, Language::English);
        assert!(
            crate::preferences::Preferences::load(&temp.path().join("data"))
                .unwrap()
                .start_in_tray
        );
        app.model.send(Command::Save(Draft {
            kind: Kind::Pull,
            name: "Another repository".into(),
            path: temp.path().join("checkout").to_string_lossy().into_owned(),
            ..Draft::default()
        }));
        settle(&mut app);
        assert!(!app.model.history.is_empty());
        click(&mut app, &context, "Remove");
        click(&mut app, &context, "Remove binding");
        settle(&mut app);
        render(&mut app, &context, vec![]);
        assert_eq!(app.model.rows.len(), 1);
        assert_eq!(app.selected, app.model.rows[0].draft.id);
        assert!(app.model.history.is_empty() && app.model.plan.is_none());
        assert!(app.model.text.is_empty() && !app.confirm && !app.remove);
    }
}
