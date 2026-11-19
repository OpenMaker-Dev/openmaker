//! The files that refused an account and password during a session (#738).
//!
//! A host counts each failed Login: FileMaker Server logs it (event 661)
//! and locks an account on a database after too many (event 1049,
//! docs/re/server-logs.md), and OpenMaker Server holds back a client
//! address after five (`om_server::login_limits`). A session opens other
//! files with the first file's credentials: its related files, the files
//! script steps open (`DataProvider::open_peer`) and those files' own
//! related files. Each is sent a set of credentials at most once: a file
//! that refused them answers the same refusal again without its host being
//! contacted. Other credentials for that file (a sign-in the window asks
//! for, #214) are another pair, tried once; the refused pair stays refused.
//! Another host or port is offered the first file's credentials once in
//! all: after any of its files refused them, none of its files is sent
//! them ([`LoginRefusals::refused_on`]).
//!
//! The memory belongs to the session: the provider that opened the first
//! file, the peers it opens and their related-file loaders share it. A
//! Reconnect keeps it; opening the first file again starts a new one. It
//! keeps a digest of the credentials, never the password.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;
use std::sync::{Arc, Mutex};

use om_data::DataError;

/// A session's memory, shared by its providers and loaders.
pub(crate) type Shared = Arc<Mutex<LoginRefusals>>;

/// A file a Login went to: a hosted file by host, port and name (without
/// case, as FileMaker compares names; `.fmp12` added), a file on this
/// computer by its path, whichever port its private host has.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum FileKey {
    Hosted {
        host: String,
        port: u16,
        name: String,
    },
    Local(OsString),
}

impl FileKey {
    pub fn hosted(host: &str, port: u16, name: &str) -> Self {
        Self::Hosted {
            host: host.trim().to_lowercase(),
            port,
            name: om_data::file_refs::hosted_name(name).to_lowercase(),
        }
    }

    pub fn local(path: &Path) -> Self {
        Self::Local(
            std::fs::canonicalize(path)
                .unwrap_or_else(|_| path.to_path_buf())
                .into_os_string(),
        )
    }
}

/// Who a Login admits with `account` and `password`, as a connection's
/// files are shared by (`om_session::Session::claim_file`, #1011): their
/// digest, never the credentials.
pub(crate) fn admission(account: &str, password: &str) -> om_session::Admission {
    om_session::Admission::from_digest(digest(account, password))
}

/// SHA-256 of the account name and password, each with its length.
fn digest(account: &str, password: &str) -> [u8; 32] {
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    for part in [account, password] {
        bytes.extend_from_slice(&(part.len() as u64).to_be_bytes());
        bytes.extend_from_slice(part.as_bytes());
    }
    om_crypto::sha256(&bytes)
}

/// Each refusal by the file and the credentials it refused: a file can
/// have refused several (the first file's and, after a Reconnect with a new
/// password, another), and a peer may still hold an earlier one.
#[derive(Default)]
pub(crate) struct LoginRefusals {
    refused: BTreeMap<(FileKey, [u8; 32]), DataError>,
}

impl LoginRefusals {
    /// The refusal `file` gave `account` and `password` earlier in the
    /// session, to answer again instead of sending them.
    pub fn refused(&self, file: &FileKey, account: &str, password: &str) -> Option<DataError> {
        self.refused
            .get(&(file.clone(), digest(account, password)))
            .cloned()
    }

    /// The refusal any file on `host` at `port` gave `account` and
    /// `password` earlier in the session: a file's account is offered to
    /// another host once, and not to any of its files after one refused
    /// it (`NativeProvider::open_peer_with`).
    pub fn refused_on(
        &self,
        host: &str,
        port: u16,
        account: &str,
        password: &str,
    ) -> Option<DataError> {
        let host = host.trim().to_lowercase();
        let digest = digest(account, password);
        self.refused
            .iter()
            .find_map(|((file, refused), error)| match file {
                FileKey::Hosted {
                    host: h, port: p, ..
                } if *h == host && *p == port && *refused == digest => Some(error.clone()),
                _ => None,
            })
    }

    /// Forget that `file` refused `account` and `password`: the account's
    /// password was just changed to it (File > Change Password…, #830).
    pub fn admitted(&mut self, file: &FileKey, account: &str, password: &str) {
        self.refused
            .remove(&(file.clone(), digest(account, password)));
    }

    /// Remember that `file`'s host refused the Login of `account` and
    /// `password` with `error`. A host holding back the client's address
    /// (214) judged no credentials, so that refusal is not remembered.
    pub fn record(&mut self, file: FileKey, account: &str, password: &str, error: &DataError) {
        if error.code == Some(i32::from(om_server::LOGIN_TOO_MANY_ATTEMPTS)) {
            return;
        }
        self.refused
            .insert((file, digest(account, password)), error.clone());
    }
}
