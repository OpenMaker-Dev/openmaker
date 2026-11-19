//! The open file's value-list catalog (`om_data::value_lists`), for
//! File > Manage > Value Lists and the authoring service's
//! `get_value_lists`. It is read with the download every catalog uses
//! (op90 of `[21]`), from a host or from a local file's private host alike,
//! and decoded fresh each time; nothing is cached or changed. Field and
//! occurrence names come from the table catalogs read at open.

use om_data::value_lists::{self, ValueListCatalog};
use om_data::DataError;

use crate::{download_node, download_tree, key_bytes, NativeProvider};

impl NativeProvider {
    pub(crate) fn read_value_list_catalog(&mut self) -> Result<ValueListCatalog, DataError> {
        self.require_open()?;
        let catalog = om_data::meta::VALUE_LISTS[0];
        // A file that never had a value list has no catalog for them; a
        // failed download is an error, never an empty catalog.
        let Some(download) = download_node(self.session_mut()?, vec![key_bytes(catalog)])? else {
            return Ok(ValueListCatalog::empty());
        };
        let tree = download_tree(std::slice::from_ref(&download))?;
        let mut catalog = value_lists::read(&tree, &self.catalog);
        // A list the account's custom set gives no access is left out
        // (session-identity.md 2.2).
        catalog.lists.retain(|list| self.shows_value_list(list.id));
        Ok(catalog)
    }
}
