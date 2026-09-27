//! Application workspaces retain their controllers while another list is visible.

use std::{
    collections::HashSet,
    fs,
    path::PathBuf,
    thread::{self, JoinHandle},
};

use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{Command, Model, Scope};
use crate::{Result, git::Lock, i18n::Language, paths, workspace::BackupStore};

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Space {
    pub id: Uuid,
    pub name: String,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Catalog {
    selected: Uuid,
    spaces: Vec<Space>,
}

/// The visible controller lives in the UI; all others keep running here.
pub(crate) struct Sessions {
    directory: PathBuf,
    catalog: Catalog,
    inactive: Vec<(Uuid, Model)>,
    pending: Option<JoinHandle<Result<Catalog>>>,
    removing: bool,
    global_pending: bool,
    pub error: Option<String>,
    // Hold the catalog lease until pending writes and controllers have stopped.
    _lock: Lock,
}

impl Space {
    pub fn label(&self, language: Language) -> &str {
        if self.id.is_nil() {
            language.text("Default workspace")
        } else {
            &self.name
        }
    }
}

// ===== impl Catalog =====

impl Catalog {
    fn load(directory: &std::path::Path) -> Result<Self> {
        let path = directory.join("spaces.json");
        let mut catalog: Self = match paths::read_file(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).context("Cannot read workspace catalog")?,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                Self::default()
            }
            Err(error) => return Err(error),
        };
        if catalog.spaces.is_empty() {
            catalog.spaces.push(Space {
                id: Uuid::nil(),
                name: String::new(),
            });
        }
        let mut ids = HashSet::new();
        let mut names = HashSet::new();
        for space in &catalog.spaces {
            ensure!(ids.insert(space.id), "Duplicate workspace ID");
            if !space.id.is_nil() {
                Self::validate_name(&space.name)?;
                ensure!(
                    names.insert(space.name.to_lowercase()),
                    "Duplicate workspace name"
                );
                let path = paths::joined(directory, &format!("spaces/{}", space.id))?;
                ensure!(
                    paths::no_link(&path)?.is_dir(),
                    "Workspace data directory is missing"
                );
                ensure!(
                    paths::no_link(&path.join("config.json"))?.is_file(),
                    "Workspace configuration is missing"
                );
            }
        }
        ensure!(
            ids.contains(&Uuid::nil()) && ids.contains(&catalog.selected),
            "Workspace catalog has an invalid selection or no default workspace"
        );
        Ok(catalog)
    }

    fn validate_name(name: &str) -> Result<()> {
        ensure!(
            !name.trim().is_empty() && name.len() <= 128 && !name.chars().any(char::is_control),
            "Enter a workspace name (1–128 bytes, no control characters)"
        );
        Ok(())
    }

    fn name(&self, id: Option<Uuid>, name: &str) -> Result<()> {
        Self::validate_name(name)?;
        ensure!(
            !self.spaces.iter().any(|space| !space.id.is_nil()
                && Some(space.id) != id
                && space.name.to_lowercase() == name.to_lowercase()),
            "A workspace with this name already exists"
        );
        Ok(())
    }

    fn save(&self, directory: &std::path::Path) -> Result<()> {
        paths::atomic_write(
            &directory.join("spaces.json"),
            &serde_json::to_vec_pretty(self)?,
        )
    }
}

// ===== impl Sessions =====

