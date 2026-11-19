//! Ordering records by a stored sort order (`DBSortSpec`, script-steps.md
//! 11.1.3): the related records of a sorted relationship
//! (file-metadata.md 4.3), with the rules FileMaker Server's script engine
//! was seen to follow, as OpenMaker Server's Sort Records does.
//!
//! Empty values sort first ascending and last descending, or last in both
//! directions with "blanks last"; a custom value list's order puts the
//! list's values first in its order (dividers ignored), then values not in
//! it, then empty values; text compares by the collation of the entry's
//! language override, else of the field's index language, else of the
//! file's; numbers, dates and times by value; ties fall to the next sort
//! field and finally to the record id. A sort field of an occurrence of
//! another table sorts by its first related record's value.

use std::cmp::Ordering;

use om_calc::ops::{self, Comparison};
use om_calc::text::sorting::{Collation, SortKey};
use om_calc::{CalcError, Ctx, Language, Value, ValueType};
use om_scripts::specs::{SortEntry, SortSpec};

use super::data::CalcSource;
use super::schema::{CalcSchema, FieldClass, TableDef};
use crate::meta::ValueSource;

/// `records` of `occurrence`'s table ordered by `spec`. Sort fields are
/// read from the stored record (or the table's globals); calculations a
/// record does not store and summaries sort as empty.
pub fn sort_records<S: CalcSource + ?Sized>(
    source: &S,
    schema: &CalcSchema,
    occurrence: u64,
    records: Vec<u64>,
    spec: &SortSpec,
    cx: &Ctx,
) -> Result<Vec<u64>, CalcError> {
    if spec.fields.is_empty() || records.len() < 2 {
        return Ok(records);
    }
    // The occurrence or a sort field's occurrence waiting for its file or
    // its table's fields: each is asked for, and the order waits (1224)
    // rather than leaving that level out.
    let mut waiting = false;
    let fields = spec
        .fields
        .iter()
        .map(|e| e.field.occurrence.unwrap_or(occurrence));
    for waits in std::iter::once(occurrence).chain(fields) {
        if schema.is_pending(waits) {
            source.need_occurrence(waits);
            waiting = true;
        }
    }
    if waiting {
        return Err(CalcError::from_code(1224).expect("nonzero"));
    }
    let Some(table) = schema.table_of(occurrence) else {
        return Ok(records);
    };
    let levels: Vec<Level> = spec
        .fields
        .iter()
        .filter_map(|entry| Level::new(entry, schema, occurrence, table, cx))
        .collect();
    source.prefetch_records(table.id, &records);
    let mut keyed = Vec::with_capacity(records.len());
    for record in records {
        let keys = levels
            .iter()
            .map(|level| level.key(source, schema, occurrence, record, cx))
            .collect::<Result<Vec<_>, _>>()?;
        keyed.push((record, keys));
    }
    keyed.sort_by(|(a_id, a), (b_id, b)| {
        levels
            .iter()
            .enumerate()
            .map(|(i, level)| level.compare(&a[i], &b[i], spec.blanks_last, cx))
            .find(|o| *o != Ordering::Equal)
            .unwrap_or_else(|| a_id.cmp(b_id))
    });
    Ok(keyed.into_iter().map(|(id, _)| id).collect())
}

/// The collation of a `Locale::Type` (script-steps.md 11.1.2): the named
/// collation of its catalog name, else the file's.
pub(super) fn collation_of(language: Language) -> Collation {
    Collation::named(language.type_name()).unwrap_or(Collation::of(language))
}

/// One sort field, ready to key records.
struct Level<'s> {
    /// The occurrence the value is read from: the sorted occurrence, or a
    /// related one (its first related record).
    occurrence: u64,
    related: bool,
    table: &'s TableDef,
    field: u64,
    repetition: u32,
    collation: Collation,
    /// A custom value list's keys, in order without dividers.
    value_list: Option<Vec<SortKey>>,
    descending: bool,
}

/// A record's value at one level.
enum Key {
    Empty,
    Text(SortKey),
    Value(Value),
}

/// The value and its position in the level's value list.
struct LevelKey {
    listed: Option<usize>,
    key: Key,
}

