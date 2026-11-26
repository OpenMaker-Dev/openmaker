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

use om_data::rules::recalc::{ContainerResult, Recalculation};
use om_data::rules::{self, RecordEnvironment};
use om_hbam::slot::{be_uint, uint_segment};
use om_hbam::{Cell, Field, OwnedNode, Table};

use crate::engine::{Environment, HostEnvironment, ReadError, TreeView, View, NO_LIMIT};
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

impl FieldBasis {
    pub fn known_storage(&self) -> Option<(bool, bool)> {
        if !(1..=3).contains(&self.kind) || !(1..=6).contains(&self.data_type) {
            return None;
        }
        Some((self.data_type == 6, self.options & 0x1_0000 != 0))
    }

    fn is_stored_container_calculation(&self) -> bool {
        self.kind == 2 && self.data_type == 6 && self.options & 0x3_0000 == 0
    }
}

/// A table's fields by id.
pub type TableBasis = BTreeMap<u64, FieldBasis>;

/// The basis of `table` as `root` (the file's key tree) defines it now.
pub fn table_basis(root: &OwnedNode, table: &Table) -> TableBasis {
    let view = TreeView::new(root, 0);
    table
        .fields
        .iter()
        .map(|field| {
            let basis = field_basis(&view, table.id, field)
                .expect("the file engine's tree never fails a read");
            (field.id, basis)
        })
        .collect()
}

/// The basis of `field` of table `table` as `view` reads its definition.
pub(crate) fn field_basis(
    view: &dyn View,
    table: u64,
    field: &Field,
) -> Result<FieldBasis, ReadError> {
    let Some(options) = field.decoded_options() else {
        return Ok(FieldBasis {
            kind: 0,
            data_type: 0,
            options: 0,
            repetitions: 0,
            formula: None,
        });
    };
    let formula = if options.field_kind == 2 {
        let (t, f) = (uint_segment(table), uint_segment(field.id));
        let part = |key: u8| -> Result<Option<Vec<u8>>, ReadError> {
            let path: [&[u8]; 6] = [&t, &[3], &[5], &f, &[FORMULA], &[key]];
            Ok(view
                .subtree(&path, NO_LIMIT)?
                .and_then(|node| node.assembled().map(|bytes| bytes.into_owned())))
        };
        match (part(CONTEXT)?, part(POLISH)?) {
            (Some(context), Some(polish)) => Some((context, polish)),
            _ => None,
        }
    } else {
        None
    };
    Ok(FieldBasis {
        kind: options.field_kind,
        data_type: options.data_type,
        options: options.options & VALUE_OPTIONS,
        repetitions: options.max_repetitions,
        formula,
    })
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

/// The calculations of `new` with a container result that were
/// calculations of another result type in `old`: their cells hold derived
/// values of the old type (a calculation keeps its old text until it is
/// recomputed), which name no member they counted, so the type change
/// drops them before the recompute ([`crate::store::Store`]'s
/// `update_table`). A field that held entered values (a normal field
/// made a container calculation) keeps them until the recompute writes
/// its results, so that a calculation the recompute cannot evaluate loses
/// nothing (#427's review); what FileMaker shows of them is unmeasured
/// (#293). Without an `old` basis there is no eager cleanup, and releases
/// during the recompute are restricted to the surviving cells' counts.
pub fn retyped_to_container(
    old: Option<&TableBasis>,
    new: &TableBasis,
) -> Result<BTreeSet<u64>, &'static str> {
    let Some(old) = old else {
        return Ok(BTreeSet::new());
    };
    let mut retyped = BTreeSet::new();
    for (id, basis) in new {
        if !basis.is_stored_container_calculation() {
            continue;
        }
        let Some(was) = old.get(id) else {
            continue;
        };
        let Some((container, _)) = was.known_storage() else {
            return Err("the old type of a stored container calculation is unknown");
        };
        if was.kind == 2 && was.options & 0x3_0000 == 0 && !container {
            retyped.insert(*id);
        }
    }
    Ok(retyped)
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
    /// Per record (its key under `[T, 05]`), the container results that
    /// changed, by cell: stored in the table's library after the walk,
    /// which reads the library while it runs ([`crate::store::Store`]'s
    /// `recalculate`).
    pub containers: Vec<(Vec<u8>, BTreeMap<u64, ContainerResult>)>,
}

/// How much new container data one recompute holds for the library
/// before it is stored, charged as each result is queued: past it, a
/// result fails on its record, which keeps its value (logged), so that a
/// formula making large data on every row cannot hold rows times its size
/// in memory (#427's review).
pub const CONTAINER_BUDGET: usize = 256 << 20;

