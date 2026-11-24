//! The desktop's preferences, kept in a JSON file for the OS account.
//!
//! The file keeps keys this version does not know, and a file that cannot be
//! read is never written over.

use std::path::{Path, PathBuf};

use om_data::host::machine::{self, UserNameSetting};
use om_data::DataError;

use crate::widgets::dialog_button;

/// The settings file named by `OPENMAKER_SETTINGS`, else `settings.json` in
/// OpenMaker's folder of the OS account's application settings:
/// `~/Library/Application Support/OpenMaker` on macOS, `%APPDATA%\OpenMaker`
/// on Windows, `$XDG_CONFIG_HOME/openmaker` (`~/.config/openmaker`)
/// elsewhere. `None` when the system gives no such folder.
pub fn default_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("OPENMAKER_SETTINGS").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(path));
    }
    machine::user_config_dir().map(|f| f.join("settings.json"))
}

const USER_NAME: &str = "user_name";

/// The JSON object in `path`; an empty one when there is no file.
pub(crate) fn read(path: &Path) -> Result<serde_json::Map<String, serde_json::Value>, DataError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Default::default()),
        Err(e) => {
            return Err(DataError::new(format!(
                "The settings file {} could not be read: {e}",
                path.display()
            )))
        }
    };
    match serde_json::from_str(&text) {
        Ok(serde_json::Value::Object(map)) => Ok(map),
        _ => Err(DataError::new(format!(
            "The settings file {} is not an OpenMaker settings file, so it was left unchanged.",
            path.display()
        ))),
    }
}

/// The User Name setting saved in `path`: System when the file or the
/// setting is absent, or names a choice this version does not know.
pub fn load_user_name(path: &Path) -> Result<UserNameSetting, DataError> {
    let map = read(path)?;
    let Some(value) = map.get(USER_NAME) else {
        return Ok(UserNameSetting::System);
    };
    Ok(match value.get("choice").and_then(|c| c.as_str()) {
        Some("other") => UserNameSetting::Other(
            value
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or_default()
                .to_owned(),
        ),
        _ => UserNameSetting::System,
    })
}

/// Save `setting` in `path`, keeping its other keys and the file's
/// permissions. Refused, with nothing changed, when the file there cannot be
/// read as a settings file, is read-only, or cannot be written.
pub fn save_user_name(path: &Path, setting: &UserNameSetting) -> Result<(), DataError> {
    let value = match setting {
        UserNameSetting::System => serde_json::json!({ "choice": "system" }),
        UserNameSetting::Other(name) => serde_json::json!({ "choice": "other", "name": name }),
    };
    update(path, |map| {
        map.insert(USER_NAME.into(), value);
        Ok(())
    })
}

/// How long a save waits for another OpenMaker's save of the settings to
/// end before it fails: saves run on the window's thread, and an instance
/// stopped while it held the lock must not freeze this one.
const LOCK_WAIT: std::time::Duration = std::time::Duration::from_millis(500);

/// Apply a change to the latest settings under a lock on a stable sibling
/// file. Locking the settings file itself would lose the lock on replacement.
pub(crate) fn update<T>(
    path: &Path,
    change: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>) -> Result<T, DataError>,
) -> Result<T, DataError> {
    let failed = |e: &dyn std::fmt::Display| {
        DataError::new(format!(
            "The settings could not be saved in {}: {e}",
            path.display()
        ))
    };
    let folder = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(folder).map_err(|e| failed(&e))?;
    let mut lock_path = path.as_os_str().to_owned();
    lock_path.push(".lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)
        .map_err(|e| failed(&e))?;
    let since = std::time::Instant::now();
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) if since.elapsed() < LOCK_WAIT => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(failed(&"another OpenMaker is saving its settings"));
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(failed(&e)),
        }
    }
    let permissions = match std::fs::metadata(path) {
        Ok(metadata) => Some(metadata.permissions()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(DataError::new(format!(
                "The settings file {} could not be read: {e}",
                path.display()
            )))
        }
    };
    // Moving the new file into place would replace a read-only one.
    if permissions
        .as_ref()
        .is_some_and(std::fs::Permissions::readonly)
    {
        return Err(DataError::new(format!(
            "The settings file {} is read-only, so it was left unchanged.",
            path.display()
        )));
    }
    let mut map = read(path)?;
    let value = change(&mut map)?;
    let mut text = serde_json::to_string_pretty(&map).map_err(|e| failed(&e))?;
    text.push('\n');
    // Written whole beside the file, then moved over it: a failed save
    // leaves the old file.
    let temporary = tempfile::NamedTempFile::new_in(folder).map_err(|e| failed(&e))?;
    std::fs::write(temporary.path(), text).map_err(|e| failed(&e))?;
    if let Some(permissions) = permissions {
        std::fs::set_permissions(temporary.path(), permissions).map_err(|e| failed(&e))?;
    }
    temporary.persist(path).map_err(|e| failed(&e.error))?;
    Ok(value)
}

