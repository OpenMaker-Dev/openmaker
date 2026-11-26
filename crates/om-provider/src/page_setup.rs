//! A file's page setup `[2, 87]` (file-format.md 3.8). FileMaker Pro
//! writes the page setup accepted in Page Setup to a file on this computer,
//! at the OK, and sends nothing for a hosted file, which keeps the setup
//! while it is open in that client.
use om_data::DataError;
use om_hbam::OwnedNode;
use om_protocol::write::{LockReply, LOCK_WRITE};

use crate::{download_tree, failure, node_from_reply, NativeProvider};

const LOCK_EXCLUSIVE: u16 = 10;
/// No other client is told of the change: FileMaker Pro reads a file's
/// page setup when it opens it [I].
const ATTENTION_NONE: u32 = 0;

impl NativeProvider {
    /// [`om_data::DataProvider::store_page_format`] for this provider.
    pub(crate) fn store_file_page_format(&mut self, value: &[u8]) -> Result<bool, DataError> {
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
        let node = om_data::meta::page_format_node(value)
            .ok_or_else(|| DataError::with_code(3, "The page setup is too large to save."))?;
        let path: [&[u8]; 2] = [&[2], &[0x87]];
        let txn = self.new_transaction();
        let session = self.session.as_mut().expect("open session");
        match session.lock(&path, txn, false) {
            Ok(LockReply::Granted { .. }) => {}
            Ok(_) => {
                return Err(DataError::with_code(
                    301,
                    "The file's page setup is in use; it was not saved.",
                ))
            }
            Err(e) => return Err(failure("lock the page setup", e)),
        }
        let mut held = LOCK_WRITE;
        let stored = (|| {
            match session.upgrade_lock(&path, txn, LOCK_WRITE, LOCK_EXCLUSIVE) {
                Ok(LockReply::Granted { .. }) => held = LOCK_EXCLUSIVE,
                Ok(_) => {
                    return Err(DataError::with_code(
                        301,
                        "The file's page setup is in use; it was not saved.",
                    ))
                }
                Err(e) => return Err(failure("lock the page setup", e)),
            }
            session
                .upload_to_host(&path, &node)
                .map_err(|e| failure("The page setup may not have been saved", e))?;
            // op13's reply does not say what was stored (schema-path.md
            // 3.3): it is read back.
            let read = stored_node(session, &path)?;
            if read.as_ref().and_then(OwnedNode::assembled).as_deref() != Some(value) {
                return Err(DataError::new(
                    "The page setup read back from the file is not the one saved.",
                ));
            }
            Ok(true)
        })();
        let _ = session.unlock(&path, txn, held, ATTENTION_NONE);
        stored
    }
}

/// The node at `path` in the open file, `None` when it has none.
fn stored_node(
    session: &mut om_session::Session,
    path: &[&[u8]],
) -> Result<Option<OwnedNode>, DataError> {
    let reply = session
        .download_from_host(path)
        .map_err(|e| failure("read the page setup back", e))?;
    let Some(download) = node_from_reply(path.iter().map(|key| key.to_vec()).collect(), reply)?
    else {
        return Ok(None);
    };
    let tree = download_tree(std::slice::from_ref(&download))?;
    Ok(tree.node(path).map(|node| OwnedNode {
        push: true,
        ..node.to_owned_node()
    }))
}
