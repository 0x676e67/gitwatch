use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender},
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    Result,
    git::Lock,
    i18n::{Language, Text},
    paths,
    pull::{PullOptions, PullTask},
    watch::{self, Event, MonitorOptions, Repository, StopToken, WatchOptions},
    workspace::{BackupStore, HistoryEntry, RemoteWorkspace, RestorePlan, Workspace},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Kind {
    #[default]
    Workspace,
    Watch,
    Pull,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Draft {
    pub id: Option<Uuid>,
    pub kind: Kind,
    pub name: String,
    pub path: String,
    pub includes: String,
    pub excludes: String,
    pub branch: String,
    pub remote: String,
    pub url: String,
    pub interval: String,
    pub delay: String,
}

pub(crate) struct Row {
    pub draft: Draft,
    pub running: bool,
    pub status: Text,
}

pub(crate) enum Command {
    Refresh,
    Save(Draft),
    Remove(Uuid),
    Start(Uuid),
    Stop(Uuid),
    Once(Uuid),
    Push(Uuid),
    Remote(String, bool),
    Fetch,
    Import(String, PathBuf),
    History(Uuid),
    Diff(Uuid, String, String),
    Preview(Uuid, String, Vec<String>),
    Contents(Uuid, String),
    Restore(Uuid),
}

enum Message {
    Rows(Vec<Row>),
    Status(Uuid, Text),
    History(Vec<HistoryEntry>),
    Branches(Vec<RemoteWorkspace>),
    Preview(RestorePlan),
    Text(Text),
    Done(Result<Text>),
}

pub(crate) struct Model {
    pub language: Language,
    directory: Option<PathBuf>,
    pub rows: Vec<Row>,
    pub history: Vec<HistoryEntry>,
    pub branches: Vec<RemoteWorkspace>,
    pub plan: Option<RestorePlan>,
    pub text: Text,
    pub logs: VecDeque<Text>,
    pub busy: bool,
    pub error: Option<String>,
    sender: Sender<Command>,
    receiver: Receiver<Message>,
    stop: StopToken,
    worker: Option<JoinHandle<()>>,
}

struct Worker {
    store: BackupStore,
    sender: Sender<Message>,
    active: HashMap<Uuid, (StopToken, JoinHandle<()>)>,
    states: HashMap<Uuid, Text>,
}

impl Kind {
    pub fn label(self, language: Language) -> &'static str {
        language.text(match self {
            Self::Workspace => "Workspace backup",
            Self::Watch => "Git watch",
            Self::Pull => "Scheduled pull",
        })
    }
}

// ===== impl Draft =====

impl Default for Draft {
    fn default() -> Self {
        Self {
            id: None,
            kind: Kind::Workspace,
            name: String::new(),
            path: String::new(),
            includes: "AGENTS.md\n.agents".into(),
            excludes: String::new(),
            branch: String::new(),
            remote: "origin".into(),
            url: String::new(),
            interval: "3600".into(),
            delay: "2".into(),
        }
    }
}

impl Draft {
    fn workspace(value: &Workspace) -> Self {
        Self {
            id: Some(value.id()),
            name: value.name().into(),
            path: value.root().to_string_lossy().into_owned(),
            includes: value.includes().join("\n"),
            excludes: value.excludes().join("\n"),
            branch: value.branch().into(),
            ..Self::default()
        }
    }

    fn watch_options(&self) -> WatchOptions {
        let mut options = WatchOptions::default();
        if !self.remote.trim().is_empty() {
            options = options.remote(self.remote.trim());
        }
        if !self.branch.trim().is_empty() {
            options = options.branch(self.branch.trim());
        }
        options
    }

    fn monitor(&self) -> Result<MonitorOptions> {
        let delay = seconds(&self.delay)?;
        Ok(MonitorOptions::default()
            .debounce(delay)
            .max_wait(delay.max(Duration::from_secs(60))))
    }

