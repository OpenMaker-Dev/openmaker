//! Recomputing stored calculations after a schema change, as FileMaker
//! Server does (schema-path.md 7.2 and "Implementation notes
//! (recalculation)"): which fields' stored values a Manage Database save
//! made stale, and the walk over a table's records that recomputes them
//! with `om_data::rules::recalc`.
//!
//! The server keeps, per table, the definitions its stored values were last
//! computed with (its *basis*: kind, type, storage, repetitions and formula
//! of each field, not names, indexes, auto-entry or validation). When
//! op73 asks for a table's deferred work, the current definitions are
//! compared with the basis: a new, deleted or retyped field, a calculation
//! whose formula, result type or storage changed, and every stored
//! calculation that reads one of them (transitively) is recomputed on every
//! record. A table without a basis (created in this session, or loaded with
//! deferred work pending after a stop) has all its stored calculations
//! recomputed.

use std::collections::{BTreeMap, BTreeSet};

use om_data::rules::recalc::Recalculation;
use om_data::rules::{self, RecordEnvironment};
use om_hbam::slot::{be_uint, uint_segment};
use om_hbam::{Cell, OwnedNode, Table};

use crate::modcount;

/// Option bits that change a field's stored values: global storage,
/// unstored calculation, and "do not evaluate if all referenced fields are
/// empty" off (schema-path.md 5.5).
const VALUE_OPTIONS: u32 = 0x1_0000 | 0x2_0000 | 0x20;

/// Keys under a field definition `[T, 03, 05, F]`: the calculation, and in
/// it the context and the Polish stream (schema-path.md 5.5).
const FORMULA: u8 = 0x05;
const CONTEXT: u8 = 0x04;
const POLISH: u8 = 0x06;

/// Deferred work bits that say a field's stored data changed: deleted,
/// container, type, storage or repetitions, kind, auto-enter, validation
/// or formula, date and timestamp conversion, repetitions reduced
/// (schema-path.md 7.2). A calculation listed with one of them, or one that
/// reads such a field, is recomputed even when the basis shows no change.
const DATA_BITS: u32 = 0x1 | 0x4 | 0x8 | 0x40 | 0x80 | 0x400 | 0x800 | 0x1000;

/// What decides a field's stored values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldBasis {
    kind: u8,
    data_type: u8,
    options: u32,
    repetitions: u16,
    /// A calculation's context and Polish stream.
    formula: Option<(Vec<u8>, Vec<u8>)>,
}

/// A table's fields by id.
pub type TableBasis = BTreeMap<u64, FieldBasis>;

/// The basis of `table` as `root` (the file's key tree) defines it now.
pub fn table_basis(root: &OwnedNode, table: &Table) -> TableBasis {
    let t = uint_segment(table.id);
    table
        .fields
        .iter()
        .filter_map(|field| {
            let options = field.decoded_options()?;
            let f = uint_segment(field.id);
            let formula = (options.field_kind == 2)
                .then(|| {
                    let part = |key: u8| {
                        root.at(&[&t, &[3], &[5], &f, &[FORMULA], &[key]])
                            .and_then(|n| n.assembled())
                            .map(|b| b.into_owned())
                    };
                    Some((part(CONTEXT)?, part(POLISH)?))
                })
                .flatten();
            Some((
                field.id,
                FieldBasis {
                    kind: options.field_kind,
                    data_type: options.data_type,
                    options: options.options & VALUE_OPTIONS,
                    repetitions: options.max_repetitions,
                    formula,
                },
            ))
        })
        .collect()
}

/// The fields whose stored values the change from `old` to `new` makes
/// stale (renaming, indexing, auto-entry and validation are not such
/// changes); `deferred` is the client's deferred work list. Without an
/// `old` basis every field is.
pub fn changed_fields(
    old: Option<&TableBasis>,
    new: &TableBasis,
    deferred: &[(Vec<u8>, u32)],
) -> BTreeSet<u64> {
    let Some(old) = old else {
        return new.keys().copied().collect();
    };
    let mut changed: BTreeSet<u64> = new
        .iter()
        .filter(|(id, basis)| old.get(id) != Some(basis))
        .map(|(id, _)| *id)
        .collect();
    changed.extend(old.keys().filter(|id| !new.contains_key(id)));
    changed.extend(
        deferred
            .iter()
            .filter(|(_, bits)| bits & DATA_BITS != 0)
            .filter_map(|(field, _)| be_uint(field)),
    );
    changed
}

/// What a recomputation did.
#[derive(Debug, Default)]
pub struct Outcome {
    pub records: usize,
    /// Records with a value that changed.
    pub changed: usize,
    /// Per calculation that failed on some records: how many, and the
    /// first failure.
    pub failed: BTreeMap<u64, (usize, String)>,
    /// Records with a value that changed, left as they were because their
    /// modification count is the largest a serial key holds.
    pub exhausted: usize,
}

