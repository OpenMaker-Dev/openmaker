//! Check `text_key` and `words` against the value and word indexes FileMaker
//! wrote: every record of every indexed text field must sit under the keys
//! computed from its stored value.
//!
//! Ignored by default: the files are git-ignored. Run with:
//!   cargo test -p om-collate --test indexes -- --ignored --nocapture

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use om_collate::DateOrder;
use om_hbam::slot::uint_segment;
use om_hbam::{DataType, HbamFile, RidList};

const FMS_SAMPLES: &str =
    "../../re/extracted/fms/deb/opt/FileMaker/FileMaker Server/Data/Databases/.Sample";
const TEST_PROJECT: &str = "../../re/work/omserver/testproject-live-1234.fmp12";

/// The FileMaker Server samples with their text-key form and date order.
const SAMPLES: [(&str, bool, DateOrder); 6] = [
    ("en", true, DateOrder::MonthDayYear),
    ("fr", true, DateOrder::DayMonthYear),
    ("es", true, DateOrder::MonthDayYear),
    ("it", true, DateOrder::DayMonthYear),
    ("de", false, DateOrder::DayMonthYear),
    ("ja", false, DateOrder::YearMonthDay),
];

/// Every file with its date order, and whether its text fields use the
/// English form (the German and Japanese samples use their own languages).
fn files() -> Vec<(HbamFile, bool, DateOrder)> {
    let mut files = vec![(
        HbamFile::open(TEST_PROJECT).expect("test project copy"),
        true,
        DateOrder::MonthDayYear,
    )];
    for (lang, english, order) in SAMPLES {
        files.push((
            HbamFile::open(Path::new(FMS_SAMPLES).join(format!("{lang}_FMServer_Sample")))
                .expect("sample present"),
            english,
            order,
        ));
    }
    files
}

/// Each record's keys in index `kind` (0 value, 1 word) of every indexed
/// field, beside the keys `keys_of` computes from its stored value. Fields
/// whose type `keys_of` does not handle (it returns `None`) are skipped.
fn compare(
    kind: u8,
    keys_of: impl Fn(DataType, &str, bool, DateOrder) -> Option<BTreeSet<Vec<u8>>>,
) -> (usize, Vec<String>) {
    let (mut checked, mut mismatches) = (0, Vec::new());
    for (file, english, order) in files() {
        let catalog = file.catalog().unwrap();
        for table in &catalog.tables {
            let table_key = uint_segment(table.id);
            let records = file.records(table.id).unwrap();
            for field in &table.fields {
                if keys_of(field.data_type, "", english, order).is_none() {
                    continue;
                }
                let field_key = uint_segment(field.id);
                let Some(index) = file
                    .subtree(&[&table_key, &[1], &field_key, &[kind]])
                    .unwrap()
                else {
                    continue;
                };
                let mut indexed: BTreeMap<u64, BTreeSet<Vec<u8>>> = BTreeMap::new();
                for entry in &index.children {
                    for id in RidList::from_node(entry).unwrap().iter() {
                        indexed
                            .entry(id)
                            .or_default()
                            .insert(entry.segment.0.to_vec());
                    }
                }
                for record in &records {
                    let value = record
                        .text(field.id)
                        .transpose()
                        .unwrap()
                        .unwrap_or_default();
                    checked += 1;
                    let have = indexed.remove(&record.id).unwrap_or_default();
                    let ours = keys_of(field.data_type, &value, english, order).unwrap_or_default();
                    if have != ours {
                        mismatches.push(format!(
                            "{}.{} {}: {value:?}",
                            table.name, field.name, record.id
                        ));
                    }
                }
            }
        }
    }
    (checked, mismatches)
}

#[test]
#[ignore = "requires the git-ignored FileMaker Server samples and test project copy"]
fn value_index_keys_match() {
    let (checked, mismatches) = compare(0, |data_type, value, english, _| {
        (english && data_type == DataType::Text).then(|| [om_collate::text_key(value)].into())
    });
    println!("value index: {checked} values checked");
    assert!(mismatches.is_empty(), "{mismatches:#?}");
    assert!(checked >= 100);
}

#[test]
#[ignore = "requires the git-ignored FileMaker Server samples and test project copy"]
fn word_index_keys_match() {
    let (checked, mismatches) = compare(1, |data_type, value, english, _| {
        // Apostrophes separate, as when this test last passed: the French
        // sample separates there, while English joins (the probe,
        // query-program.md 7.1), which these files' text did not need.
        (english && data_type == DataType::Text).then(|| {
            om_collate::words_with(value, false)
                .into_iter()
                .map(om_collate::text_key)
                .collect()
        })
    });
    println!("word index: {checked} values checked");
    assert!(mismatches.is_empty(), "{mismatches:#?}");
    assert!(checked >= 100);
}

/// Number, date, time and timestamp value indexes: an empty value sits under
/// `01`, a value with no digit under `fe ff`, any other under its typed key
/// (query-program.md 7.3; `om_query::keys::typed_value_keys`).
#[test]
#[ignore = "requires the git-ignored FileMaker Server samples and test project copy"]
fn typed_value_index_keys_match() {
    let (checked, mismatches) = compare(0, |data_type, value, _, order| {
        let typed = matches!(
            data_type,
            DataType::Number | DataType::Date | DataType::Time | DataType::Timestamp
        );
        let key = match data_type {
            _ if typed && value.trim().is_empty() => Some(vec![0x01]),
            DataType::Number => om_collate::number_key(value),
            DataType::Date => om_collate::date_key(value, order),
            DataType::Time => om_collate::time_key(value),
            DataType::Timestamp => om_collate::timestamp_key(value, order),
            _ => return None,
        };
        let invalid = || (!value.bytes().any(|b| b.is_ascii_digit())).then(|| vec![0xfe, 0xff]);
        Some(key.or_else(invalid).into_iter().collect())
    });
    println!("typed value index: {checked} values checked");
    assert!(mismatches.is_empty(), "{mismatches:#?}");
    assert!(checked >= 20, "{checked}");
}
