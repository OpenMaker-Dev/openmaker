//! Per-user FileMaker client license storage. Only the OS credential store
//! persists the key; dialogs and worker diagnostics never expose its value.

use om_data::DataError;
use zeroize::{Zeroize, Zeroizing};

/// An in-memory license, redacted in diagnostics and cleared on drop.
pub struct LicenseKey(Zeroizing<String>);

impl LicenseKey {
    pub fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }

    /// Borrow the secret only when storing it or configuring a connection.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for LicenseKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LicenseKey([redacted])")
    }
}

/// Persistence boundary, replaceable by an isolated store in UI tests.
/// Implementations must not put secret contents in returned errors.
pub trait LicenseStore: Send {
    fn load(&mut self) -> Result<Option<LicenseKey>, DataError>;
    fn save(&mut self, key: &LicenseKey) -> Result<(), DataError>;
    fn remove(&mut self) -> Result<(), DataError>;
}

/// macOS Keychain, Windows Credential Manager or Linux Secret Service.
pub struct OsLicenseStore {
    service: String,
    account: String,
}

impl Default for OsLicenseStore {
    fn default() -> Self {
        Self {
            service: "org.openmaker.OpenMaker".into(),
            account: "filemaker-client-license".into(),
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
impl OsLicenseStore {
    fn entry(&self) -> Result<keyring::Entry, DataError> {
        keyring::Entry::new(&self.service, &self.account).map_err(|_| store_error())
    }
}

fn store_error() -> DataError {
    // Some backend errors contain secret bytes. Never format those errors.
    DataError::new(
        "The system credential store could not be accessed. Unlock your credential store and \
         try again.",
    )
}

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
impl LicenseStore for OsLicenseStore {
    fn load(&mut self) -> Result<Option<LicenseKey>, DataError> {
        match self.entry()?.get_password() {
            Ok(value) => Ok(Some(LicenseKey::new(value))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(store_error()),
        }
    }

    fn save(&mut self, key: &LicenseKey) -> Result<(), DataError> {
        self.entry()?
            .set_password(key.expose())
            .map_err(|_| store_error())
    }

    fn remove(&mut self) -> Result<(), DataError> {
        match self.entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(store_error()),
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
impl LicenseStore for OsLicenseStore {
    fn load(&mut self) -> Result<Option<LicenseKey>, DataError> {
        Err(store_error())
    }
    fn save(&mut self, _: &LicenseKey) -> Result<(), DataError> {
        Err(store_error())
    }
    fn remove(&mut self) -> Result<(), DataError> {
        Err(store_error())
    }
}

pub(crate) enum LicenseAction {
    Save,
    Remove,
    Close,
}

pub(crate) struct LicenseDialog {
    pub input: Zeroizing<String>,
    pub pending: bool,
    pub stored: Option<bool>,
    pub error: Option<String>,
}

impl Default for LicenseDialog {
    fn default() -> Self {
        Self {
            input: Zeroizing::new(String::new()),
            pending: true,
            stored: None,
            error: None,
        }
    }
}

impl LicenseDialog {
    pub fn accept(&mut self, result: Result<bool, DataError>) {
        self.pending = false;
        match result {
            Ok(stored) => {
                self.stored = Some(stored);
                self.input.zeroize();
                self.error = None;
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<LicenseAction> {
        let mut action = None;
        egui::Modal::new(egui::Id::unique("filemaker_license")).show(ctx, |ui| {
            ui.set_width(400.0);
            ui.heading("FileMaker License");
            ui.label("Optional. FileMaker Server may require a client license; OpenMaker Server does not.");
            ui.add_space(8.0);
            ui.label(if self.pending {
                "Accessing credential store…"
            } else if self.stored == Some(true) {
                "A license is saved in your system credential store."
            } else if self.stored == Some(false) {
                "No license is saved."
            } else {
                "License storage is unavailable."
            });
            ui.add_enabled_ui(!self.pending, |ui| {
                let label = ui.label("License key");
                ui.add(
                    egui::TextEdit::singleline(&mut *self.input)
                        .password(true)
                        .desired_width(f32::INFINITY),
                ).labelled_by(label.id);
                ui.small("Saved for this OS account. Used for future connections when Use saved license is checked.");
                if let Some(error) = &self.error {
                    ui.colored_label(crate::theme::TEXT, error);
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(!self.input.trim().is_empty(), egui::Button::new("Save License")).clicked() {
                        action = Some(LicenseAction::Save);
                    }
                    if ui.add_enabled(self.stored == Some(true), egui::Button::new("Remove License")).clicked() {
                        action = Some(LicenseAction::Remove);
                    }
                    if ui.button("Done").clicked() {
                        action = Some(LicenseAction::Close);
                    }
                });
            });
        });
        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn license_diagnostics_are_redacted() {
        let key = LicenseKey::new("not-a-real-license".into());
        assert_eq!(format!("{key:?}"), "LicenseKey([redacted])");
    }

    #[test]
    #[ignore = "writes and removes a temporary test credential in the real OS store"]
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    fn native_store_round_trip() {
        let account = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let mut store = OsLicenseStore {
            service: "org.openmaker.tests".into(),
            account,
        };
        assert!(store.load().unwrap().is_none());
        store
            .save(&LicenseKey::new("synthetic-license-one".into()))
            .unwrap();
        // A different store instance, as on the next launch, reads the entry.
        let mut reopened = OsLicenseStore {
            service: store.service.clone(),
            account: store.account.clone(),
        };
        assert_eq!(
            reopened.load().unwrap().unwrap().expose(),
            "synthetic-license-one"
        );
        reopened
            .save(&LicenseKey::new("synthetic-license-two".into()))
            .unwrap();
        assert_eq!(
            store.load().unwrap().unwrap().expose(),
            "synthetic-license-two"
        );
        reopened.remove().unwrap();
        assert!(store.load().unwrap().is_none());
        store.remove().unwrap();
    }
}
