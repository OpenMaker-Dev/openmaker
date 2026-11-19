//! Temporary library acquisitions carried independently of record submission.
//! Numeric member ids and MD5s are insufficient: both native unique ids and
//! every immutable member byte must survive before ownership can transfer.
use super::*;
use om_hbam::{KeySegment, OwnedNode, SlotReader};

type Key = (u64, Vec<u8>);
pub(crate) type Proofs = BTreeMap<Key, Option<Fingerprint>>;
pub(crate) type Pending = BTreeMap<u32, BTreeMap<Key, i32>>;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Fingerprint {
    library: Vec<u8>,
    body: OwnedNode,
}

#[derive(Clone)]
pub(crate) struct Asset {
    table: u64,
    member: Vec<u8>,
    proof: Fingerprint,
    owned: i32,
    // Set before sending a decrement. A lost reply never permits a retry.
    unknown: bool,
    released: bool,
}

fn unresolved() -> DataError {
    DataError::with_code(3, "Unresolved: temporary container ownership could not be proved. Drafts and retained container bytes are kept.")
}

impl NativeProvider {
    pub(crate) fn acquired_asset(&self, table: u64, member: &[u8], count: i32) -> Option<Asset> {
        Some(Asset {
            table,
            member: member.to_vec(),
            proof: self.asset_proofs.get(&(table, member.to_vec()))?.clone()?,
            owned: count,
            unknown: false,
            released: false,
        })
    }
    fn asset_node(&mut self, path: &[Vec<u8>]) -> Result<OwnedNode, DataError> {
        let refs: Vec<_> = path.iter().map(Vec::as_slice).collect();
        let reply = self
            .session_mut()?
            .download(&refs)
            .map_err(|e| failure("revalidate a temporary container", e))?;
        if reply.lock_error != 0 {
            return Err(unresolved());
        }
        if reply.node_state == om_protocol::NodeState::Terminal {
            return Ok(OwnedNode::leaf(
                path.last().cloned().ok_or_else(unresolved)?,
                reply.value,
            ));
        }
        let tree = KeyTree::from_slots(SlotReader::with_path(
            &reply.slots,
            path.iter().map(|p| KeySegment(p)).collect(),
        ))
        .map_err(|_| unresolved())?;
        tree.node(&refs)
            .map(|n| n.to_owned_node())
            .ok_or_else(unresolved)
    }

    pub(crate) fn asset_fingerprint(
        &mut self,
        table: u64,
        member: &[u8],
    ) -> Result<(Fingerprint, u32), DataError> {
        let library = self
            .asset_node(&[key_bytes(table), vec![0x1f], vec![1], vec![0xd8]])?
            .value
            .filter(|v| v.len() == 16)
            .ok_or_else(unresolved)?;
        let mut body =
            self.asset_node(&[key_bytes(table), vec![0x1f], vec![5], member.to_vec()])?;
        if body
            .child(&[0xd8])
            .and_then(|n| n.value.as_ref())
            .is_none_or(|v| v.len() != 16)
        {
            return Err(unresolved());
        }
        let count = body
            .child(&[0x12])
            .and_then(|n| n.value.as_deref())
            .and_then(|v| <[u8; 4]>::try_from(v).ok())
            .map(u32::from_be_bytes)
            .ok_or_else(unresolved)?;
        body.children
            .retain(|n| !matches!(n.segment.as_slice(), [0x12] | [0xfc] | [0xfc, 1..=3]));
        Ok((Fingerprint { library, body }, count))
    }

    /// Before acquisition, distinguish a truly absent table library from an
    /// existing header whose native identity is unreadable or malformed.
    pub(crate) fn attachment_library_identity(
        &mut self,
        table: u64,
    ) -> Result<Option<Vec<u8>>, DataError> {
        let path = [key_bytes(table), vec![0x1f], vec![1]];
        let reply = self
            .session_mut()?
            .download(&path.iter().map(Vec::as_slice).collect::<Vec<_>>())
            .map_err(|_| unresolved())?;
        if reply.lock_error != 0 {
            return Err(unresolved());
        }
        if reply.node_state == om_protocol::NodeState::Missing
            && reply.value.is_empty()
            && reply.slots.is_empty()
        {
            return Ok(None);
        }
        if reply.node_state != om_protocol::NodeState::NonTerminal {
            return Err(unresolved());
        }
        let tree = KeyTree::from_slots(SlotReader::with_path(
            &reply.slots,
            path.iter().map(|p| KeySegment(p)).collect(),
        ))
        .map_err(|_| unresolved())?;
        let id = tree
            .node(&[&path[0], &path[1], &path[2], &[0xd8]])
            .and_then(|n| n.value)
            .filter(|v| v.len() == 16)
            .ok_or_else(unresolved)?;
        Ok(Some(id.to_vec()))
    }

