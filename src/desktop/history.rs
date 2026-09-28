//! Saved versions with a separate preview before any restore.

use eframe::egui::{self, Color32};

use super::{Desktop, Icon};
use crate::interface::{Command, lines};

impl Desktop {
    pub(super) fn history_window(&mut self, context: &egui::Context) {
        let language = self.model.language;
        let Some(row) = self
            .model
            .rows
            .iter()
            .find(|row| row.draft.id == self.selected)
        else {
            self.history_open = false;
            return;
        };
        let Some(id) = row.draft.id else { return };
        let running = row.running;
        let mut command = None;
        let response = egui::Modal::new(egui::Id::new("backup-history")).show(context, |ui| {
            ui.set_width((context.content_rect().width() - 64.0).clamp(280.0, 660.0));
            ui.heading(language.text("History"));
            ui.add(egui::Label::new(&row.draft.name).truncate())
                .on_hover_text(&row.draft.name);
            if self.model.busy {
                ui.spinner();
            }
            if let Some(error) = &self.model.error {
                egui::ScrollArea::vertical()
                    .id_salt("history-error")
                    .max_height(40.0)
                    .show(ui, |ui| {
                        ui.colored_label(Color32::LIGHT_RED, language.error(error));
                    });
            }
            if let Some(entry) = self.model.history.get(self.history) {
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(
                            !self.model.busy && !running,
                            Icon::Preview.button(language.text("Preview restore")),
                        )
                        .clicked()
                    {
                        command = Some(Command::Preview(
                            id,
                            entry.commit().into(),
                            lines(&self.restore_files),
                        ));
                        self.confirm = false;
                    }
                    if let Some(older) = self.model.history.get(self.history + 1)
                        && ui
                            .add_enabled(
                                !self.model.busy,
                                Icon::Diff.button(language.text("Diff to previous")),
                            )
                            .clicked()
                    {
                        command = Some(Command::Diff(
                            id,
                            older.commit().into(),
                            entry.commit().into(),
                        ));
                    }
                });
                ui.label(language.text("Restore selected paths (one per line; empty means all)"));
                egui::ScrollArea::vertical()
                    .id_salt("history-paths")
                    .max_height(60.0)
                    .show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut self.restore_files)
                                .desired_rows(2)
                                .desired_width(f32::INFINITY),
                        );
                    });
                if running {
                    ui.small(language.text("Stop this task before restoring."));
                }
            } else if !self.model.busy {
                ui.label(language.text("No saved versions yet. Run a backup first."));
            }
            egui::ScrollArea::vertical()
                .id_salt("history-versions")
                .max_height(100.0)
                .show(ui, |ui| {
                    for (index, entry) in self.model.history.iter().enumerate() {
                        if ui
                            .selectable_label(
                                self.history == index,
                                format!(
                                    "{}  {}",
                                    entry.commit().get(..12).unwrap_or(entry.commit()),
                                    entry.summary()
                                ),
                            )
                            .clicked()
                        {
                            self.history = index;
                            self.model.text.clear();
                        }
                    }
                });
            if !self.model.text.is_empty() {
                egui::ScrollArea::vertical()
                    .id_salt("history-comparison")
                    .max_height(60.0)
                    .show(ui, |ui| {
                        ui.monospace(self.model.text.render(language));
                    });
            }
            if let Some(message) = &self.model.feedback {
                ui.add(egui::Label::new(message.render(language)).truncate())
                    .on_hover_text(message.render(language));
            }
            if ui
                .add_enabled(
                    !self.model.busy,
                    Icon::Close.button(language.text("Back to task")),
                )
                .clicked()
            {
                self.history_open = false;
                self.model.text.clear();
            }
        });
        if let Some(command) = command {
            self.model.send(command);
        }
        if !self.model.busy && response.should_close() {
            self.history_open = false;
            self.model.text.clear();
        }
    }
}
