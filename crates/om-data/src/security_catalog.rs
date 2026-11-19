//! A file's security catalog as Manage Security and the `get_security`
//! command show it ([`crate::DataProvider::security_catalog`],
//! docs/desktop-client.md, docs/authoring.md): its accounts, privilege sets
//! and extended privileges, decoded by the host from `[23]`
//! (docs/re/local-files.md 2, docs/re/session-identity.md).
//!
//! It holds only decoded facts: no password, password verifier or other
//! credential, which the host never sends. A fact the host could not read is
//! `None`. Nothing here can be changed: [`EDITING_UNSUPPORTED`] says why.

/// Why accounts and privilege sets cannot be created, changed or deleted.
pub const EDITING_UNSUPPORTED: &str = "OpenMaker cannot change accounts or privilege sets yet. \
     How FileMaker writes an account (its record's 16-byte member 04 and a non-empty \
     password's hash) is not specified (issue #148), and most of a privilege set's members are \
     not decoded (issue #137).";

/// The accounts, privilege sets and extended privileges of a file, each in
/// the catalog's key order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SecurityCatalog {
    pub accounts: Vec<SecurityAccount>,
    pub privilege_sets: Vec<PrivilegeSetInfo>,
    pub extended_privileges: Vec<ExtendedPrivilegeInfo>,
}

/// An account (`[23, 1, 5, A]`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecurityAccount {
    /// Its record key. Record `01` is [Guest].
    pub key: Vec<u8>,
    /// Its name; `None` when it is not text, as [Guest]'s localized name
    /// is not.
    pub name: Option<String>,
    /// Its kind; `None` when it cannot be read.
    pub kind: Option<AccountKind>,
    /// Whether it is active; `None` when its status cannot be read.
    pub active: Option<bool>,
    /// Whether its password must be changed at its next login; `None`
    /// when its status cannot be read.
    pub must_change_password: Option<bool>,
    /// The record key of its privilege set; `None` when it cannot be read.
    pub privilege_set: Option<Vec<u8>>,
}

impl SecurityAccount {
    /// Whether it is [Guest] (record `01`, docs/re/local-files.md 2.2).
    pub fn is_guest(&self) -> bool {
        self.key == [1]
    }

    /// The name to show: its own, "[Guest]" for record `01` without one.
    pub fn display_name(&self) -> Option<&str> {
        match &self.name {
            Some(name) => Some(name),
            None if self.is_guest() => Some("[Guest]"),
            None => None,
        }
    }
}

/// How an account is authenticated (its kind `02`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountKind {
    /// 0: an account of the file itself, whose password the file stores.
    FileMakerFile,
    /// Any other value: another service authenticates it (an external
    /// server, OAuth or Claris ID). Which value is which is not observed.
    Other(u8),
}

impl AccountKind {
    pub fn from_byte(kind: u8) -> Self {
        match kind {
            0 => AccountKind::FileMakerFile,
            other => AccountKind::Other(other),
        }
    }
}

/// A privilege set (`[23, 2, 5, P]`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivilegeSetInfo {
    /// Its record key. Keys 1, 2 and 3 are the built-in sets in every
    /// observed file.
    pub key: Vec<u8>,
    /// Its name; `None` when it is not text.
    pub name: Option<String>,
    /// Its record privilege flags `02` as stored; `None` when they cannot
    /// be read.
    pub record_flags: Option<Vec<u8>>,
    /// How many days a password lasts under it, 0 for no limit; `None`
    /// when it cannot be read.
    pub password_days: Option<u16>,
}

impl PrivilegeSetInfo {
    /// What its record flags allow in all tables, for the flag values
    /// observed (docs/re/session-identity.md 2); any other value, and
    /// record-specific rules, are [`RecordAccess::Unknown`].
    pub fn record_access(&self) -> RecordAccess {
        match self.record_flags.as_deref().and_then(<[u8]>::first) {
            Some(0x55) => RecordAccess::CreateEditDelete,
            Some(0x54) => RecordAccess::CreateEdit,
            Some(0x10) => RecordAccess::ViewOnly,
            _ => RecordAccess::Unknown,
        }
    }
}

/// What a privilege set's record flags allow in all tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordAccess {
    /// `55`: create, edit and delete.
    CreateEditDelete,
    /// `54`: create and edit, without delete.
    CreateEdit,
    /// `10`: view only.
    ViewOnly,
    /// A value not observed, or flags that cannot be read.
    Unknown,
}

impl RecordAccess {
    pub fn label(self) -> &'static str {
        match self {
            RecordAccess::CreateEditDelete => "Create, edit and delete in all tables",
            RecordAccess::CreateEdit => "Create and edit in all tables",
            RecordAccess::ViewOnly => "View only in all tables",
            RecordAccess::Unknown => "Not decoded",
        }
    }
}

/// An extended privilege (`[23, 3, 5, E]`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtendedPrivilegeInfo {
    /// Its record key.
    pub key: Vec<u8>,
    /// Its keyword, such as `fmapp`; `None` when it is not text.
    pub keyword: Option<String>,
    /// The record keys of the privilege sets that have it.
    pub sets: Vec<Vec<u8>>,
}

impl SecurityCatalog {
    /// The privilege set with record key `key`.
    pub fn set(&self, key: &[u8]) -> Option<&PrivilegeSetInfo> {
        self.privilege_sets.iter().find(|set| set.key == key)
    }

    /// The accounts in the privilege set `key`.
    pub fn accounts_in<'a>(
        &'a self,
        key: &'a [u8],
    ) -> impl Iterator<Item = &'a SecurityAccount> + 'a {
        self.accounts
            .iter()
            .filter(move |account| account.privilege_set.as_deref() == Some(key))
    }

    /// The extended privileges the privilege set `key` has.
    pub fn extended_of<'a>(
        &'a self,
        key: &'a [u8],
    ) -> impl Iterator<Item = &'a ExtendedPrivilegeInfo> + 'a {
        self.extended_privileges
            .iter()
            .filter(move |privilege| privilege.sets.iter().any(|set| set == key))
    }
}
