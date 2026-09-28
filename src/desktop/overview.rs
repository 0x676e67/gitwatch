//! Task progress and saved backup results, independent of diagnostic logs.

use eframe::egui::{self, Color32, RichText};

use super::{pull_countdown, theme};
use crate::{
    i18n::Text,
    interface::{Kind, Model, Row},
    workspace::UploadState,
};

pub(super) fn once_label(kind: Kind, auto_push: bool) -> &'static str {
    match kind {
        Kind::Workspace if auto_push => "Back up & upload",
        Kind::Workspace => "Back up now",
        Kind::Watch => "Commit changes",
        Kind::Pull => "Pull now",
    }
}

pub(super) fn status_color(text: &Text) -> Color32 {
    match text {
        Text::Error(_) => Color32::LIGHT_RED,
        Text::Message(_, values)
            if values
                .iter()
                .any(|value| status_color(value) == Color32::LIGHT_RED) =>
        {
            Color32::LIGHT_RED
        }
        _ => theme::MUTED,
    }
}

pub(super) fn has_activity(text: &Text) -> bool {
    !matches!(text, Text::Message("Running" | "Stopped", _))
}

pub(super) fn show(ui: &mut egui::Ui, row: &Row, model: &Model) {
    let language = model.language;
    egui::Frame::new()
        .fill(theme::SURFACE)
        .corner_radius(10)
        .inner_margin(12)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.strong(language.text("Current status"));
                ui.colored_label(
                    if row.running {
                        theme::ACCENT
                    } else {
                        theme::MUTED
                    },
                    language.text(if row.running {
                        "Automatic runs on"
                    } else {
                        "Automatic runs off"
                    }),
                );
            });
            if row.draft.kind == Kind::Pull && row.running {
                let next = row
                    .draft
                    .id
                    .and_then(|id| model.pull_schedule.get(&id))
                    .copied()
                    .flatten();
                ui.label(pull_countdown(next, language));
            }
            if has_activity(&row.status) {
                ui.add(
                    egui::Label::new(
                        RichText::new(row.status.render(language)).color(status_color(&row.status)),
                    )
                    .truncate(),
                )
                .on_hover_text(row.status.render(language));
            }
            if row.draft.kind == Kind::Workspace {
                ui.separator();
                ui.strong(language.text("Latest saved backup"));
                if let Some(report) = &row.backup {
                    ui.label(language.format(
                        "Files: {0} · {1}",
                        &[
                            &report.files().to_string(),
                            report.commit().get(..12).unwrap_or(report.commit()),
                        ],
                    ));
                    let (label, color) = match report.upload() {
                        UploadState::Disabled => ("Saved on this computer", theme::MUTED),
                        UploadState::Pending => {
                            ("Saved locally · not uploaded yet", Color32::YELLOW)
                        }
                        UploadState::Synced => ("Saved locally · uploaded", theme::ACCENT),
                        UploadState::Failed { .. } => {
                            ("Saved locally · upload failed", Color32::LIGHT_RED)
                        }
                    };
                    ui.colored_label(color, language.text(label));
                    if let UploadState::Failed { message } = report.upload() {
                        ui.add(egui::Label::new(language.error(message)).truncate())
                            .on_hover_text(language.error(message));
                        ui.small(language.text("Retry with Upload saved backup in More actions."));
                    }
                } else {
                    ui.label(language.text("No saved backup result available."));
                }
            }
        });
    ui.add_space(6.0);
    ui.small(language.text(match row.draft.kind {
        Kind::Workspace if model.auto_push && model.remote.is_some() => "Selected files are saved to a separate backup repository, then uploaded. Source files stay unchanged.",
        Kind::Workspace => "Selected files are saved to a separate local backup repository. Source files stay unchanged.",
        Kind::Watch => "Changes are committed in this Git repository. Upload follows the task's remote setting.",
        Kind::Pull => "Pull updates this local repository using the selected strategy.",
    }));
    ui.add_space(8.0);
}