    pub(crate) fn note_asset_acquisition(&mut self, table: u64, member: &[u8]) {
        let proof = self.asset_fingerprint(table, member).ok().map(|(p, _)| p);
        let key = (table, member.to_vec());
        let outstanding = self
            .transaction
            .as_ref()
            .and_then(|t| t.containers.get(&key))
            .copied()
            .unwrap_or_default()
            + self
                .pending_assets
                .values()
                .filter_map(|p| p.get(&key))
                .sum::<i32>();
        if outstanding == 0 {
            self.asset_proofs.insert(key, proof);
        } else if let Some(old) = self.asset_proofs.get(&key) {
            if old != &proof {
                self.asset_proofs.insert(key, None);
            }
        } else {
            self.asset_proofs.insert(key, proof);
        }
    }

    /// A direct write may now store these acquisitions. Remove them from
    /// pending ownership before submission, including an ambiguous reply;
    /// no later transaction may treat a stored reference as its draft.
    pub(crate) fn store_pending_assets(
        &mut self,
        table: &om_hbam::Table,
        values: &BTreeMap<u64, String>,
        cells: Option<&[u64]>,
    ) {
        if let Some(pending) = self.pending_assets.get_mut(&self.window) {
            for member in super::containers::references(table, values, cells) {
                if let Some(count) = pending.get_mut(&(table.id, member)) {
                    *count = (*count - 1).max(0);
                }
            }
            pending.retain(|_, n| *n > 0);
        }
    }

    pub(crate) fn cached_root_assets(&self, detached: bool) -> Result<Vec<Asset>, DataError> {
        let t = self.transaction.as_ref().ok_or_else(unresolved)?;
        let counts: BTreeMap<_, _> = t
            .containers
            .iter()
            .filter(|(_, n)| **n > 0)
            .map(|(k, n)| (k.clone(), *n))
            .collect();
        let acquisitions: BTreeMap<_, _> = t
            .root_acquisitions
            .iter()
            .filter(|(_, n)| **n > 0)
            .map(|(k, n)| (k.clone(), *n))
            .collect();
        if detached && !counts.is_empty() {
            return Err(unresolved());
        }
        let counts = if detached {
            acquisitions.clone()
        } else {
            counts
        };
        let (wire, _) = self.single_root_live_shape().ok_or_else(unresolved)?;
        if counts != acquisitions
            || counts.keys().any(|(table, _)| *table != wire)
            || self
                .held_container_members()
                .iter()
                .any(|k| !counts.contains_key(k))
        {
            return Err(unresolved());
        }
        counts
            .into_iter()
            .map(|((table, member), count)| {
                self.acquired_asset(table, &member, count)
                    .ok_or_else(unresolved)
            })
            .collect()
    }

    pub(crate) fn carry_assets(&mut self) -> Result<Vec<Asset>, DataError> {
        let counts = self
            .transaction
            .as_ref()
            .ok_or_else(unresolved)?
            .containers
            .clone();
        let mut keys = self.held_container_members();
        keys.extend(counts.keys().cloned());
        let mut assets = Vec::new();
        for (table, member) in keys {
            let owned = counts
                .get(&(table, member.clone()))
                .copied()
                .unwrap_or_default();
            let (proof, count) = self.asset_fingerprint(table, &member)?;
            if owned > 0
                && (self.asset_proofs.get(&(table, member.clone())) != Some(&Some(proof.clone()))
                    || count < owned as u32)
            {
                return Err(unresolved());
            }
            assets.push(Asset {
                table,
                member,
                proof,
                owned,
                unknown: false,
                released: false,
            });
        }
        Ok(assets)
    }

    pub(crate) fn assets_match(&self, assets: &[Asset]) -> bool {
        let Some(t) = &self.transaction else {
            return true;
        };
        let counts: BTreeMap<_, _> = assets
            .iter()
            .filter(|a| a.owned > 0)
            .map(|a| ((a.table, a.member.clone()), a.owned))
            .collect();
        let actual: BTreeMap<_, _> = t
            .containers
            .iter()
            .filter(|(_, n)| **n > 0)
            .map(|(k, n)| (k.clone(), *n))
            .collect();
        counts == actual
            && self
                .held_container_members()
                .iter()
                .all(|(t, m)| assets.iter().any(|a| a.table == *t && a.member == *m))
    }

    pub(crate) fn validate_assets(&mut self, assets: &[Asset]) -> Result<(), DataError> {
        for asset in assets {
            if asset.released {
                continue;
            }
            if asset.unknown {
                return Err(unresolved());
            }
            let (proof, count) = self.asset_fingerprint(asset.table, &asset.member)?;
            if proof != asset.proof || count < asset.owned.max(1) as u32 {
                return Err(unresolved());
            }
        }
        Ok(())
    }

