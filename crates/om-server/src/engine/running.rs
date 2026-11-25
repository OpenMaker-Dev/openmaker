//! The engine a server runs (docs/server-storage.md 5, "The engine"): one
//! for the whole server, chosen when it starts (`--engine`, section 11).
//! Its surface is what both engines serve: a reading of a file
//! ([`Engine::read`]), the engine's state of a file ([`Engine::read_only`],
//! [`Engine::writes_refused`], [`Engine::file_on_disk`]), an export
//! ([`Engine::export`]), a submit ([`Engine::submit`]), and what the engine
//! can do ([`Engine::capabilities`]). It is an enum, not a trait: the server
//! holds one value of it and every caller asks it the same things.

use std::io;

use super::export::{Exported, Kind, Target};
use super::reading::Reading;
use super::store::StoreEngine;
use super::{Credentials, Draft, ExportError, FileEngine, OnDisk, Operation, Outcome, Progress};
use crate::anchored::FileId;
use crate::hosting::Hosted;
use crate::store::ReadOnlyCause;

/// The engine a server runs.
pub enum Engine {
    /// Each file one `.fmp12`, loaded whole and saved whole.
    File(FileEngine),
    /// Each file a tree in the store folder's page file, read-only.
    Store(StoreEngine),
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Engine::File(_) => f.write_str("Engine::File"),
            Engine::Store(engine) => f.debug_tuple("Engine::Store").field(engine).finish(),
        }
    }
}

impl Default for Engine {
    fn default() -> Self {
        Engine::File(FileEngine::default())
    }
}

/// What an engine can do, for a feature to ask before it relies on it
/// (docs/server-storage.md 5, 11): what it lacks answers a typed
/// "unavailable".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    /// It stores changes to its files.
    pub writes: bool,
    /// A backup copies the file on disk; otherwise it is written from a
    /// view.
    pub copies_files_on_disk: bool,
}

impl Engine {
    /// The engine, by reference: what its methods ask.
    pub fn as_ref(&self) -> EngineRef<'_> {
        match self {
            Engine::File(engine) => EngineRef::File(engine),
            Engine::Store(engine) => EngineRef::Store(engine),
        }
    }

    /// What the engine can do.
    pub fn capabilities(&self) -> Capabilities {
        self.as_ref().capabilities()
    }

    /// The version of `file` to read, held for as long as the reading.
    pub fn read<'h>(&self, file: &'h Hosted) -> Reading<'h> {
        self.as_ref().read(file)
    }

    pub fn submit(&self, file: &Hosted, operation: Operation) -> Outcome {
        self.as_ref().submit(file, operation)
    }

    pub fn submit_with(
        &self,
        file: &Hosted,
        operation: Operation,
        credentials: Credentials,
    ) -> Outcome {
        self.as_ref().submit_with(file, operation, credentials)
    }

    pub fn try_submit(&self, file: &Hosted, operation: Operation) -> Option<Outcome> {
        self.as_ref().try_submit(file, operation)
    }

    pub fn submit_draft(&self, file: &Hosted, draft: Draft) -> Outcome {
        self.as_ref().submit_draft(file, draft)
    }

    pub fn read_only(&self, file: &Hosted) -> Option<ReadOnlyCause> {
        self.as_ref().read_only(file)
    }

    pub fn writes_refused(&self, file: &Hosted) -> Option<String> {
        self.as_ref().writes_refused(file)
    }

    pub fn export(
        &self,
        file: &Hosted,
        kind: Kind,
        target: Target<'_, '_>,
        progress: Progress<'_>,
    ) -> Result<Exported, ExportError> {
        self.as_ref().export(file, kind, target, progress)
    }

    pub fn stored(&self, file: &Hosted) -> io::Result<OnDisk> {
        self.as_ref().stored(file)
    }

    /// Answer reads through the test view, checked against the engine's
    /// (`Server::checking_reads`): the file engine's alone.
    #[cfg(debug_assertions)]
    pub(crate) fn check_reads(&self) {
        if let Engine::File(engine) = self {
            engine.check_reads();
        }
    }

    /// Check each operation's writes through its transaction
    /// (`Server::checking_reads`): the file engine's alone, as the store
    /// engine writes nothing.
    #[cfg(debug_assertions)]
    pub(crate) fn check_writes(&self) {
        if let Engine::File(engine) = self {
            engine.check_writes();
        }
    }

    /// The reads the test view compared on a store copy, and those it did
    /// not (`ServerHandle::store_comparisons`): the file engine's alone.
    #[cfg(debug_assertions)]
    pub(crate) fn store_comparisons(&self) -> Option<super::Compared> {
        match self {
            Engine::File(engine) => engine.store_comparisons(),
            Engine::Store(_) => None,
        }
    }

    /// Fail every read of `path` in the file named `file`
    /// (`ServerHandle::failing_reads`); on the store engine, the empty path
    /// makes the file's root fail to open.
    #[cfg(debug_assertions)]
    pub(crate) fn fail_reads(&self, file: &str, path: Vec<Vec<u8>>) {
        match self {
            Engine::File(engine) => engine.fail_reads(file, path),
            Engine::Store(engine) if path.is_empty() => engine.break_root(file),
            Engine::Store(_) => {}
        }
    }
}

