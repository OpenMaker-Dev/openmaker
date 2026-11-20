//! Sorting in a server script session: Sort Records with its stored order,
//! Sort Records by Field, and the order of a sorted relationship's related
//! records (script-steps.md 11.1, file-metadata.md 4.3).
//!
//! The rules FileMaker Server's script engine was seen to follow (11.1.3):
//! empty values first ascending and last descending, or last in both
//! directions with "blanks last"; text by the collation of the entry's
//! language override, else of the field's index language; numbers, dates
//! and times by value; a custom value list's order with values not in the
//! list after its values (dividers ignored); ties to the next field, and
//! finally to the record id. A sort field of another occurrence of a
//! different table sorts by its first related record's value (Claris); an
//! unstored calculation by its value on each record. Value lists from
//! fields and the summary reorder are not supported (3).

use std::cmp::Ordering;

use om_calc::eval::CalcHost;
use om_calc::ops::{self, Comparison};
use om_calc::text::sorting::{Collation, SortKey};
use om_calc::{Ctx, Value, ValueType};
use om_data::meta::ValueSource;
use om_scripts::runtime::{ErrorCode, FieldRef, HostResult};
use om_scripts::specs::{SortEntry, SortSpec, SpecField};

use super::ServerHost;

/// "Unsupported", as a step without a handler reports it.
const UNSUPPORTED: ErrorCode = ErrorCode(3);

/// One sort field, ready to key records.
struct Level {
    /// Where the value is read: the field's occurrence, and whether that is
    /// a related occurrence (its first related record).
    field: FieldRef,
    related: bool,
    collation: Collation,
    /// The custom values' keys, in list order.
    value_list: Option<Vec<SortKey>>,
    descending: bool,
}

/// A record's key at one level.
enum Key {
    Empty,
    Text(SortKey),
    Value(Value),
}

struct LevelKey {
    /// The position in the level's value list.
    listed: Option<usize>,
    key: Key,
}

/// The collation of a `Locale::Type` (script-steps.md 11.1.2); 0 (none)
/// and languages without a tailoring collate as `default`.
fn collation_of(locale_type: u32, default: Collation) -> Collation {
    let name = match locale_type {
        2 => "Unicode_Raw",
        3 => "Unicode_Standard",
        4 => "Catalog",
        16 => "Catalan",
        17 => "Croatian",
        18 => "Czech",
        19 => "Danish",
        21 | 77 => "English",
        22 => "Finnish",
        23 => "Finnish_Custom",
        24 => "French",
        25 => "German",
        26 => "German_Dictionary",
        28 => "Hungarian",
        29 => "Icelandic",
        31 => "Japanese",
        33 => "Polish",
        35 => "Romanian",
        37 => "Slovak",
        38 => "Slovenian",
        39 => "Spanish",
        40 => "Spanish_Traditional",
        41 => "Swedish",
        42 => "Swedish_Custom",
        43 => "Turkish",
        44 => "Ukrainian",
        50 => "Estonian",
        51 => "Lithuanian",
        52 => "Latvian",
        _ => return default,
    };
    Collation::named(name).unwrap_or(default)
}

impl ServerHost {
    /// Sort Records [Restore] with the stored order `spec` (11.1): the
    /// window's found set in that order, the first record current.
    pub(super) fn sort_window(&mut self, spec: &SortSpec) -> HostResult {
        self.require_view()?;
        self.leave()?;
        if spec.fields.is_empty() {
            return Ok(());
        }
        let window = self.window()?;
        let (occurrence, found) = (window.occurrence, window.found.clone());
        let sorted = self.sorted(occurrence, found, spec)?;
        let window = self.window_mut()?;
        window.found = sorted;
        window.current = 0;
        window.sort = Some(spec.clone());
        Ok(())
    }

    /// Sort Records by Field (154): `direction` 1 ascending, 2 descending;
    /// 3 (by the field's value list on the layout) is not supported.
    pub(super) fn sort_window_by_field(&mut self, field: FieldRef, direction: u16) -> HostResult {
        if !matches!(direction, 1 | 2) {
            return Err(UNSUPPORTED.into());
        }
        let spec = SortSpec {
            keep_sorted: true,
            blanks_last: false,
            fields: vec![SortEntry {
                field: SpecField {
                    occurrence: Some(field.occurrence),
                    field: field.field,
                    repetition: field.repetition.max(1) as u16,
                },
                summary: None,
                value_list: None,
                descending: direction == 2,
                locale: 0,
            }],
        };
        self.sort_window(&spec)
    }

    /// `records` of `occurrence`'s table in the order of `spec`.
    pub(super) fn sorted(
        &self,
        occurrence: u64,
        records: Vec<u64>,
        spec: &SortSpec,
    ) -> Result<Vec<u64>, ErrorCode> {
        let levels = spec
            .fields
            .iter()
            .map(|entry| self.level(occurrence, entry))
            .collect::<Result<Vec<_>, _>>()?;
        let mut keyed = Vec::with_capacity(records.len());
        for (i, record) in records.into_iter().enumerate() {
            // A key may evaluate a calculation or follow a relationship.
            if i % super::ABORT_CHECK == 0 && self.aborted() {
                return Err(ErrorCode::CANCELED);
            }
            let keys = levels
                .iter()
                .map(|level| self.level_key(occurrence, record, level))
                .collect::<Result<Vec<_>, _>>()?;
            keyed.push((record, keys));
        }
        let cx = self.cx();
        keyed.sort_by(|(a_id, a), (b_id, b)| {
            levels
                .iter()
                .enumerate()
                .map(|(i, level)| compare(&a[i], &b[i], level, spec.blanks_last, &cx))
                .find(|o| *o != Ordering::Equal)
                .unwrap_or_else(|| a_id.cmp(b_id))
        });
        Ok(keyed.into_iter().map(|(id, _)| id).collect())
    }