impl Sessions {
    pub fn new(data: Option<PathBuf>, language: Language) -> Result<(Self, Model)> {
        let directory = data.map_or_else(BackupStore::default_directory, Ok)?;
        // Initialize the legacy store before adding any catalog files to its directory.
        let store = BackupStore::open(directory)?;
        let directory = store.directory().to_path_buf();
        let lock = Lock::acquire(&directory.join("spaces-interface.lock"))
            .context("Another task interface is using this data directory")?;
        let catalog = Catalog::load(&directory)?;
        let mut inactive = Vec::new();
        let mut selected = None;
        for space in &catalog.spaces {
            let path = if space.id.is_nil() {
                directory.clone()
            } else {
                directory.join("spaces").join(space.id.to_string())
            };
            let model = Model::open(
                Some(path),
                language,
                if space.id.is_nil() {
                    Scope::Local
                } else {
                    Scope::Remote
                },
            );
            if space.id == catalog.selected {
                selected = Some(model);
            } else {
                inactive.push((space.id, model));
            }
        }
        let model = selected.context("Selected workspace is unavailable")?;
        Ok((
            Self {
                directory,
                catalog,
                inactive,
                pending: None,
                removing: false,
                global_pending: false,
                error: None,
                _lock: lock,
            },
            model,
        ))
    }

    pub fn selected(&self) -> Uuid {
        self.catalog.selected
    }

    pub fn list(&self) -> &[Space] {
        &self.catalog.spaces
    }

    pub fn label(&self, language: Language) -> &str {
        self.catalog
            .spaces
            .iter()
            .find(|space| space.id == self.selected())
            .map_or(language.text("Default workspace"), |space| {
                space.label(language)
            })
    }

    pub fn busy(&self) -> bool {
        self.pending.is_some() || self.global_pending
    }

    pub fn switch(&mut self, model: &mut Model, id: Uuid) {
        let result = self.select(model, id);
        self.error = result.err().map(|error| format!("{error:#}"));
    }

    fn select(&mut self, model: &mut Model, id: Uuid) -> Result<()> {
        ensure!(!self.busy(), "Working in background…");
        if id == self.selected() {
            return Ok(());
        }
        let index = self
            .inactive
            .iter()
            .position(|(key, _)| *key == id)
            .context("Workspace is unavailable")?;
        let mut catalog = self.catalog.clone();
        catalog.selected = id;
        catalog.save(&self.directory)?;
        std::mem::swap(model, &mut self.inactive[index].1);
        self.inactive[index].0 = self.catalog.selected;
        self.catalog = catalog;
        Ok(())
    }

    pub fn create(&mut self, name: String, remote: String) {
        let result = (|| -> Result<()> {
            ensure!(!self.busy(), "Working in background…");
            let name = name.trim().to_owned();
            let remote = remote.trim().to_owned();
            self.catalog.name(None, &name)?;
            ensure!(!remote.is_empty(), "A remote repository is required");
            let directory = self.directory.clone();
            let mut catalog = self.catalog.clone();
            self.pending = Some(thread::spawn(move || {
                let parent = paths::joined(&directory, "spaces")?;
                fs::create_dir_all(&parent)?;
                let temporary = tempfile::tempdir_in(&parent)?;
                let store = BackupStore::open(temporary.path())?;
                store.set_remote(Some(&remote), false)?;
                drop(store);
                let id = Uuid::new_v4();
                let destination = parent.join(id.to_string());
                fs::rename(temporary.path(), &destination)?;
                catalog.spaces.push(Space { id, name });
                if let Err(error) = catalog.save(&directory) {
                    fs::rename(&destination, temporary.path())
                        .context("Cannot roll back workspace creation; data was retained")?;
                    return Err(error);
                }
                Ok(catalog)
            }));
            Ok(())
        })();
        self.error = result.err().map(|error| format!("{error:#}"));
    }

    pub fn rename(&mut self, name: &str) {
        let result = (|| -> Result<()> {
            ensure!(!self.busy(), "Working in background…");
            ensure!(
                !self.selected().is_nil(),
                "The default workspace cannot be renamed or removed"
            );
            let name = name.trim();
            self.catalog.name(Some(self.selected()), name)?;
            let mut catalog = self.catalog.clone();
            let selected = catalog.selected;
            catalog
                .spaces
                .iter_mut()
                .find(|space| space.id == selected)
                .context("Workspace is unavailable")?
                .name = name.into();
            catalog.save(&self.directory)?;
            self.catalog = catalog;
            Ok(())
        })();
        self.error = result.err().map(|error| format!("{error:#}"));
    }

