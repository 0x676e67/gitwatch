use std::{
    io::{Cursor, Read},
    path::Path,
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

use anyhow::ensure;
use eframe::egui::{self, Color32, RichText};

use super::theme;
use crate::{Result, i18n::Language, repository::Summary, watch::StopToken};

type Key = (String, String);

enum Message {
    Summary(Result<Box<Summary>>),
    Avatar(egui::ColorImage),
}

/// Keeps one cancellable scan in flight; stale selections never replace the view.
#[derive(Default)]
pub(super) struct Panel {
    key: Option<Key>,
    pending: Option<(Key, Receiver<Message>, StopToken)>,
    summary: Option<Summary>,
    error: Option<String>,
    avatar: Option<egui::TextureHandle>,
    updated: Option<Instant>,
}

impl Panel {
    pub fn refresh(&mut self) {
        self.updated = None;
    }

    pub fn show(&mut self, ui: &mut egui::Ui, path: &str, remote: &str, language: Language) {
        let key = (path.to_owned(), remote.to_owned());
        if self.key.as_ref() != Some(&key) {
            if let Some((_, _, stop)) = &self.pending {
                stop.stop();
            }
            self.key = Some(key.clone());
            self.summary = None;
            self.error = None;
            self.avatar = None;
            self.updated = None;
        }
        self.poll(ui.ctx());
        if self.pending.is_none()
            && self
                .updated
                .is_none_or(|time| time.elapsed() >= Duration::from_secs(60))
        {
            self.start(key, ui.ctx().clone());
        }
        ui.horizontal(|ui| {
            ui.strong(language.text("Repository"));
            if ui
                .small_button(language.text("Refresh information"))
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
                if let Some(avatar) = &self.avatar {
                    ui.add(egui::Image::new(avatar).fit_to_exact_size(egui::vec2(40.0, 40.0)).corner_radius(20))
                        .on_hover_text(language.text("Repository owner on GitHub"));
                }
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
            let total: usize = summary.lines.iter().map(|line| line.text).sum();
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
                    ui.colored_label(colors[index % colors.len()], format!("{} {percentage:.1}%", line.language));
                }
            });
            egui::CollapsingHeader::new(language.text("File types")).show(ui, |ui| {
                egui::Grid::new("language-counts").striped(true).show(ui, |ui| {
                    for label in ["File type", "Files", "Text lines"] { ui.strong(language.text(label)); } ui.end_row();
                    for line in &summary.lines {
                        ui.label(&line.language);
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
            let owner = summary
                .as_ref()
                .ok()
                .and_then(|summary| summary.remote.as_ref())
                .and_then(|remote| remote.github.as_ref())
                .map(|(owner, _)| owner.clone());
            if sender
                .send(Message::Summary(summary.map(Box::new)))
                .is_err()
            {
                return;
            }
            context.request_repaint();
            if !stop.is_stopped()
                && let Some(owner) = owner
                && let Ok(image) = avatar(&owner)
            {
                let _ = sender.send(Message::Avatar(image));
                context.request_repaint();
            }
        });
    }

    fn poll(&mut self, context: &egui::Context) {
        let Some((key, receiver, stop)) = &self.pending else {
            return;
        };
        loop {
            match receiver.try_recv() {
                Ok(message) if self.key.as_ref() == Some(key) && !stop.is_stopped() => {
                    match message {
                        Message::Summary(result) => {
                            self.updated = Some(Instant::now());
                            match result {
                                Ok(summary) => {
                                    if self
                                        .summary
                                        .as_ref()
                                        .and_then(|previous| previous.remote.as_ref())
                                        != summary.remote.as_ref()
                                    {
                                        self.avatar = None;
                                    }
                                    self.summary = Some(*summary);
                                    self.error = None;
                                }
                                Err(error) => {
                                    self.summary = None;
                                    self.avatar = None;
                                    self.error = Some(format!("{error:#}"));
                                }
                            }
                        }
                        Message::Avatar(image) => {
                            self.avatar = Some(context.load_texture(
                                "repository-owner",
                                image,
                                egui::TextureOptions::LINEAR,
                            ))
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

fn avatar(owner: &str) -> Result<egui::ColorImage> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(3)
        .timeout_global(Some(Duration::from_secs(5)))
        .build()
        .into();
    let mut response = agent
        .get(format!("https://github.com/{owner}.png?size=96"))
        .call()?;
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 1024 * 1024, "Avatar is too large");
    let mut reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(512);
    limits.max_image_height = Some(512);
    limits.max_alloc = Some(4 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode()?.to_rgba8();
    Ok(egui::ColorImage::from_rgba_unmultiplied(
        [image.width() as usize, image.height() as usize],
        image.as_raw(),
    ))
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
            avatar: None,
            updated: None,
        };
        sender
            .send(Message::Summary(Err(anyhow::anyhow!("cancelled"))))
            .unwrap();
        drop(sender);
        panel.poll(&egui::Context::default());
        assert!(panel.pending.is_none());
        assert!(panel.updated.is_none() && panel.error.is_none());
    }
}
