//! Native desktop task management, history and explicit restore previews.

mod icons;
mod repository;
mod restore;
mod theme;
mod tray;

use std::{path::PathBuf, time::Duration};

use eframe::egui::{self, Color32, RichText};
use uuid::Uuid;

use self::icons::Icon;
use crate::{
    Result,
    i18n::Language,
    interface::{Command, Draft, Kind, Model, lines, spaces::Sessions},
    pull::PullStrategy,
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
    spaces: Sessions,
    space_form: Option<bool>,
    space_name: String,
    space_remote: String,
    repository: repository::Panel,
    tray: Option<tray::Tray>,
    tray_error: Option<String>,
    hide_on_start: bool,
    closing: bool,
    desktop_settings: bool,
    selected: Option<Uuid>,
    draft: Option<Draft>,
    saving: bool,
    activity_open: bool,
    follow_logs: bool,
    history: usize,
    restore_files: String,
    confirm: bool,
    remove: bool,
    remote: String,
    auto_push: bool,
    settings: bool,
    restore: Option<restore::Flow>,
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
    let (spaces, mut model) = Sessions::new(data, language)?;
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
            let (tray, tray_error) = match tray::Tray::new(language, &context.egui_ctx) {
                Ok(tray) => (Some(tray), None),
                Err(error) => (None, Some(format!("{error:#}"))),
            };
            let hide_on_start = start_in_tray && tray.is_some();
            Ok(Box::new(Desktop {
                model,
                spaces,
                space_form: None,
                space_name: String::new(),
                space_remote: String::new(),
                repository: repository::Panel::default(),
                tray,
                tray_error,
                hide_on_start,
                closing: false,
                desktop_settings: false,
                selected: None,
                draft: None,
                saving: false,
                activity_open: true,
                follow_logs: true,
                history: 0,
                restore_files: String::new(),
                confirm: false,
                remove: false,
                remote: String::new(),
                auto_push: false,
                settings: false,
                restore: None,
            }))
        }),
    )
    .map_err(|error| anyhow::anyhow!("Desktop failed: {error}"))
}