impl<'s> Level<'s> {
    /// `None` for a field the schema does not have, which does not order.
    fn new(
        entry: &SortEntry,
        schema: &'s CalcSchema,
        occurrence: u64,
        table: &'s TableDef,
        cx: &Ctx,
    ) -> Option<Level<'s>> {
        let field_occurrence = entry.field.occurrence.unwrap_or(occurrence);
        let field_table = schema.table_of(field_occurrence).unwrap_or(table);
        let related = field_table.id != table.id;
        let def = field_table.fields.get(&entry.field.field)?;
        let language = match (entry.locale, def.language) {
            (0, 0) => cx.locale.language,
            (0, own) | (own, _) => Language::from_id(u16::try_from(own).unwrap_or(0)),
        };
        let collation = collation_of(language);
        // A list the account may not read orders nothing.
        let value_list = entry.value_list.and_then(|id| {
            let list = schema
                .value_lists()
                .iter()
                .find(|l| l.id == id && schema.shows_value_list(l.id))?;
            match &list.source {
                ValueSource::Custom(values) => Some(
                    values
                        .iter()
                        .filter(|v| *v != "-")
                        .map(|v| collation.sort_key(v))
                        .collect(),
                ),
                _ => None,
            }
        });
        Some(Level {
            occurrence: if related {
                field_occurrence
            } else {
                occurrence
            },
            related,
            table: field_table,
            field: entry.field.field,
            repetition: u32::from(entry.field.repetition.max(1)),
            collation,
            value_list,
            descending: entry.descending,
        })
    }

    fn key<S: CalcSource + ?Sized>(
        &self,
        source: &S,
        schema: &CalcSchema,
        occurrence: u64,
        record: u64,
        cx: &Ctx,
    ) -> Result<LevelKey, CalcError> {
        let at = if self.related {
            source
                .related(schema, occurrence, record, self.occurrence, cx)?
                .first()
                .copied()
        } else {
            Some(record)
        };
        let def = &self.table.fields[&self.field];
        let held = match at {
            _ if def.global => source
                .globals(self.table.id)?
                .get(self.field, self.repetition)
                .cloned(),
            Some(at) if def.class == FieldClass::Data || !def.is_unstored_calculation() => source
                .record(self.table.id, at)?
                .and_then(|values| values.get(self.field, self.repetition).cloned()),
            _ => None,
        };
        let value = match held {
            Some(value) if def.class != FieldClass::Summary && !value.is_empty() => value,
            _ => {
                return Ok(LevelKey {
                    listed: None,
                    key: Key::Empty,
                })
            }
        };
        let text_key = || self.collation.sort_key(value.to_text(cx).as_str());
        let listed = self.value_list.as_ref().and_then(|list| {
            let key = text_key();
            list.iter().position(|item| *item == key)
        });
        let key = match value.value_type() {
            ValueType::Text | ValueType::Container => Key::Text(text_key()),
            _ => Key::Value(value),
        };
        Ok(LevelKey { listed, key })
    }

    /// Two records' keys at this level.
    fn compare(&self, a: &LevelKey, b: &LevelKey, blanks_last: bool, cx: &Ctx) -> Ordering {
        let order = |o: Ordering| if self.descending { o.reverse() } else { o };
        if self.value_list.is_some() {
            // The list's values in its order, then other values, then empty.
            let rank = |k: &LevelKey| match (k.listed, &k.key) {
                (Some(position), _) => (0, position),
                (None, Key::Empty) => (2, 0),
                (None, _) => (1, 0),
            };
            let o = rank(a)
                .cmp(&rank(b))
                .then_with(|| compare_keys(&a.key, &b.key, cx));
            return order(o);
        }
        match (&a.key, &b.key) {
            (Key::Empty, Key::Empty) => Ordering::Equal,
            (Key::Empty, _) if blanks_last || self.descending => Ordering::Greater,
            (Key::Empty, _) => Ordering::Less,
            (_, Key::Empty) if blanks_last || self.descending => Ordering::Less,
            (_, Key::Empty) => Ordering::Greater,
            (x, y) => order(compare_keys(x, y, cx)),
        }
    }
}

fn compare_keys(a: &Key, b: &Key, cx: &Ctx) -> Ordering {
    match (a, b) {
        (Key::Text(x), Key::Text(y)) => x.cmp(y),
        (Key::Value(x), Key::Value(y)) => match ops::compare(x, y, cx) {
            Comparison::Ordered(o) => o,
            _ => Ordering::Equal,
        },
        (Key::Empty, Key::Empty) => Ordering::Equal,
        (Key::Empty, _) => Ordering::Less,
        (_, Key::Empty) => Ordering::Greater,
        // A text and a typed value (a number field holding text): text after.
        (Key::Text(_), Key::Value(_)) => Ordering::Greater,
        (Key::Value(_), Key::Text(_)) => Ordering::Less,
    }
}
