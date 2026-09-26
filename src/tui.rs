//! A terminal interface sharing the desktop task controller and backup store.

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
    interface::{Command, Draft, Kind, Model},
};

struct Screen {
    model: Model,
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
}

/// Opens the TUI and restores terminal modes on normal exit or failure.
pub fn run(data: Option<PathBuf>) -> Result<()> {
    let mut terminal = ratatui::try_init()?;
    struct RestoreTerminal;
    impl Drop for RestoreTerminal {
        fn drop(&mut self) {
            ratatui::restore();
        }
    }
    let _restore = RestoreTerminal;
    let mut screen = Screen {
        model: Model::new(data),
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
    };
    loop {
        screen.model.poll();
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
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            break;
        }
        if !screen.key(key.code, key.modifiers) {
            break;
        }
    }
    Ok(())
}

impl Screen {
    fn draw(&self, frame: &mut Frame) {
        let [header, body, footer] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(4),
        ])
        .areas(frame.area());
        frame.render_widget(
            Paragraph::new("gitwatch  /  Workspaces · Repository watch · Scheduled pull")
                .block(Block::bordered())
                .style(Style::default().fg(Color::Cyan)),
            header,
        );
        if let Some(draft) = &self.draft {
            let all_fields = [
                ("Name", &draft.name),
                ("Local path", &draft.path),
                ("Includes (Alt+Enter: newline)", &draft.includes),
                ("Excludes (one glob per line)", &draft.excludes),
                ("Branch (optional)", &draft.branch),
                ("Remote (empty = no push for watch)", &draft.remote),
                ("Clone URL (pull)", &draft.url),
                ("Pull interval, seconds", &draft.interval),
                ("Watch delay, seconds", &draft.delay),
            ];
            let indices = fields(draft.kind);
            let fields: Vec<_> = indices.iter().map(|index| all_fields[*index]).collect();
            let list: Vec<_> = fields
                .iter()
                .enumerate()
                .map(|(i, (label, value))| {
                    ListItem::new(format!(
                        "{} {label}: {}",
                        if i == self.field { ">" } else { " " },
                        value.replace('\n', " | ")
                    ))
                    .style(if i == self.field {
                        Style::default().fg(Color::Yellow)
                    } else {
                        Style::default()
                    })
                })
                .collect();
            frame.render_widget(
                List::new(list).block(Block::bordered().title(format!(
                    "{:?} task — F2: mode; Tab: field; Ctrl+S: save; Esc: cancel",
                    draft.kind
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
                format!("\nAuto-push: {}", self.auto_push)
            } else if self.import_path.is_some() {
                format!(
                    "\nBranch: {}",
                    self.model
                        .branches
                        .get(self.import_branch)
                        .map(|b| b.branch())
                        .unwrap_or("none")
                )
            } else {
                String::new()
            };
            frame.render_widget(
                Paragraph::new(format!("{text}{extra}")).block(Block::bordered().title(title)),
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
                        "{} {} [{:?}]\n  {}",
                        if r.running { "●" } else { "○" },
                        r.draft.name,
                        r.draft.kind,
                        r.status
                    ))
                })
                .collect();
            let mut selection =
                ListState::default().with_selected((!rows.is_empty()).then_some(self.selected));
            frame.render_stateful_widget(
                List::new(rows)
                    .block(Block::bordered().title("Tasks"))
                    .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan)),
                left,
                &mut selection,
            );
            let mut detail = String::new();
            if self.remove {
                detail.push_str("Remove selected local binding? History is retained. y: confirm / Esc: cancel\n\n");
            }
            if let Some(plan) = &self.model.plan {
                detail.push_str(&format!(
                    "Restore {}\n{}\n",
                    plan.commit(),
                    plan.root().display()
                ));
                detail.push_str(if self.confirm { "Apply this preview? Existing files are copied to recovery first. y: confirm / Esc: cancel\n" } else { "[: previous file  ]: next file  Enter: contents  R: review confirmation\n" });
                for (i, entry) in plan.entries().iter().enumerate() {
                    detail.push_str(&format!(
                        "{} {:?} {}\n",
                        if i == self.entry { ">" } else { " " },
                        entry.change(),
                        entry.path()
                    ));
                }
            } else {
                detail.push_str(
                    "History ([: previous, ]: next; v: restore preview; d: diff to older)\n",
                );
                for (i, entry) in self.model.history.iter().enumerate() {
                    detail.push_str(&format!(
                        "{} {} {}\n",
                        if i == self.history { ">" } else { " " },
                        entry.commit().get(..12).unwrap_or(entry.commit()),
                        entry.summary()
                    ));
                }
                if !self.model.branches.is_empty() {
                    detail.push_str("\nFetched branches (i: select and import):\n");
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
            detail.push_str(&self.model.text);
            detail.push_str("\n\nActivity\n");
            for text in self.model.logs.iter().rev().take(20) {
                detail.push_str(text);
                detail.push('\n');
            }
            frame.render_widget(
                Paragraph::new(detail)
                    .wrap(Wrap { trim: false })
                    .scroll((self.scroll, 0))
                    .block(Block::bordered().title("History / Restore / Activity — PgUp/PgDn")),
                right,
            );
        }
        let status = self.model.error.as_deref().unwrap_or(if self.model.busy {
            "Working…"
        } else {
            "Ready"
        });
        frame.render_widget(Paragraph::new(format!("n: new  e: edit  s: start/stop  b: run once  h: history  p: push  x: remove\nu: backup remote  f: fetch  F5: refresh  q: quit  ↑↓: select\n{status}")).style(Style::default().fg(if self.model.error.is_some() { Color::Red } else { Color::Gray })), footer);
    }

    fn key(&mut self, key: KeyCode, modifiers: KeyModifiers) -> bool {
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
            KeyCode::Char('u') => self.remote = Some(String::new()),
            KeyCode::Char('f') => self.model.send(Command::Fetch),
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
        Kind::Workspace => &[0, 1, 2, 3, 4],
        Kind::Watch => &[0, 1, 4, 5, 8],
        Kind::Pull => &[0, 1, 6, 5, 4, 7],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_form_saves_a_task_and_renders_its_persisted_name() {
        let temp = tempfile::TempDir::new().unwrap();
        let mut screen = Screen {
            model: Model::new(Some(temp.path().join("data"))),
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
        };
        let settle = |screen: &mut Screen| {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while screen.model.busy && std::time::Instant::now() < deadline {
                screen.model.poll();
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(!screen.model.busy);
            assert!(screen.model.error.is_none(), "{:?}", screen.model.error);
        };
        settle(&mut screen);
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
        screen.key(KeyCode::Char('s'), KeyModifiers::CONTROL);
        settle(&mut screen);
        assert_eq!(screen.model.rows[0].draft.name, "Upstream mirror");
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
        screen.key(KeyCode::Char('e'), KeyModifiers::NONE);
        screen.key(KeyCode::Char('!'), KeyModifiers::NONE);
        screen.key(KeyCode::Char('s'), KeyModifiers::CONTROL);
        settle(&mut screen);
        assert_eq!(screen.model.rows.len(), 1);
        assert_eq!(screen.model.rows[0].draft.name, "Upstream mirror!");
        assert!(!screen.key(KeyCode::Char('q'), KeyModifiers::NONE));
    }
}