    fn pull_options(&self) -> Result<PullOptions> {
        let mut options = PullOptions::new(&self.path).interval(seconds(&self.interval)?);
        if !self.remote.trim().is_empty() {
            options = options.remote(self.remote.trim());
        }
        if !self.branch.trim().is_empty() {
            options = options.branch(self.branch.trim());
        }
        if !self.url.trim().is_empty() {
            options = options.url(self.url.trim());
        }
        Ok(options)
    }
}

// ===== impl Model =====

impl Model {
    #[cfg(test)]
    pub fn new(data: Option<PathBuf>) -> Self {
        Self::with_language(data, Language::English)
    }

    pub fn with_language(data: Option<PathBuf>, language: Language) -> Self {
        let directory = data
            .clone()
            .or_else(|| BackupStore::default_directory().ok());
        let (sender, commands) = mpsc::channel();
        let (messages, receiver) = mpsc::channel();
        let stop = StopToken::default();
        let worker_stop = stop.clone();
        let worker = thread::spawn(move || {
            let result = (|| -> Result<()> {
                let directory = match data {
                    Some(path) => path,
                    None => BackupStore::default_directory()?,
                };
                let mut worker = Worker {
                    store: BackupStore::open(directory)?,
                    sender: messages.clone(),
                    active: HashMap::new(),
                    states: HashMap::new(),
                };
                worker.refresh()?;
                let _ = messages.send(Message::Done(Ok(
                    "Ready. Tasks start only when requested.".into()
                )));
                while !worker_stop.is_stopped() {
                    worker.reap();
                    match commands.recv_timeout(Duration::from_millis(100)) {
                        Ok(command) => {
                            let result = worker.command(command);
                            let _ = messages.send(Message::Done(result));
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                Ok(())
            })();
            if let Err(error) = result {
                let _ = messages.send(Message::Done(Err(error)));
            }
        });
        Self {
            rows: Vec::new(),
            history: Vec::new(),
            branches: Vec::new(),
            plan: None,
            text: Text::default(),
            language,
            directory,
            logs: VecDeque::new(),
            busy: true,
            error: None,
            sender,
            receiver,
            stop,
            worker: Some(worker),
        }
    }

    pub fn send(&mut self, command: Command) {
        if self.busy {
            return;
        }
        self.error = None;
        match self.sender.send(command) {
            Ok(()) => self.busy = true,
            Err(_) => self.error = Some("Background controller is unavailable".into()),
        }
    }

    pub fn poll(&mut self) {
        while let Ok(message) = self.receiver.try_recv() {
            match message {
                Message::Rows(mut rows) => {
                    for row in &mut rows {
                        if row.running
                            && row.status == Text::from("Running")
                            && let Some(previous) = self
                                .rows
                                .iter()
                                .find(|r| r.running && r.draft.id == row.draft.id)
                        {
                            row.status.clone_from(&previous.status);
                        }
                    }
                    self.rows = rows;
                }
                Message::History(history) => {
                    self.history = history;
                    self.plan = None;
                    self.text.clear();
                }
                Message::Branches(branches) => self.branches = branches,
                Message::Preview(plan) => {
                    self.plan = Some(plan);
                    self.text.clear();
                }
                Message::Text(text) => self.text = text,
                Message::Status(id, text) => {
                    if let Some(row) = self.rows.iter_mut().find(|r| r.draft.id == Some(id)) {
                        row.status = text.clone();
                    }
                    self.log(Text::format("{0}: {1}", [Text::value(id), text]));
                }
                Message::Done(result) => {
                    self.busy = false;
                    match result {
                        Ok(text) => self.log(text),
                        Err(error) => {
                            let text = format!("{error:#}");
                            self.error = Some(text.clone());
                            self.log(Text::Error(text));
                        }
                    }
                }
            }
        }
    }

    pub fn set_language(&mut self, language: Language) {
        let result = self
            .directory
            .as_deref()
            .context("Cannot locate user data directory")
            .and_then(|directory| language.save(directory));
        match result {
            Ok(()) => self.language = language,
            Err(error) => self.error = Some(format!("{error:#}")),
        }
    }

    fn log(&mut self, text: Text) {
        self.logs.push_back(text);
        while self.logs.len() > 200 {
            self.logs.pop_front();
        }
    }
}

impl Drop for Model {
    fn drop(&mut self) {
        self.stop.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

// ===== impl Worker =====

impl Worker {
    fn tasks(&self) -> Result<Vec<Draft>> {
        let path = self.store.directory().join("tasks.json");
        if !path.try_exists()? {
            return Ok(Vec::new());
        }
        Ok(serde_json::from_slice(&paths::read_file(&path)?)?)
    }

    fn drafts(&self) -> Result<Vec<Draft>> {
        let mut drafts: Vec<_> = self
            .store
            .workspaces()?
            .iter()
            .map(Draft::workspace)
            .collect();
        drafts.extend(self.tasks()?);
        Ok(drafts)
    }

    fn find(&self, id: Uuid) -> Result<Draft> {
        self.drafts()?
            .into_iter()
            .find(|d| d.id == Some(id))
            .context("Task no longer exists")
    }

    fn refresh(&self) -> Result<()> {
        let rows = self
            .drafts()?
            .into_iter()
            .map(|draft| {
                let running = draft.id.is_some_and(|id| self.active.contains_key(&id));
                let status = draft
                    .id
                    .and_then(|id| self.states.get(&id))
                    .cloned()
                    .unwrap_or_else(|| if running { "Running" } else { "Stopped" }.into());
                Row {
                    draft,
                    running,
                    status,
                }
            })
            .collect();
        let _ = self.sender.send(Message::Rows(rows));
        Ok(())
    }

    fn reap(&mut self) {
        let finished: Vec<_> = self
            .active
            .iter()
            .filter(|(_, (_, h))| h.is_finished())
            .map(|(id, _)| *id)
            .collect();
        if finished.is_empty() {
            return;
        }
        for id in finished {
            if let Some((_, handle)) = self.active.remove(&id) {
                let _ = handle.join();
            }
            self.states.insert(id, "Stopped; see activity".into());
        }
        let _ = self.refresh();
    }

    fn command(&mut self, command: Command) -> Result<Text> {
        match command {
            Command::Refresh => self.refresh()?,
            Command::Save(mut draft) => {
                ensure!(!draft.name.trim().is_empty(), "Enter a task name");
                ensure!(!draft.path.trim().is_empty(), "Enter a local path");
                if let Some(id) = draft.id {
                    ensure!(
                        !self.active.contains_key(&id),
                        "Stop the task before editing it"
                    );
                }
                if draft.kind == Kind::Workspace {
                    let existing = self
                        .store
                        .workspaces()?
                        .into_iter()
                        .find(|w| Some(w.id()) == draft.id);
                    let builder = match existing {
                        Some(workspace) => workspace.edit(),
                        None => Workspace::builder(&draft.name, &draft.path),
                    };
                    let mut builder = builder
                        .name(&draft.name)
                        .root(&draft.path)
                        .includes(lines(&draft.includes))
                        .excludes(lines(&draft.excludes));
                    if !draft.branch.trim().is_empty() {
                        builder = builder.branch(draft.branch.trim());
                    }
                    let workspace = builder.paused(true).build()?;
                    if draft.id.is_some() {
                        self.store.update(workspace)?;
                    } else {
                        self.store.register(workspace)?;
                    }
                } else {
                    match draft.kind {
                        Kind::Watch => {
                            Repository::open(&draft.path, None, draft.watch_options())?;
                            draft.monitor()?;
                        }
                        Kind::Pull => {
                            PullTask::new(draft.pull_options()?)?;
                        }
                        Kind::Workspace => {}
                    }
                    let _lock = Lock::acquire(&self.store.directory().join("tasks.lock"))?;
                    let mut tasks = self.tasks()?;
                    let id = *draft.id.get_or_insert_with(Uuid::new_v4);
                    tasks.retain(|t| t.id != Some(id));
                    tasks.push(draft);
                    paths::atomic_write(
                        &self.store.directory().join("tasks.json"),
                        &serde_json::to_vec_pretty(&tasks)?,
                    )?;
                }
                self.refresh()?;
            }
            Command::Remove(id) => {
                ensure!(
                    !self.active.contains_key(&id),
                    "Stop the task before removing it"
                );
                if self.find(id)?.kind == Kind::Workspace {
                    self.store.remove(id)?;
                } else {
                    let _lock = Lock::acquire(&self.store.directory().join("tasks.lock"))?;
                    let mut tasks = self.tasks()?;
                    tasks.retain(|t| t.id != Some(id));
                    paths::atomic_write(
                        &self.store.directory().join("tasks.json"),
                        &serde_json::to_vec_pretty(&tasks)?,
                    )?;
                }
                self.refresh()?;
            }
            Command::Start(id) => {
                ensure!(!self.active.contains_key(&id), "Task is already running");
                let draft = self.find(id)?;
                if draft.kind == Kind::Workspace {
                    let workspace = self
                        .store
                        .workspaces()?
                        .into_iter()
                        .find(|w| w.id() == id)
                        .context("Unknown workspace")?;
                    self.store.update(workspace.edit().paused(false).build()?)?;
                }
                let store = self.store.clone();
                let sender = self.sender.clone();
                let stop = StopToken::default();
                let token = stop.clone();
                let handle = thread::spawn(move || {
                    let report = |event| {
                        let _ = sender.send(Message::Status(id, event_text(event)));
                    };
                    let result = (|| -> Result<()> {
                        match draft.kind {
                            Kind::Workspace => watch::watch_workspace(
                                store,
                                id,
                                draft.monitor()?.commit_on_start(true),
                                token,
                                report,
                            ),
                            Kind::Watch => watch::watch_repository(
                                Repository::open(&draft.path, None, draft.watch_options())?,
                                draft.monitor()?,
                                token,
                                report,
                            ),
                            Kind::Pull => {
                                PullTask::new(draft.pull_options()?)?.run(token, |result| {
                                    let text = result
                                        .map(|r| {
                                            Text::format(
                                                "{0} at {1}",
                                                [
                                                    if r.changed() {
                                                        "Updated"
                                                    } else {
                                                        "Unchanged"
                                                    }
                                                    .into(),
                                                    Text::value(r.after()),
                                                ],
                                            )
                                        })
                                        .unwrap_or_else(|e| Text::Error(e.to_string()));
                                    let _ = sender.send(Message::Status(id, text));
                                })
                            }
                        }
                    })();
                    if let Err(error) = result {
                        let _ = sender.send(Message::Status(id, Text::Error(error.to_string())));
                    }
                });
                self.active.insert(id, (stop, handle));
                self.states.remove(&id);
                self.refresh()?;
            }
            Command::Stop(id) => {
                if let Some((stop, handle)) = self.active.remove(&id) {
                    stop.stop();
                    let _ = handle.join();
                }
                if self.find(id)?.kind == Kind::Workspace {
                    let workspace = self
                        .store
                        .workspaces()?
                        .into_iter()
                        .find(|w| w.id() == id)
                        .context("Unknown workspace")?;
                    self.store.update(workspace.edit().paused(true).build()?)?;
                }
                self.states.insert(id, "Stopped".into());
                self.refresh()?;
            }
            Command::Once(id) => {
                ensure!(
                    !self.active.contains_key(&id),
                    "Stop the task before running it once"
                );
                let draft = self.find(id)?;
                let text = match draft.kind {
                    Kind::Workspace => event_text(Event::Backup(self.store.backup(id)?)),
                    Kind::Watch => event_text(Event::Repository(
                        Repository::open(&draft.path, None, draft.watch_options())?.commit()?,
                    )),
                    Kind::Pull => {
                        let report = PullTask::new(draft.pull_options()?)?.update()?;
                        Text::format("Updated to {0}", [Text::value(report.after())])
                    }
                };
                self.states.insert(id, text.clone());
                self.refresh()?;
                return Ok(text);
            }
            Command::Push(id) => {
                self.store.push(id)?;
                return Ok("Uploaded workspace branch".into());
            }
            Command::Remote(url, automatic) => {
                self.store
                    .set_remote((!url.is_empty()).then_some(url.as_str()), automatic)?;
            }
            Command::Fetch => {
                let _ = self.sender.send(Message::Branches(self.store.fetch()?));
            }
            Command::Import(branch, root) => {
                self.store.import(&branch, root)?;
                self.refresh()?;
            }
            Command::History(id) => {
                let _ = self
                    .sender
                    .send(Message::History(self.store.history(id, None, 100)?));
            }
            Command::Diff(id, from, to) => {
                let _ = self
                    .sender
                    .send(Message::Text(Text::value(self.store.diff(id, &from, &to)?)));
            }
            Command::Preview(id, revision, selected) => {
                ensure!(
                    !self.active.contains_key(&id),
                    "Stop the task before preparing a restore"
                );
                let _ = self.sender.send(Message::Preview(
                    self.store.preview_restore(id, &revision, &selected)?,
                ));
            }
            Command::Contents(plan, path) => {
                let (before, after) = self.store.restore_contents(plan, &path)?;
                let text = Text::format(
                    "--- Current ---\n{0}\n--- Selected ---\n{1}",
                    [
                        before
                            .as_deref()
                            .map(display_bytes)
                            .unwrap_or("(missing)".into()),
                        display_bytes(&after),
                    ],
                );
                let _ = self.sender.send(Message::Text(text));
            }
            Command::Restore(id) => {
                let plan = self.store.restore_plan(id)?;
                ensure!(
                    !self.active.contains_key(&plan.workspace()),
                    "Stop the task before restoring"
                );
                let report = self.store.apply_restore(id)?;
                let text = Text::format(
                    "Restored {0} files. Recovery: {1}",
                    [
                        Text::value(report.written().len()),
                        Text::value(report.recovery().display()),
                    ],
                );
                ensure!(
                    report.error().is_none(),
                    "{text}; partial restore: {}",
                    report.error().unwrap_or("")
                );
                return Ok(text);
            }
        }
        Ok("Completed".into())
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        for (stop, _) in self.active.values() {
            stop.stop();
        }
        for (_, (_, handle)) in self.active.drain() {
            let _ = handle.join();
        }
    }
}

pub(crate) fn lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

fn seconds(text: &str) -> Result<Duration> {
    let seconds: f64 = text
        .trim()
        .parse()
        .context("Interval must be a positive number of seconds")?;
    let duration = Duration::try_from_secs_f64(seconds)?;
    ensure!(!duration.is_zero(), "Interval must be positive");
    Ok(duration)
}

pub(crate) fn display_bytes(bytes: &[u8]) -> Text {
    match std::str::from_utf8(bytes) {
        Ok(text) if !text.contains('\0') => Text::value(text),
        _ => Text::format("(binary file, {0} bytes)", [Text::value(bytes.len())]),
    }
}

pub(crate) fn upload_text(upload: &crate::workspace::UploadState) -> Text {
    use crate::workspace::UploadState;
    match upload {
        UploadState::Disabled => "Disabled".into(),
        UploadState::Pending => "Pending".into(),
        UploadState::Synced => "Synced".into(),
        UploadState::Failed { message } => {
            Text::format("Failed: {0}", [Text::Error(message.clone())])
        }
    }
}

fn event_text(event: Event) -> Text {
    match event {
        Event::Watching(source) => Text::format(
            "Watching: {0}",
            [match source.as_str() {
                "content polling" => "content polling".into(),
                "native events with periodic content checks" => {
                    "native events with periodic content checks".into()
                }
                _ => Text::value(source),
            }],
        ),
        Event::Pending => "Changes pending".into(),
        Event::Repository(report) => {
            let status = Text::format(
                "Local: {0}; upload: {1}",
                [
                    report
                        .commit()
                        .map(Text::value)
                        .unwrap_or("unchanged".into()),
                    upload_text(report.upload()),
                ],
            );
            match report.skipped() {
                Some(reason) => {
                    Text::format("{0}; skipped: {1}", [status, Text::Error(reason.into())])
                }
                None => status,
            }
        }
        Event::Backup(report) => Text::format(
            "Local: {0}; {1} files; upload: {2}",
            [
                Text::value(report.commit()),
                Text::value(report.files()),
                upload_text(report.upload()),
            ],
        ),
        Event::Upload(upload) => Text::format("Upload: {0}", [upload_text(&upload)]),
        Event::Error(error) => Text::Error(error),
        Event::Stopped => "Stopped".into(),
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, time::Instant};

    use super::*;

    fn wait(model: &mut Model) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while model.busy && Instant::now() < deadline {
            model.poll();
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!model.busy, "Controller did not finish");
    }

    fn command(model: &mut Model, command: Command) {
        model.send(command);
        wait(model);
        assert!(model.error.is_none(), "{:?}", model.error);
    }

    #[test]
    fn shared_controller_persists_tasks_and_requires_a_current_restore_preview() {
        let temp = tempfile::TempDir::new().unwrap();
        let source = temp.path().join("project");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("notes.md"), "first").unwrap();
        let data = temp.path().join("data");
        let mut model = Model::new(Some(data.clone()));
        wait(&mut model);
        command(
            &mut model,
            Command::Save(Draft {
                name: "Notes".into(),
                path: source.to_string_lossy().into_owned(),
                includes: "notes.md".into(),
                ..Draft::default()
            }),
        );
        let id = model.rows[0].draft.id.unwrap();
        command(&mut model, Command::Once(id));
        command(&mut model, Command::History(id));
        let commit = model.history[0].commit().to_owned();
        fs::write(source.join("notes.md"), "second").unwrap();
        command(&mut model, Command::Preview(id, commit.clone(), vec![]));
        let plan = model.plan.as_ref().unwrap().id();
        command(&mut model, Command::Contents(plan, "notes.md".into()));
        assert!(
            model.text.render(Language::English).contains("first")
                && model.text.render(Language::English).contains("second")
        );
        fs::write(source.join("notes.md"), "third").unwrap();
        model.send(Command::Restore(plan));
        wait(&mut model);
        assert!(
            model
                .error
                .as_ref()
                .unwrap()
                .contains("changed after preview")
        );
        command(&mut model, Command::Preview(id, commit, vec![]));
        let plan = model.plan.as_ref().unwrap().id();
        command(&mut model, Command::Restore(plan));
        assert_eq!(
            fs::read_to_string(source.join("notes.md")).unwrap(),
            "first"
        );
        command(&mut model, Command::Start(id));
        model.send(Command::Preview(id, "HEAD".into(), vec![]));
        wait(&mut model);
        assert!(
            model.error.as_ref().unwrap().contains("Stop the task"),
            "error={:?}; activity={:?}",
            model.error,
            model.logs
        );
        command(&mut model, Command::Stop(id));
        command(
            &mut model,
            Command::Save(Draft {
                kind: Kind::Pull,
                name: "Upstream".into(),
                path: temp.path().join("clone").to_string_lossy().into_owned(),
                url: "https://example.invalid/repo.git".into(),
                ..Draft::default()
            }),
        );
        drop(model);
        let mut model = Model::new(Some(data));
        wait(&mut model);
        assert_eq!(model.rows.len(), 2);
        assert!(model.rows.iter().all(|r| !r.running));
        command(&mut model, Command::Remove(id));
        assert_eq!(model.rows.len(), 1);
    }
}
