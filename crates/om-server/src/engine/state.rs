//! What the engine says of a hosted file that is not a version's
//! (docs/server-storage.md 5, "The engine's state of a file"): whether the
//! file takes changes and why not, and which file on disk the engine last
//! read or saved for it.
//!
//! None of these is read through a view. They describe the engine's hold
//! of the file on disk and of its clients, not the content of a version:
//! two versions with the same tree can differ in them (a save that renames
//! a new copy over the file changes its identity on disk and nothing in
//! the tree), and a reader that holds an older version must be told how
//! they stand now (a backup protects the file on disk now, a client is
//! refused a change by the file as it is now, whatever version it read).
//! An engine with versions keeps them once for each file. The file engine
//! keeps them in its store and reads them under the store's read lock.
//! Whether the file takes no more changes is also copied beside the store
//! as each write lets go of it, an unwound one too
//! (`Hosted::writes_refused`), for the administrator's list, which reads it
//! without waiting for a writer; the store's own is the one the engine
//! answers.

use super::{FileEngine, WRITER};
use crate::anchored::FileId;
use crate::hosting::Hosted;
use crate::store::ReadOnlyCause;

impl FileEngine {
    /// Why `file` takes no changes from its clients, if it does not: the
    /// first cause that made it so (`Store::set_read_only`). A file that
    /// takes no more changes at all is told by [`Self::writes_refused`].
    pub fn read_only(&self, file: &Hosted) -> Option<ReadOnlyCause> {
        file.read_store().read_only_cause()
    }

    /// Why `file` takes no more changes until it is hosted again, if it
    /// does not: a save's outcome was unknown (`store::SaveError::Unknown`,
    /// or a save left by an unwind after it renamed its copy over the file).
    pub fn writes_refused(&self, file: &Hosted) -> Option<String> {
        file.read_store().writes_refused().map(str::to_owned)
    }

    /// The device and inode of the file on disk the engine last read or
    /// saved for `file`, where the system gives them: what a backup
    /// protects and what another slot must not host again.
    pub fn file_on_disk(&self, file: &Hosted) -> Option<FileId> {
        file.read_store().file()
    }

    /// Hold `file` read-only for `cause` from now on, unless it already is
    /// for another (the first cause stays, `Store::set_read_only`).
    pub(crate) fn hold_read_only(&self, file: &Hosted, cause: ReadOnlyCause) {
        file.write_store(&WRITER).set_read_only(cause);
    }

    /// `file` was loaded from the file on disk `on_disk`, where the system
    /// gives its identity: set as the file is loaded, before anyone can
    /// reach it and before any save, which writes only where the path
    /// still leads to that file.
    pub(crate) fn loaded_from(&self, file: &Hosted, on_disk: Option<FileId>) {
        file.write_store(&WRITER).set_file(on_disk);
    }
}
