//! A terminal interface sharing the desktop task controller and backup store.

mod sync;

use std::{path::PathBuf, time::Duration};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Style},
    widgets::{Block, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::{
    Result,
    i18n::Language,
    interface::{Command, Draft, Kind, Model, spaces::Sessions},
    pull::PullStrategy,
};

struct Screen {
    model: Model,
    spaces: Sessions,
    closing: bool,
    space_menu: bool,
    space_index: usize,
    space_form: Option<bool>,
    space_name: String,
    space_remote: String,
    selected: usize,
    history: usize,
    entry: usize,
    scroll: u16,
    draft: Option<Draft>,
    field: usize,
    confirm: bool,
    remove: bool,
    remote: Option<String>,
    restore_files: Option<String>,
    import_path: Option<String>,
    import_branch: usize,
    auto_push: bool,
    sync: Option<sync::Flow>,
}

/// Opens the TUI and restores terminal modes on normal exit or failure.
pub fn run(data: Option<PathBuf>) -> Result<()> {
    let language = Language::resolve(None, data.as_deref())?;
    run_with_language(data, language)
}

/// Opens the terminal interface with an explicit initial language.
pub fn run_with_language(data: Option<PathBuf>, language: Language) -> Result<()> {
    let (spaces, model) = Sessions::new(data, language)?;
    let mut terminal = ratatui::try_init()?;
    struct RestoreTerminal;
    impl Drop for RestoreTerminal {
        fn drop(&mut self) {
            ratatui::restore();
        }
    }
    let _restore = RestoreTerminal;
    let mut screen = Screen {
        model,
        spaces,
        closing: false,
        space_menu: false,
        space_index: 0,
        space_form: None,
        space_name: String::new(),
        space_remote: String::new(),
        selected: 0,
        history: 0,
        entry: 0,
        scroll: 0,
        draft: None,
        field: 0,
        confirm: false,
        remove: false,
        remote: None,
        restore_files: None,
        import_path: None,
        import_branch: 0,
        auto_push: false,
        sync: None,
    };
    loop {
        if screen.closing && screen.spaces.shutdown_finished(&screen.model) {
            break;
        }
        if !screen.closing && screen.spaces.poll(&mut screen.model) {
            screen.clear_workspace();
        }
        terminal.draw(|frame| screen.draw(frame))?;
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        if screen.closing {
            continue;
        }
        if (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
            || !screen.key(key.code, key.modifiers)
        {
            screen.closing = true;
            screen.spaces.request_shutdown(&screen.model);
        }
    }
    Ok(())
}

impl Drop for Screen {
    fn drop(&mut self) {
        self.spaces.request_shutdown(&self.model);
    }
}

impl Screen {
    fn draw(&self, frame: &mut Frame) {
        let language = self.model.language;
        if self.closing {
            frame.render_widget(
                Paragraph::new(language.text("Waiting for current operations to finish safely."))
                    .block(Block::bordered().title(language.text("Stopping tasks…"))),
                frame.area(),
            );
            return;
        }
        let [header, body, footer] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(4),
        ])
        .areas(frame.area());
        frame.render_widget(
            Paragraph::new(
                self.model
                    .update
                    .as_ref()
                    .map(|release| {
                        language.format(
                            "gitwatch {0} is available. Run gitwatch self update.",
                            &[release.version()],
                        )
                    })
                    .unwrap_or_else(|| {
                        format!(
                            "gitwatch / {} — {}",
                            self.spaces.label(language),
                            language.text("F4: workspaces")
                        )
                    }),
            )
            .block(Block::bordered())
            .style(Style::default().fg(Color::Cyan)),
            header,
        );
        if let Some(sync) = &self.sync {
            sync.draw(frame, body, &self.model);
        } else if self.space_menu {
            let mut text = String::new();
            if let Some(create) = self.space_form {
                text.push_str(&format!(
                    "{} {}: {}\n",
                    if self.field == 0 { ">" } else { " " },
                    language.text("Name"),
                    self.space_name
                ));
                if create {
                    text.push_str(&format!(
                        "{} {}: {}\n",
                        if self.field == 1 { ">" } else { " " },
                        language.text("Remote repository (required)"),
                        self.space_remote
                    ));
                }
                text.push_str(language.text("Tab: next field  Ctrl+S: save  Esc: cancel"));
            } else {
                for (index, space) in self.spaces.list().iter().enumerate() {
                    text.push_str(&format!(
                        "{} {}{}\n",
                        if index == self.space_index { ">" } else { " " },
                        space.label(language),
                        if space.id == self.spaces.selected() {
                            " *"
                        } else {
                            ""
                        }
                    ));
                }
                text.push_str(language.text("↑↓: select  Enter: switch  n: new  e: rename current  x: remove current  Esc: close"));
                text.push('\n');
                text.push_str(
                    language
                        .text("Only empty workspaces can be removed. Backup data stays on disk."),
                );
                text.push('\n');
                text.push_str(language.text("Each workspace keeps its own tasks and backup history. Switching leaves started tasks running."));
            }
            frame.render_widget(
                Paragraph::new(text)
                    .wrap(Wrap { trim: false })
                    .block(Block::bordered().title(language.text("Workspaces"))),
                body,
            );
        } else if let Some(draft) = &self.draft {
            let all_fields: [(&str, &str); 11] = [
                ("Name", &draft.name),
                ("Local path", &draft.path),
                ("Includes (Alt+Enter: newline)", &draft.includes),
                ("Excludes (one glob per line)", &draft.excludes),
                ("Branch (optional)", &draft.branch),
                ("Remote (empty = no push for watch)", &draft.remote),
                ("Clone URL (pull)", &draft.url),
                ("Pull interval, seconds", &draft.interval),
                ("Watch delay, seconds", &draft.delay),
                (
                    "Pull strategy (Left/Right)",
                    draft.pull_strategy.label(language),
                ),
                (
                    "Follow symbolic links (Left/Right)",
                    language.text(if draft.follow_links { "yes" } else { "no" }),
                ),
            ];
            let indices = fields(draft.kind);
            let fields: Vec<_> = indices.iter().map(|index| all_fields[*index]).collect();
            let mut list: Vec<_> = fields
                .iter()
                .enumerate()
                .map(|(i, (label, value))| {
                    ListItem::new(format!(
                        "{} {}: {}",
                        if i == self.field { ">" } else { " " },
                        language.text(label),
                        value.replace('\n', " | ")
                    ))
                    .style(if i == self.field {
                        Style::default().fg(Color::Yellow)
                    } else {
                        Style::default()
                    })
                })
                .collect();
            if draft.kind == Kind::Pull {
                list.push(ListItem::new(draft.pull_strategy.description(language)));
            }
            if draft.kind == Kind::Workspace {
                list.push(ListItem::new(language.text("Back up target contents, including outside this project. Restore requires ordinary destination paths.")));
            }
            frame.render_widget(
                List::new(list).block(Block::bordered().title(language.format(
                    "{0} task — F2: mode; Tab: field; Ctrl+S: save; Esc: cancel",
                    &[draft.kind.label(language)],
                ))),
                body,
            );
        } else if let Some(text) = self
            .remote
            .as_ref()
            .or(self.restore_files.as_ref())
            .or(self.import_path.as_ref())
        {
            let title = if self.remote.is_some() {
                "Backup remote URL (Ctrl+P: toggle auto-push; Enter: save; Esc: cancel)"
            } else if self.import_path.is_some() {
                "Import: ↑↓ choose branch; type local directory; Enter: import; Esc: cancel"
            } else {
                "Restore paths: one per line, empty = all (Alt+Enter: newline; Enter: preview)"
            };
            let extra = if self.remote.is_some() {
                language.format(
                    "\nAuto-push: {0}",
                    &[language.text(if self.auto_push { "yes" } else { "no" })],
                )
            } else if self.import_path.is_some() {
                language.format(
                    "\nBranch: {0}",
                    &[self
                        .model
                        .branches
                        .get(self.import_branch)
                        .map(|b| b.branch())
                        .unwrap_or(language.text("none"))],
                )
            } else {
                String::new()
            };
            frame.render_widget(
                Paragraph::new(format!("{text}{extra}"))
                    .block(Block::bordered().title(language.text(title))),
                body,
            );
        } else {
            let [left, right] =
                Layout::horizontal([Constraint::Percentage(36), Constraint::Percentage(64)])
                    .areas(body);
            let rows: Vec<_> = self
                .model
                .rows
                .iter()
                .map(|r| {
                    ListItem::new(format!(
                        "{} {} [{}]\n  {}",
                        if r.running { "●" } else { "○" },
                        r.draft.name,
                        r.draft.kind.label(language),
                        r.status.render(language)
                    ))
                })
                .collect();
            let mut selection =
                ListState::default().with_selected((!rows.is_empty()).then_some(self.selected));
            frame.render_stateful_widget(
                List::new(rows)
                    .block(Block::bordered().title(language.text("Tasks")))
                    .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan)),
                left,
                &mut selection,
            );
            let mut detail = String::new();
            if self.remove {
                detail.push_str(language.text("Remove selected local binding? History is retained. y: confirm / Esc: cancel\n\n"));
            }
            if let Some(plan) = &self.model.plan {
                if plan.is_remote() {
                    detail.push_str(language.text("Use the previewed remote contents. Both backup histories and extra local files are kept. Upload separately to share the result."));
                    detail.push('\n');
                }
                detail.push_str(&language.format(
                    "Restore {0}\n{1}\n",
                    &[plan.commit(), &plan.root().display().to_string()],
                ));
                detail.push_str(language.text(if self.confirm { "Apply this preview? Existing files are copied to recovery first. y: confirm / Esc: cancel\n" } else { "[: previous file  ]: next file  Enter: contents  R: review confirmation\n" }));
                for (i, entry) in plan.entries().iter().enumerate() {
                    detail.push_str(&format!(
                        "{} {} {}\n",
                        if i == self.entry { ">" } else { " " },
                        language.text(&format!("{:?}", entry.change())),
                        entry.path()
                    ));
                }
            } else {
                detail.push_str(language.text(
                    "History ([: previous, ]: next; v: restore preview; d: diff to older)\n",
                ));
                for (i, entry) in self.model.history.iter().enumerate() {
                    detail.push_str(&format!(
                        "{} {} {}\n",
                        if i == self.history { ">" } else { " " },
                        entry.commit().get(..12).unwrap_or(entry.commit()),
                        entry.summary()
                    ));
                }
                if !self.model.branches.is_empty() {
                    detail.push_str(language.text("\nFetched branches (i: select and import):\n"));
                    for branch in &self.model.branches {
                        detail.push_str(&format!(
                            "{}  {}\n",
                            branch.branch(),
                            branch.manifest().name()
                        ));
                    }
                }
            }
            detail.push('\n');
            detail.push_str(&self.model.text.render(language));
            detail.push_str(language.text("\n\nActivity\n"));
            for text in self.model.logs.iter().rev().take(20) {
                detail.push_str(&text.render(language));
                detail.push('\n');
            }
            frame.render_widget(
                Paragraph::new(detail)
                    .wrap(Wrap { trim: false })
                    .scroll((self.scroll, 0))
                    .block(
                        Block::bordered()
                            .title(language.text("History / Restore / Activity — PgUp/PgDn")),
                    ),
                right,
            );
        }
        let status = self
            .spaces
            .error
            .as_ref()
            .or(self.model.error.as_ref())
            .map(|error| language.error(error))
            .or_else(|| self.spaces.background_error(language))
            .unwrap_or_else(|| {
                language
                    .text(if self.model.busy || self.spaces.busy() {
                        "Working…"
                    } else {
                        "Ready"
                    })
                    .into()
            });
        let shortcuts = language.text("n: new  e: edit  s: start/stop  b: run once  h: history  p: push  x: remove\nu: backup remote  f: fetch  o: restore remote  F3: language  F5: refresh  q: quit  ↑↓: select");
        frame.render_widget(
            Paragraph::new(format!(
                "F6: {}\n{shortcuts}\n{status}",
                language.text("Two-way sync")
            ))
            .style(Style::default().fg(if self.model.error.is_some() {
                Color::Red
            } else {
                Color::Gray
            })),
            footer,
        );
    }

    fn key(&mut self, key: KeyCode, modifiers: KeyModifiers) -> bool {
        if let Some(sync) = &mut self.sync {
            if !sync.key(key, modifiers, &mut self.model) {
                self.sync = None;
            }
            return true;
        }
        if key == KeyCode::F(3) {
            let language = match self.model.language {
                Language::English => Language::Chinese,
                Language::Chinese => Language::English,
            };
            self.spaces
                .global(&mut self.model, Command::Language(language));
            return true;
        }
        if key == KeyCode::F(4) {
            self.space_menu = true;
            self.space_index = self
                .spaces
                .list()
                .iter()
                .position(|space| space.id == self.spaces.selected())
                .unwrap_or(0);
            self.space_form = None;
            return true;
        }
        if self.space_menu {
            self.workspace_key(key, modifiers);
            return true;
        }
        if key == KeyCode::Esc {
            self.draft = None;
            self.remote = None;
            self.restore_files = None;
            self.import_path = None;
            self.confirm = false;
            self.remove = false;
            self.model.plan = None;
            return true;
        }
        if let Some(draft) = &mut self.draft {
            if key == KeyCode::Char('s') && modifiers.contains(KeyModifiers::CONTROL) {
                if !self.model.busy {
                    self.model.send(Command::Save(draft.clone()));
                    self.draft = None;
                }
                return true;
            }
            if key == KeyCode::F(2) && draft.id.is_none() {
                draft.kind = match draft.kind {
                    Kind::Workspace => Kind::Watch,
                    Kind::Watch => Kind::Pull,
                    Kind::Pull => Kind::Workspace,
                };
                if draft.kind == Kind::Watch {
                    draft.remote.clear();
                }
                self.field = 0;
            } else if key == KeyCode::Tab {
                self.field = (self.field + 1) % fields(draft.kind).len();
            } else if key == KeyCode::BackTab {
                self.field = (self.field + fields(draft.kind).len() - 1) % fields(draft.kind).len();
            } else if fields(draft.kind)[self.field] == 10 {
                if matches!(key, KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')) {
                    draft.follow_links = !draft.follow_links;
                }
            } else if fields(draft.kind)[self.field] == 9 {
                use PullStrategy::{FastForwardOnly, Merge, Rebase};
                draft.pull_strategy = match (draft.pull_strategy, key) {
                    (FastForwardOnly, KeyCode::Right | KeyCode::Char(' '))
                    | (Rebase, KeyCode::Left) => Merge,
                    (Merge, KeyCode::Right | KeyCode::Char(' '))
                    | (FastForwardOnly, KeyCode::Left) => Rebase,
                    (Rebase, KeyCode::Right | KeyCode::Char(' ')) | (Merge, KeyCode::Left) => {
                        FastForwardOnly
                    }
                    (strategy, _) => strategy,
                };
            } else {
                let value = match fields(draft.kind)[self.field] {
                    0 => &mut draft.name,
                    1 => &mut draft.path,
                    2 => &mut draft.includes,
                    3 => &mut draft.excludes,
                    4 => &mut draft.branch,
                    5 => &mut draft.remote,
                    6 => &mut draft.url,
                    7 => &mut draft.interval,
                    _ => &mut draft.delay,
                };
                edit(value, key, modifiers);
            }
            return true;
        }
        if self.remote.is_some() || self.restore_files.is_some() || self.import_path.is_some() {
            if self.remote.is_some()
                && key == KeyCode::Char('p')
                && modifiers.contains(KeyModifiers::CONTROL)
            {
                self.auto_push = !self.auto_push;
                return true;
            }
            if self.import_path.is_some() {
                if key == KeyCode::Up {
                    self.import_branch = self.import_branch.saturating_sub(1);
                    return true;
                }
                if key == KeyCode::Down {
                    self.import_branch =
                        (self.import_branch + 1).min(self.model.branches.len().saturating_sub(1));
                    return true;
                }
            }
            if key == KeyCode::Enter && !modifiers.contains(KeyModifiers::ALT) && !self.model.busy {
                if let Some(url) = self.remote.take() {
                    self.model.send(Command::Remote(url, self.auto_push));
                }
                if let Some(files) = self.restore_files.take()
                    && let Some(id) = self.model.rows.get(self.selected).and_then(|r| r.draft.id)
                    && let Some(entry) = self.model.history.get(self.history)
                {
                    self.model.send(Command::Preview(
                        id,
                        entry.commit().into(),
                        crate::interface::lines(&files),
                    ));
                }
                if let Some(path) = self.import_path.take()
                    && let Some(branch) = self.model.branches.get(self.import_branch)
                {
                    self.model
                        .send(Command::Import(branch.branch().into(), path.into()));
                }
            } else if let Some(text) = self
                .remote
                .as_mut()
                .or(self.restore_files.as_mut())
                .or(self.import_path.as_mut())
            {
                edit(text, key, modifiers);
            }
            return true;
        }
        let row = self.model.rows.get(self.selected);
        let id = row.and_then(|r| r.draft.id);
        match key {
            KeyCode::F(6) => {
                if let Some(row) = row
                    && row.draft.kind == Kind::Workspace
                    && let Some(id) = id
                {
                    self.sync = Some(sync::Flow::new(&mut self.model, id));
                }
            }
            KeyCode::Char('q') => return false,
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(self.model.rows.len().saturating_sub(1));
                self.clear_detail();
            }
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                self.clear_detail();
            }
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(8),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(8),
            KeyCode::Char('n') => {
                self.draft = Some(Draft::default());
                self.field = 0;
            }
            KeyCode::Char('e') => {
                self.draft = row.map(|r| r.draft.clone());
                self.field = 0;
            }
            KeyCode::Char('s') => {
                if let Some(id) = id {
                    self.model.send(if row.is_some_and(|r| r.running) {
                        Command::Stop(id)
                    } else {
                        Command::Start(id)
                    });
                }
            }
            KeyCode::Char('b') => {
                if let Some(id) = id {
                    self.model.send(Command::Once(id));
                }
            }
            KeyCode::Char('h') => {
                if let Some(id) = id {
                    self.history = 0;
                    self.model.send(Command::History(id));
                }
            }
            KeyCode::Char('p') => {
                if let Some(id) = id {
                    self.model.send(Command::Push(id));
                }
            }
            KeyCode::Char('x') => self.remove = id.is_some(),
            KeyCode::Char('u') => {
                if self.spaces.selected().is_nil() {
                    self.model.error =
                        Some("The default workspace is local; its remote cannot be changed".into());
                } else {
                    self.remote = Some(self.model.remote.clone().unwrap_or_default());
                    self.auto_push = self.model.auto_push;
                }
            }
            KeyCode::Char('f') => self.model.send(Command::Fetch),
            KeyCode::Char('o') => {
                if let Some(id) = id
                    && row.is_some_and(|row| row.draft.kind == Kind::Workspace)
                {
                    self.entry = 0;
                    self.scroll = 0;
                    self.confirm = false;
                    self.model.plan = None;
                    self.model.text.clear();
                    self.model.send(Command::PreviewRemote(id));
                }
            }
            KeyCode::Char('i') => {
                if !self.model.branches.is_empty() {
                    self.import_path = Some(String::new());
                }
            }
            KeyCode::F(5) => self.model.send(Command::Refresh),
            KeyCode::Char('[') => {
                if self.model.plan.is_some() {
                    self.entry = self.entry.saturating_sub(1);
                } else {
                    self.history = self.history.saturating_sub(1);
                }
            }
            KeyCode::Char(']') => {
                if let Some(plan) = &self.model.plan {
                    self.entry = (self.entry + 1).min(plan.entries().len().saturating_sub(1));
                } else {
                    self.history =
                        (self.history + 1).min(self.model.history.len().saturating_sub(1));
                }
            }
            KeyCode::Char('v') => {
                if !self.model.history.is_empty() {
                    self.restore_files = Some(String::new());
                    self.entry = 0;
                }
            }
            KeyCode::Char('d') => {
                if let Some(id) = id
                    && let Some(newer) = self.model.history.get(self.history)
                    && let Some(older) = self.model.history.get(self.history + 1)
                {
                    self.model.send(Command::Diff(
                        id,
                        older.commit().into(),
                        newer.commit().into(),
                    ));
                }
            }
            KeyCode::Enter => {
                if let Some(plan) = &self.model.plan
                    && let Some(entry) = plan.entries().get(self.entry)
                {
                    self.model
                        .send(Command::Contents(plan.id(), entry.path().into()));
                }
            }
            KeyCode::Char('R') => self.confirm = self.model.plan.is_some(),
            KeyCode::Char('y') => {
                if self.confirm
                    && let Some(plan) = &self.model.plan
                {
                    self.model.send(Command::Restore(plan.id()));
                    self.confirm = false;
                    self.model.plan = None;
                } else if self.remove
                    && let Some(id) = id
                {
                    self.model.send(Command::Remove(id));
                    self.remove = false;
                }
            }
            _ => {}
        }
        true
    }

    fn clear_detail(&mut self) {
        self.model.history.clear();
        self.model.plan = None;
        self.model.text.clear();
        self.history = 0;
        self.scroll = 0;
        self.confirm = false;
        self.remove = false;
    }

    fn workspace_key(&mut self, key: KeyCode, modifiers: KeyModifiers) {
        if key == KeyCode::Esc {
            if self.space_form.take().is_none() {
                self.space_menu = false;
            }
            return;
        }
        if self.spaces.busy() {
            return;
        }
        if let Some(create) = self.space_form {
            if key == KeyCode::Char('s') && modifiers.contains(KeyModifiers::CONTROL) {
                if create {
                    self.spaces
                        .create(self.space_name.clone(), self.space_remote.clone());
                } else {
                    self.spaces.rename(&self.space_name);
                }
                if self.spaces.error.is_none() {
                    self.space_form = None;
                }
            } else if matches!(key, KeyCode::Tab | KeyCode::BackTab) {
                self.field = if create { 1 - self.field } else { 0 };
            } else {
                edit(
                    if self.field == 0 {
                        &mut self.space_name
                    } else {
                        &mut self.space_remote
                    },
                    key,
                    modifiers,
                );
            }
            return;
        }
        let before = self.spaces.selected();
        match key {
            KeyCode::Down => {
                self.space_index =
                    (self.space_index + 1).min(self.spaces.list().len().saturating_sub(1))
            }
            KeyCode::Up => self.space_index = self.space_index.saturating_sub(1),
            KeyCode::Enter => {
                if let Some(space) = self.spaces.list().get(self.space_index) {
                    self.spaces.switch(&mut self.model, space.id);
                }
                if self.spaces.error.is_none() {
                    self.space_menu = false;
                }
            }
            KeyCode::Char('n') => {
                self.space_form = Some(true);
                self.space_name.clear();
                self.space_remote.clear();
                self.field = 0;
            }
            KeyCode::Char('e') if !before.is_nil() => {
                self.space_form = Some(false);
                self.space_name = self.spaces.label(self.model.language).into();
                self.field = 0;
            }
            KeyCode::Char('x') => self.spaces.remove(&mut self.model),
            _ => {}
        }
        if self.spaces.selected() != before {
            self.clear_workspace();
        }
    }

    fn clear_workspace(&mut self) {
        self.sync = None;
        self.selected = 0;
        self.draft = None;
        self.remote = None;
        self.restore_files = None;
        self.import_path = None;
        self.import_branch = 0;
        self.entry = 0;
        self.clear_detail();
    }
}

