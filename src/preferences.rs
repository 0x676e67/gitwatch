use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    time::SystemTime,
};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::{Result, git::Lock, i18n::Language, paths};

/// Local interface preferences, shared so saving one setting preserves the others.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Preferences {
    pub language: Option<Language>,
    pub start_in_tray: bool,
    pub started_tasks: BTreeSet<uuid::Uuid>,
    pub pull_deadlines: BTreeMap<uuid::Uuid, SystemTime>,
}

impl Preferences {
    pub fn load(directory: &Path) -> Result<Self> {
        let path = directory.join("preferences.json");
        if !path.try_exists()? {
            return Ok(Self::default());
        }
        serde_json::from_slice(&paths::read_file(&path)?).context("Cannot read local preferences")
    }

    pub fn update(directory: &Path, change: impl FnOnce(&mut Self)) -> Result<()> {
        fs::create_dir_all(directory)?;
        let _lock = Lock::acquire(&directory.join("store.lock"))?;
        let mut preferences = Self::load(directory)?;
        change(&mut preferences);
        paths::atomic_write(
            &directory.join("preferences.json"),
            &serde_json::to_vec_pretty(&preferences)?,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setting_updates_preserve_each_other_and_old_preferences() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("preferences.json"), r#"{"language":"en"}"#).unwrap();
        assert!(!Preferences::load(temp.path()).unwrap().start_in_tray);
        assert!(
            Preferences::load(temp.path())
                .unwrap()
                .started_tasks
                .is_empty()
        );
        let id = uuid::Uuid::new_v4();
        let deadline = SystemTime::now();
        Preferences::update(temp.path(), |value| {
            value.started_tasks.insert(id);
            value.pull_deadlines.insert(id, deadline);
        })
        .unwrap();
        Preferences::update(temp.path(), |value| value.start_in_tray = true).unwrap();
        Language::Chinese.save(temp.path()).unwrap();
        let preferences = Preferences::load(temp.path()).unwrap();
        assert!(preferences.start_in_tray);
        assert_eq!(preferences.language, Some(Language::Chinese));
        assert!(preferences.started_tasks.contains(&id));
        assert_eq!(preferences.pull_deadlines.get(&id), Some(&deadline));
        Preferences::update(temp.path(), |value| value.start_in_tray = false).unwrap();
        assert_eq!(
            Preferences::load(temp.path()).unwrap().language,
            Some(Language::Chinese)
        );
        assert!(!temp.path().join("backup.git").exists());
    }
}