    /// Each of `records` (in their order, sorted by `spec`) numbered by its
    /// group: a record starts a new group when one of its keys differs from
    /// the previous record's (summaries' break fields, file-metadata.md
    /// 5.3).
    pub(super) fn groups(
        &self,
        occurrence: u64,
        records: &[u64],
        spec: &SortSpec,
    ) -> Result<Vec<usize>, ErrorCode> {
        let levels = spec
            .fields
            .iter()
            .map(|entry| self.level(occurrence, entry))
            .collect::<Result<Vec<_>, _>>()?;
        let cx = self.cx();
        let mut groups = Vec::with_capacity(records.len());
        let mut previous: Option<Vec<LevelKey>> = None;
        let mut group = 0;
        for &record in records {
            let keys = levels
                .iter()
                .map(|level| self.level_key(occurrence, record, level))
                .collect::<Result<Vec<_>, _>>()?;
            if let Some(previous) = &previous {
                let differs = levels.iter().enumerate().any(|(i, level)| {
                    compare(&previous[i], &keys[i], level, spec.blanks_last, &cx) != Ordering::Equal
                });
                group += usize::from(differs);
            }
            groups.push(group);
            previous = Some(keys);
        }
        Ok(groups)
    }

    fn level(&self, occurrence: u64, entry: &SortEntry) -> Result<Level, ErrorCode> {
        if entry.summary.is_some() {
            return Err(UNSUPPORTED);
        }
        let field_occurrence = entry.field.occurrence.unwrap_or(occurrence);
        let (table, field) = self
            .schema
            .field(field_occurrence, entry.field.field)
            .ok_or(ErrorCode::FIELD_MISSING)?;
        let related =
            field_occurrence != occurrence && self.schema.table_of(occurrence) != Some(table);
        let file = Collation::of(self.locale.language);
        let own = field
            .options
            .get(4..8)
            .and_then(|b| b.try_into().ok())
            .map_or(0, u32::from_be_bytes);
        let collation = match entry.locale {
            0 => collation_of(own, file),
            other => collation_of(other, file),
        };
        let value_list = match entry.value_list {
            None => None,
            Some(list) => Some(self.custom_values(list, collation)?),
        };
        Ok(Level {
            field: FieldRef {
                occurrence: if related {
                    field_occurrence
                } else {
                    occurrence
                },
                field: entry.field.field,
                repetition: u32::from(entry.field.repetition.max(1)),
            },
            related,
            collation,
            value_list,
            descending: entry.descending,
        })
    }

    /// The keys of a custom value list's values, in order, dividers left
    /// out (the shared value-list decode, `om_data::meta`); a list of
    /// another type is not supported.
    fn custom_values(&self, list: u64, collation: Collation) -> Result<Vec<SortKey>, ErrorCode> {
        let list = self
            .calc_schema()
            .and_then(|schema| schema.value_lists().iter().find(|l| l.id == list))
            .ok_or(UNSUPPORTED)?;
        let ValueSource::Custom(values) = &list.source else {
            return Err(UNSUPPORTED);
        };
        Ok(values
            .iter()
            .filter(|item| *item != "-")
            .map(|item| collation.sort_key(item))
            .collect())
    }

    fn level_key(
        &self,
        occurrence: u64,
        record: u64,
        level: &Level,
    ) -> Result<LevelKey, ErrorCode> {
        let field = level.field;
        let at = if level.related {
            self.related_set(occurrence, record, field.occurrence)
                .map_err(|e| ErrorCode(e.code()))?
                .first()
                .copied()
        } else {
            Some(record)
        };
        let value = match at {
            Some(at) => {
                CalcHost::field_value(self, field.occurrence, at, field.field, field.repetition)
                    .map_err(|e| ErrorCode(e.code()))?
            }
            None => Value::empty_text(),
        };
        if value.is_empty() {
            return Ok(LevelKey {
                listed: None,
                key: Key::Empty,
            });
        }
        let text_key = || level.collation.sort_key(value.to_text(&self.cx()).as_str());
        let listed = level.value_list.as_ref().and_then(|list| {
            let key = text_key();
            list.iter().position(|item| *item == key)
        });
        let key = match value.value_type() {
            ValueType::Text | ValueType::Container => Key::Text(text_key()),
            _ => Key::Value(value),
        };
        Ok(LevelKey { listed, key })
    }
}

/// Two records' keys at one level.
fn compare(a: &LevelKey, b: &LevelKey, level: &Level, blanks_last: bool, cx: &Ctx) -> Ordering {
    let order = |o: Ordering| if level.descending { o.reverse() } else { o };
    if level.value_list.is_some() {
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
        (Key::Empty, _) if blanks_last || level.descending => Ordering::Greater,
        (Key::Empty, _) => Ordering::Less,
        (_, Key::Empty) if blanks_last || level.descending => Ordering::Less,
        (_, Key::Empty) => Ordering::Greater,
        (x, y) => order(compare_keys(x, y, cx)),
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

#[cfg(test)]
mod tests {
    use super::*;
    use om_calc::Language;

    #[test]
    fn locale_types_select_their_collation() {
        let english = Collation::of(Language::English);
        assert_eq!(collation_of(0, english), english);
        assert_eq!(collation_of(41, english), Collation::Swedish);
        assert_eq!(collation_of(25, english), Collation::German);
        assert_eq!(collation_of(30, english), english, "Italian: no tailoring");
        let swedish = collation_of(41, english);
        assert!(swedish.sort_key("zeta") < swedish.sort_key("Äpfel"));
        let german = collation_of(25, english);
        assert!(german.sort_key("Äpfel") < german.sort_key("alpha"));
    }
}
