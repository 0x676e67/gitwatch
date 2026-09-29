use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    widgets::{Block, Paragraph, Wrap},
};
use uuid::Uuid;

use crate::{
    interface::{Command, Model},
    pull::PullStrategy,
    workspace::{SyncOptions, SyncPhase},
};

pub(super) struct Flow {
    task: Uuid,
    options: SyncOptions,
    selected: usize,
    editing: Option<String>,
    scroll: u16,
}

impl Flow {
    pub(super) fn new(model: &mut Model, task: Uuid) -> Self {
        let options = model
            .rows
            .iter()
            .find(|row| row.draft.id == Some(task))
            .and_then(|row| row.sync.as_ref())
            .map(|s| s.options.clone())
            .unwrap_or_default();
        model.sync = None;
        model.sync_contents = None;
        model.send(Command::SyncStatus(task));
        Self {
            task,
            options,
            selected: 0,
            editing: None,
            scroll: 0,
        }
    }

    pub(super) fn draw(&self, frame: &mut Frame, area: Rect, model: &Model) {
        let lang = model.language;
        let mut text = format!(
            "{} · {} · {} s\n",
            lang.text("Two-way sync"),
            self.options.strategy.label(lang),
            self.options.interval
        );
        if let Some(status) = &model.sync {
            text.push_str(lang.text(match status.phase { SyncPhase::Disabled => "File backup only. Source files are not changed automatically.", SyncPhase::Ready => "Selected files are synchronized in both directions.", SyncPhase::Preview => "Review file changes. Nothing has been written yet.", SyncPhase::Conflict => "Sync paused. Your source files have not been overwritten.", SyncPhase::Applying => "Application was interrupted. Resume the saved operation; new local edits will be protected." }));
            text.push('\n');
            for entry in &status.entries {
                text.push_str(&format!("{:?} {}\n", entry.change(), entry.path()));
            }
            for (index, path) in status.conflicts.iter().enumerate() {
                text.push_str(&format!(
                    "{} {}\n",
                    if index == self.selected { ">" } else { " " },
                    path
                ));
            }
        }
        if let Some(contents) = &model.sync_contents {
            for (label, bytes) in [
                ("Local version", &contents.local),
                ("Remote version", &contents.remote),
                ("Common ancestor", &contents.base),
            ] {
                text.push_str(&format!(
                    "\n{}\n{}\n",
                    lang.text(label),
                    bytes
                        .as_deref()
                        .map(|bytes| std::str::from_utf8(bytes)
                            .map(|text| text.chars().take(2000).collect::<String>())
                            .unwrap_or_else(|_| "[binary]".into()))
                        .unwrap_or_else(|| "[deleted]".into())
                ));
            }
        }
        if let Some(editor) = &self.editing {
            text.push_str(&format!("\n{}\n{editor}\n", lang.text("Edit merged text")));
        }
        text.push('\n');
        text.push_str(lang.text("1/2/3: Rebase/Merge/ff-only  i: interval  p: preview  w: save settings\ny: confirm changes  o: remote preview  u: undo preview  d: disable\n↑↓: conflict  v: compare  l/r: keep local/remote  e: edit text\nCtrl+S: save resolution  Enter: continue  c: cancel operation  Esc: close"));
        frame.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .scroll((self.scroll, 0))
                .block(Block::bordered().title(lang.text("Two-way sync"))),
            area,
        );
    }

    pub(super) fn key(&mut self, key: KeyCode, modifiers: KeyModifiers, model: &mut Model) -> bool {
        if key == KeyCode::Esc {
            return false;
        }
        if model.busy {
            return true;
        }
        let Some(status) = model.sync.clone() else {
            return true;
        };
        let path = status.conflicts.get(self.selected).cloned();
        if let Some(editor) = &mut self.editing {
            if key == KeyCode::Char('s') && modifiers.contains(KeyModifiers::CONTROL) {
                if let Some(path) = path {
                    model.send(Command::SyncResolve(
                        self.task,
                        path,
                        Some(editor.as_bytes().to_vec()),
                    ));
                    self.editing = None;
                }
            } else if key == KeyCode::Enter {
                editor.push('\n');
            } else {
                super::edit(editor, key, modifiers);
            }
            return true;
        }
        match key {
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(8),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(8),
            KeyCode::Char('1') => self.options.strategy = PullStrategy::Rebase,
            KeyCode::Char('2') => self.options.strategy = PullStrategy::Merge,
            KeyCode::Char('3') => self.options.strategy = PullStrategy::FastForwardOnly,
            KeyCode::Char('i') => {
                self.options.interval = match self.options.interval {
                    60 => 300,
                    300 => 900,
                    _ => 60,
                }
            }
            KeyCode::Char('p') => {
                model.send(Command::SyncPreview(self.task, self.options.clone(), false))
            }
            KeyCode::Char('w') => model.send(Command::SyncConfigure(
                self.task,
                self.options.clone(),
                true,
            )),
            KeyCode::Char('o') => {
                model.send(Command::SyncPreview(self.task, self.options.clone(), true))
            }
            KeyCode::Char('u') => model.send(Command::SyncUndo(self.task)),
            KeyCode::Char('d') => model.send(Command::SyncConfigure(
                self.task,
                self.options.clone(),
                false,
            )),
            KeyCode::Char('y')
                if matches!(status.phase, SyncPhase::Preview | SyncPhase::Applying) =>
            {
                if let Some(token) = status.token {
                    model.send(Command::SyncConfirm(self.task, token));
                }
            }
            KeyCode::Char('c') => model.send(Command::SyncCancel(self.task)),
            KeyCode::Enter if status.phase == SyncPhase::Conflict => {
                model.send(Command::SyncContinue(self.task))
            }
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                model.sync_contents = None;
            }
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(status.conflicts.len().saturating_sub(1));
                model.sync_contents = None;
            }
            KeyCode::Char('v') => {
                if let Some(path) = path {
                    model.send(Command::SyncContents(self.task, path));
                }
            }
            KeyCode::Char('l' | 'r') => {
                if let Some(path) = path
                    && let Some(contents) = &model.sync_contents
                {
                    model.send(Command::SyncResolve(
                        self.task,
                        path,
                        if key == KeyCode::Char('l') {
                            contents.local.clone()
                        } else {
                            contents.remote.clone()
                        },
                    ));
                    model.sync_contents = None;
                }
            }
            KeyCode::Char('e') => {
                if let Some(contents) = &model.sync_contents
                    && let Some(bytes) = contents.local.as_deref().or(contents.remote.as_deref())
                    && bytes.len() <= 256 * 1024
                    && let Ok(text) = std::str::from_utf8(bytes)
                {
                    self.editing = Some(text.to_owned());
                }
            }
            _ => {}
        }
        true
    }
}
