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

/// How long a read of the saved license may keep a connection waiting
/// before the connection goes ahead without it. A store that is not asking
/// the user anything answers in well under a second.
pub(crate) const LOAD_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// How long the answer of a read that came after its load gave up is used
/// by a later load: long enough to retry after answering the system's
/// prompt; older answers may be out of date, and the store is read again.
const LATE_ANSWER: std::time::Duration = std::time::Duration::from_secs(60);

/// A read's answer and when it came.
type Read = (std::time::Instant, Result<Option<LicenseKey>, DataError>);

/// What the store thread is asked, with where its answer goes.
enum Operation {
    Load(std::sync::mpsc::Sender<Read>),
    Save(LicenseKey, std::sync::mpsc::Sender<Result<(), DataError>>),
    Remove(std::sync::mpsc::Sender<Result<(), DataError>>),
}

/// A store whose operations run in order on a thread of their own, so that
/// opening a host waits for the saved license at most [`LOAD_WAIT`]. The OS
/// can hold a read for as long as it waits for the user: macOS Keychain
/// asks whether OpenMaker may use the item (always, for a build whose code
/// signature the item does not trust yet), and an unlocked-store prompt does
/// the same elsewhere. OpenMaker Server needs no license, so its connections
/// must not wait on that ([#151](https://github.com/OpenMaker-Dev/openmaker/issues/151)).
///
/// A read that did not answer in time is not asked again: the next load
/// waits for the same one, and takes its answer once the user gave it, for
/// [`LATE_ANSWER`] after it came. Saving and removing are refused while such a read is still waiting: they
/// would queue behind it and hold the worker until the user answers. Once
/// they run they wait for the store without a limit, as the user asked for
/// them in the License dialog and a write that is abandoned would still
/// happen later; they also drop the answer of any earlier read, which is
/// then out of date.
pub(crate) struct ThreadedStore {
    /// The store until its thread has started.
    inner: Option<Box<dyn LicenseStore>>,
    thread: Option<std::sync::mpsc::Sender<Operation>>,
    reading: Option<std::sync::mpsc::Receiver<Read>>,
    wait: std::time::Duration,
}

/// The store thread is gone (its store panicked): nothing reaches the store
/// again until OpenMaker starts again.
fn stopped() -> DataError {
    DataError::new(
        "The system credential store stopped answering OpenMaker. Quit and open OpenMaker \
         again to use the saved license.",
    )
}

impl ThreadedStore {
    pub(crate) fn new(inner: Box<dyn LicenseStore>, wait: std::time::Duration) -> Self {
        Self {
            inner: Some(inner),
            thread: None,
            reading: None,
            wait,
        }
    }

    /// Send `operation` to the store thread, started on first use. The store
    /// moves to the thread once it runs: a thread that cannot start leaves
    /// it here for the next try.
    fn send(&mut self, operation: Operation) -> Result<(), DataError> {
        if self.thread.is_none() {
            let (tx, rx) = std::sync::mpsc::channel::<Operation>();
            let (hand, take) = std::sync::mpsc::channel::<Box<dyn LicenseStore>>();
            std::thread::Builder::new()
                .name("om-license-store".into())
                .spawn(move || {
                    let Ok(mut inner) = take.recv() else {
                        return;
                    };
                    // Ends when the worker drops its store. A closed answer
                    // channel means nobody waits any more.
                    for operation in rx {
                        match operation {
                            Operation::Load(answer) => {
                                let read = inner.load();
                                let _ = answer.send((std::time::Instant::now(), read));
                            }
                            Operation::Save(key, answer) => {
                                let _ = answer.send(inner.save(&key));
                            }
                            Operation::Remove(answer) => {
                                let _ = answer.send(inner.remove());
                            }
                        }
                    }
                })
                .map_err(|_| store_error())?;
            let inner = self.inner.take().ok_or_else(stopped)?;
            hand.send(inner).map_err(|_| stopped())?;
            self.thread = Some(tx);
        }
        self.thread
            .as_ref()
            .and_then(|thread| thread.send(operation).ok())
            .ok_or_else(stopped)
    }

