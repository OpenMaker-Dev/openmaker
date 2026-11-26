//! A file's page setup `[2, 87]` (file-format.md 3.8). FileMaker Pro
//! writes the page setup accepted in Page Setup to a file on this computer,
//! at the OK, and sends nothing for a hosted file, which keeps the setup
//! while it is open in that client.
use om_data::meta::{self, StoredPageFormat};
use om_data::DataError;
use om_protocol::write::{LockReply, LOCK_WRITE};

use crate::{download_tree, failure, node_from_reply, NativeProvider};

const LOCK_EXCLUSIVE: u16 = 10;
/// No other client is told of the change: FileMaker Pro reads a file's
/// page setup when it opens it [I].
const ATTENTION_NONE: u32 = 0;
const KEPT: &str = "The file's page setup was kept";

impl NativeProvider {
    /// [`om_data::DataProvider::store_page_format`] for this provider.
    pub(crate) fn store_file_page_format(
        &mut self,
        expected: Option<&[u8]>,
        value: &[u8],
    ) -> Result<bool, DataError> {
        // A hosted file's page setup is the client's for the session, as
        // FileMaker Pro keeps it: nothing is sent (write-path.md 5.9).
        if self.local.is_none() {
            return Ok(false);
        }
        self.require_open()?;
        if self.read_only() {
            return Err(DataError::with_code(
                6,
                "This file is read-only: its page setup cannot be saved.",
            ));
        }
        // The in-process host stores a direct upload only for [Full
        // Access] (as op240). Whether FileMaker Pro saves the page setup of
        // an account without it is unmeasured.
        if !self
            .privileges
            .principal
            .as_ref()
            .is_some_and(|principal| principal.is_full_access())
        {
            return Err(DataError::with_code(
                9,
                "OpenMaker saves a page setup in the file only for an account with \
                 [Full Access]; it is kept while the file is open.",
            ));
        }
        let node = meta::page_format_node(value)
            .ok_or_else(|| DataError::with_code(3, "The page setup is too large to save."))?;
        let path: [&[u8]; 2] = [&[2], &[0x87]];
        let txn = self.new_transaction();
        let session = self.session.as_mut().expect("open session");
        match session.lock(&path, txn, false) {
            Ok(LockReply::Granted { .. }) => {}
            Ok(_) => return Err(in_use()),
            Err(e) => {
                // The host may have granted a lock whose answer was not read.
                let _ = session.kill_all_locks(txn);
                return Err(failure("lock the page setup", e));
            }
        }
        let mut held = LOCK_WRITE;
        let stored = (|| {
            match session.upgrade_lock(&path, txn, LOCK_WRITE, LOCK_EXCLUSIVE) {
                Ok(LockReply::Granted { .. }) => held = LOCK_EXCLUSIVE,
                Ok(_) => return Err(in_use()),
                Err(e) => return Err(failure("lock the page setup", e)),
            }
            // What the file stores now, under the lock: only what the
            // caller decided on, stored as Pro stores it, and only records
            // FileMaker Pro makes again from `Comm`, are replaced (AGENTS.md
            // rule 5).
            match current(session, &path)? {
                StoredPageFormat::Unreadable => {
                    return Err(DataError::with_code(
                        3,
                        format!("{KEPT}: OpenMaker cannot read it."),
                    ))
                }
                StoredPageFormat::Readable(now) if !meta::replaceable_page_format(&now) => {
                    return Err(DataError::with_code(
                        3,
                        format!("{KEPT}: it holds records OpenMaker would not keep."),
                    ))
                }
                StoredPageFormat::Readable(now) if Some(now.as_slice()) == expected => {}
                StoredPageFormat::None if expected.is_none() => {}
                _ => {
                    return Err(DataError::with_code(
                        301,
                        format!("{KEPT}: it changed since it was read."),
                    ))
                }
            }
            session
                .upload_to_host(&path, &node)
                .map_err(|e| failure("The page setup may not have been saved", e))?;
            // op13's reply does not say what was stored (schema-path.md
            // 3.3): it is read back.
            if current(session, &path)? != StoredPageFormat::Readable(value.to_vec()) {
                return Err(DataError::new(
                    "The page setup read back from the file is not the one saved.",
                ));
            }
            Ok(true)
        })();
        // After a failure the lock's type may be either (an upgrade whose
        // answer was not read): the transaction's locks all go then, as a
        // script save's do.
        let released = session.unlock(&path, txn, held, ATTENTION_NONE);
        if released.is_err() || stored.is_err() {
            let killed = session.kill_all_locks(txn);
            if let (Ok(true), Err(_), Err(error)) = (&stored, &released, killed) {
                // Saved and read back: the caller is told so, or its next
                // save would expect what the file no longer stores.
                log::warn!("The page setup was saved, but its lock could not be released: {error}");
            }
        }
        stored
    }
}

fn in_use() -> DataError {
    DataError::with_code(301, format!("{KEPT}: it is in use."))
}

/// What the open file stores at `path`, the page setup key.
fn current(
    session: &mut om_session::Session,
    path: &[&[u8]],
) -> Result<StoredPageFormat, DataError> {
    let reply = session
        .download_from_host(path)
        .map_err(|e| failure("read the page setup", e))?;
    let Some(download) = node_from_reply(path.iter().map(|key| key.to_vec()).collect(), reply)?
    else {
        return Ok(StoredPageFormat::None);
    };
    let tree = download_tree(std::slice::from_ref(&download))?;
    Ok(meta::stored_page_format(&tree))
}
