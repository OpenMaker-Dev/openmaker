//! OpenMaker Server's store engine (`docs/server-storage.md` section 6): a
//! change log, copy-on-write page trees with a hash of every page, views,
//! checkpoints and recovery, and the consensus core that replicates the
//! log (section 7.1, [`consensus`]). The byte formats are in
//! `docs/server-storage-format.md`.
//!
//! The crate knows paths, values, files, entries and pages. It knows nothing
//! of records, catalogs or the protocol.

pub mod apply;
pub mod consensus;
pub mod entry;
pub mod incident;
pub mod log;
pub mod model;
pub mod os;
pub mod page;
pub mod pagefile;
pub mod sim;
pub mod state;
pub mod store;
pub mod sys;
pub mod tree;