/// Settings' OK: save `setting` in `path` (when the system gives a settings
/// file) and use it from now on, or, refused, change nothing.
pub fn set_user_name(path: Option<&Path>, setting: UserNameSetting) -> Result<(), DataError> {
    if let Some(path) = path {
        save_user_name(path, &setting)?;
    }
    machine::set_user_name_setting(setting);
    Ok(())
}

/// At launch: use the User Name setting saved in `path`. A file that cannot
/// be read leaves System in effect; the message says why.
pub fn apply_saved(path: &Path) -> Result<(), DataError> {
    machine::set_user_name_setting(load_user_name(path)?);
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Choice {
    System,
    Other,
}

/// What the Settings dialog asks for.
pub(crate) enum SettingsAction {
    Save(UserNameSetting),
    Cancel,
}

/// The Settings dialog: FileMaker Pro 26's General tab shows "User Name"
/// with "System: “full name”" and "Other" and its text field
/// (calc-functions.md 6.1.3). The other tabs and settings are not offered.
pub(crate) struct SettingsDialog {
    choice: Choice,
    other: String,
    pub error: Option<String>,
}

impl SettingsDialog {
    /// The dialog showing the setting in effect.
    pub fn new() -> Self {
        let (choice, other) = match machine::user_name_setting() {
            UserNameSetting::System => (Choice::System, String::new()),
            UserNameSetting::Other(name) => (Choice::Other, name),
        };
        Self {
            choice,
            other,
            error: None,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<SettingsAction> {
        let mut action = None;
        let modal = egui::Modal::new(egui::Id::unique("openmaker_settings")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.heading("Settings");
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let _ = ui.selectable_label(true, "General");
            });
            ui.separator();
            let heading = ui.strong("User Name");
            let system = &machine::Machine::current().system_user_name;
            ui.radio_value(
                &mut self.choice,
                Choice::System,
                format!("System: \u{201c}{system}\u{201d}"),
            );
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.choice, Choice::Other, "Other");
                ui.add_enabled(
                    self.choice == Choice::Other,
                    egui::TextEdit::singleline(&mut self.other).desired_width(f32::INFINITY),
                )
                .labelled_by(heading.id);
            });
            if let Some(error) = &self.error {
                ui.add_space(4.0);
                ui.colored_label(crate::theme::TEXT, error);
            }
            ui.add_space(8.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if dialog_button(ui, "OK", true, true).clicked()
                    || ui.input(|i| i.key_pressed(egui::Key::Enter))
                {
                    action = Some(SettingsAction::Save(match self.choice {
                        Choice::System => UserNameSetting::System,
                        Choice::Other => UserNameSetting::Other(self.other.clone()),
                    }));
                }
                if dialog_button(ui, "Cancel", false, true).clicked() {
                    action = Some(SettingsAction::Cancel);
                }
            });
        });
        if modal.should_close() && action.is_none() {
            action = Some(SettingsAction::Cancel);
        }
        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A save waits a bounded time for another instance's lock, then
    /// fails with nothing changed, rather than freeze the window.
    #[test]
    fn a_save_gives_up_on_a_lock_another_instance_holds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let other = UserNameSetting::Other("Pat Doe".into());
        let held = std::fs::File::create(dir.path().join("settings.json.lock")).unwrap();
        held.lock().unwrap();
        let since = std::time::Instant::now();
        assert!(save_user_name(&path, &other).is_err());
        assert!(since.elapsed() < 10 * LOCK_WAIT);
        assert!(!path.exists());
        held.unlock().unwrap();
        save_user_name(&path, &other).unwrap();
        assert_eq!(load_user_name(&path).unwrap(), other);
    }

    #[test]
    fn the_user_name_keeps_other_settings_and_an_unreadable_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/settings.json");
        assert_eq!(load_user_name(&path).unwrap(), UserNameSetting::System);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"later": [1, "kept"]}"#).unwrap();
        let other = UserNameSetting::Other("Pat Doe".into());
        save_user_name(&path, &other).unwrap();
        assert_eq!(load_user_name(&path).unwrap(), other);
        let saved: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved["later"], serde_json::json!([1, "kept"]));
        save_user_name(&path, &UserNameSetting::System).unwrap();
        assert_eq!(load_user_name(&path).unwrap(), UserNameSetting::System);
        // A read-only file is left as it is; a writable one keeps its
        // permissions.
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&path, permissions.clone()).unwrap();
        let kept = std::fs::read_to_string(&path).unwrap();
        assert!(save_user_name(&path, &other).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), kept);
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        std::fs::set_permissions(&path, permissions).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
            save_user_name(&path, &other).unwrap();
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o640);
        }
        std::fs::write(&path, "not json").unwrap();
        assert!(load_user_name(&path).is_err());
        assert!(save_user_name(&path, &other).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not json");
        assert_eq!(machine::user_name_setting(), UserNameSetting::System);
    }
}
