//! Task-scoped synchronization settings, previews and conflict resolution.

use eframe::egui::{self, Color32};
use uuid::Uuid;

use crate::{
    interface::{Command, Model},
    pull::PullStrategy,
    workspace::{SyncChange, SyncOptions, SyncPhase},
};

pub(super) struct Flow {
    task: Uuid,
    name: String,
    path: String,
    options: SyncOptions,
    interval: String,
    loaded: bool,
    token: Option<Uuid>,
    acknowledged: bool,
    selected: Option<String>,
    editor: String,
    resolved: bool,
    editing: bool,
}

impl Flow {
    pub(super) fn new(model: &mut Model, task: Uuid, name: String, path: String) -> Self {
        model.sync = None;
        model.sync_contents = None;
        model.send(Command::SyncStatus(task));
        Self {
            task,
            name,
            path,
            options: SyncOptions::default(),
            interval: "60".into(),
            loaded: false,
            token: None,
            acknowledged: false,
            selected: None,
            editor: String::new(),
            resolved: false,
            editing: false,
        }
    }

    /// Keeps confirmation outside the scrolling contents, including on small windows.
    pub(super) fn show(&mut self, context: &egui::Context, model: &mut Model) -> bool {
        let language = model.language;
        let status = model
            .sync
            .as_ref()
            .filter(|s| s.workspace == self.task)
            .cloned();
        if let Some(status) = &status {
            if !self.loaded {
                self.options = status.options.clone();
                self.interval = status.options.interval.to_string();
                self.loaded = true;
            }
            if self.token != status.token {
                self.token = status.token;
                self.acknowledged = false;
                self.selected = None;
                model.sync_contents = None;
                self.editing = false;
            }
        }
        let mut close = false;
        let response = egui::Modal::new(egui::Id::new("two-way-sync-flow")).show(context, |ui| {
            ui.set_width((context.content_rect().width() - 64.0).clamp(280.0, 660.0));
            ui.heading(language.text("Two-way sync"));
            ui.add(egui::Label::new(&self.name).truncate()).on_hover_text(&self.name);
            ui.add(egui::Label::new(&self.path).truncate()).on_hover_text(&self.path);
            ui.separator();
            if let Some(error) = &model.error {
                egui::ScrollArea::vertical().id_salt("sync-error").max_height(48.0).show(ui, |ui| { ui.colored_label(Color32::LIGHT_RED, language.error(error)); });
            }
            if model.busy { ui.horizontal(|ui| { ui.spinner(); ui.label(language.text("Working…")); }); }
            let Some(status) = status else {
                if !model.busy && ui.button(language.text("Retry")).clicked() { model.send(Command::SyncStatus(self.task)); }
                if ui.add_enabled(!model.busy, egui::Button::new(language.text("Close"))).clicked() { close = true; }
                return;
            };
            let reserved = if status.phase == SyncPhase::Conflict { 400.0 } else { 320.0 };
            let content_height = (context.content_rect().height() - reserved).clamp(60.0, 370.0);
            ui.add_enabled_ui(!model.busy, |ui| {
                egui::ScrollArea::vertical().id_salt("sync-content").max_height(content_height).show(ui, |ui| {
                    match status.phase {
                        SyncPhase::Ready | SyncPhase::Disabled => {
                            ui.label(language.text(if status.enabled { "Selected files are synchronized in both directions." } else { "File backup only. Source files are not changed automatically." }));
                            ui.label(language.text("Two-way sync can replace and delete selected files. Other files and the source Git repository are kept."));
                            ui.add_space(8.0);
                            ui.label(language.text("Integration strategy"));
                            egui::ComboBox::from_id_salt("sync-strategy").selected_text(self.options.strategy.label(language)).show_ui(ui, |ui| {
                                for strategy in [PullStrategy::Rebase, PullStrategy::Merge, PullStrategy::FastForwardOnly] {
                                    ui.selectable_value(&mut self.options.strategy, strategy, strategy.label(language));
                                }
                            });
                            ui.small(self.options.strategy.description(language));
                            ui.label(language.text("Remote check interval, seconds"));
                            ui.text_edit_singleline(&mut self.interval);
                            if status.enabled {
                                ui.separator();
                                ui.label(language.text("Need to discard local changes? Preview the remote version before replacing files."));
                                if ui.button(language.text("Use remote version…")).clicked() {
                                    model.send(Command::SyncPreview(self.task, status.options.clone(), true));
                                }
                                if status.recovery.is_some() && ui.button(language.text("Undo last sync…")).clicked() { model.send(Command::SyncUndo(self.task)); }
                            }
                            if let Some(recovery) = status.recovery() { ui.small(format!("{}: {}", language.text("Recovery copies"), recovery.display())); }
                        }
                        SyncPhase::Preview | SyncPhase::Applying => {
                            ui.label(language.text(if status.phase == SyncPhase::Applying { "Application was interrupted. Resume the saved operation; new local edits will be protected." } else { "Review file changes. Nothing has been written yet." }));
                            if status.entries.is_empty() { ui.label(language.text("Files already match. Confirm this alignment.")); }
                            for entry in &status.entries {
                                let label = match entry.change() { SyncChange::Add => "Add", SyncChange::Replace => "Replace", SyncChange::Delete => "Delete" };
                                ui.horizontal(|ui| { ui.label(language.text(label)); ui.add(egui::Label::new(entry.path()).truncate()).on_hover_text(entry.path()); });
                            }
                        }
                        SyncPhase::Conflict => {
                            ui.colored_label(Color32::YELLOW, language.text("Sync paused. Your source files have not been overwritten."));
                            if status.conflicts.is_empty() {
                                ui.label(language.text("History needs attention. Continue a resolved integration, cancel to change strategy, or preview the remote version."));
                            }
                            for path in &status.conflicts {
                                if ui.selectable_label(self.selected.as_ref() == Some(path), path).clicked() {
                                    self.selected = Some(path.clone());
                                    self.resolved = false;
                                    self.editing = false;
                                    self.editor.clear();
                                    model.sync_contents = None;
                                    model.send(Command::SyncContents(self.task, path.clone()));
                                }
                            }
                            if let Some(path) = self.selected.clone() && let Some(contents) = model.sync_contents.as_ref() {
                                let local = &contents.local;
                                let remote = &contents.remote;
                                ui.separator();
                                ui.label(&path);
                                egui::CollapsingHeader::new(language.text("Common ancestor")).show(ui, |ui| { ui.label(contents.base.as_deref().map(|bytes| std::str::from_utf8(bytes).map(|text| text.chars().take(2000).collect::<String>()).unwrap_or_else(|_| "[binary]".into())).unwrap_or_else(|| "[deleted]".into())); });
                                ui.columns(2, |columns| {
                                    for (column, label, contents) in [(0, "Local version", &local), (1, "Remote version", &remote)] {
                                        columns[column].strong(language.text(label));
                                        let text = contents.as_deref().map(|bytes| std::str::from_utf8(bytes).map(|s| s.chars().take(2000).collect::<String>()).unwrap_or_else(|_| language.text("Binary file").to_owned())).unwrap_or_else(|| language.text("(missing)").to_owned());
                                        columns[column].label(text);
                                    }
                                });
                                if self.editing {
                                    ui.add(egui::TextEdit::multiline(&mut self.editor).desired_rows(5).desired_width(f32::INFINITY));
                                }
                            }
                        }
                    }
                });
                ui.separator();
                match status.phase {
                    SyncPhase::Preview | SyncPhase::Applying => {
                        ui.checkbox(&mut self.acknowledged, language.text("I reviewed these changes. Keep recovery copies and apply them."));
                        ui.horizontal_wrapped(|ui| {
                            if let Some(token) = status.token && ui.add_enabled(self.acknowledged, egui::Button::new(language.text("Confirm sync changes"))).clicked() { model.send(Command::SyncConfirm(self.task, token)); }
                            if status.phase == SyncPhase::Preview && ui.button(language.text("Cancel operation")).clicked() { model.send(Command::SyncCancel(self.task)); }
                        });
                    }
                    SyncPhase::Conflict => {
                        self.resolution_controls(ui, model);
                        ui.horizontal_wrapped(|ui| {
                            if ui.add_enabled(status.conflicts.is_empty(), egui::Button::new(language.text("Continue integration"))).clicked() { model.send(Command::SyncContinue(self.task)); }
                            if ui.button(language.text("Use remote version…")).clicked() { model.send(Command::SyncPreview(self.task, status.options.clone(), true)); }
                            if ui.button(language.text("Cancel operation")).clicked() { model.send(Command::SyncCancel(self.task)); }
                        });
                    }
                    SyncPhase::Ready | SyncPhase::Disabled => {
                        let interval = self.interval.parse::<u64>().ok().filter(|seconds| (10..=86400).contains(seconds));
                        if interval.is_none() { ui.colored_label(Color32::LIGHT_RED, language.text("Enter an interval from 10 to 86400 seconds.")); }
                        ui.horizontal_wrapped(|ui| {
                            if ui.add_enabled(interval.is_some(), egui::Button::new(language.text(if status.enabled { "Save sync settings" } else { "Preview first sync" }))).clicked() && let Some(interval) = interval {
                                let options = self.options.clone().interval(interval);
                                model.send(if status.enabled { Command::SyncConfigure(self.task, options, true) } else { Command::SyncPreview(self.task, options, false) });
                            }
                            if status.enabled {
                                if ui.button(language.text("Start automatic runs")).clicked() { model.send(Command::Start(self.task)); close = true; }
                                if ui.button(language.text("Disable two-way sync")).clicked() { model.send(Command::SyncConfigure(self.task, status.options.clone(), false)); }
                            }
                        });
                    }
                }
                if ui.button(language.text("Close")).clicked() { close = true; }
            });
        });
        close || (!model.busy && response.should_close())
    }
    fn resolution_controls(&mut self, ui: &mut egui::Ui, model: &mut Model) {
        let language = model.language;
        let Some(path) = self.selected.clone() else {
            return;
        };
        let Some(contents) = &model.sync_contents else {
            return;
        };
        let mut action = None;
        ui.horizontal_wrapped(|ui| {
            if ui.button(language.text("Keep local file")).clicked() {
                action = Some(Command::SyncResolve(
                    self.task,
                    path.clone(),
                    contents.local.clone(),
                ));
                self.selected = None;
            }
            if ui.button(language.text("Keep remote file")).clicked() {
                action = Some(Command::SyncResolve(
                    self.task,
                    path.clone(),
                    contents.remote.clone(),
                ));
                self.selected = None;
            }
            if ui.button(language.text("Edit merged text")).clicked() {
                if let Some(bytes) = contents.local.as_deref().or(contents.remote.as_deref())
                    && bytes.len() <= 256 * 1024
                    && let Ok(text) = std::str::from_utf8(bytes)
                {
                    self.editor = text.to_owned();
                    self.editing = true;
                } else {
                    model.error = Some(
                        "Binary or large files must use an explicit local or remote version".into(),
                    );
                }
            }
        });
        if self.editing {
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(
                    &mut self.resolved,
                    language.text("I have resolved this file"),
                );
                if ui
                    .add_enabled(
                        self.resolved,
                        egui::Button::new(language.text("Save resolution")),
                    )
                    .clicked()
                {
                    action = Some(Command::SyncResolve(
                        self.task,
                        path,
                        Some(self.editor.as_bytes().to_vec()),
                    ));
                    self.selected = None;
                }
            });
        }
        if let Some(action) = action {
            model.send(action);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{Duration, Instant},
    };

    use super::*;
    use crate::{
        i18n::Language,
        workspace::{BackupStore, Workspace},
    };

    fn settle(model: &mut Model) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while model.busy && Instant::now() < deadline {
            model.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!model.busy);
        assert!(model.error.is_none(), "{:?}", model.error);
    }

    fn render(
        flow: &mut Flow,
        model: &mut Model,
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
            |ui| {
                flow.show(ui.ctx(), model);
            },
        );
        output.textures_delta.clear();
        output
    }

    fn bounds(output: &egui::FullOutput, label: &str) -> egui::Rect {
        output
            .shapes
            .iter()
            .find_map(|shape| {
                if let egui::Shape::Text(text) = &shape.shape
                    && text.galley.job.text == label
                {
                    let bounds = egui::Rect::from_min_size(text.pos, text.galley.size());
                    assert!(
                        shape.clip_rect.contains_rect(bounds),
                        "Clipped action: {label}"
                    );
                    assert!(
                        bounds.bottom() <= 560.0 && bounds.left() >= 0.0 && bounds.right() <= 800.0,
                        "Offscreen action: {label} {bounds:?}"
                    );
                    Some(bounds)
                } else {
                    None
                }
            })
            .unwrap_or_else(|| panic!("Missing action: {label}"))
    }

    fn click(flow: &mut Flow, model: &mut Model, context: &egui::Context, label: &str) {
        render(flow, model, context, vec![]);
        let output = render(flow, model, context, vec![]);
        let pos = bounds(&output, label).center();
        for pressed in [true, false] {
            render(
                flow,
                model,
                context,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
    }

    #[test]
    fn sync_confirmation_is_visible_bilingual_and_requires_acknowledgement() {
        let temp = tempfile::tempdir().unwrap();
        let remote = temp.path().join("remote.git");
        crate::git::init_bare(&remote).unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("note.md"), "remote content").unwrap();
        let seed = BackupStore::open(temp.path().join("seed")).unwrap();
        let task = Workspace::builder("Notes", &source)
            .include("note.md")
            .build()
            .unwrap();
        seed.register(task.clone()).unwrap();
        seed.set_remote(Some(remote.to_str().unwrap()), true)
            .unwrap();
        seed.backup(task.id()).unwrap();
        for (index, language) in [Language::English, Language::Chinese]
            .into_iter()
            .enumerate()
        {
            let root = temp.path().join(format!("destination-{index}"));
            fs::create_dir(&root).unwrap();
            let store = BackupStore::open(temp.path().join(format!("store-{index}"))).unwrap();
            store
                .set_remote(Some(remote.to_str().unwrap()), false)
                .unwrap();
            store.fetch().unwrap();
            store.import("Notes", &root).unwrap();
            let mut model = Model::with_language(Some(store.directory().to_path_buf()), language);
            settle(&mut model);
            let mut flow = Flow::new(
                &mut model,
                task.id(),
                "Very long task name ".repeat(12),
                root.to_string_lossy().repeat(10),
            );
            settle(&mut model);
            let context = egui::Context::default();
            super::super::theme::apply(&context);
            super::super::configure_fonts(&context);
            context.all_styles_mut(|style| style.animation_time = 0.0);
            click(
                &mut flow,
                &mut model,
                &context,
                language.text("Preview first sync"),
            );
            settle(&mut model);
            assert!(!root.join("note.md").exists());
            let actual = model.sync.clone().unwrap();
            let mut large = serde_json::to_value(&actual).unwrap();
            let entry = large["entries"][0].clone();
            large["entries"] = serde_json::Value::Array((0..420).map(|_| entry.clone()).collect());
            model.sync = Some(serde_json::from_value(large).unwrap());
            render(&mut flow, &mut model, &context, vec![]);
            let output = render(&mut flow, &mut model, &context, vec![]);
            bounds(&output, language.text("Confirm sync changes"));
            bounds(&output, language.text("Cancel operation"));
            click(
                &mut flow,
                &mut model,
                &context,
                language.text("Confirm sync changes"),
            );
            assert!(!model.busy, "Confirmation must require acknowledgement");
            assert!(!root.join("note.md").exists());
            model.sync = Some(actual);
            click(
                &mut flow,
                &mut model,
                &context,
                language.text("I reviewed these changes. Keep recovery copies and apply them."),
            );
            click(
                &mut flow,
                &mut model,
                &context,
                language.text("Confirm sync changes"),
            );
            settle(&mut model);
            assert_eq!(
                fs::read_to_string(root.join("note.md")).unwrap(),
                if index == 0 {
                    "remote content"
                } else {
                    "remote change 0\n"
                }
            );
            assert!(model.sync.as_ref().unwrap().enabled());
            assert_eq!(
                model.sync.as_ref().unwrap().options().integration(),
                PullStrategy::Rebase
            );
            fs::write(source.join("note.md"), format!("remote change {index}\n")).unwrap();
            seed.backup(task.id()).unwrap();
            fs::write(root.join("note.md"), "local conflict\n").unwrap();
            model.send(Command::Once(task.id()));
            let deadline = Instant::now() + Duration::from_secs(30);
            while model.busy && Instant::now() < deadline {
                model.poll();
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(model.error.is_some());
            assert_eq!(model.sync.as_ref().unwrap().phase(), SyncPhase::Conflict);
            click(&mut flow, &mut model, &context, "note.md");
            settle(&mut model);
            click(
                &mut flow,
                &mut model,
                &context,
                language.text("Keep remote file"),
            );
            settle(&mut model);
            click(
                &mut flow,
                &mut model,
                &context,
                language.text("Continue integration"),
            );
            settle(&mut model);
            assert_eq!(
                fs::read_to_string(root.join("note.md")).unwrap(),
                "local conflict\n"
            );
            click(
                &mut flow,
                &mut model,
                &context,
                language.text("I reviewed these changes. Keep recovery copies and apply them."),
            );
            click(
                &mut flow,
                &mut model,
                &context,
                language.text("Confirm sync changes"),
            );
            settle(&mut model);
            assert_eq!(
                fs::read_to_string(root.join("note.md")).unwrap(),
                format!("remote change {index}\n")
            );
            model.request_shutdown();
        }
    }
}
