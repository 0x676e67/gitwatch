use std::{
    path::Path,
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

use eframe::egui::{self, Color32, RichText};

use super::{icons::Icon, theme};
use crate::{Result, i18n::Language, repository::Summary, watch::StopToken};

type Key = (String, String);

/// Keeps one cancellable scan in flight; stale selections never replace the view.
#[derive(Default)]
pub(super) struct Panel {
    key: Option<Key>,
    pending: Option<(Key, Receiver<Result<Summary>>, StopToken)>,
    summary: Option<Summary>,
    error: Option<String>,
    updated: Option<Instant>,
}

impl Panel {
    pub fn refresh(&mut self) {
        self.updated = None;
    }

    pub fn show(&mut self, ui: &mut egui::Ui, path: &str, remote: &str, language: Language) {
        if self
            .key
            .as_ref()
            .is_none_or(|(previous_path, previous_remote)| {
                previous_path != path || previous_remote != remote
            })
        {
            if let Some((_, _, stop)) = &self.pending {
                stop.stop();
            }
            self.key = Some((path.to_owned(), remote.to_owned()));
            self.summary = None;
            self.error = None;
            self.updated = None;
        }
        self.poll();
        if self.pending.is_none()
            && self
                .updated
                .is_none_or(|time| time.elapsed() >= Duration::from_secs(60))
        {
            self.start((path.to_owned(), remote.to_owned()), ui.ctx().clone());
        }
        ui.horizontal(|ui| {
            ui.strong(language.text("Repository"));
            if ui
                .add(Icon::Refresh.button(language.text("Refresh information")))
                .clicked()
            {
                self.refresh();
            }
            if self.pending.is_some() {
                ui.spinner();
            }
        });
        if let Some(error) = &self.error {
            ui.weak(language.text("Repository information unavailable"))
                .on_hover_text(error);
        }
        let Some(summary) = &self.summary else {
            return;
        };
        egui::Frame::new().fill(theme::SURFACE).corner_radius(10).inner_margin(12).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.vertical(|ui| {
                    if let Some(remote) = &summary.remote {
                        let title = remote.github.as_ref().map(|(owner, name)| format!("{owner}/{name}"));
                        ui.hyperlink_to(RichText::new(title.as_deref().unwrap_or(&remote.url)).strong(), &remote.url);
                    } else {
                        ui.strong(summary.root.file_name().unwrap_or_default().to_string_lossy());
                    }
                    ui.weak(if summary.branch.is_empty() { language.text("Detached HEAD") } else { &summary.branch });
                });
            });
            ui.horizontal_wrapped(|ui| {
                for (label, value) in [("Commits", summary.commits.to_string()), ("Contributors", summary.contributors.len().to_string()), ("Local branches", summary.branches.to_string()), ("Tags", summary.tags.to_string()), ("Committed files", summary.files.to_string())] {
                    ui.label(format!("{}  {value}", language.text(label)));
                }
            });
            if summary.shallow { ui.colored_label(Color32::YELLOW, language.text("Shallow clone: history statistics are incomplete.")); }
            ui.small(language.text("Commits and contributors: local HEAD, including merges."));
            if let Some(commit) = &summary.latest {
                ui.separator();
                ui.strong(language.text("Latest commit"));
                ui.label(&commit.subject);
                ui.horizontal_wrapped(|ui| {
                    if let Some(remote) = &summary.remote && remote.github.is_some() {
                        ui.hyperlink_to(commit.id.get(..12).unwrap_or(&commit.id), format!("{}/commit/{}", remote.url.trim_end_matches('/'), commit.id));
                    } else { ui.monospace(commit.id.get(..12).unwrap_or(&commit.id)); }
                    ui.label(&commit.author);
                    ui.weak(&commit.time);
                });
            }
            ui.separator();
            let total: usize = summary.lines.iter().fold(0usize, |total, line| total.saturating_add(line.text));
            ui.label(format!("{}  {total}", language.text("Text lines")));
            let colors = [theme::ACCENT, Color32::from_rgb(112, 168, 245), Color32::from_rgb(224, 180, 115), Color32::from_rgb(181, 143, 228), Color32::from_rgb(228, 143, 160), theme::MUTED];
            if total > 0 {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 8.0), egui::Sense::hover());
                let mut left = rect.left();
                for (index, line) in summary.lines.iter().enumerate() {
                    let width = rect.width() * (line.text as f32 / total as f32);
                    ui.painter().rect_filled(egui::Rect::from_min_size(egui::pos2(left, rect.top()), egui::vec2(width, rect.height())), 0, colors[index % colors.len()]);
                    left += width;
                }
            }
            ui.horizontal_wrapped(|ui| {
                for (index, line) in summary.lines.iter().take(6).enumerate() {
                    let percentage = if total == 0 { 0.0 } else { line.text as f64 / total as f64 * 100.0 };
                    ui.colored_label(colors[index % colors.len()], format!("{} {percentage:.1}%", line.file_type));
                }
            });
            egui::CollapsingHeader::new(language.text("File types")).show(ui, |ui| {
                egui::Grid::new("language-counts").striped(true).show(ui, |ui| {
                    for label in ["File type", "Files", "Text lines"] { ui.strong(language.text(label)); } ui.end_row();
                    for line in &summary.lines {
                        ui.label(&line.file_type);
                        for value in [line.files, line.text] { ui.label(value.to_string()); }
                        ui.end_row();
                    }
                });
            });
            ui.small(language.text("Git counts committed text lines at local HEAD, including comments and blanks. Groups use file extensions."));
            if summary.skipped > 0 { ui.small(language.format("Binary files excluded from line counts: {0}.", &[&summary.skipped.to_string()])); }
            egui::CollapsingHeader::new(language.text("Top contributors")).show(ui, |ui| {
                for (author, count) in summary.contributors.iter().take(5) {
                    ui.label(format!("{author}  ·  {count}"));
                }
                ui.small(language.text("Authors are grouped using Git mailmap."));
            });
        });
    }

    fn start(&mut self, key: Key, context: egui::Context) {
        let (sender, receiver) = mpsc::channel();
        let stop = StopToken::default();
        self.pending = Some((key.clone(), receiver, stop.clone()));
        thread::spawn(move || {
            let summary = Summary::read(Path::new(&key.0), &key.1, &stop);
            if sender.send(summary).is_err() {
                return;
            }
            context.request_repaint();
        });
    }

    fn poll(&mut self) {
        let Some((key, receiver, stop)) = &self.pending else {
            return;
        };
        loop {
            match receiver.try_recv() {
                Ok(result) if self.key.as_ref() == Some(key) && !stop.is_stopped() => {
                    self.updated = Some(Instant::now());
                    match result {
                        Ok(summary) => {
                            self.summary = Some(summary);
                            self.error = None;
                        }
                        Err(error) => {
                            self.summary = None;
                            self.error = Some(format!("{error:#}"));
                        }
                    }
                }
                Ok(_) => {}
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending = None;
                    break;
                }
            }
        }
    }
}

impl Drop for Panel {
    fn drop(&mut self) {
        if let Some((_, _, stop)) = &self.pending {
            stop.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returning_to_a_previous_selection_does_not_accept_its_cancelled_scan() {
        let key = ("repository".into(), "origin".into());
        let (sender, receiver) = mpsc::channel();
        let stop = StopToken::default();
        stop.stop();
        let mut panel = Panel {
            key: Some(key.clone()),
            pending: Some((key, receiver, stop)),
            summary: None,
            error: None,
            updated: None,
        };
        sender.send(Err(anyhow::anyhow!("cancelled"))).unwrap();
        drop(sender);
        panel.poll();
        assert!(panel.pending.is_none());
        assert!(panel.updated.is_none() && panel.error.is_none());
    }
}
