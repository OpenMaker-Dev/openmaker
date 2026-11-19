//! Optional authenticated-file security metadata (session-identity.md 1-3).
//!
//! Request the account's reference leaf directly: downloading its parent
//! could also return authentication material. No security download enters
//! the schema/page caches or diagnostics. Zeroizing guards cover malformed,
//! refused and successful replies, including unexpected slot streams.

use om_data::host::{security, Privileges};
use om_data::{CloseRequests, DataError};
use om_hbam::KeyTree;
use om_protocol::{DownloadReply, NodeState};
use om_session::Session;
use zeroize::{Zeroize, Zeroizing};

use crate::{download_tree, validate_download, Download};

#[derive(Default)]
pub(crate) struct CatalogPrivileges {
    pub principal: Option<Principal>,
    pub account: Option<Privileges>,
    pub access: Option<security::SetAccess>,
    pub full_access: Option<Privileges>,
}

/// Opaque identities from this Login and its own account's assigned-set leaf.
/// Names and another account's catalog entries cannot substitute for these.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Principal {
    account: Vec<u8>,
    set: Vec<u8>,
}

impl Principal {
    pub(crate) fn is_full_access(&self) -> bool {
        self.set == [1]
    }
}

pub(crate) fn read_current_account(
    session: &mut Session,
    result_key: &[u8],
    cancel: &CloseRequests,
) -> CatalogPrivileges {
    read_with(result_key, &mut |path| {
        // Related-file hydration must stop before its next network operation
        // when Close is waiting, including optional metadata reads.
        if cancel.pending() {
            return Err(DataError::new("Security metadata unavailable."));
        }
        session
            .download(path)
            .map_err(|_| DataError::new("Security metadata unavailable."))
    })
}

/// Keep a node's borrowed decode inside the zeroizing download's lifetime.
struct PrivateDownload(Download);

impl Drop for PrivateDownload {
    fn drop(&mut self) {
        self.0.slots.zeroize();
    }
}

type Fetch<'a> = dyn FnMut(&[&[u8]]) -> Result<DownloadReply, DataError> + 'a;

fn private_node(fetch: &mut Fetch<'_>, path: &[&[u8]]) -> Option<PrivateDownload> {
    let mut reply = fetch(path).ok()?;
    let value = Zeroizing::new(std::mem::take(&mut reply.value));
    let data = PrivateDownload(Download {
        path: path.iter().map(|s| s.to_vec()).collect(),
        slots: std::mem::take(&mut reply.slots),
    });
    if reply.lock_error != 0 || reply.node_state != NodeState::NonTerminal || !value.is_empty() {
        return None;
    }
    validate_download(&data.0).ok()?;
    Some(data)
}

fn read_set(
    fetch: &mut Fetch<'_>,
    key: &[u8],
    extended: Option<&om_hbam::Node<'_>>,
) -> Option<(Privileges, Option<security::SetAccess>)> {
    let path = [&[23][..], &[2], &[5], key];
    let data = private_node(fetch, &path)?;
    let tree = download_tree(std::slice::from_ref(&data.0)).ok()?;
    let set = tree.node(&path)?;
    Some((
        security::privilege_set(set, extended)?,
        security::set_access(set).ok(),
    ))
}

/// Erase a reply in place, so even unexpected node data has a guarded
/// lifetime. Retaining the slices' lengths also lets tests inspect erasure.
struct AccountReply<'a>(&'a mut DownloadReply);

impl Drop for AccountReply<'_> {
    fn drop(&mut self) {
        self.0.value.as_mut_slice().zeroize();
        self.0.slots.as_mut_slice().zeroize();
    }
}

/// Extract just the framed set key and erase every returned byte before
/// releasing the reply, on success, refusal and malformed data alike.
fn account_reference(reply: &mut DownloadReply) -> Option<Vec<u8>> {
    let reply = AccountReply(reply);
    (reply.0.lock_error == 0
        && reply.0.node_state == NodeState::Terminal
        && reply.0.slots.is_empty())
    .then(|| security::account_key(&reply.0.value).map(<[u8]>::to_vec))
    .flatten()
}