    /// Removes only an empty workspace from the catalog; its data stays on disk.
    pub fn remove(&mut self, model: &mut Model) {
        let result = (|| -> Result<()> {
            ensure!(!self.busy(), "Working in background…");
            let id = self.selected();
            ensure!(
                !id.is_nil(),
                "The default workspace cannot be renamed or removed"
            );
            ensure!(
                !model.busy && model.error.is_none() && model.rows.is_empty(),
                "Only an empty, idle workspace can be removed"
            );
            let mut catalog = self.catalog.clone();
            catalog.spaces.retain(|space| space.id != id);
            catalog.selected = Uuid::nil();
            let directory = self.directory.clone();
            self.pending = Some(thread::spawn(move || {
                let path = paths::joined(&directory, &format!("spaces/{id}"))?;
                let store = BackupStore::open(&path)?;
                let _tasks = Lock::acquire(&path.join("tasks.lock"))?;
                let _store = store.lock()?;
                let tasks: Vec<super::Draft> = match paths::read_file(&path.join("tasks.json")) {
                    Ok(bytes) => serde_json::from_slice(&bytes)?,
                    Err(error)
                        if error
                            .downcast_ref::<std::io::Error>()
                            .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
                    {
                        Vec::new()
                    }
                    Err(error) => return Err(error),
                };
                ensure!(
                    tasks.is_empty() && store.config()?.workspaces.is_empty(),
                    "Only an empty, idle workspace can be removed"
                );
                catalog.save(&directory)?;
                Ok(catalog)
            }));
            self.removing = true;
            model.busy = true;
            Ok(())
        })();
        self.error = result.err().map(|error| format!("{error:#}"));
    }

    pub fn poll(&mut self, model: &mut Model) -> bool {
        let before = self.selected();
        model.poll();
        for (_, model) in &mut self.inactive {
            model.poll();
        }
        if self.pending.as_ref().is_some_and(|job| job.is_finished())
            && let Some(job) = self.pending.take()
        {
            if std::mem::take(&mut self.removing) {
                model.busy = false;
            }
            match job
                .join()
                .unwrap_or_else(|_| Err(anyhow::anyhow!("Workspace creation failed")))
            {
                Ok(catalog) => {
                    for space in &catalog.spaces {
                        if !self
                            .catalog
                            .spaces
                            .iter()
                            .any(|existing| existing.id == space.id)
                        {
                            let path = self.directory.join("spaces").join(space.id.to_string());
                            self.inactive.push((
                                space.id,
                                Model::open(Some(path), model.language, Scope::Remote),
                            ));
                        }
                    }
                    if catalog.selected != self.selected()
                        && let Some((id, next)) = self
                            .inactive
                            .iter_mut()
                            .find(|(id, _)| *id == catalog.selected)
                    {
                        std::mem::swap(model, next);
                        *id = self.catalog.selected;
                    }
                    self.inactive
                        .retain(|(id, _)| catalog.spaces.iter().any(|space| space.id == *id));
                    self.catalog = catalog;
                }
                Err(error) => self.error = Some(format!("{error:#}")),
            }
        }
        let root = if self.selected().is_nil() {
            &*model
        } else {
            let Some((_, root)) = self.inactive.iter().find(|(id, _)| id.is_nil()) else {
                return self.selected() != before;
            };
            root
        };
        let language = root.language;
        if self.global_pending && !root.busy {
            self.error.clone_from(&root.error);
            self.global_pending = false;
        }
        let update = root.update.clone();
        #[cfg(feature = "desktop")]
        let start_in_tray = root.start_in_tray;
        for target in std::iter::once(model).chain(self.inactive.iter_mut().map(|(_, model)| model))
        {
            target.language = language;
            target.update.clone_from(&update);
            #[cfg(feature = "desktop")]
            {
                target.start_in_tray = start_in_tray;
            }
        }
        self.selected() != before
    }