    pub(crate) fn detach_assets(&mut self) {
        if let Some(t) = &mut self.transaction {
            t.containers.clear();
        }
    }

    pub(crate) fn detach_carried_assets(&mut self, assets: &[Asset]) {
        if let Some(t) = &mut self.transaction {
            for a in assets {
                if let Some(n) = t.containers.get_mut(&(a.table, a.member.clone())) {
                    *n = (*n - a.owned).max(0);
                }
            }
        }
    }

    pub(crate) fn attach_assets(&mut self, assets: &[Asset]) {
        if let Some(t) = &mut self.transaction {
            for a in assets.iter().filter(|a| a.owned > 0) {
                t.containers.insert((a.table, a.member.clone()), a.owned);
                self.asset_proofs
                    .insert((a.table, a.member.clone()), Some(a.proof.clone()));
            }
        }
    }

    pub(crate) fn transaction_asset_counts(&self) -> BTreeMap<(u64, String), u32> {
        self.transaction
            .as_ref()
            .map(|t| {
                t.containers
                    .iter()
                    .filter(|(_, n)| **n > 0)
                    .map(|((t, m), n)| ((*t, om_data::containers::reference_text(m)), *n as u32))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn asset_counts(assets: &[Asset]) -> BTreeMap<(u64, String), u32> {
        assets
            .iter()
            .filter(|a| a.owned > 0)
            .map(|a| {
                (
                    (a.table, om_data::containers::reference_text(&a.member)),
                    a.owned as u32,
                )
            })
            .collect()
    }

    pub(crate) fn release_carried_assets(&mut self, assets: &mut [Asset]) -> Result<(), DataError> {
        self.validate_assets(assets)?;
        for a in assets {
            let path = vec![key_bytes(a.table), vec![0x1f], vec![5], a.member.clone()];
            a.release_with(|| {
                self.session_mut()?
                    .adjust_reference_count(path.clone(), -1)
                    .map_err(|_| unresolved())
            })?;
        }
        Ok(())
    }
}

impl Asset {
    pub(crate) fn same_library(&self, identity: &[u8]) -> bool {
        self.proof.library == identity
    }

    pub(crate) fn ownership_known(&self) -> bool {
        !self.unknown
    }

    pub(crate) fn ownership(&self) -> ((u64, Vec<u8>), i32) {
        ((self.table, self.member.clone()), self.owned)
    }
    pub(crate) fn add_to(&self, assets: &mut Vec<Self>) {
        if let Some(old) = assets
            .iter_mut()
            .find(|a| a.table == self.table && a.member == self.member)
        {
            if old.proof != self.proof {
                old.unknown = true;
            }
            old.owned = old.owned.saturating_add(self.owned);
            old.released = false;
        } else {
            assets.push(self.clone());
        }
    }
    pub(crate) fn names(&self, table: u64, member: &[u8]) -> bool {
        self.table == table && self.member == member && self.owned > 0
    }

    pub(crate) fn release_started(&mut self) -> Result<(), DataError> {
        if self.unknown {
            return Err(unresolved());
        }
        self.unknown = true;
        Ok(())
    }

    pub(crate) fn release_acknowledged(&mut self, count: i32) {
        self.owned = self.owned.saturating_sub(count).max(0);
        self.released = self.owned == 0;
        self.unknown = false;
    }

    fn release_with(
        &mut self,
        mut release: impl FnMut() -> Result<u32, DataError>,
    ) -> Result<(), DataError> {
        if self.unknown {
            return Err(unresolved());
        }
        while self.owned > 0 {
            self.unknown = true;
            let count = release()?;
            if count == om_protocol::container::COUNT_FAILED {
                return Err(unresolved());
            }
            self.owned -= 1;
            self.unknown = false;
            self.released = self.owned == 0;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lost_release_reply_keeps_bytes_and_never_repeats_the_decrement() {
        let body = OwnedNode::leaf(b"FILE".to_vec(), b"irreplaceable bytes".to_vec());
        let mut asset = Asset {
            table: 0x8001,
            member: vec![1],
            proof: Fingerprint {
                library: vec![7; 16],
                body: body.clone(),
            },
            owned: 2,
            unknown: false,
            released: false,
        };
        let mut sent = 0;
        assert!(asset
            .release_with(|| {
                sent += 1;
                if sent == 1 {
                    Ok(1)
                } else {
                    Err(DataError::new("reply lost after application"))
                }
            })
            .is_err());
        assert_eq!(sent, 2);
        assert_eq!(asset.owned, 1);
        assert!(asset.unknown);
        assert!(asset
            .release_with(|| {
                sent += 1;
                Ok(0)
            })
            .is_err());
        assert_eq!(sent, 2);
        assert_eq!(asset.proof.body, body);
    }
}