/// Recompute `recalc`'s calculations on every record under `records`
/// (`[T, 05]`), in place, one record at a time: a record whose values
/// change gets them and its modification count one higher (as op73's
/// other record work, schema-path.md 7.3). A calculation that fails on a
/// record leaves that record's value as it was. `environment` gives what
/// `Get` answers; it is asked once, or per record when a calculation reads
/// `Get ( UUID )`. A record node that holds a value is not a record
/// FileMaker writes and is left as it is: setting a cell would make a list
/// of the value (`om_hbam::writer::holds_value`).
pub fn recompute_records(
    records: &mut OwnedNode,
    recalc: &Recalculation,
    environment: &mut dyn FnMut() -> RecordEnvironment,
) -> Outcome {
    let per_record = recalc.uses_uuid();
    let mut shared = environment();
    let mut outcome = Outcome::default();
    for record in &mut records.children {
        if om_hbam::writer::holds_value(record) {
            continue;
        }
        outcome.records += 1;
        if per_record {
            shared = environment();
        }
        let result = recalc.recompute(&texts(record), &shared);
        // A calculation failing in several repetitions of the record is
        // one record it failed on.
        let mut seen = BTreeSet::new();
        for (field, error) in result.failed {
            if !seen.insert(field) {
                continue;
            }
            let entry = outcome
                .failed
                .entry(field)
                .or_insert_with(|| (0, error.to_string()));
            entry.0 += 1;
        }
        if result.values.is_empty() {
            continue;
        }
        // Changed without a new count, a client's copy would not show as
        // stale: a record whose count cannot advance keeps its values (#361).
        if modcount::bump(record, 1).is_none() {
            outcome.exhausted += 1;
            continue;
        }
        for (field, text) in &result.values {
            let key = uint_segment(*field);
            match text {
                Some(text) => record.set_child(om_scripts::text_node(&key, text)),
                None => {
                    record.remove(&[&key]);
                }
            }
        }
        outcome.changed += 1;
    }
    outcome
}

/// The text of every stored cell of a record but its modification count,
/// by cell id (repetition n of field F under `F * 256 + n`).
pub(crate) fn texts(record: &OwnedNode) -> BTreeMap<u64, String> {
    record
        .children
        .iter()
        .filter(|cell| cell.segment != [modcount::MOD_COUNT])
        .filter_map(|cell| {
            let id = be_uint(&cell.segment)?;
            Some((id, Cell::from_owned(cell).text()?.ok()?))
        })
        .collect()
}

/// What `Get` answers while record rules compute values on the server: the
/// account (also the user name), the server's clock in the storage forms
/// the rules read (en-US, as the desktop client's), and `uuid`. As the
/// file's own calculation it has no window (`Get ( LayoutNumber )` empty)
/// and the server's clock is the host's; whose privilege set is current is
/// the operation's to say ([`rules::WriteContext`]), so a formula reading
/// it is refused unless the caller sets it.
pub fn record_environment(account: &str, uuid: String) -> RecordEnvironment {
    let now = jiff::Zoned::now();
    let date = format!("{}/{}/{}", now.month(), now.day(), now.year());
    let hour = match now.hour() % 12 {
        0 => 12,
        h => h,
    };
    let time = format!(
        "{hour}:{:02}:{:02} {}",
        now.minute(),
        now.second(),
        if now.hour() < 12 { "AM" } else { "PM" }
    );
    let timestamp = format!("{date} {time}");
    RecordEnvironment {
        account_name: account.to_string(),
        user_name: account.to_string(),
        context: rules::WriteContext {
            layout: rules::WriteLayout::NoWindow,
            privilege_set: None,
            host_timestamp: Some(timestamp.clone()),
            record_number: None,
        },
        timestamp,
        date,
        time,
        uuid,
        containers: None,
        interrupted: None,
        limits: om_calc::eval::CalcLimits::DEFAULT,
        lookups: Default::default(),
        serials: Default::default(),
        globals: None,
        // A server script's transaction holds its records in its own
        // session, which its source reads (`scripts::host`).
        held: None,
        related_needs: None,
    }
}

/// A version 4 UUID in FileMaker's `Get ( UUID )` form from 16 random
/// bytes.
pub fn uuid_text(mut bytes: [u8; 16]) -> String {
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02X}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// A fresh random UUID from the system's random source.
pub fn random_uuid() -> String {
    use ring::rand::SecureRandom as _;
    let mut bytes = [0u8; 16];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .expect("the system random source is available");
    uuid_text(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn basis(formula: &[u8]) -> FieldBasis {
        FieldBasis {
            kind: 2,
            data_type: 1,
            options: 0,
            repetitions: 1,
            formula: Some((vec![3, 0xd0, 0, 2], formula.to_vec())),
        }
    }

    #[test]
    fn a_changed_formula_type_storage_or_field_set_is_a_change() {
        let text = FieldBasis {
            kind: 1,
            data_type: 1,
            options: 0,
            repetitions: 1,
            formula: None,
        };
        let old = TableBasis::from([(1, text.clone()), (2, basis(&[0x10])), (3, text.clone())]);
        // Unchanged: nothing (a rename or an index is not in the basis).
        assert!(changed_fields(Some(&old), &old, &[]).is_empty());
        let mut new = old.clone();
        new.insert(2, basis(&[0x11]));
        new.remove(&3);
        new.insert(4, basis(&[0x10]));
        let mut number = text.clone();
        number.data_type = 2;
        new.insert(1, number);
        assert_eq!(
            changed_fields(Some(&old), &new, &[]),
            BTreeSet::from([1, 2, 3, 4])
        );
        // The deferred list adds fields whose data changed; the index bits
        // alone (0x10, 0x20) and a rename (0x200) do not.
        assert_eq!(
            changed_fields(Some(&old), &old, &[(vec![2], 0x80), (vec![1], 0x230)]),
            BTreeSet::from([2])
        );
        // Without a basis, every field.
        assert_eq!(changed_fields(None, &old, &[]), BTreeSet::from([1, 2, 3]));
    }

    #[test]
    fn uuids_have_the_version_4_form() {
        let text = uuid_text([0xff; 16]);
        assert_eq!(text, "FFFFFFFF-FFFF-4FFF-BFFF-FFFFFFFFFFFF");
        assert_ne!(random_uuid(), random_uuid());
    }
}