impl eframe::App for Desktop {
    fn logic(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(tray) = &mut self.tray {
            tray.poll(context);
            tray.set_language(self.model.language);
        }
        let requested = self.tray.as_ref().is_some_and(|tray| tray.quitting())
            || (self.tray.is_none() && context.input(|input| input.viewport().close_requested()));
        if self.shutdown(context, requested) {
            return;
        }
        if self.spaces.poll(&mut self.model) {
            self.clear_workspace();
            egui::DragAndDrop::clear_payload(context);
        }
        if let Some(tray) = &mut self.tray {
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

impl Drop for Desktop {
    fn drop(&mut self) {
        self.spaces.request_shutdown(&self.model);
    }
}

impl Desktop {
    fn shutdown(&mut self, context: &egui::Context, requested: bool) -> bool {
        if !self.closing && !requested {
            return false;
        }
        let starting = !self.closing;
        self.closing = true;
        self.spaces.request_shutdown(&self.model);
        if starting {
            self.repository = repository::Panel::default();
        }
        if self.spaces.shutdown_finished(&self.model) {
            context.send_viewport_cmd(egui::ViewportCommand::Close);
        } else {
            // Keep the event loop alive until joins cannot block window destruction.
            // https://docs.rs/egui/latest/egui/enum.ViewportCommand.html#variant.CancelClose
            context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if starting {
                context.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                context.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            }
            context.request_repaint_after(Duration::from_millis(50));
        }
        true
    }

    fn workspace_controls(&mut self, ui: &mut egui::Ui) {
        let language = self.model.language;
        let before = self.spaces.selected();
        let mut selected = before;
        ui.add_space(8.0);
        ui.add_enabled_ui(!self.spaces.busy(), |ui| {
            ui.horizontal_wrapped(|ui| {
                egui::ComboBox::from_id_salt("workspace-selector")
                    .selected_text(self.spaces.label(language))
                    .show_ui(ui, |ui| {
                        for space in self.spaces.list() {
                            ui.selectable_value(&mut selected, space.id, space.label(language));
                        }
                    });
                if ui.button(language.text("New workspace")).clicked() {
                    self.space_form = Some(true);
                    self.space_name.clear();
                    self.space_remote.clear();
                }
                if !before.is_nil() {
                    if ui.button(language.text("Rename workspace")).clicked() {
                        self.space_form = Some(false);
                        self.space_name = self.spaces.label(language).into();
                    }
                    if ui
                        .add_enabled(
                            !self.model.busy && self.model.rows.is_empty(),
                            egui::Button::new(language.text("Remove workspace")),
                        )
                        .on_hover_text(language.text(
                            "Only empty workspaces can be removed. Backup data stays on disk.",
                        ))
                        .clicked()
                    {
                        self.spaces.remove(&mut self.model);
                    }
                }
            });
        });
        if selected != before {
            self.spaces.switch(&mut self.model, selected);
        }
        if self.spaces.selected() != before {
            self.clear_workspace();
            egui::DragAndDrop::clear_payload(ui.ctx());
        }
        if self.spaces.busy() {
            ui.label(language.text("Working in background…"));
        }
        if let Some(create) = self.space_form {
            let mut open = true;
            let mut save = false;
            egui::Window::new(language.text(if create { "New workspace" } else { "Rename workspace" }))
                .open(&mut open).collapsible(false).show(ui.ctx(), |ui| {
                    field(ui, language.text("Name"), &mut self.space_name);
                    if create {
                        ui.label(language.text("Remote repository (required)"));
                        ui.text_edit_singleline(&mut self.space_remote);
                        ui.small(language.text("Each workspace keeps its own tasks and backup history. Switching leaves started tasks running."));
                    }
                    save = ui.add_enabled(!self.spaces.busy() && !self.space_name.trim().is_empty() && (!create || !self.space_remote.trim().is_empty()), Icon::Save.button(language.text("Save"))).clicked();
                });
            if save {
                if create {
                    self.spaces
                        .create(self.space_name.clone(), self.space_remote.clone());
                } else {
                    self.spaces.rename(&self.space_name);
                }
                if self.spaces.error.is_none() {
                    self.space_form = None;
                }
            }
            if !open {
                self.space_form = None;
            }
        }
    }

    fn task_list(&mut self, ui: &mut egui::Ui) {
        let language = self.model.language;
        ui.heading(language.text("Tasks"));
        ui.label(language.text("Started tasks resume when you reopen the app."));
        egui::ScrollArea::vertical().id_salt("tasks").auto_shrink([false, false]).show(ui, |ui| {
                        if self.model.rows.is_empty() { ui.add_space(24.0); ui.label(language.text("Add a workspace backup, watch a Git repository, or schedule repository updates.")); }
                        let mut reorder = None;
                        for (index, row) in self.model.rows.iter().enumerate() {
                            let id = row.draft.id;
                            let preview_id = egui::Id::new(("task-preview", id));
                            let starting = id.is_some_and(|id| ui.ctx().drag_started_id() == Some(egui::Id::new(("task-drag", id))));
                            if starting && !self.model.busy && let Some(id) = id {
                                egui::DragAndDrop::set_payload(ui.ctx(), id);
                            }
                            let dragging = id.is_some_and(|id| egui::DragAndDrop::payload::<Uuid>(ui.ctx()).as_deref() == Some(&id)) && ui.input(|input| input.pointer.primary_down());
                            let layer = egui::LayerId::new(egui::Order::Tooltip, preview_id);
                            let card = ui.scope_builder(egui::UiBuilder::new().layer_id(if dragging { layer } else { ui.layer_id() }), |ui| {
                                // Keep painting the preview when its original slot scrolls out of view.
                                if dragging { ui.set_clip_rect(egui::Rect::EVERYTHING); }
                                ui.push_id(id, |ui| egui::Frame::new().fill(if self.selected == id { theme::SELECTED } else { theme::SURFACE })
                                .stroke(egui::Stroke::new(1.0, if self.selected == id { theme::ACCENT } else { theme::BORDER }))
                                .corner_radius(10).inner_margin(14).show(ui, |ui| {
                                ui.set_min_width((ui.available_width() - 2.0).max(0.0));
                                ui.horizontal(|ui| {
                                    if let Some(id) = id {
                                        ui.add_enabled_ui(!self.model.busy, |ui| {
                                            let (_, rect) = ui.allocate_space(egui::vec2(20.0, 24.0));
                                            let handle = ui.interact(rect, egui::Id::new(("task-drag", id)), egui::Sense::drag());
                                            handle.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, ui.is_enabled(), language.text("Drag to reorder")));
                                            Icon::Grip.paint(ui.painter(), egui::Rect::from_center_size(rect.center(), egui::vec2(16.0, 16.0)), if handle.dragged() { theme::ACCENT } else { theme::MUTED });
                                            handle.dnd_set_drag_payload(id);
                                            handle.on_hover_text(language.text("Drag to reorder"));
                                        });
                                    }
                                if ui.selectable_label(self.selected == id, RichText::new(&row.draft.name).strong().size(18.0)).clicked() {
                                    self.selected = id; self.settings = false; self.draft = None; self.model.history.clear(); self.model.plan = None; self.model.text.clear(); self.history = 0; self.confirm = false; self.remove = false;
                                }
                                });
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
                            })).inner
                            }).inner;
                            if dragging {
                                // Preserve the grab point while the list scrolls beneath the pointer.
                                if starting && let Some(origin) = ui.input(|input| input.pointer.press_origin()) {
                                    ui.data_mut(|data| data.insert_temp(preview_id, origin - card.response.rect.min));
                                }
                                if let Some(pointer) = ui.input(|input| input.pointer.interact_pos()) {
                                    let offset = ui.data(|data| data.get_temp::<egui::Vec2>(preview_id)).unwrap_or_default();
                                    ui.ctx().transform_layer_shapes(layer, egui::emath::TSTransform::from_translation(pointer - card.response.rect.min - offset));
                                }
                                ui.painter().rect_stroke(card.response.rect, 10, egui::Stroke::new(1.0, theme::BORDER), egui::StrokeKind::Inside);
                            } else {
                                ui.data_mut(|data| data.remove::<egui::Vec2>(preview_id));
                            }
                            if let Some(target) = id && !self.model.busy {
                                let mut drop_rect = card.response.rect.expand2(egui::vec2(0.0, 3.0));
                                if index + 1 == self.model.rows.len() { drop_rect.max.y = drop_rect.max.y.max(ui.clip_rect().bottom()); }
                                let drop = ui.interact(drop_rect, egui::Id::new(("task-drop", target)), egui::Sense::hover());
                                if let Some(source) = drop.dnd_hover_payload::<Uuid>() && *source != target {
                                    let after = ui.input(|input| input.pointer.hover_pos().is_some_and(|pos| pos.y > card.response.rect.center().y));
                                    let y = if after { card.response.rect.bottom() } else { card.response.rect.top() };
                                    ui.painter().line_segment([egui::pos2(drop.rect.left(), y), egui::pos2(drop.rect.right(), y)], egui::Stroke::new(2.0, theme::ACCENT));
                                    if drop.dnd_release_payload::<Uuid>().is_some() {
                                        reorder = Some(Command::Reorder { id: *source, target, after });
                                    }
                                }
                            }
                            ui.add_space(6.0);
                        }
                        if let Some(command) = reorder { self.model.send(command); }
                        if egui::DragAndDrop::payload::<Uuid>(ui.ctx()).is_some() && ui.input(|input| input.pointer.primary_down()) {
                            let clip = ui.clip_rect().intersect(ui.max_rect());
                            if let Some(pos) = ui.input(|input| input.pointer.hover_pos()) && clip.contains(pos) {
                                let direction = if pos.y < clip.top() + 32.0 { 1.0 } else if pos.y > clip.bottom() - 32.0 { -1.0 } else { 0.0 };
                                let delta = direction * 360.0 * ui.input(|input| input.stable_dt.min(0.05));
                                ui.scroll_with_delta_animation(egui::vec2(0.0, delta), egui::style::ScrollAnimation::none());
                            }
                        }
                    });
    }

    fn activity(&mut self, ui: &mut egui::Ui) {
        let language = self.model.language;
        let open = self.activity_open;
        let panel = egui::Panel::bottom(if open {
            "activity-panel"
        } else {
            "activity-collapsed"
        });
        let panel = if open {
            panel
                .default_size((ui.available_height() * 0.3).clamp(90.0, 150.0))
                .size_range(
                    85.0..=(ui.available_height() - 180.0)
                        .min(ui.available_height() * 0.7)
                        .max(85.0),
                )
                .resizable(true)
        } else {
            panel.exact_size(36.0)
        };
        panel.show(ui, |ui| {
            let mut follow_latest = false;
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add(
                        (if open { Icon::Collapse } else { Icon::Expand })
                            .button(language.text("Activity")),
                    )
                    .clicked()
                {
                    self.activity_open = !open;
                }
                if open {
                    follow_latest = ui
                        .checkbox(&mut self.follow_logs, language.text("Follow latest"))
                        .changed()
                        && self.follow_logs;
                    if ui
                        .add_enabled(
                            !self.model.logs.is_empty(),
                            egui::Button::new(language.text("Copy logs")),
                        )
                        .clicked()
                    {
                        ui.ctx().copy_text(
                            self.model
                                .logs
                                .iter()
                                .map(|message| message.render(language))
                                .collect::<Vec<_>>()
                                .join("\n"),
                        );
                    }
                    if ui
                        .add_enabled(
                            !self.model.logs.is_empty(),
                            egui::Button::new(language.text("Clear logs")),
                        )
                        .clicked()
                    {
                        self.model.logs.clear();
                    }
                }
            });
            if open {
                egui::ScrollArea::vertical()
                    .id_salt("activity")
                    .auto_shrink([false, false])
                    .stick_to_bottom(self.follow_logs)
                    .animated(false)
                    .show(ui, |ui| {
                        if self.model.logs.is_empty() {
                            ui.weak(language.text("No activity yet."));
                        }
                        for message in &self.model.logs {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(message.render(language)).monospace(),
                                )
                                .wrap()
                                .selectable(true),
                            );
                        }
                        if follow_latest {
                            ui.scroll_to_cursor(Some(egui::Align::BOTTOM));
                        }
                    });
            }
        });
    }

    fn clear_workspace(&mut self) {
        self.selected = None;
        self.draft = None;
        self.saving = false;
        self.settings = false;
        self.space_form = None;
        self.history = 0;
        self.confirm = false;
        self.remove = false;
        self.restore_files.clear();
        self.restore = None;
        self.remote.clear();
        self.model.plan = None;
        self.model.history.clear();
        self.model.text.clear();
        self.repository = repository::Panel::default();
    }

    fn paint(&mut self, ui: &mut egui::Ui) {
        let language = self.model.language;
        if self.closing {
            ui.vertical_centered(|ui| {
                ui.add_space(32.0);
                ui.spinner();
                ui.heading(language.text("Stopping tasks…"));
                ui.label(language.text("Waiting for current operations to finish safely."));
            });
            return;
        }
        if self.spaces.poll(&mut self.model) {
            self.clear_workspace();
            egui::DragAndDrop::clear_payload(ui.ctx());
        }
        if self.saving && !self.model.busy {
            self.saving = false;
            if self.model.error.is_none() {
                self.draft = None;
            }
        }
        if self.restore.is_some()
            && let Some(plan) = &self.model.plan
        {
            self.selected = Some(plan.workspace());
        }
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
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(24),
            )
            .show(ui, |ui| {
                let mut selected_language = language;
                ui.horizontal(|ui| {
                    ui.heading(RichText::new("gitwatch").size(28.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_enabled_ui(!self.model.busy, |ui| {
                            egui::ComboBox::from_id_salt("language")
                                .selected_text(language.name())
                                .show_ui(ui, |ui| {
                                    for choice in [Language::English, Language::Chinese] {
                                        ui.selectable_value(
                                            &mut selected_language,
                                            choice,
                                            choice.name(),
                                        );
                                    }
                                });
                        });
                        if ui
                            .add(Icon::Settings.button(language.text("Settings")))
                            .clicked()
                        {
                            self.desktop_settings = !self.desktop_settings;
                        }
                    });
                });

                if selected_language != language {
                    self.spaces
                        .global(&mut self.model, Command::Language(selected_language));
                }
                self.workspace_controls(ui);
                ui.add_space(10.0);
                ui.horizontal_wrapped(|ui| {
                    if !self.spaces.selected().is_nil()
                        && ui
                            .add_enabled(
                                !self.model.busy,
                                Icon::Backup.button(language.text("Backup settings")),
                            )
                            .clicked()
                    {
                        self.set_backup_settings(!self.settings);
                    }
                    if !self.spaces.selected().is_nil()
                        && ui
                            .add_enabled(
                                !self.model.busy && self.model.remote.is_some(),
                                Icon::Download.button(language.text("Restore remote backup")),
                            )
                            .clicked()
                    {
                        self.settings = false;
                        self.draft = None;
                        self.restore = Some(restore::Flow::new(&mut self.model, None));
                    }
                    if ui
                        .add_enabled(
                            !self.model.busy,
                            Icon::Add.button(language.text("Add task")),
                        )
                        .clicked()
                    {
                        self.draft = Some(Draft::default());
                        self.settings = false;
                    }
                    if ui
                        .add_enabled(
                            !self.model.busy,
                            Icon::Refresh.button(language.text("Refresh")),
                        )
                        .clicked()
                    {
                        self.model.send(Command::Refresh);
                        self.repository.refresh();
                    }
                    if self.tray.is_some()
                        && ui
                            .add(Icon::Tray.button(language.text("Minimize to tray")))
                            .clicked()
                    {
                        tray::hide(ui.ctx());
                    }
                });
                ui.add_space(8.0);
                ui.separator();
                if let Some(release) = &self.model.update {
                    egui::Frame::new()
                        .fill(theme::SELECTED)
                        .corner_radius(8)
                        .inner_margin(12)
                        .show(ui, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    language
                                        .format("gitwatch {0} is available.", &[release.version()]),
                                );
                                ui.monospace("gitwatch self update")
                                    .on_hover_text(language.text(
                                    "Choose Quit in the tray menu, then update from a terminal:",
                                ));
                                ui.hyperlink_to(language.text("Release notes"), release.url());
                            });
                        });
                }
                if self.model.busy {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(language.text("Working in background…"));
                    });
                }
                let background_error = self.spaces.background_error(language);
                if self.model.error.is_some()
                    || self.spaces.error.is_some()
                    || background_error.is_some()
                {
                    egui::ScrollArea::vertical()
                        .id_salt("errors")
                        .max_height(64.0)
                        .show(ui, |ui| {
                            for error in [&self.model.error, &self.spaces.error]
                                .into_iter()
                                .flatten()
                                .map(|error| language.error(error))
                                .chain(background_error)
                            {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(error).color(Color32::LIGHT_RED),
                                    )
                                    .wrap()
                                    .selectable(true),
                                );
                            }
                        });
                }
                if let Some(error) = &self.tray_error {
                    ui.colored_label(
                        Color32::YELLOW,
                        language.text("System tray unavailable; closing this window will exit."),
                    )
                    .on_hover_text(error);
                }
                self.activity(ui);
                let width = ui.available_width();
                egui::Panel::left("task-list-panel")
                    .default_size(300.0)
                    .size_range(220.0..=(width - 340.0).clamp(220.0, 520.0))
                    .resizable(true)
                    .show(ui, |ui| self.task_list(ui));
                egui::CentralPanel::default().show(ui, |ui| {
                    if self.settings && !self.spaces.selected().is_nil() {
                        egui::ScrollArea::vertical()
                            .id_salt("backup-settings")
                            .auto_shrink([false, false])
                            .show(ui, |ui| self.settings(ui));
                    } else if self.draft.is_some() {
                        self.form(ui);
                    } else {
                        self.details(ui);
                    }
                });
            });
        if let Some(flow) = &mut self.restore {
            if let Some(plan) = &self.model.plan {
                self.selected = Some(plan.workspace());
            }
            if flow.show(ui.ctx(), &mut self.model) {
                self.restore = None;
            }
        }
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
                        self.spaces
                            .global(&mut self.model, Command::StartInTray(enabled));
                    }
                });
                ui.label(language.text(
                    "Applies the next time you open gitwatch. Started tasks resume automatically.",
                ));
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
        let pull_strategy = draft.pull_strategy;
        let repository_path = draft.path.clone();
        let repository_remote = draft.remote.clone();
        ui.heading(&draft.name);
        ui.add_enabled_ui(!self.model.busy, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add(
                        (if running { Icon::Stop } else { Icon::Start })
                            .button(language.text(if running { "Stop" } else { "Start" })),
                    )
                    .clicked()
                {
                    self.model.send(if running {
                        Command::Stop(id)
                    } else {
                        Command::Start(id)
                    });
                }
                if ui
                    .add(Icon::Once.button(language.text("Run once")))
                    .clicked()
                {
                    self.model.send(Command::Once(id));
                }
                if ui
                    .add_enabled(!running, Icon::Edit.button(language.text("Edit")))
                    .clicked()
                {
                    self.draft = Some(draft);
                }
                if ui
                    .add_enabled(!running, Icon::Remove.button(language.text("Remove")))
                    .clicked()
                {
                    self.remove = true;
                }
                if kind == Kind::Workspace {
                    if self.model.remote.is_some()
                        && ui.add_enabled(!running, Icon::Download.button(language.text("Restore from remote")))
                            .on_hover_text(language.text("Fetch the latest backup and preview changes before writing local files."))
                            .clicked()
                    {
                        self.restore = Some(restore::Flow::new(&mut self.model, Some(id)));
                    }
                    if ui
                        .add(Icon::History.button(language.text("History")))
                        .clicked()
                    {
                        self.model.send(Command::History(id));
                    }
                    if self.model.remote.is_some()
                        && ui
                            .add(Icon::Upload.button(language.text("Upload")))
                            .clicked()
                    {
                        self.model.send(Command::Push(id));
                    }
                }
            });
            if self.remove {
                ui.label(language.text(if kind == Kind::Workspace {
                    "Remove this local binding? Backup history will be retained."
                } else {
                    "Remove this task? Local files will be kept."
                }));
                ui.horizontal(|ui| {
                    if ui
                        .add(Icon::Remove.button(language.text("Remove binding")))
                        .clicked()
                    {
                        self.model.send(Command::Remove(id));
                        self.remove = false;
                    }
                    if ui
                        .add(Icon::Close.button(language.text("Cancel")))
                        .clicked()
                    {
                        self.remove = false;
                    }
                });
            }
        });
        if kind != Kind::Workspace {
            ui.add_space(16.0);
            egui::ScrollArea::vertical().id_salt("repository-details").auto_shrink([false, false]).show(ui, |ui| {
                self.repository.show(ui, &repository_path, &repository_remote, language);
                ui.add_space(12.0);
                if kind == Kind::Pull {
                    ui.label(format!("{}: {}", language.text("Pull strategy"), pull_strategy.label(language)));
                    ui.small(pull_strategy.description(language));
                    ui.small(language.text("Conflicts stop the task. Resolve or abort them in Git, then restart it."));
                } else {
                    ui.small(language.text("Commits changes in the selected Git repository. The task stops automatic writes if the branch changes."));
                }
            });
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt("details")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::CollapsingHeader::new(language.text("Source repository")).show(ui, |ui| {
                    self.repository
                        .show(ui, &repository_path, "origin", language)
                });
                if self.model.plan.is_some() && self.restore.is_none() {
                    restore::preview(ui, &mut self.model, &mut self.confirm, !running);
                } else {
                    ui.separator();
                    ui.heading(language.text("History"));
                    if self.model.history.is_empty() {
                        ui.weak(language.text("Choose History to load saved versions."));
                    }
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
                        }
                    }
                    if let Some(entry) = self.model.history.get(self.history) {
                        let revision = entry.commit().to_owned();
                        let older = self
                            .model
                            .history
                            .get(self.history + 1)
                            .map(|e| e.commit().to_owned());
                        ui.label(
                            language.text("Restore selected paths (one per line; empty means all)"),
                        );
                        ui.text_edit_multiline(&mut self.restore_files);
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    !self.model.busy && !running,
                                    Icon::Preview.button(language.text("Preview restore")),
                                )
                                .clicked()
                            {
                                self.model.send(Command::Preview(
                                    id,
                                    revision.clone(),
                                    lines(&self.restore_files),
                                ));
                            }
                            if let Some(older) = older
                                && ui
                                    .add_enabled(
                                        !self.model.busy,
                                        Icon::Diff.button(language.text("Diff to previous")),
                                    )
                                    .clicked()
                            {
                                self.model.send(Command::Diff(id, older, revision));
                            }
                        });
                        if running {
                            ui.label(language.text("Stop this task before restoring."));
                        }
                    }
                }
                if self.model.plan.is_none() && !self.model.text.is_empty() {
                    ui.separator();
                    ui.add(
                        egui::TextEdit::multiline(&mut self.model.text.render(language))
                            .interactive(false)
                            .font(egui::TextStyle::Monospace)
                            .desired_rows(15)
                            .desired_width(f32::INFINITY),
                    );
                }
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
        ui.horizontal_wrapped(|ui| {
            save = ui
                .add_enabled(
                    !self.model.busy,
                    Icon::Save.button(language.text("Save task")),
                )
                .clicked();
            cancel = ui
                .add_enabled(!self.saving, Icon::Close.button(language.text("Cancel")))
                .clicked();
            ui.label(language.text("Saving does not start a task."));
        });
        egui::ScrollArea::vertical()
            .id_salt(("form", draft.kind, draft.id))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if self.saving { ui.disable(); }
                ui.label(language.text("Task type"));
                if draft.id.is_none() {
                    let previous = draft.kind;
                    egui::ComboBox::from_id_salt("task-kind").selected_text(draft.kind.label(language)).show_ui(ui, |ui| {
                        for kind in [Kind::Workspace, Kind::Watch, Kind::Pull] { ui.selectable_value(&mut draft.kind, kind, kind.label(language)); }
                    });
                    if draft.kind != previous {
                        let name = std::mem::take(&mut draft.name);
                        let path = std::mem::take(&mut draft.path);
                        *draft = Draft { kind: draft.kind, name, path, ..Draft::default() };
                        if draft.kind == Kind::Watch { draft.remote.clear(); }
                    }
                } else { ui.label(draft.kind.label(language)); }
                field(ui, language.text("Name"), &mut draft.name);
                field(ui, language.text("Local path"), &mut draft.path);
                ui.horizontal(|ui| {
                    if ui
                        .add(Icon::Folder.button(language.text("Choose folder…")))
                        .clicked()
                        && let Some(path) = rfd::FileDialog::new().pick_folder()
                    {
                        draft.path = path.to_string_lossy().into_owned();
                    }
                    if draft.kind == Kind::Watch
                        && ui
                            .add(Icon::File.button(language.text("Choose file…")))
                            .clicked()
                        && let Some(path) = rfd::FileDialog::new().pick_file()
                    {
                        draft.path = path.to_string_lossy().into_owned();
                    }
                });
                if draft.kind == Kind::Workspace {
                    ui.label(
                        language.text("Selected paths, relative to the workspace (one per line)"),
                    );
                    ui.add(egui::TextEdit::multiline(&mut draft.includes).desired_width(f32::INFINITY));
                    ui.horizontal(|ui| {
                        if ui
                            .add(Icon::File.button(language.text("Add files…")))
                            .clicked()
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
                        if ui
                            .add(Icon::Folder.button(language.text("Add folder…")))
                            .clicked()
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
                    ui.add(egui::TextEdit::multiline(&mut draft.excludes).desired_width(f32::INFINITY));
                    ui.checkbox(&mut draft.follow_links, language.text("Follow symbolic links"));
                    ui.label(language.text("Back up target contents, including outside this project. Restore requires ordinary destination paths."));
                    ui.label(language.text(
                        "Source files are read only. Each workspace has its own backup branch.",
                    ));
                }
                field(ui, language.text("Branch (optional)"), &mut draft.branch);
                if draft.kind != Kind::Workspace {
                    field(
                        ui,
                        language.text(if draft.kind == Kind::Pull { "Remote name" } else { "Remote (empty disables watch upload)" }),
                        &mut draft.remote,
                    );
                }
                if draft.kind == Kind::Pull {
                    egui::ComboBox::from_label(language.text("Pull strategy"))
                        .selected_text(draft.pull_strategy.label(language))
                        .show_ui(ui, |ui| {
                            for strategy in [
                                PullStrategy::FastForwardOnly,
                                PullStrategy::Merge,
                                PullStrategy::Rebase,
                            ] {
                                ui.selectable_value(
                                    &mut draft.pull_strategy,
                                    strategy,
                                    strategy.label(language),
                                );
                            }
                        });
                    ui.small(draft.pull_strategy.description(language));
                    ui.small(language.text(
                        "Conflicts stop the task. Resolve or abort them in Git, then restart it.",
                    ));
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
            self.saving = true;
        }
        if cancel {
            self.draft = None;
        }
    }

    fn set_backup_settings(&mut self, visible: bool) {
        self.settings = visible;
        self.draft = None;
        self.remote = self.model.remote.clone().unwrap_or_default();
        self.auto_push = self.model.auto_push;
    }

    fn settings(&mut self, ui: &mut egui::Ui) {
        let language = self.model.language;
        if self.spaces.selected().is_nil() {
            return;
        }
        ui.heading(language.text("Workspace backup remote"));
        ui.label(language.text(
            "Enter a URL to replace the configured remote. Fetch only reads remote history.",
        ));
        ui.add(
            egui::TextEdit::singleline(&mut self.remote)
                .desired_width(f32::INFINITY)
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
                        !self.remote.trim().is_empty(),
                        Icon::Save.button(language.text("Save remote")),
                    )
                    .clicked()
                {
                    self.model
                        .send(Command::Remote(self.remote.clone(), self.auto_push));
                }
            });
        });
        ui.label(language.text(
            "To bring files from another computer, choose Restore remote backup in the toolbar.",
        ));
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
        egui::FontData::from_static(include_bytes!("fonts/GitwatchSansSC-Regular.ttf")).into(),
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

    fn contrast(foreground: Color32, background: Color32) -> f32 {
        let luminance = |color: Color32| {
            let linear = egui::Rgba::from(color);
            0.2126 * linear.r() + 0.7152 * linear.g() + 0.0722 * linear.b()
        };
        let foreground = luminance(foreground);
        let background = luminance(background);
        (foreground.max(background) + 0.05) / (foreground.min(background) + 0.05)
    }

    fn settle(app: &mut Desktop) {
        settle_for(app, Duration::from_secs(10));
    }

    fn settle_for(app: &mut Desktop, timeout: Duration) {
        wait_for(app, timeout);
        assert!(app.model.error.is_none(), "{:?}", app.model.error);
    }

    fn wait_for(app: &mut Desktop, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while (app.model.busy || app.spaces.busy()) && Instant::now() < deadline {
            if app.spaces.poll(&mut app.model) {
                app.clear_workspace();
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!app.model.busy && !app.spaces.busy());
    }

    fn render(
        app: &mut Desktop,
        context: &egui::Context,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        render_sized(app, context, events, egui::vec2(800.0, 560.0))
    }

    fn render_sized(
        app: &mut Desktop,
        context: &egui::Context,
        events: Vec<egui::Event>,
        size: egui::Vec2,
    ) -> egui::FullOutput {
        let mut output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                events,
                ..Default::default()
            },
            |ui| app.paint(ui),
        );
        output.textures_delta.clear();
        output
    }

    fn drag_task(
        app: &mut Desktop,
        context: &egui::Context,
        source: Uuid,
        target: Uuid,
        after: bool,
        cancel: bool,
    ) {
        let size = egui::vec2(1140.0, 780.0);
        render_sized(app, context, vec![], size);
        let output = render_sized(app, context, vec![], size);
        let name = app
            .model
            .rows
            .iter()
            .find(|row| row.draft.id == Some(source))
            .unwrap()
            .draft
            .name
            .clone();
        let clip = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == name => Some(shape.clip_rect),
                _ => None,
            })
            .unwrap();
        let start = context
            .read_response(egui::Id::new(("task-drag", source)))
            .unwrap()
            .rect
            .center();
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        render_sized(
            app,
            context,
            vec![egui::Event::PointerMoved(start), button(start, true)],
            size,
        );
        let output = render_sized(
            app,
            context,
            vec![egui::Event::PointerMoved(start + egui::vec2(0.0, 12.0))],
            size,
        );
        let preview = |output: &egui::FullOutput| {
            output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Text(text)
                    if text.galley.job.text == name && shape.clip_rect.width() > size.x * 2.0 =>
                {
                    Some(text.pos)
                }
                _ => None,
            })
        };
        let offset =
            preview(&output).expect("Missing floating task card") - start - egui::vec2(0.0, 12.0);
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Rect(rect) if rect.fill == egui::Color32::TRANSPARENT && rect.stroke.color == theme::BORDER && rect.rect.contains(start))), "Missing placeholder at the original position");
        let moved = start + egui::vec2(30.0, 40.0);
        let output = render_sized(app, context, vec![egui::Event::PointerMoved(moved)], size);
        assert!(
            (preview(&output).unwrap() - moved - offset).length() < 0.1,
            "The card must follow the pointer without changing the grab offset"
        );
        assert_eq!(
            egui::DragAndDrop::payload::<Uuid>(context).as_deref(),
            Some(&source),
            "Drag handle did not start at {start:?}, clip={clip:?}"
        );
        let mut destination = start;
        for _ in 0..400 {
            let rect = context
                .read_response(egui::Id::new(("task-drop", target)))
                .unwrap()
                .rect;
            destination = egui::pos2(
                rect.center().x,
                if after {
                    rect.bottom() - 12.0
                } else {
                    rect.top() + 12.0
                },
            );
            if clip.contains(destination) {
                break;
            }
            let edge = egui::pos2(
                rect.center().x,
                if destination.y < clip.top() {
                    clip.top() + 4.0
                } else {
                    clip.bottom() - 4.0
                },
            );
            render_sized(app, context, vec![egui::Event::PointerMoved(edge)], size);
        }
        assert!(
            clip.contains(destination),
            "Drag autoscroll did not reach the task: source={source} target={target} clip={clip:?} destination={destination:?}"
        );
        let output = render_sized(
            app,
            context,
            vec![egui::Event::PointerMoved(destination)],
            size,
        );
        assert!(
            (preview(&output).unwrap() - destination - offset).length() < 0.1,
            "Scrolling must not move the floating card away from the pointer"
        );
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::LineSegment { stroke, .. } if stroke.color == theme::ACCENT && stroke.width == 2.0)), "Missing drop position indicator: source={source}, target={target}, destination={destination:?}, drop={:?}, clip={clip:?}, busy={}", context.read_response(egui::Id::new(("task-drop", target))), app.model.busy);
        if cancel {
            let output = render_sized(
                app,
                context,
                vec![egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                size,
            );
            assert!(preview(&output).is_none(), "Escape must remove the preview");
        }
        let output = render_sized(app, context, vec![button(destination, false)], size);
        assert!(
            preview(&output).is_none(),
            "Releasing must remove the preview"
        );
        settle(app);
    }

    fn click(app: &mut Desktop, context: &egui::Context, label: &str) {
        let find = |output: &egui::FullOutput| {
            output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.job.text == label => {
                    let center = text.pos + text.galley.size() / 2.0;
                    shape.clip_rect.contains(center).then_some(center)
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
                .filter(|shape| {
                    matches!(&shape.shape, egui::epaint::Shape::Text(_))
                        && shape.clip_rect.min.x > 300.0
                        && shape.clip_rect.min.y > 300.0
                        && shape.clip_rect.height() > 10.0
                })
                .max_by(|a, b| a.clip_rect.min.y.total_cmp(&b.clip_rect.min.y))
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
            // Wait for wheel smoothing before clicking a widget that just scrolled into view.
            for _ in 0..12 {
                output = render(app, context, vec![]);
            }
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

    fn fixture(directory: &std::path::Path) -> Desktop {
        let (spaces, model) =
            Sessions::new(Some(directory.join("data")), Language::English).unwrap();
        Desktop {
            model,
            spaces,
            space_form: None,
            space_name: String::new(),
            space_remote: String::new(),
            repository: repository::Panel::default(),
            tray: None,
            tray_error: None,
            hide_on_start: false,
            closing: false,
            desktop_settings: false,
            selected: None,
            draft: None,
            saving: false,
            activity_open: true,
            follow_logs: true,
            history: 0,
            restore_files: String::new(),
            confirm: false,
            remove: false,
            remote: String::new(),
            auto_push: false,
            settings: false,
            restore: None,
        }
    }

    #[test]
    fn shutdown_keeps_painting_until_controller_initialization_finishes() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = fixture(temp.path());
        settle(&mut app);
        let blocked = crate::workspace::BackupStore::open(temp.path().join("blocked")).unwrap();
        let lock = crate::git::Lock::wait(&blocked.directory().join("store.lock")).unwrap();
        app.model = Model::new(Some(blocked.directory().to_path_buf()));
        let context = egui::Context::default();
        configure_fonts(&context);
        for language in [Language::English, Language::Chinese] {
            app.model.language = language;
            let start = Instant::now();
            let mut output = context.run_ui(Default::default(), |ui| {
                assert!(app.shutdown(ui.ctx(), true));
                app.paint(ui);
            });
            output.textures_delta.clear();
            assert!(start.elapsed() < Duration::from_secs(2));
            let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
            assert!(commands.contains(&egui::ViewportCommand::CancelClose));
            assert!(!commands.contains(&egui::ViewportCommand::Close));
            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text == language.text("Stopping tasks…"))));
        }
        drop(lock);
        let until = Instant::now() + Duration::from_secs(15);
        while !app.spaces.shutdown_finished(&app.model) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(app.spaces.shutdown_finished(&app.model));
        let mut output = context.run_ui(Default::default(), |ui| {
            app.shutdown(ui.ctx(), false);
        });
        output.textures_delta.clear();
        assert!(
            output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .contains(&egui::ViewportCommand::Close)
        );
    }

    #[test]
    fn desktop_layout_and_form_errors() {
        let temp = tempfile::TempDir::new().unwrap();
        let mut app = fixture(temp.path());
        settle(&mut app);
        let context = egui::Context::default();
        theme::apply(&context);
        context.all_styles_mut(|style| style.animation_time = 0.0);
        configure_fonts(&context);
        let has_text = |output: &egui::FullOutput, expected: &str| {
            output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text == expected))
        };
        let output = render(&mut app, &context, vec![]);
        for label in [
            "Backup settings",
            "Restore remote backup",
            "Workspace backup remote",
            "Rename workspace",
            "Remove workspace",
        ] {
            assert!(
                !has_text(&output, label),
                "Irrelevant default workspace control: {label}"
            );
        }
        click(&mut app, &context, "Add task");
        app.draft.as_mut().unwrap().name = "Keep my input".into();
        click(&mut app, &context, "Workspace backup");
        click(&mut app, &context, "Git watch");
        assert_eq!(app.draft.as_ref().unwrap().kind, Kind::Watch);
        assert!(app.draft.as_ref().unwrap().remote.is_empty());
        click(&mut app, &context, "Git watch");
        click(&mut app, &context, "Scheduled pull");
        assert_eq!(app.draft.as_ref().unwrap().remote, "origin");
        assert_eq!(app.draft.as_ref().unwrap().name, "Keep my input");
        app.draft.as_mut().unwrap().interval = "not a number".into();
        click(&mut app, &context, "Save task");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while app.model.busy && std::time::Instant::now() < deadline {
            app.model.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
        render(&mut app, &context, vec![]);
        assert!(app.model.error.is_some());
        assert!(!app.saving);
        assert_eq!(app.draft.as_ref().unwrap().name, "Keep my input");
        assert_eq!(app.draft.as_ref().unwrap().interval, "not a number");
        app.model.error = Some("Git reported a long diagnostic.\n".repeat(100));
        let output = render(&mut app, &context, vec![]);
        assert!(
            output
                .shapes
                .iter()
                .any(|shape| matches!(&shape.shape, egui::Shape::Text(text)
            if text.galley.job.text == "Save task"
                && shape.clip_rect.contains(text.pos + text.galley.size() / 2.0)
                && text.pos.y < 560.0)),
            "Long errors must not hide the task form"
        );
        app.model.error = None;
        click(&mut app, &context, "Cancel");
        click(&mut app, &context, "Follow latest");
        assert!(!app.follow_logs);
        app.model.logs = (0..100)
            .map(|index| crate::i18n::Text::value(format!("Log entry {index}")))
            .collect();
        render(&mut app, &context, vec![]);
        click(&mut app, &context, "Follow latest");
        let output = render(&mut app, &context, vec![]);
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text)
            if text.galley.job.text == "Log entry 99" && shape.clip_rect.contains(text.pos + text.galley.size() / 2.0))), "Enabling follow must reveal the latest log entry");
        click(&mut app, &context, "Clear logs");
        assert!(app.model.logs.is_empty());
        let output = render(&mut app, &context, vec![]);
        assert!(has_text(&output, "No activity yet."));
        let size = egui::vec2(1140.0, 780.0);
        for (id, delta) in [
            ("activity-panel", egui::vec2(0.0, -150.0)),
            ("task-list-panel", egui::vec2(90.0, 0.0)),
        ] {
            render_sized(&mut app, &context, vec![], size);
            let resize = egui::Id::new(id).with("__resize");
            let start = context.read_response(resize).unwrap().rect.center();
            let button = |pos, pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            render_sized(
                &mut app,
                &context,
                vec![egui::Event::PointerMoved(start), button(start, true)],
                size,
            );
            render_sized(
                &mut app,
                &context,
                vec![egui::Event::PointerMoved(start + delta)],
                size,
            );
            render_sized(&mut app, &context, vec![button(start + delta, false)], size);
            render_sized(&mut app, &context, vec![], size);
            let end = context.read_response(resize).unwrap().rect.center();
            assert!(
                (end - start).length() > 60.0,
                "Panel did not resize: {id}, {start:?} -> {end:?}"
            );
        }
        click(&mut app, &context, "Activity");
        assert!(!app.activity_open);
        click(&mut app, &context, "Activity");
        assert!(app.activity_open);
    }

    #[test]
    fn remote_restore_button_previews_before_confirming() {
        let temp = tempfile::tempdir().unwrap();
        let remote = temp.path().join("remote.git");
        fs::create_dir(&remote).unwrap();
        crate::test_git::git(&remote, &["init", "--bare"]);
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        let file = source.join("notes.md");
        fs::write(&file, "remote contents").unwrap();
        fs::create_dir(source.join("files")).unwrap();
        for index in 0..360 {
            fs::write(source.join(format!("files/{index}.md")), "backup file").unwrap();
        }
        let seed = crate::workspace::BackupStore::open(temp.path().join("seed")).unwrap();
        let task = crate::workspace::Workspace::builder("Notes", &source)
            .include("notes.md")
            .include("files")
            .build()
            .unwrap();
        seed.register(task.clone()).unwrap();
        seed.set_remote(Some(remote.to_str().unwrap()), true)
            .unwrap();
        seed.backup(task.id()).unwrap();
        let destination = temp.path().join("destination");
        fs::create_dir(&destination).unwrap();
        let file = destination.join("notes.md");
        let mut app = fixture(temp.path());
        settle_for(&mut app, Duration::from_secs(60));
        app.spaces
            .create("Shared".into(), remote.to_string_lossy().into_owned());
        settle_for(&mut app, Duration::from_secs(60));
        let space = app
            .spaces
            .list()
            .iter()
            .find(|space| !space.id.is_nil())
            .unwrap()
            .id;
        app.spaces.switch(&mut app.model, space);
        settle_for(&mut app, Duration::from_secs(60));
        let context = egui::Context::default();
        theme::apply(&context);
        context.all_styles_mut(|style| style.animation_time = 0.0);
        configure_fonts(&context);
        for language in [Language::English, Language::Chinese] {
            app.spaces
                .global(&mut app.model, Command::Language(language));
            settle_for(&mut app, Duration::from_secs(60));
            if language == Language::English {
                click(&mut app, &context, language.text("Restore remote backup"));
                settle_for(&mut app, Duration::from_secs(60));
                assert!(!app.settings);
                assert_eq!(app.model.branches.len(), 1);
                app.restore.as_mut().unwrap().path = destination.to_string_lossy().into_owned();
                click(&mut app, &context, language.text("Preview restore"));
            } else {
                fs::write(&file, "unpublished local contents").unwrap();
                click(&mut app, &context, language.text("Restore from remote"));
            }
            settle_for(&mut app, Duration::from_secs(60));
            assert_eq!(app.model.rows[0].draft.id, Some(task.id()));
            assert!(!app.model.rows[0].running);
            assert!(app.model.plan.as_ref().unwrap().is_remote());
            assert_eq!(app.model.plan.as_ref().unwrap().entries().len(), 361);
            if language == Language::English {
                assert!(!file.exists(), "Preparing the preview must not write files");
            } else {
                assert_eq!(
                    fs::read_to_string(&file).unwrap(),
                    "unpublished local contents"
                );
            }
            assert!(app.model.logs.iter().any(|text| text.render(language)
                == language.text("Restore preview is ready. No files have been written.")));
            if language == Language::Chinese {
                fs::write(&file, "changed after preview").unwrap();
                click(&mut app, &context, language.text("Confirm restore"));
                wait_for(&mut app, Duration::from_secs(60));
                assert!(
                    app.model
                        .error
                        .as_ref()
                        .unwrap()
                        .contains("changed after preview")
                );
                render(&mut app, &context, vec![]);
                assert_eq!(fs::read_to_string(&file).unwrap(), "changed after preview");
                click(&mut app, &context, language.text("Retry preview"));
                settle_for(&mut app, Duration::from_secs(60));
            }
            // Even with hundreds of entries the primary action stays on the 800x560 screen.
            let output = render(&mut app, &context, vec![]);
            let label = language.text("Confirm restore");
            let (bounds, clip) = output
                .shapes
                .iter()
                .find_map(|shape| {
                    if let egui::Shape::Text(text) = &shape.shape
                        && text.galley.job.text == label
                    {
                        Some((
                            egui::Rect::from_min_size(text.pos, text.galley.size()),
                            shape.clip_rect,
                        ))
                    } else {
                        None
                    }
                })
                .expect("Confirmation must be visible without scrolling the file list");
            assert!(
                egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 560.0))
                    .contains_rect(bounds),
                "{bounds:?}"
            );
            assert!(
                clip.contains_rect(bounds),
                "The confirmation must not be clipped"
            );
            click(&mut app, &context, label);
            settle_for(&mut app, Duration::from_secs(60));
            assert_eq!(fs::read_to_string(&file).unwrap(), "remote contents");
            assert_eq!(
                fs::read_dir(destination.join("files")).unwrap().count(),
                360
            );
            click(&mut app, &context, language.text("Back to task"));
            assert!(app.restore.is_none());
            assert_eq!(app.selected, Some(task.id()));
            assert!(!app.model.rows[0].running);
        }
    }

    #[test]
    fn desktop_buttons_drive_backup_preview_and_explicit_restore() {
        let temp = tempfile::TempDir::new().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        let file = source.join("notes.md");
        fs::write(&file, "saved").unwrap();
        let mut app = fixture(temp.path());
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
        click(&mut app, &context, language.text("Add task"));
        let draft = app.draft.as_mut().unwrap();
        draft.name = "Desktop notes".into();
        draft.path = source.to_string_lossy().into_owned();
        draft.includes = "notes.md".into();
        click(&mut app, &context, language.text("Follow symbolic links"));
        click(&mut app, &context, language.text("Save task"));
        settle(&mut app);
        assert_eq!(app.model.rows.len(), 1);
        assert!(app.model.rows[0].draft.follow_links);
        let selected = app.model.rows[0].draft.id;
        for _ in 0..2 {
            for action in ["Add task", "Refresh"] {
                click(&mut app, &context, language.text(action));
                settle(&mut app);
                let output = render(&mut app, &context, vec![]);
                assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::epaint::Shape::Text(text) if text.galley.job.text == "Desktop notes")), "Task must remain visible in {action}");
                for shape in &output.shapes {
                    if let egui::Shape::Text(text) = &shape.shape
                        && text.galley.job.text == "Desktop notes"
                    {
                        for section in &text.galley.job.sections {
                            assert!(
                                contrast(section.format.color, theme::SELECTED) >= 4.5,
                                "Task text must remain readable in {action}"
                            );
                        }
                    }
                }
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
        click(&mut app, &context, language.text("Start"));
        settle(&mut app);
        assert!(app.model.rows[0].running);
        click(&mut app, &context, language.text("Run once"));
        settle(&mut app);
        assert!(app.model.rows[0].running);
        assert!(
            app.model
                .logs
                .iter()
                .any(|text| text.render(language) == language.text("Run once requested"))
        );
        click(&mut app, &context, language.text("Stop"));
        settle(&mut app);
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
        for (previous, next, strategy) in [
            ("Fast-forward only", "Merge", PullStrategy::Merge),
            ("Merge", "Rebase", PullStrategy::Rebase),
        ] {
            click(&mut app, &context, "Edit");
            click(&mut app, &context, previous);
            click(&mut app, &context, next);
            click(&mut app, &context, "Save task");
            settle(&mut app);
            assert_eq!(app.model.rows[0].draft.pull_strategy, strategy);
        }
        click(&mut app, &context, "English");
        click(&mut app, &context, "简体中文");
        settle(&mut app);
        click(&mut app, &context, "编辑");
        click(&mut app, &context, "变基");
        click(&mut app, &context, "仅快进");
        click(&mut app, &context, "保存任务");
        settle(&mut app);
        assert_eq!(
            app.model.rows[0].draft.pull_strategy,
            PullStrategy::FastForwardOnly
        );
        for index in 1..6 {
            app.model.send(Command::Save(Draft {
                kind: Kind::Pull,
                name: format!("Repository {index}"),
                path: temp
                    .path()
                    .join(format!("repo-{index}"))
                    .to_string_lossy()
                    .into_owned(),
                ..Draft::default()
            }));
            settle(&mut app);
        }
        let ids = |app: &Desktop| {
            app.model
                .rows
                .iter()
                .map(|row| row.draft.id.unwrap())
                .collect::<Vec<_>>()
        };
        let original = ids(&app);
        let selection = app.selected;
        drag_task(&mut app, &context, original[0], original[5], true, false);
        let mut reordered = original[1..].to_vec();
        reordered.push(original[0]);
        assert_eq!(ids(&app), reordered);
        assert_eq!(app.selected, selection);
        drag_task(&mut app, &context, original[0], original[1], false, false);
        assert_eq!(ids(&app), original);
        drag_task(&mut app, &context, original[0], original[1], true, true);
        assert_eq!(ids(&app), original);

        click(&mut app, &context, "新建工作空间");
        assert_eq!(app.space_form, Some(true));
        app.space_name = "Personal".into();
        app.space_remote = "https://example.invalid/personal.git".into();
        click(&mut app, &context, "保存");
        settle(&mut app);
        assert_eq!(app.spaces.list().len(), 2);
        click(&mut app, &context, "默认工作空间");
        click(&mut app, &context, "Personal");
        settle(&mut app);
        assert!(app.model.rows.is_empty());
        assert!(app.model.history.is_empty() && app.model.plan.is_none());
        assert!(app.selected.is_none() && app.draft.is_none() && !app.settings);
        click(&mut app, &context, "重命名工作空间");
        app.space_name = "Private".into();
        click(&mut app, &context, "保存");
        assert_eq!(app.spaces.label(Language::English), "Private");
        click(&mut app, &context, "简体中文");
        click(&mut app, &context, "English");
        settle(&mut app);
        assert_eq!(app.model.language, Language::English);
        click(&mut app, &context, "Remove workspace");
        settle(&mut app);
        assert!(app.spaces.selected().is_nil());
        assert_eq!(ids(&app), original);
    }
}
