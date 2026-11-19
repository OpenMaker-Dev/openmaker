//! Listing a host's files for the Hosts dialog, before any file is open:
//! what a host publishes, and why a listing failed. A host that publishes
//! no files lists none; that is not a failure.

use std::sync::atomic::AtomicBool;

use crate::{DataError, HostedFile};

/// What a host lists in the Hosts dialog.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostListing {
    /// The host's name and version as it gave them when the client
    /// connected (`Server 26.0.3`); empty when the provider does not know.
    pub host_name: String,
    pub host_version: String,
    /// The files the host publishes, by name, in the order to show them.
    pub files: Vec<HostedFile>,
}

impl HostListing {
    /// A listing of `files` from a provider that knows nothing else.
    pub fn of_files(files: Vec<HostedFile>) -> Self {
        Self {
            files,
            ..Self::default()
        }
    }
}

/// Why a host's files could not be listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListingFailure {
    /// The host could not be reached, or no trusted connection made.
    Unreachable,
    /// The host answered but offers no file list this client can read.
    Unsupported,
    /// The host's list was malformed or incomplete.
    Malformed,
    /// The listing was canceled.
    Canceled,
    /// Another failure, from a provider that does not say which.
    Other,
}

/// A failed listing: which kind of failure, and what to tell the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListingError {
    pub failure: ListingFailure,
    pub message: String,
}

impl ListingError {
    pub fn new(failure: ListingFailure, message: impl Into<String>) -> Self {
        Self {
            failure,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ListingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ListingError {}

impl From<DataError> for ListingError {
    fn from(error: DataError) -> Self {
        Self::new(ListingFailure::Other, error.message)
    }
}

impl From<ListingError> for DataError {
    /// FileMaker's error 1 (user canceled) for a canceled listing and 3
    /// (command unavailable) for a host without a readable list.
    fn from(error: ListingError) -> Self {
        match error.failure {
            ListingFailure::Canceled => DataError::with_code(1, error.message),
            ListingFailure::Unsupported => DataError::with_code(3, error.message),
            _ => DataError::new(error.message),
        }
    }
}

/// Lists hosts' files apart from any open file: on whatever thread calls
/// it, over connections of its own ([`crate::DataProvider::host_browser`]).
pub trait HostBrowser: Send + Sync {
    /// The files `host` publishes, read over a connection of the browser's
    /// own that is closed before this returns, whatever the outcome. Once
    /// `canceled` is set the listing stops between steps (a step under way
    /// finishes or times out first) with [`ListingFailure::Canceled`].
    fn browse(
        &self,
        host: &str,
        port: u16,
        canceled: &AtomicBool,
    ) -> Result<HostListing, ListingError>;
}
