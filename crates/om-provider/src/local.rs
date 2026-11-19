//! The files on this computer one window's files have open together
//! (docs/re/local-files.md 4, "Related files and other files"): the file
//! the user opened, the files its scripts open (`open_peer`) and the
//! related files occurrences show (external.rs). Each file has one private
//! host (`om_server::LocalHost`: one engine, one lock, one port), which
//! every session of the family that has the file open shares, each through
//! a [`Lease`] of its own and a login of its own. The last lease to go
//! closes the host and releases the file; closing one session never stops
//! another's.
//!
//! A family is the providers sharing one [`LocalHosts`]: a provider made
//! with `NativeProvider::new` starts one, and the peers and related files
//! it opens join it. Another family (another window, another process) finds
//! a file open here in use (300), as FileMaker Pro finds a file another
//! copy of FileMaker has open.
//!
//! Files are told apart by their canonical path (links followed): two
//! names for one file are one file, opened once. Two different files with
//! the same name are never open in one family at once: the second is
//! refused (802), so a file name names one file to calculations, scripts
//! and globals [I: FileMaker Pro opens one file of a name at a time].

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use om_data::file_refs::hosted_key;
use om_data::DataError;
use om_server::{LocalHost, LocalHostError};

/// The hosts of one family's files, each as long as a lease has it.
#[derive(Default)]
pub(crate) struct LocalHosts {
    open: Mutex<Vec<Weak<LocalHost>>>,
}

impl LocalHosts {
    fn lock(&self) -> MutexGuard<'_, Vec<Weak<LocalHost>>> {
        self.open.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A lease of the file at `path`: of its host when the family has the
    /// file open already, else of a new host serving it to the client
    /// whose certificate is `client_certificate_pem`. Nothing logs in.
    pub fn acquire(
        self: &Arc<Self>,
        path: &Path,
        client_certificate_pem: &[u8],
    ) -> Result<Lease, DataError> {
        let canonical = std::fs::canonicalize(path).map_err(|e| {
            let error = match e.kind() {
                std::io::ErrorKind::NotFound => LocalHostError::Missing(path.to_path_buf()),
                _ => LocalHostError::Unreadable(e),
            };
            open_error(path, &error)
        })?;
        let mut open = self.lock();
        open.retain(|host| host.strong_count() > 0);
        let name = canonical
            .file_name()
            .map(|n| hosted_key(&n.to_string_lossy()));
        for host in open.iter().filter_map(Weak::upgrade) {
            if host.path() == canonical {
                return Ok(Lease::new(host, self));
            }
            if name.as_deref() == Some(&hosted_key(host.file_name())) {
                return Err(DataError::with_code(
                    802,
                    format!(
                        "“{}” could not be opened: another file of that name is open from {}.",
                        host.file_name(),
                        host.path().parent().unwrap_or(host.path()).display()
                    ),
                ));
            }
        }
        let host = LocalHost::open(&canonical, client_certificate_pem)
            .map_err(|e| open_error(path, &e))?;
        let host = Arc::new(host);
        open.push(Arc::downgrade(&host));
        Ok(Lease::new(host, self))
    }

    /// How many files the family has open (tests).
    #[cfg(test)]
    fn len(&self) -> usize {
        self.lock()
            .iter()
            .filter(|host| host.strong_count() > 0)
            .count()
    }
}

/// FileMaker's code and a message for a file that did not open.
pub(crate) fn open_error(path: &Path, error: &LocalHostError) -> DataError {
    DataError::with_code(
        error.code(),
        format!(
            "“{}” could not be opened: {error}.",
            path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned()
            )
        ),
    )
}

/// One session's hold on a file's host ([`LocalHosts::acquire`]). It
/// dereferences to the host; the last lease of a host closes it.
pub(crate) struct Lease {
    host: Option<Arc<LocalHost>>,
    hosts: Arc<LocalHosts>,
}

impl Lease {
    fn new(host: Arc<LocalHost>, hosts: &Arc<LocalHosts>) -> Self {
        Self {
            host: Some(host),
            hosts: Arc::clone(hosts),
        }
    }

    /// Give the lease up. `false` when it was the host's last and the host
    /// could not release the file in time (a script that did not stop):
    /// that is logged, and the file stays in use until it lets go.
    pub fn release(mut self) -> bool {
        self.give_up()
    }

    fn give_up(&mut self) -> bool {
        let Some(host) = self.host.take() else {
            return true;
        };
        // Under the family's lock: a lease of the same file taken meanwhile
        // waits for this host to be gone rather than finding it in use.
        let mut open = self.hosts.lock();
        let host = match Arc::try_unwrap(host) {
            Ok(host) => host,
            // Another session still has the file.
            Err(shared) => {
                drop(shared);
                return true;
            }
        };
        open.retain(|host| host.strong_count() > 0);
        let path = host.path().to_path_buf();
        let released = host.close();
        drop(open);
        if !released {
            log::error!(
                "{}: not released when it closed; it stays in use until its last task ends",
                path.display()
            );
        }
        released
    }
}

impl Clone for Lease {
    fn clone(&self) -> Self {
        Self {
            host: self.host.clone(),
            hosts: Arc::clone(&self.hosts),
        }
    }
}

impl std::ops::Deref for Lease {
    type Target = LocalHost;

    fn deref(&self) -> &LocalHost {
        self.host
            .as_deref()
            .expect("a lease has its host until dropped")
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.give_up();
    }
}

impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Lease").field(&self.host).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_is_refused_with_100_and_holds_nothing() {
        let hosts = Arc::new(LocalHosts::default());
        let missing = std::env::temp_dir().join(format!(
            "om-local-hosts-missing-{}/Peer.fmp12",
            std::process::id()
        ));
        let error = hosts.acquire(&missing, b"").unwrap_err();
        assert_eq!(error.code, Some(100), "{error:?}");
        assert!(error.message.contains("Peer.fmp12"), "{}", error.message);
        assert_eq!(hosts.len(), 0);
    }
}
