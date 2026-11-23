//! The desktop's Recent and Favorites commands and their per-user settings.
//! A saved file identifies a destination only. Sign-in belongs to the open flow.

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
    ClearRecent,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct History {
    tab: Tab,
    recent: Vec<SavedFile>,
    favorites: Vec<SavedFile>,
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

    pub fn change(&mut self, path: Option<&Path>, change: Change) -> Result<(), DataError> {
        if let Some(path) = path {
            *self = crate::settings::update(path, |map| {
                let mut history = Self::from_settings(map)?;
                history.apply(change);
                let value = serde_json::to_value(&history).map_err(|err| {
                    DataError::new(format!("File history could not be saved: {err}"))
                })?;
                map.insert(KEY.into(), value);
                Ok(history)
            })?;
        } else {
            self.apply(change);
        }
        Ok(())
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
            Change::ClearRecent => self.recent.clear(),
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
        assert_eq!(second.files(Tab::Recent), &[local, hosted.clone()]);
        second.change(Some(&path), Change::ClearRecent).unwrap();
        first
            .change(Some(&path), Change::Select(Tab::Favorites))
            .unwrap();
        let restored = History::load(&path).unwrap();
        assert_eq!(restored.tab(), Tab::Favorites);
        assert!(restored.files(Tab::Recent).is_empty());
        assert_eq!(restored.files(Tab::Favorites), &[hosted]);
    }
}