/// The engine a server runs, by reference ([`Engine::as_ref`]): what the
/// hosting's functions ask, which tests also call with the file engine
/// alone ([`AsEngine`]).
#[derive(Clone, Copy)]
pub enum EngineRef<'e> {
    File(&'e FileEngine),
    Store(&'e StoreEngine),
}

impl<'e> EngineRef<'e> {
    /// What the engine can do.
    pub fn capabilities(self) -> Capabilities {
        match self {
            EngineRef::File(_) => Capabilities {
                writes: true,
                copies_files_on_disk: true,
            },
            EngineRef::Store(_) => Capabilities {
                writes: false,
                copies_files_on_disk: false,
            },
        }
    }

    /// The version of `file` to read, held for as long as the reading.
    pub fn read<'h>(self, file: &'h Hosted) -> Reading<'h> {
        match self {
            EngineRef::File(engine) => engine.read(file),
            EngineRef::Store(engine) => engine.read(file),
        }
    }

    pub fn submit(self, file: &Hosted, operation: Operation) -> Outcome {
        match self {
            EngineRef::File(engine) => engine.submit(file, operation),
            EngineRef::Store(engine) => engine.submit(file, operation),
        }
    }

    pub fn submit_with(
        self,
        file: &Hosted,
        operation: Operation,
        credentials: Credentials,
    ) -> Outcome {
        match self {
            EngineRef::File(engine) => engine.submit_with(file, operation, credentials),
            EngineRef::Store(engine) => engine.submit_with(file, operation, credentials),
        }
    }

    pub fn try_submit(self, file: &Hosted, operation: Operation) -> Option<Outcome> {
        match self {
            EngineRef::File(engine) => engine.try_submit(file, operation),
            EngineRef::Store(engine) => Some(engine.submit(file, operation)),
        }
    }

    pub fn submit_draft(self, file: &Hosted, draft: Draft) -> Outcome {
        match self {
            EngineRef::File(engine) => engine.submit_draft(file, draft),
            EngineRef::Store(engine) => engine.submit_draft(file, draft),
        }
    }

    pub fn read_only(self, file: &Hosted) -> Option<ReadOnlyCause> {
        match self {
            EngineRef::File(engine) => engine.read_only(file),
            EngineRef::Store(engine) => engine.read_only(file),
        }
    }

    pub fn writes_refused(self, file: &Hosted) -> Option<String> {
        match self {
            EngineRef::File(engine) => engine.writes_refused(file),
            EngineRef::Store(_) => None,
        }
    }

    pub fn file_on_disk(self, file: &Hosted) -> Option<FileId> {
        match self {
            EngineRef::File(engine) => engine.file_on_disk(file),
            EngineRef::Store(engine) => engine.file_on_disk(file),
        }
    }

    /// Hold `file` read-only for `cause` from now on; on the store engine,
    /// which takes no changes, every file is already.
    pub(crate) fn hold_read_only(self, file: &Hosted, cause: ReadOnlyCause) {
        if let EngineRef::File(engine) = self {
            engine.hold_read_only(file, cause);
        }
    }

    /// `file` was loaded from the file on disk `on_disk`: the file engine
    /// saves only where the path still leads to it.
    pub(crate) fn loaded_from(self, file: &Hosted, on_disk: Option<FileId>) {
        if let EngineRef::File(engine) = self {
            engine.loaded_from(file, on_disk);
        }
    }

    /// Export the version of `file` the engine holds now, as `kind` says.
    /// The store engine writes it from the view whichever kind.
    pub fn export(
        self,
        file: &Hosted,
        kind: Kind,
        target: Target<'_, '_>,
        progress: Progress<'_>,
    ) -> Result<Exported, ExportError> {
        match self {
            EngineRef::File(engine) => engine.export(file, kind, target, progress),
            EngineRef::Store(_) => {
                let reading = self.read(file);
                let page_zero = reading.read().derived.page_zero().to_vec();
                reading.serve_once(|read| {
                    super::paged::export(read.view, &page_zero, target, progress)
                })
            }
        }
    }

    /// The file on disk a backup copies: the file engine's last save, or
    /// the file it loaded. The store engine has none; its backups are
    /// written from the view ([`Capabilities::copies_files_on_disk`]).
    pub fn stored(self, file: &Hosted) -> io::Result<OnDisk> {
        match self {
            EngineRef::File(engine) => engine.stored(file),
            EngineRef::Store(_) => Err(io::Error::other(
                "the store engine's file is no file on disk: its backup is written from its view",
            )),
        }
    }
}

/// What can be asked as an engine: the server's, or the file engine alone
/// (the engine's own tests host files with it).
pub trait AsEngine {
    fn as_engine(&self) -> EngineRef<'_>;
}

impl AsEngine for Engine {
    fn as_engine(&self) -> EngineRef<'_> {
        self.as_ref()
    }
}

impl AsEngine for FileEngine {
    fn as_engine(&self) -> EngineRef<'_> {
        EngineRef::File(self)
    }
}

impl<T: AsEngine + ?Sized> AsEngine for std::sync::Arc<T> {
    fn as_engine(&self) -> EngineRef<'_> {
        (**self).as_engine()
    }
}

impl AsEngine for EngineRef<'_> {
    fn as_engine(&self) -> EngineRef<'_> {
        *self
    }
}
