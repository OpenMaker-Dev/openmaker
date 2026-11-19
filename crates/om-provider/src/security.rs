//! Manage Security's read (`docs/authoring.md`, Describe Security): the
//! open file's accounts, privilege sets and extended privileges, decoded by
//! the host. The host sends only decoded facts, never an account's password
//! verifier or other credential, and only to an account of the file in its
//! built-in [Full Access] set (`om-server`'s `security.rs`). Nothing is
//! downloaded from the security catalog here. Only OpenMaker Server offers
//! the operation; any other host is sent nothing.

use om_data::security_catalog::{
    AccountKind, ExtendedPrivilegeInfo, PrivilegeSetInfo, SecurityAccount, SecurityCatalog,
};
use om_protocol::authoring::{self, Reply, Request, OP_DESCRIBE_SECURITY};

use crate::authoring::AuthoringError;
use crate::{failure, NativeProvider};

impl NativeProvider {
    /// The open file's security catalog, as the host decodes it.
    pub fn security(&mut self) -> Result<SecurityCatalog, AuthoringError> {
        self.require_open()
            .map_err(|e| AuthoringError::NotSent(e.message))?;
        let claimed = self
            .session
            .as_mut()
            .expect("open session")
            .is_a(authoring::REPO_ID)
            .map_err(|e| {
                AuthoringError::NotSent(failure("ask the host for security", e).message)
            })?;
        if !claimed {
            return Err(AuthoringError::Unavailable(format!(
                "The host (“{}”) does not offer OpenMaker's authoring extension. OpenMaker reads \
                 a file's security only through it, so that no account record leaves the host.",
                self.host_version
            )));
        }
        match self.authoring_request(&Request::Describe)? {
            Reply::Described(d) if d.operations & OP_DESCRIBE_SECURITY != 0 => {}
            Reply::Refused {
                status, message, ..
            } => {
                return Err(AuthoringError::Refused {
                    status,
                    message,
                    current: None,
                })
            }
            _ => {
                return Err(AuthoringError::Unavailable(
                    "This OpenMaker Server does not offer reading a file's security.".into(),
                ))
            }
        }
        match self.authoring_request(&Request::DescribeSecurity)? {
            Reply::Security(catalog) => Ok(convert(catalog)),
            Reply::Refused {
                status, message, ..
            } => Err(AuthoringError::Refused {
                status,
                message,
                current: None,
            }),
            Reply::NotSupported { status } => Err(AuthoringError::Refused {
                status,
                message: "The host does not implement reading a file's security.".into(),
                current: None,
            }),
            Reply::Described(_)
            | Reply::Replaced(_)
            | Reply::Created(_)
            | Reply::Placed(_)
            | Reply::LayoutCreated(_)
            | Reply::Framed(_)
            | Reply::Deleted(_)
            | Reply::Saved => Err(AuthoringError::NotSent(
                "The host answered with something else.".into(),
            )),
        }
    }
}

fn convert(catalog: authoring::SecurityCatalog) -> SecurityCatalog {
    SecurityCatalog {
        accounts: catalog
            .accounts
            .into_iter()
            .map(|account| SecurityAccount {
                key: account.key,
                name: account.name,
                kind: account.kind.map(AccountKind::from_byte),
                active: account.active,
                must_change_password: account.must_change_password,
                privilege_set: account.privilege_set,
            })
            .collect(),
        privilege_sets: catalog
            .privilege_sets
            .into_iter()
            .map(|set| PrivilegeSetInfo {
                key: set.key,
                name: set.name,
                record_flags: set.record_flags,
                password_days: set.password_days,
            })
            .collect(),
        extended_privileges: catalog
            .extended_privileges
            .into_iter()
            .map(|privilege| ExtendedPrivilegeInfo {
                key: privilege.key,
                keyword: privilege.keyword,
                sets: privilege.sets,
            })
            .collect(),
    }
}