    pub fn global(&mut self, model: &mut Model, command: Command) {
        let root = if self.selected().is_nil() {
            model
        } else {
            let Some((_, root)) = self.inactive.iter_mut().find(|(id, _)| id.is_nil()) else {
                return;
            };
            root
        };
        if root.busy {
            self.error = Some("Working in background…".into());
        } else {
            root.send(command);
            self.global_pending = true;
        }
    }

    pub fn background_error(&self, language: Language) -> Option<String> {
        self.inactive.iter().find_map(|(id, model)| {
            let error = model.error.as_ref()?;
            let space = self.catalog.spaces.iter().find(|space| space.id == *id)?;
            Some(format!(
                "{}: {}",
                space.label(language),
                language.error(error)
            ))
        })
    }
}

impl Drop for Sessions {
    fn drop(&mut self) {
        for (_, model) in &self.inactive {
            model.stop.stop();
        }
        if let Some(job) = self.pending.take() {
            let _ = job.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::{
        interface::{Draft, Kind},
        preferences::Preferences,
        test_git::git,
    };

    fn until(
        sessions: &mut Sessions,
        model: &mut Model,
        ready: impl Fn(&Sessions, &Model) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            sessions.poll(model);
            if ready(sessions, model) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "Timed out: {:?}, {:?}",
                sessions.error,
                model.error
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait(sessions: &mut Sessions, model: &mut Model) {
        until(sessions, model, |sessions, model| {
            !sessions.busy()
                && !model.busy
                && sessions.inactive.iter().all(|(_, model)| !model.busy)
        });
    }

    fn command(sessions: &mut Sessions, model: &mut Model, command: Command) {
        model.send(command);
        wait(sessions, model);
        assert!(model.error.is_none(), "{:?}", model.error);
    }

    #[test]
    fn legacy_tasks_and_background_controllers_survive_switches_and_restart() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        git(&source, &["init", "-b", "main"]);
        let commit = |text: &str| {
            fs::write(source.join("note"), text).unwrap();
            git(&source, &["add", "."]);
            git(
                &source,
                &[
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "-m",
                    text,
                ],
            );
        };
        commit("first");
        let checkout = temp.path().join("checkout");
        let store = BackupStore::open(&data).unwrap();
        store
            .set_remote(Some("https://example.invalid/legacy.git"), false)
            .unwrap();
        let legacy = Draft {
            id: Some(Uuid::new_v4()),
            name: "Old task".into(),
            kind: Kind::Pull,
            path: checkout.to_string_lossy().into_owned(),
            url: source.to_string_lossy().into_owned(),
            interval: "600".into(),
            ..Draft::default()
        };
        let task_id = legacy.id.unwrap();
        let bytes = serde_json::to_vec(&vec![legacy]).unwrap();
        fs::write(data.join("tasks.json"), &bytes).unwrap();
        Preferences::update(&data, |preferences| {
            preferences.started_tasks.insert(task_id);
        })
        .unwrap();
        let config = fs::read(data.join("config.json")).unwrap();

        let (mut sessions, mut model) =
            Sessions::new(Some(data.clone()), Language::English).unwrap();
        wait(&mut sessions, &mut model);
        until(&mut sessions, &mut model, |_, model| {
            model
                .pull_schedule
                .get(&task_id)
                .is_some_and(Option::is_some)
        });
        assert_eq!(model.rows[0].draft.id, Some(task_id));
        assert!(model.rows[0].running);
        assert_eq!(fs::read(data.join("tasks.json")).unwrap(), bytes);
        assert_eq!(fs::read(data.join("config.json")).unwrap(), config);
        assert!(!data.join("spaces.json").exists());
        assert!(Sessions::new(Some(data.clone()), Language::English).is_err());
        model.send(Command::Remote(
            "https://example.invalid/new.git".into(),
            false,
        ));
        wait(&mut sessions, &mut model);
        assert!(
            model
                .error
                .as_deref()
                .unwrap()
                .contains("default workspace")
        );
        assert_eq!(
            store.remote().unwrap().0.as_deref(),
            Some("https://example.invalid/legacy.git")
        );
        command(&mut sessions, &mut model, Command::Refresh);
        sessions.remove(&mut model);
        assert!(sessions.error.is_some());
        sessions.create("Work".into(), String::new());
        assert!(sessions.error.is_some() && !sessions.busy());
        sessions.create("Work".into(), "https://example.invalid/work.git".into());
        wait(&mut sessions, &mut model);
        assert!(sessions.error.is_none(), "{:?}", sessions.error);
        let id = sessions.list()[1].id;
        let path = data.join("spaces").join(id.to_string());
        let deadline = Preferences::load(&data).unwrap().pull_deadlines[&task_id];
        sessions.switch(&mut model, id);
        assert!(model.rows.is_empty());
        assert!(model.notifications.is_none());
        assert_eq!(
            model.remote.as_deref(),
            Some("https://example.invalid/work.git")
        );
        model.send(Command::Remote(String::new(), false));
        wait(&mut sessions, &mut model);
        assert!(
            model
                .error
                .as_deref()
                .unwrap()
                .contains("remote repository is required")
        );
        command(&mut sessions, &mut model, Command::Refresh);
        assert_eq!(
            Preferences::load(&data).unwrap().pull_deadlines[&task_id],
            deadline
        );
        commit("second");
        let root = &mut sessions
            .inactive
            .iter_mut()
            .find(|(id, _)| id.is_nil())
            .unwrap()
            .1;
        assert!(root.rows[0].running);
        root.send(Command::Once(task_id));
        until(&mut sessions, &mut model, |_, _| {
            fs::read_to_string(checkout.join("note")).is_ok_and(|text| text == "second")
        });
        assert!(
            model.rows.is_empty(),
            "Background updates must not replace the visible task list"
        );

        let notes = temp.path().join("notes");
        fs::create_dir(&notes).unwrap();
        fs::write(notes.join("note"), "private").unwrap();
        command(
            &mut sessions,
            &mut model,
            Command::Save(Draft {
                name: "Notes".into(),
                path: notes.to_string_lossy().into_owned(),
                includes: "note".into(),
                delay: "1".into(),
                ..Draft::default()
            }),
        );
        let backup_id = model.rows[0].draft.id.unwrap();
        command(&mut sessions, &mut model, Command::Once(backup_id));
        command(&mut sessions, &mut model, Command::History(backup_id));
        assert_eq!(model.history.len(), 1);
        command(&mut sessions, &mut model, Command::Start(backup_id));
        sessions.remove(&mut model);
        assert!(sessions.error.is_some());
        sessions.rename("Personal");
        assert!(sessions.error.is_none());
        sessions.global(&mut model, Command::Language(Language::Chinese));
        wait(&mut sessions, &mut model);
        assert_eq!(model.language, Language::Chinese);
        assert_eq!(
            Preferences::load(&data).unwrap().language,
            Some(Language::Chinese)
        );
        assert_eq!(Preferences::load(&path).unwrap().language, None);
        sessions.switch(&mut model, Uuid::nil());
        assert_eq!(model.rows.len(), 1);
        assert_eq!(model.rows[0].draft.id, Some(task_id));
        assert!(model.history.is_empty());
        fs::write(notes.join("note"), "background edit").unwrap();
        let custom_store = BackupStore::open(&path).unwrap();
        until(&mut sessions, &mut model, |_, _| {
            custom_store
                .history(backup_id, None, 10)
                .is_ok_and(|history| history.len() >= 2)
        });
        sessions.switch(&mut model, id);
        let persisted_deadline = Preferences::load(&data).unwrap().pull_deadlines[&task_id];
        drop(model);
        drop(sessions);

        let (mut sessions, mut model) =
            Sessions::new(Some(data.clone()), Language::Chinese).unwrap();
        wait(&mut sessions, &mut model);
        assert_eq!(sessions.selected(), id);
        assert_eq!(sessions.label(Language::English), "Personal");
        assert_eq!(model.rows[0].draft.id, Some(backup_id));
        assert!(model.rows[0].running);
        let root = &sessions
            .inactive
            .iter()
            .find(|(id, _)| id.is_nil())
            .unwrap()
            .1;
        assert_eq!(root.rows[0].draft.id, Some(task_id));
        assert!(root.rows[0].running);
        assert_eq!(
            Preferences::load(&data).unwrap().pull_deadlines[&task_id],
            persisted_deadline
        );
        command(&mut sessions, &mut model, Command::Stop(backup_id));
        command(&mut sessions, &mut model, Command::Remove(backup_id));
        sessions.remove(&mut model);
        wait(&mut sessions, &mut model);
        assert!(sessions.error.is_none());
        assert!(sessions.selected().is_nil());
        assert!(path.join("backup.git").is_dir());
        assert_eq!(Catalog::load(&data).unwrap().spaces.len(), 1);
        command(&mut sessions, &mut model, Command::Stop(task_id));
    }

    #[test]
    fn failed_catalog_writes_and_invalid_data_do_not_replace_existing_workspaces() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let (mut sessions, mut model) =
            Sessions::new(Some(data.clone()), Language::English).unwrap();
        wait(&mut sessions, &mut model);
        // A directory at the catalog path makes atomic persistence fail on all platforms.
        fs::create_dir(data.join("spaces.json")).unwrap();
        sessions.create("Work".into(), "https://example.invalid/work.git".into());
        wait(&mut sessions, &mut model);
        assert!(sessions.error.is_some());
        assert_eq!(sessions.list().len(), 1);
        assert_eq!(fs::read_dir(data.join("spaces")).unwrap().count(), 0);
        fs::remove_dir(data.join("spaces.json")).unwrap();
        sessions.create("Work".into(), "-invalid".into());
        wait(&mut sessions, &mut model);
        assert!(sessions.error.is_some());
        assert_eq!(fs::read_dir(data.join("spaces")).unwrap().count(), 0);
        sessions.create("Work".into(), "https://example.invalid/work.git".into());
        wait(&mut sessions, &mut model);
        assert!(sessions.error.is_none());
        let id = sessions.list()[1].id;
        sessions.switch(&mut model, id);
        wait(&mut sessions, &mut model);
        let source = temp.path().join("external-source");
        fs::create_dir(&source).unwrap();
        let binding = crate::workspace::Workspace::builder("Added through CLI", &source)
            .include("note")
            .build()
            .unwrap();
        let binding_id = binding.id();
        let store = BackupStore::open(data.join("spaces").join(id.to_string())).unwrap();
        store.register(binding).unwrap();
        assert!(
            model.rows.is_empty(),
            "The interface has not refreshed the external binding yet"
        );
        sessions.remove(&mut model);
        wait(&mut sessions, &mut model);
        assert!(
            sessions
                .error
                .as_deref()
                .is_some_and(|error| error.contains("Only an empty"))
        );
        assert_eq!(sessions.selected(), id);
        store.remove(binding_id).unwrap();
        sessions.switch(&mut model, Uuid::nil());
        sessions.create("work".into(), "https://example.invalid/work.git".into());
        assert!(sessions.error.is_some() && !sessions.busy());
        let saved = fs::read(data.join("spaces.json")).unwrap();
        fs::remove_file(data.join("spaces.json")).unwrap();
        fs::create_dir(data.join("spaces.json")).unwrap();
        sessions.switch(&mut model, id);
        assert!(sessions.error.is_some() && sessions.selected().is_nil());
        fs::remove_dir(data.join("spaces.json")).unwrap();
        fs::write(data.join("spaces.json"), &saved).unwrap();
        drop(model);
        drop(sessions);
        fs::write(data.join("spaces.json"), b"broken").unwrap();
        assert!(Sessions::new(Some(data.clone()), Language::English).is_err());
        fs::write(data.join("spaces.json"), saved).unwrap();
        fs::rename(
            data.join("spaces").join(id.to_string()),
            data.join("retained"),
        )
        .unwrap();
        assert!(Sessions::new(Some(data), Language::English).is_err());
    }
}
