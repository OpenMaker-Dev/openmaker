//! The desktop's Recent and Favorites lists (Home and File > Recent,
//! File > Favorites) and their per-user settings. A saved file identifies a
//! destination only: account names and passwords belong to the open flow
//! and are never stored here.

use std::path::{Path, PathBuf};

use om_data::DataError;
use serde::{Deserialize, Serialize};

const KEY: &str = "file_history";
const RECENT_LIMIT: usize = 20;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum SavedFile {
    Local {
        path: PathBuf,
    },
    Hosted {
        host: String,
        port: u16,
        file: String,
    },
}

impl SavedFile {
    pub fn local(path: PathBuf) -> Self {
        Self::Local {
            path: std::path::absolute(&path).unwrap_or(path),
        }
    }

    pub fn name(&self) -> String {
        match self {
            Self::Local { path } => path
                .file_stem()
                .unwrap_or(path.as_os_str())
                .to_string_lossy()
                .into_owned(),
            Self::Hosted { file, .. } => file.strip_suffix(".fmp12").unwrap_or(file).to_owned(),
        }
    }

    pub fn location(&self) -> String {
        match self {
            Self::Local { path } => path.display().to_string(),
            Self::Hosted { host, port, .. } if *port == crate::connect::DEFAULT_PORT => {
                host.clone()
            }
            Self::Hosted { host, port, .. } => format!("{host}:{port}"),
        }
    }
}

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Tab {
    #[default]
    Recent,
    Favorites,
}

impl Tab {
    pub fn label(self) -> &'static str {
        match self {
            Self::Recent => "Recent",
            Self::Favorites => "Favorites",
        }
    }
}

/// Changes shared by Home, the File menu and successful opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    Select(Tab),
    Opened(SavedFile),
    SetFavorite(SavedFile, bool),
    RemoveRecent(SavedFile),
}

impl Change {
    /// Whether the user asked for this change directly (a star, a Remove),
    /// rather than it following from an open or a tab switch.
    pub fn requested(&self) -> bool {
        matches!(self, Self::SetFavorite(..) | Self::RemoveRecent(_))
    }
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct History {
    tab: Tab,
    recent: Vec<SavedFile>,
    favorites: Vec<SavedFile>,
    /// Changes that could not be saved, in order: the next save replays
    /// them onto the file's history.
    #[serde(skip)]
    unsaved: Vec<Change>,
    #[serde(flatten)]
    other: serde_json::Map<String, serde_json::Value>,
}

impl History {
    pub fn tab(&self) -> Tab {
        self.tab
    }

    pub fn files(&self, tab: Tab) -> &[SavedFile] {
        match tab {
            Tab::Recent => &self.recent,
            Tab::Favorites => &self.favorites,
        }
    }

    pub fn load(path: &Path) -> Result<Self, DataError> {
        Self::from_settings(&crate::settings::read(path)?)
    }

    fn from_settings(map: &serde_json::Map<String, serde_json::Value>) -> Result<Self, DataError> {
        match map.get(KEY) {
            None => Ok(Self::default()),
            Some(value) => serde_json::from_value(value.clone())
                .map_err(|err| DataError::new(format!("File history could not be read: {err}"))),
        }
    }

    /// Apply `change` and save it in the settings file at `path`, merged into
    /// what another instance may have saved there since. When it cannot be
    /// saved, the file is left as it was and the change still applies to
    /// this list; it is saved with the next change that can be, or shows
    /// until the app quits.
    pub fn change(&mut self, path: Option<&Path>, change: Change) -> Result<(), DataError> {
        let Some(path) = path else {
            self.apply(change);
            return Ok(());
        };
        let mut changes = std::mem::take(&mut self.unsaved);
        changes.push(change.clone());
        let saved = crate::settings::update(path, |map| {
            let mut history = Self::from_settings(map)?;
            for change in &changes {
                history.apply(change.clone());
            }
            let value = serde_json::to_value(&history)
                .map_err(|err| DataError::new(format!("File history could not be saved: {err}")))?;
            map.insert(KEY.into(), value);
            Ok(history)
        });
        match saved {
            Ok(history) => {
                *self = history;
                Ok(())
            }
            Err(error) => {
                self.apply(change);
                self.unsaved = changes;
                Err(error)
            }
        }
    }

    fn apply(&mut self, change: Change) {
        match change {
            Change::Select(tab) => self.tab = tab,
            Change::Opened(file) => {
                self.recent.retain(|entry| *entry != file);
                self.recent.insert(0, file);
                self.recent.truncate(RECENT_LIMIT);
            }
            Change::SetFavorite(file, true) if !self.favorites.contains(&file) => {
                self.favorites.push(file);
            }
            Change::SetFavorite(_, true) => {}
            Change::SetFavorite(file, false) => self.favorites.retain(|entry| *entry != file),
            Change::RemoveRecent(file) => self.recent.retain(|entry| *entry != file),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reopening_and_pinning_merge_into_the_latest_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let local = SavedFile::local(dir.path().join("Tasks.fmp12"));
        let hosted = SavedFile::Hosted {
            host: "example.test".into(),
            port: 5004,
            file: "Tasks.fmp12".into(),
        };
        let mut first = History::load(&path).unwrap();
        let mut second = History::load(&path).unwrap();
        for file in [local.clone(), hosted.clone(), local.clone()] {
            first.change(Some(&path), Change::Opened(file)).unwrap();
        }
        second
            .change(Some(&path), Change::SetFavorite(hosted.clone(), true))
            .unwrap();
        assert_eq!(second.files(Tab::Recent), &[local.clone(), hosted.clone()]);
        second
            .change(Some(&path), Change::RemoveRecent(local))
            .unwrap();
        first
            .change(Some(&path), Change::Select(Tab::Favorites))
            .unwrap();
        let restored = History::load(&path).unwrap();
        assert_eq!(restored.tab(), Tab::Favorites);
        assert_eq!(restored.files(Tab::Recent), std::slice::from_ref(&hosted));
        assert_eq!(restored.files(Tab::Favorites), &[hosted]);
    }

    #[test]
    fn an_unreadable_settings_file_is_kept_and_its_changes_saved_later() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "not a settings file").unwrap();
        let file = SavedFile::local(dir.path().join("Tasks.fmp12"));
        let mut history = History::default();
        assert!(history
            .change(Some(&path), Change::Opened(file.clone()))
            .is_err());
        assert_eq!(history.files(Tab::Recent), std::slice::from_ref(&file));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "not a settings file"
        );
        // Once the file can be saved, the next change carries the earlier one.
        std::fs::write(&path, "{}").unwrap();
        history
            .change(Some(&path), Change::Select(Tab::Favorites))
            .unwrap();
        let restored = History::load(&path).unwrap();
        assert_eq!(restored.tab(), Tab::Favorites);
        assert_eq!(restored.files(Tab::Recent), &[file]);
    }
}