fn edit(text: &mut String, key: KeyCode, modifiers: KeyModifiers) {
    match key {
        KeyCode::Backspace => {
            text.pop();
        }
        KeyCode::Enter if modifiers.contains(KeyModifiers::ALT) => text.push('\n'),
        KeyCode::Char(c) if !modifiers.contains(KeyModifiers::CONTROL) => text.push(c),
        _ => {}
    }
}

fn fields(kind: Kind) -> &'static [usize] {
    match kind {
        Kind::Workspace => &[0, 1, 2, 3, 4, 10],
        Kind::Watch => &[0, 1, 4, 5, 8],
        Kind::Pull => &[0, 1, 6, 5, 4, 7, 9],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_form_saves_a_task_and_renders_its_persisted_name() {
        let temp = tempfile::TempDir::new().unwrap();
        let (spaces, model) =
            Sessions::new(Some(temp.path().join("data")), Language::English).unwrap();
        let mut screen = Screen {
            model,
            spaces,
            closing: false,
            space_menu: false,
            space_index: 0,
            space_form: None,
            space_name: String::new(),
            space_remote: String::new(),
            selected: 0,
            history: 0,
            entry: 0,
            scroll: 0,
            draft: None,
            field: 0,
            confirm: false,
            remove: false,
            remote: None,
            restore_files: None,
            import_path: None,
            import_branch: 0,
            auto_push: false,
            sync: None,
        };
        let settle = |screen: &mut Screen| {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while (screen.model.busy || screen.spaces.busy())
                && std::time::Instant::now() < deadline
            {
                if screen.spaces.poll(&mut screen.model) {
                    screen.clear_workspace();
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(!screen.model.busy && !screen.spaces.busy());
            assert!(screen.model.error.is_none(), "{:?}", screen.model.error);
        };
        settle(&mut screen);
        screen.key(KeyCode::Char('n'), KeyModifiers::NONE);
        for _ in 0..5 {
            screen.key(KeyCode::Tab, KeyModifiers::NONE);
        }
        screen.key(KeyCode::Right, KeyModifiers::NONE);
        assert!(screen.draft.as_ref().unwrap().follow_links);
        screen.key(KeyCode::Left, KeyModifiers::NONE);
        assert!(!screen.draft.as_ref().unwrap().follow_links);
        screen.key(KeyCode::Esc, KeyModifiers::NONE);
        screen.key(KeyCode::Char('n'), KeyModifiers::NONE);
        screen.key(KeyCode::F(2), KeyModifiers::NONE);
        screen.key(KeyCode::F(2), KeyModifiers::NONE);
        for c in "Upstream mirror".chars() {
            screen.key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        screen.key(KeyCode::Tab, KeyModifiers::NONE);
        for c in temp.path().join("mirror").to_string_lossy().chars() {
            screen.key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        screen.key(KeyCode::Tab, KeyModifiers::NONE);
        for c in "https://example.invalid/repo.git".chars() {
            screen.key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        for _ in 0..4 {
            screen.key(KeyCode::Tab, KeyModifiers::NONE);
        }
        screen.key(KeyCode::Right, KeyModifiers::NONE);
        screen.key(KeyCode::Right, KeyModifiers::NONE);
        screen.key(KeyCode::Left, KeyModifiers::NONE);
        screen.key(KeyCode::Char('s'), KeyModifiers::CONTROL);
        settle(&mut screen);
        assert_eq!(screen.model.rows[0].draft.name, "Upstream mirror");
        assert_eq!(
            screen.model.rows[0].draft.pull_strategy,
            PullStrategy::Merge
        );
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 32)).unwrap();
        terminal.draw(|frame| screen.draw(frame)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Upstream mirror"));
        assert!(rendered.contains("Stopped"));
        screen.key(KeyCode::F(3), KeyModifiers::NONE);
        settle(&mut screen);
        assert_eq!(screen.model.language, Language::Chinese);
        let preferences =
            std::fs::read_to_string(temp.path().join("data/preferences.json")).unwrap();
        assert!(preferences.contains("zh-CN"));
        for (width, height) in [(120, 32), (80, 24)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| screen.draw(frame)).unwrap();
            let rendered = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(rendered.contains("Upstream mirror"));
            assert!(rendered.replace(' ', "").contains("已停止"), "{rendered}");
        }
        screen.key(KeyCode::Char('e'), KeyModifiers::NONE);
        screen.key(KeyCode::Char('!'), KeyModifiers::NONE);
        screen.key(KeyCode::Char('s'), KeyModifiers::CONTROL);
        settle(&mut screen);
        assert_eq!(screen.model.rows.len(), 1);
        assert_eq!(screen.model.rows[0].draft.name, "Upstream mirror!");
        screen.key(KeyCode::F(4), KeyModifiers::NONE);
        screen.key(KeyCode::Char('n'), KeyModifiers::NONE);
        for c in "Private".chars() {
            screen.key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        screen.key(KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert!(screen.spaces.error.is_some());
        screen.key(KeyCode::Tab, KeyModifiers::NONE);
        for c in "https://example.invalid/private.git".chars() {
            screen.key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        screen.key(KeyCode::Char('s'), KeyModifiers::CONTROL);
        settle(&mut screen);
        assert_eq!(screen.spaces.list().len(), 2);
        terminal.draw(|frame| screen.draw(frame)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Private"));
        screen.key(KeyCode::Down, KeyModifiers::NONE);
        screen.key(KeyCode::Enter, KeyModifiers::NONE);
        settle(&mut screen);
        assert!(screen.model.rows.is_empty() && !screen.space_menu);
        assert_eq!(screen.spaces.label(Language::English), "Private");
        screen.key(KeyCode::F(3), KeyModifiers::NONE);
        settle(&mut screen);
        assert_eq!(screen.model.language, Language::English);
        screen.key(KeyCode::F(4), KeyModifiers::NONE);
        screen.key(KeyCode::Char('e'), KeyModifiers::NONE);
        screen.key(KeyCode::Char('!'), KeyModifiers::NONE);
        screen.key(KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert_eq!(screen.spaces.label(Language::English), "Private!");
        screen.key(KeyCode::Char('x'), KeyModifiers::NONE);
        settle(&mut screen);
        assert!(screen.spaces.selected().is_nil());
        assert_eq!(screen.model.rows[0].draft.name, "Upstream mirror!");
        screen.key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!screen.key(KeyCode::Char('q'), KeyModifiers::NONE));
    }
}