/// Recompute `recalc`'s calculations on every record under `records`
/// (`[T, 05]`), in place, one record at a time: a record whose values
/// change gets them and its modification count one higher (as op73's
/// other record work, schema-path.md 7.3). A calculation that fails on a
/// record leaves that record's value as it was. `environment` gives what
/// `Get` answers; it is asked once, or per record when a calculation reads
/// `Get ( UUID )`, for the record's own UUID and clock reading. The first
/// one's sources ([`rules::WriteSources`]: `Random`'s generator, the
/// conversion date, the stopwatch) serve the whole recalculation, so that
/// it draws one `Random` sequence, as one operation does. A record node
/// that holds a value is not a record
/// FileMaker writes and is left as it is: setting a cell would make a list
/// of the value (`om_hbam::writer::holds_value`). `before_change` sees each
/// record as it is just before it changes, for a journal that may have to
/// put it back.
pub fn recompute_records(
    records: &mut OwnedNode,
    recalc: &Recalculation,
    environment: &mut dyn FnMut() -> RecordEnvironment,
    before_change: &mut dyn FnMut(&OwnedNode),
) -> Outcome {
    let per_record = recalc.uses_uuid();
    let mut shared = environment();
    let mut outcome = Outcome::default();
    let mut budget = CONTAINER_BUDGET;
    for record in &mut records.children {
        if om_hbam::writer::holds_value(record) {
            continue;
        }
        outcome.records += 1;
        if per_record {
            let sources = shared.sources.take();
            shared = RecordEnvironment {
                sources,
                ..environment()
            };
        }
        let queue = &mut outcome.containers;
        let growth = if queue.len() == queue.capacity() {
            queue.capacity().max(1)
        } else {
            0
        };
        let overhead = growth * std::mem::size_of::<(Vec<u8>, BTreeMap<u64, ContainerResult>)>()
            + record.segment.len();
        let mut available = budget.saturating_sub(overhead);
        let result = recalc.recompute(&texts(record), &shared, &mut available);
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
        if result.values.is_empty() && result.containers.is_empty() {
            continue;
        }
        before_change(record);
        // Changed without a new count, a client's copy would not show as
        // stale: a record whose count cannot advance keeps its values (#361).
        if modcount::bump(record, 1).is_none() {
            outcome.exhausted += 1;
            continue;
        }
        if !result.containers.is_empty() {
            // A result costs at least one node, so a nonempty map means
            // its costs and the record's queue allocation both fit.
            budget = available;
            if growth != 0 {
                queue.reserve_exact(growth);
            }
            queue.push((record.segment.clone(), result.containers));
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
/// it is refused unless the caller sets it. The rest of what its formulas
/// read of the world ([`rules::WriteSources`]) is the host's, as an
/// operation's is its engine's ([`record_environment_in`]): one reading of
/// the host's clock, a `Random` generator seeded from the system's random
/// source, and a stopwatch on the host's monotonic clock.
pub fn record_environment(account: &str, uuid: String) -> RecordEnvironment {
    with_sources(account, uuid, &HostEnvironment)
}

/// [`record_environment`] for an operation run with `environment`, which
/// supplies everything its formulas read of the world, drawn in this order:
/// the UUID, one clock reading (the date, time and timestamp texts and the
/// UTC instant), eight bytes of the random source to seed `Random`'s
/// generator with (so that each operation draws its own sequence), and the
/// stopwatch of its time budgets.
pub fn record_environment_in(account: &str, environment: &dyn Environment) -> RecordEnvironment {
    with_sources(account, crate::engine::uuid(environment), environment)
}

fn with_sources(account: &str, uuid: String, environment: &dyn Environment) -> RecordEnvironment {
    let now = environment.now();
    let mut seed = [0u8; 8];
    environment.fill_random(&mut seed);
    // Floored, not truncated toward zero: a reading just before 1970 keeps
    // its UTC day.
    let sources = rules::WriteSources::new(
        now.timestamp().as_nanosecond().div_euclid(1_000),
        u64::from_le_bytes(seed),
        environment.stopwatch(),
    );
    RecordEnvironment {
        sources: Some(sources),
        ..clock_environment(account, uuid, &now)
    }
}

/// [`record_environment`]'s texts at the time `now`, with the process's
/// own sources.
fn clock_environment(account: &str, uuid: String, now: &jiff::Zoned) -> RecordEnvironment {
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
            sequence: om_data::rules::WriteSequence::Unmeasured,
            file_variables: None,
            other_globals: None,
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
        others_held: Default::default(),
        related_needs: None,
        sources: None,
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