    /// Send a write and wait for its answer, unless a read is still waiting
    /// for the store.
    fn write(
        &mut self,
        operation: impl FnOnce(std::sync::mpsc::Sender<Result<(), DataError>>) -> Operation,
    ) -> Result<(), DataError> {
        if let Some(reading) = &self.reading {
            if let Err(std::sync::mpsc::TryRecvError::Empty) = reading.try_recv() {
                return Err(DataError::new(
                    "The system credential store has not answered an earlier request yet. If \
                     it is asking whether OpenMaker may use the saved license, answer it and \
                     try again.",
                ));
            }
            self.reading = None;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.send(operation(tx))?;
        rx.recv().map_err(|_| stopped())?
    }
}

impl LicenseStore for ThreadedStore {
    fn load(&mut self) -> Result<Option<LicenseKey>, DataError> {
        // An answer that came long after its load gave up may be out of
        // date (another OpenMaker may have changed the key): read again.
        if let Some(reading) = &self.reading {
            match reading.try_recv() {
                Ok((at, result)) if at.elapsed() <= LATE_ANSWER => {
                    self.reading = None;
                    return result;
                }
                Ok(_) | Err(std::sync::mpsc::TryRecvError::Disconnected) => self.reading = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.reading.is_none() {
            let (tx, rx) = std::sync::mpsc::channel();
            self.send(Operation::Load(tx))?;
            self.reading = Some(rx);
        }
        let reading = self.reading.as_ref().expect("a read was just sent");
        match reading.recv_timeout(self.wait) {
            Ok((_, result)) => {
                self.reading = None;
                result
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(DataError::new(
                "The system credential store did not answer in time. If it is asking whether \
                 OpenMaker may use the saved license, answer it and try again.",
            )),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                self.reading = None;
                Err(stopped())
            }
        }
    }

    fn save(&mut self, key: &LicenseKey) -> Result<(), DataError> {
        let key = LicenseKey::new(key.expose().to_owned());
        self.write(|answer| Operation::Save(key, answer))
    }

    fn remove(&mut self) -> Result<(), DataError> {
        self.write(Operation::Remove)
    }
}

pub(crate) enum LicenseAction {
    Save,
    Remove,
    /// Read the store again after it could not say whether a key is saved.
    Check,
    Close,
}

pub(crate) struct LicenseDialog {
    pub input: Zeroizing<String>,
    pub pending: bool,
    /// The request waited for saves or removes the key: the draft goes
    /// when it succeeds. A status read keeps it.
    pub writing: bool,
    pub stored: Option<bool>,
    pub error: Option<String>,
}

impl Default for LicenseDialog {
    fn default() -> Self {
        Self {
            input: Zeroizing::new(String::new()),
            pending: true,
            writing: false,
            stored: None,
            error: None,
        }
    }
}

impl LicenseDialog {
    pub fn accept(&mut self, result: Result<bool, DataError>) {
        self.pending = false;
        let writing = std::mem::take(&mut self.writing);
        match result {
            Ok(stored) => {
                self.stored = Some(stored);
                if writing {
                    self.input.zeroize();
                }
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
                    // The store could not say whether a key is saved: it may
                    // have answered since (#151's prompt).
                    if self.stored.is_none() && ui.button("Check Again").clicked() {
                        action = Some(LicenseAction::Check);
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

    /// A store whose reads wait until the test lets each through, as macOS
    /// Keychain waits for the user to allow OpenMaker to use the item.
    struct Held {
        gate: std::sync::mpsc::Receiver<()>,
        reads: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        saves: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl LicenseStore for Held {
        fn load(&mut self) -> Result<Option<LicenseKey>, DataError> {
            let read = 1 + self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.gate.recv().map_err(|_| store_error())?;
            Ok(Some(LicenseKey::new(format!("synthetic-license-{read}"))))
        }
        fn save(&mut self, _: &LicenseKey) -> Result<(), DataError> {
            self.saves.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        fn remove(&mut self) -> Result<(), DataError> {
            Ok(())
        }
    }

    #[test]
    fn a_read_the_store_holds_gives_up_in_time_and_answers_a_later_load() {
        let (release, gate) = std::sync::mpsc::channel();
        // Should a save queue behind the held read again, it would wait for
        // a release the test never gets to: this one comes late instead, so
        // the test fails rather than hangs.
        let watchdog = release.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(20));
            for _ in 0..4 {
                let _ = watchdog.send(());
            }
        });
        let reads = std::sync::Arc::default();
        let saves = std::sync::Arc::default();
        let mut store = ThreadedStore::new(
            Box::new(Held {
                gate,
                reads: std::sync::Arc::clone(&reads),
                saves: std::sync::Arc::clone(&saves),
            }),
            std::time::Duration::from_millis(20),
        );
        // Opening a host goes ahead without the license; trying again
        // waits for the same read instead of asking the store again.
        assert!(store.load().is_err());
        assert!(store.load().is_err());
        release.send(()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let answered = |store: &mut ThreadedStore| loop {
            match store.load() {
                Ok(key) => break key.unwrap().expose().to_owned(),
                Err(_) => assert!(std::time::Instant::now() < deadline),
            }
        };
        assert_eq!(answered(&mut store), "synthetic-license-1");
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 1);
        // A save while a read still waits is refused rather than queued
        // behind it, where it would hold the worker.
        assert!(store.load().is_err());
        let key = LicenseKey::new("synthetic".into());
        assert!(store.save(&key).is_err());
        assert_eq!(saves.load(std::sync::atomic::Ordering::SeqCst), 0);
        release.send(()).unwrap();
        while store.save(&key).is_err() {
            assert!(std::time::Instant::now() < deadline);
        }
        assert_eq!(saves.load(std::sync::atomic::Ordering::SeqCst), 1);
        // The earlier read's answer is out of date after the save: the next
        // load reads again.
        release.send(()).unwrap();
        assert_eq!(answered(&mut store), "synthetic-license-3");
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