fn read_with(result_key: &[u8], fetch: &mut Fetch<'_>) -> CatalogPrivileges {
    let Some(account) = security::account_key(result_key) else {
        return CatalogPrivileges::default();
    };
    let reference = fetch(&[&[23], &[1], &[5], account, &[0x0b]])
        .ok()
        .and_then(|mut reply| account_reference(&mut reply));
    let extended_path = [&[23][..], &[3], &[5]];
    let extended = private_node(fetch, &extended_path);
    let tree: Option<KeyTree<'_>> = extended
        .as_ref()
        .and_then(|data| download_tree(std::slice::from_ref(&data.0)).ok());
    let members = tree.as_ref().and_then(|t| t.node(&extended_path));
    let principal = reference.as_ref().map(|set| Principal {
        account: account.to_vec(),
        set: set.clone(),
    });
    let (account, access) = reference
        .as_ref()
        .and_then(|key| read_set(fetch, key, members))
        .map(|(account, access)| (Some(account), access))
        .unwrap_or_default();
    // Built-in set key 1 is [Full Access] (session-identity.md 2).
    // A missing or differently named member does not become that set.
    let full_access = if reference.as_deref() == Some(&[1]) {
        account.clone()
    } else {
        read_set(fetch, &[1], members).map(|(set, _)| set)
    }
    .filter(|set| set.account_set_name == "[Full Access]");
    CatalogPrivileges {
        principal,
        access,
        account,
        full_access,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_hbam::slot::{encode_chunk, minimal_code, BEGIN, POP};
    use om_protocol::Resume;

    fn set_reply() -> DownloadReply {
        let name = om_scsu::encode_scsu("Office");
        let mut slots = vec![BEGIN];
        slots.extend(
            encode_chunk(
                minimal_code(false, 1, Some(name.len())).unwrap(),
                &[0x10],
                &name,
            )
            .unwrap(),
        );
        slots.push(POP);
        DownloadReply::node(slots, Resume::complete())
    }

    #[test]
    fn account_reply_buffers_are_erased_on_success_refusal_and_malformed_nodes() {
        let mut reply = DownloadReply::terminal(vec![2, 0x80, 0x84]);
        assert_eq!(account_reference(&mut reply), Some(vec![0x80, 0x84]));
        assert_eq!(reply.value, [0, 0, 0]);
        for (state, lock_error) in [
            (NodeState::NonTerminal, 0),
            (NodeState::Terminal, 9),
            (NodeState::Terminal, 0),
        ] {
            let mut reply = DownloadReply::terminal(b"synthetic-authentication-material".to_vec());
            reply.slots = b"unexpected-sensitive-node-data".to_vec();
            reply.node_state = state;
            reply.lock_error = lock_error;
            assert!(account_reference(&mut reply).is_none());
            assert!(reply.value.iter().all(|byte| *byte == 0));
            assert!(reply.slots.iter().all(|byte| *byte == 0));
        }
    }

    #[test]
    fn sensitive_account_parents_are_never_requested_and_optional_failures_keep_known_names() {
        let mut paths = Vec::new();
        let mut fetch = |path: &[&[u8]]| {
            paths.push(path.iter().map(|s| s.to_vec()).collect::<Vec<_>>());
            match path {
                [a, b, c, account, leaf]
                    if *a == [23]
                        && *b == [1]
                        && *c == [5]
                        && *account == [0x80, 0x81]
                        && *leaf == [0x0b] =>
                {
                    Ok(DownloadReply::terminal(vec![2, 0x80, 0x84]))
                }
                [a, b, c, set] if *a == [23] && *b == [2] && *c == [5] && *set == [0x80, 0x84] => {
                    Ok(set_reply())
                }
                _ => Err(DataError::new("refused optional metadata")),
            }
        };
        let got = read_with(&[2, 0x80, 0x81], &mut fetch);
        let account = got.account.unwrap();
        assert_eq!(account.account_set_name, "Office");
        assert_eq!(account.record_access, None);
        assert!(account.extended.is_empty());
        assert!(got.full_access.is_none());
        assert!(paths
            .iter()
            .filter(|p| p.get(1) == Some(&vec![1]))
            .all(|p| p.len() == 5 && p.last() == Some(&vec![0x0b])));

        for reference in [vec![1, 3, 4], vec![], vec![0]] {
            let got = read_with(&[2, 0x80, 0x81], &mut |path| {
                if path.len() == 5 {
                    Ok(DownloadReply::terminal(reference.clone()))
                } else {
                    Err(DataError::new("refused"))
                }
            });
            assert!(got.account.is_none());
        }
        let got = read_with(&[1, 3], &mut |_| Err(DataError::new("refused")));
        assert!(got.account.is_none());
    }
}
