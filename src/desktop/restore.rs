//! Guided remote selection and explicit file restoration.

use eframe::egui::{self, Color32};
use uuid::Uuid;

use super::{Icon, field};
use crate::interface::{Command, Kind, Model};

#[derive(PartialEq)]
enum Step {
    Choose,
    Preview,
    Applying,
    Failed,
    Complete,
}

pub(super) struct Flow {
    step: Step,
    branch: usize,
    pub(super) path: String,
    task: Option<Uuid>,
    confirm: bool,
}

impl Flow {
    pub(super) fn new(model: &mut Model, task: Option<Uuid>) -> Self {
        model.plan = None;
        model.text.clear();
        if let Some(id) = task {
            model.send(Command::PreviewRemote(id));
        } else {
            model.branches.clear();
            model.send(Command::Fetch);
        }
        Self {
            step: if task.is_some() {
                Step::Preview
            } else {
                Step::Choose
            },
            branch: 0,
            path: String::new(),
            task,
            confirm: true,
        }
    }

    /// Returns whether the user closed the flow.
    pub(super) fn show(&mut self, context: &egui::Context, model: &mut Model) -> bool {
        if let Some(plan) = &model.plan
            && std::path::Path::new(&self.path) != plan.root()
        {
            self.path = plan.root().to_string_lossy().into_owned();
        }
        if self.step == Step::Applying && !model.busy {
            self.step = if model.error.is_none() {
                Step::Complete
            } else {
                Step::Failed
            };
        }
        let language = model.language;
        let mut close = false;
        egui::Modal::new(egui::Id::new("remote-restore-flow")).show(context, |ui| {
            ui.set_width((context.content_rect().width() - 64.0).clamp(280.0, 660.0));
            ui.heading(language.text("Restore remote backup"));
            ui.label(language.text(match self.step {
                Step::Choose => "1. Choose backup and destination",
                Step::Preview | Step::Applying | Step::Failed => "2. Review and confirm",
                Step::Complete => "3. Restore complete",
            }));
            ui.separator();
            if let Some(error) = &model.error {
                egui::ScrollArea::vertical().id_salt("restore-error").max_height(60.0).show(ui, |ui| {
                    ui.colored_label(Color32::LIGHT_RED, language.error(error));
                });
            }
            if model.busy {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(language.text(match self.step {
                        Step::Choose => "Loading remote backups…",
                        Step::Preview => "Preparing file preview…",
                        _ => "Restoring files…",
                    }));
                });
            }
            match self.step {
                Step::Choose => self.choose(ui, model),
                Step::Preview => {
                    if model.plan.is_some() {
                        if preview(ui, model, &mut self.confirm, true) {
                            self.step = Step::Applying;
                        }
                        if model.plan.is_none() {
                            close = true;
                        }
                    } else if !model.busy {
                        let bound = self.task.filter(|id| model.rows.iter().any(|r| r.draft.id == Some(*id)));
                        if let Some(id) = bound {
                            if ui.button(language.text("Retry preview")).clicked() {
                                model.send(Command::PreviewRemote(id));
                            }
                        } else if ui.button(language.text("Back to selection")).clicked() {
                            self.step = Step::Choose;
                        }
                    }
                }
                Step::Applying => {}
                Step::Failed => {
                    ui.label(language.text("Restore could not finish. Create a new preview before trying again."));
                    if let Some(id) = self.task
                        && ui.button(language.text("Retry preview")).clicked()
                    {
                        model.plan = None;
                        self.confirm = true;
                        self.step = Step::Preview;
                        model.send(Command::PreviewRemote(id));
                    }
                }
                Step::Complete => {
                    ui.label(language.text("The backup files are now in your local directory."));
                    ui.label(language.format("Destination: {0}", &[&self.path]));
                    if let Some(row) = model.rows.iter().find(|row| row.draft.id == self.task) {
                        ui.label(row.status.render(language));
                    }
                    ui.label(language.text("The task is stopped. Start it when you are ready to back up local changes."));
                    if ui.button(language.text("Back to task")).clicked() {
                        close = true;
                    }
                }
            }
            if self.step == Step::Choose || self.step == Step::Failed || (self.step == Step::Preview && model.plan.is_none()) {
                ui.separator();
                if self.step != Step::Failed {
                    ui.small(language.text("Files are written only after you confirm. A new task stays stopped until you start it."));
                }
                if ui.add_enabled(!model.busy, egui::Button::new(language.text("Cancel"))).clicked() {
                    close = true;
                }
            }
        });
        if close {
            model.plan = None;
            model.text.clear();
        }
        close
    }

    fn choose(&mut self, ui: &mut egui::Ui, model: &mut Model) {
        let language = model.language;
        let mut command = None;
        ui.add_enabled_ui(!model.busy, |ui| {
            if let Some(remote) = &model.remote {
                ui.label(remote);
            }
            if model.branches.is_empty() && !model.busy && model.error.is_none() {
                ui.label(
                    language
                        .text("No backups found. Upload a backup from your other computer first."),
                );
            }
            let previous = self.branch;
            egui::ComboBox::from_label(language.text("Backup"))
                .selected_text(
                    model
                        .branches
                        .get(self.branch)
                        .map(|b| b.manifest().name())
                        .unwrap_or(language.text("Choose branch")),
                )
                .show_ui(ui, |ui| {
                    for (index, branch) in model.branches.iter().enumerate() {
                        ui.selectable_value(
                            &mut self.branch,
                            index,
                            format!("{} ({})", branch.manifest().name(), branch.branch()),
                        );
                    }
                });
            let selected = model.branches.get(self.branch);
            let bound = selected.and_then(|branch| {
                model
                    .rows
                    .iter()
                    .find(|row| row.draft.id == Some(branch.manifest().id()))
            });
            if previous != self.branch {
                self.path.clear();
            }
            if let Some(row) = bound {
                self.path.clone_from(&row.draft.path);
            }
            ui.add_enabled_ui(bound.is_none(), |ui| {
                field(ui, language.text("Destination folder"), &mut self.path);
                if ui
                    .add(Icon::Folder.button(language.text("Choose directory…")))
                    .clicked()
                    && let Some(path) = rfd::FileDialog::new().pick_folder()
                {
                    self.path = path.to_string_lossy().into_owned();
                }
            });
            let running = bound.is_some_and(|row| row.running);
            let conflicting = bound.is_some_and(|row| {
                row.draft.kind != Kind::Workspace || selected.is_some_and(|branch| row.draft.branch != branch.branch())
            });
            if running {
                ui.label(language.text("Stop this task before restoring."));
            }
            if conflicting {
                ui.label(language.text("This task is already linked differently. Open it in the task list to review its settings."));
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        selected.is_some() && !self.path.trim().is_empty() && !running && !conflicting,
                        Icon::Preview.button(language.text("Preview restore")),
                    )
                    .clicked()
                    && let Some(branch) = selected
                {
                    self.task = Some(branch.manifest().id());
                    self.step = Step::Preview;
                    self.confirm = true;
                    command = Some(if bound.is_some() {
                        Command::PreviewRemote(branch.manifest().id())
                    } else {
                        Command::ImportPreview(branch.branch().into(), self.path.clone().into())
                    });
                }
                if ui.button(language.text("Refresh remote backups")).clicked() {
                    command = Some(Command::Fetch);
                }
            });
        });
        if let Some(command) = command {
            model.send(command);
        }
    }
}

