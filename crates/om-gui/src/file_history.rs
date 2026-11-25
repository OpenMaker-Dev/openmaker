//! The desktop's Recent and Favorites lists (Home and File > Recent,
//! File > Favorites), Home's saved hosts, and their per-user settings. A
//! saved file or host identifies a destination only: account names and
//! passwords belong to the open flow and are never stored here.

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
            Self::Hosted { host, port, .. } => location(host, *port),
        }
    }

    /// Whether the settings file can keep it: JSON keeps text, so a local
    /// path that is not valid Unicode cannot be.
    fn storable(&self) -> bool {
        match self {
            Self::Local { path } => path.to_str().is_some(),
            Self::Hosted { .. } => true,
        }
    }
}

/// A host saved under Home's Hosts: where it is, and what the sidebar calls
/// it. One address and port is one host; the address is compared in any
/// case, as host names are.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SavedHost {
    pub address: String,
    pub port: u16,
    /// Shown in the sidebar instead of the address; empty for none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
}

impl SavedHost {
    /// The host and port, as a listing is asked of them.
    pub fn server(&self) -> (String, u16) {
        (self.address.clone(), self.port)
    }

    /// Whether this is the host at `address` and `port`.
    pub fn is(&self, address: &str, port: u16) -> bool {
        self.port == port && self.address.eq_ignore_ascii_case(address)
    }

    /// Its address, with the port when it is not FileMaker's.
    pub fn location(&self) -> String {
        location(&self.address, self.port)
    }

    /// What the sidebar and the host's page call it.
    pub fn label(&self) -> String {
        match self.name.trim() {
            "" => self.location(),
            name => name.to_owned(),
        }
    }
}

/// `address`, with `:port` when the port is not FileMaker's.
pub(crate) fn location(address: &str, port: u16) -> String {
    if port == crate::connect::DEFAULT_PORT {
        address.to_owned()
    } else if address.contains(':') {
        // An IPv6 address, bracketed so its colons are not the port's.
        format!("[{address}]:{port}")
    } else {
        format!("{address}:{port}")
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
    /// Save `host` under Hosts, in place of the host at `replacing` when it
    /// edits one, and in place of any other with its address and port.
    SaveHost {
        replacing: Option<(String, u16)>,
        host: SavedHost,
    },
    /// Save `host` under Hosts unless one at its address and port is
    /// saved: Open Network File's Add to Hosts, which leaves a name
    /// another instance gave it.
    AddHost(SavedHost),
    /// Forget the saved host at this address and port.
    RemoveHost(String, u16),
}

impl Change {
    /// Whether the user asked for this change directly (a star, a Remove),
    /// rather than it following from an open or a tab switch.
    pub fn requested(&self) -> bool {
        matches!(
            self,
            Self::SetFavorite(..)
                | Self::RemoveRecent(_)
                | Self::SaveHost { .. }
                | Self::AddHost(_)
                | Self::RemoveHost(..)
        )
    }
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct History {
    tab: Tab,
    recent: Vec<SavedFile>,
    favorites: Vec<SavedFile>,
    hosts: Vec<SavedHost>,
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

    /// The saved hosts, in the order they were added.
    pub fn hosts(&self) -> &[SavedHost] {
        &self.hosts
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
        // Refused, not queued: it could never be saved, and would hold back
        // every change after it.
        if let Change::Opened(file) | Change::SetFavorite(file, true) = &change {
            if !file.storable() {
                return Err(DataError::new(format!(
                    "{} can't be kept in Recent or Favorites: its location is not valid Unicode.",
                    file.name()
                )));
            }
        }
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
            Ok(mut history) => {
                // The list shown is this instance's: another's tab choice
                // saved meanwhile is only where the next start opens.
                history.tab = match change {
                    Change::Select(tab) => tab,
                    _ => self.tab,
                };
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
            Change::SaveHost { replacing, host } => {
                let replaced = replacing.and_then(|(address, port)| {
                    self.hosts.iter().position(|saved| saved.is(&address, port))
                });
                let at = replaced.or_else(|| {
                    self.hosts
                        .iter()
                        .position(|saved| saved.is(&host.address, host.port))
                });
                let Some(at) = at else {
                    self.hosts.push(host);
                    return;
                };
                // In the edited host's place; any other entry for the same
                // address and port goes.
                let (address, port) = host.server();
                self.hosts[at] = host;
                let mut index = 0;
                self.hosts.retain(|saved| {
                    let keep = index == at || !saved.is(&address, port);
                    index += 1;
                    keep
                });
            }
            Change::AddHost(host) => {
                if !self
                    .hosts
                    .iter()
                    .any(|saved| saved.is(&host.address, host.port))
                {
                    self.hosts.push(host);
                }
            }
            Change::RemoveHost(address, port) => {
                self.hosts.retain(|saved| !saved.is(&address, port))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file whose path the settings file cannot keep is refused rather
    /// than queued, so the changes after it still save.
    #[cfg(unix)]
    #[test]
    fn a_path_that_is_not_unicode_does_not_hold_back_later_saves() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut history = History::default();
        let odd = SavedFile::local(
            dir.path()
                .join(std::ffi::OsStr::from_bytes(b"Bad\xff.fmp12")),
        );
        assert!(history
            .change(Some(&path), Change::Opened(odd.clone()))
            .is_err());
        assert!(history
            .change(Some(&path), Change::SetFavorite(odd, true))
            .is_err());
        let tasks = SavedFile::local(dir.path().join("Tasks.fmp12"));
        history
            .change(Some(&path), Change::Opened(tasks.clone()))
            .unwrap();
        assert_eq!(History::load(&path).unwrap().files(Tab::Recent), [tasks]);
    }

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
        // The second instance goes on showing its own tab after a save;
        // the first's choice is where the next start opens.
        second
            .change(Some(&path), Change::Opened(hosted.clone()))
            .unwrap();
        assert_eq!(second.tab(), Tab::Recent);
        let restored = History::load(&path).unwrap();
        assert_eq!(restored.tab(), Tab::Favorites);
        assert_eq!(restored.files(Tab::Recent), std::slice::from_ref(&hosted));
        assert_eq!(restored.files(Tab::Favorites), &[hosted]);

        // Saved hosts merge the same way. An edit keeps the host's place,
        // and one that gives it another's address and port replaces both.
        let host = |address: &str, port: u16, name: &str| SavedHost {
            address: address.into(),
            port,
            name: name.into(),
        };
        let save = |history: &mut History, replacing: Option<&SavedHost>, saved: &SavedHost| {
            let change = Change::SaveHost {
                replacing: replacing.map(SavedHost::server),
                host: saved.clone(),
            };
            history.change(Some(&path), change).unwrap();
        };
        let staging = host("staging.example.test", 5004, "");
        let production = host("fms.example.test", 5003, "Production");
        save(&mut first, None, &staging);
        save(&mut second, None, &production);
        let named = host("STAGING.example.test", 5004, "Staging");
        save(&mut first, None, &named);
        assert_eq!(first.hosts(), &[named.clone(), production.clone()]);
        let moved = host("fms.example.test", 5003, "Moved");
        save(&mut second, Some(&named), &moved);
        second
            .change(
                Some(&path),
                Change::RemoveHost("nowhere.example.test".into(), 5003),
            )
            .unwrap();
        // Open Network File's Add to Hosts leaves a host saved meanwhile,
        // and its name, as it was.
        first
            .change(
                Some(&path),
                Change::AddHost(host("fms.example.test", 5003, "")),
            )
            .unwrap();
        let restored = History::load(&path).unwrap();
        assert_eq!(restored.hosts(), &[moved]);
        assert!(!std::fs::read_to_string(&path).unwrap().contains("account"));
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