/// Returns whether the user confirmed a restore.
pub(super) fn preview(
    ui: &mut egui::Ui,
    model: &mut Model,
    confirm: &mut bool,
    stopped: bool,
) -> bool {
    let language = model.language;
    let Some(plan) = &model.plan else {
        return false;
    };
    ui.label(language.text("Preview only — no files have been written."));
    ui.label(language.format("Destination: {0}", &[&plan.root().display().to_string()]));
    ui.label(language.format("Version: {0}", &[plan.commit()]));
    let mut counts = [0usize; 3];
    for entry in plan.entries() {
        use crate::workspace::Change;
        counts[match entry.change() {
            Change::Add => 0,
            Change::Replace => 1,
            Change::Unchanged => 2,
        }] += 1;
    }
    ui.label(language.format(
        "{0} new · {1} replaced · {2} unchanged",
        &[
            &counts[0].to_string(),
            &counts[1].to_string(),
            &counts[2].to_string(),
        ],
    ));
    if plan.is_remote() {
        ui.label(language.text("Use the previewed remote contents. Both backup histories and extra local files are kept. Upload separately to share the result."));
    }
    let mut command = None;
    egui::ScrollArea::vertical()
        .id_salt("restore-files")
        .max_height((ui.ctx().content_rect().height() * 0.14).clamp(40.0, 100.0))
        .show(ui, |ui| {
            for entry in plan.entries() {
                if ui
                    .add_enabled(
                        !model.busy,
                        egui::Button::new(format!(
                            "{}  {}",
                            language.text(&format!("{:?}", entry.change())),
                            entry.path()
                        )),
                    )
                    .clicked()
                {
                    command = Some(Command::Contents(plan.id(), entry.path().into()));
                }
            }
        });
    if !model.text.is_empty() {
        egui::ScrollArea::vertical()
            .id_salt("restore-contents")
            .max_height(60.0)
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut model.text.render(language))
                        .interactive(false)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(f32::INFINITY),
                );
            });
    }
    if *confirm {
        ui.colored_label(Color32::YELLOW, language.text("This replaces the previewed files. Current files are saved in the local recovery directory first. Extra files are kept."));
        if ui
            .add_enabled(
                !model.busy && stopped,
                Icon::Restore.button(language.text("Confirm restore")),
            )
            .clicked()
        {
            command = Some(Command::Restore(plan.id()));
            *confirm = false;
        }
    } else if ui
        .add_enabled(
            stopped && !model.busy,
            Icon::Restore.button(language.text("Continue to confirmation")),
        )
        .clicked()
    {
        *confirm = true;
    }
    let applying = matches!(command, Some(Command::Restore(_)));
    if let Some(command) = command {
        model.send(command);
    }
    if !applying
        && ui
            .add_enabled(
                !model.busy,
                Icon::Close.button(language.text("Close preview")),
            )
            .clicked()
    {
        model.plan = None;
        *confirm = false;
    }
    applying
}
