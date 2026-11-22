//! Imports and exports stop when the host interrupts them (its script was
//! aborted or ran out of time): before the next row or record, with error
//! 1, the import writing no more rows and the export no file. An import
//! also stops at a row whose rule the host does not support checking.

mod support;

use om_transfer::export::{self, ExportRequest};
use om_transfer::import::{self, ImportRequest};
use om_transfer::spec::{
    ExportField, ExportOptions, FieldSpec, ImportAction, ImportOptions, Key, MapEntry, Mapping,
};
use om_transfer::{ExportCharset, FileType, ImportCharset, Profile};
use support::{f, Mem, CONTACTS};

fn import_request(action: ImportAction) -> ImportRequest {
    ImportRequest {
        options: ImportOptions {
            table: Key::from_id(CONTACTS),
            action,
            flags: 0,
            charset: ImportCharset::Utf8,
            map: vec![MapEntry {
                field: Key::from_id(f::FIRST),
                mapping: if action == ImportAction::UpdateMatching {
                    Mapping::Match
                } else {
                    Mapping::Import
                },
            }],
        },
        profile: Profile::new(FileType::TabSeparated),
        profile_ex: None,
        field_flags: None,
    }
}

#[test]
fn an_interrupted_import_stops_before_the_next_row() {
    let rows: String = (0..100).map(|i| format!("Row {i}\r")).collect();
    let mut mem = Mem::new();
    mem.interrupt_after = Some(3);
    let request = import_request(ImportAction::Add);
    let error = import::import(
        &mut mem,
        &request,
        rows.as_bytes(),
        &om_calc::Locale::en_us(),
    )
    .unwrap_err();
    assert_eq!(error.code, 1);
    // Three rows were tried (the probe file's "always" rules refuse them
    // here), the fourth check stopped the import.
    assert_eq!(mem.checks, 4);

    // Updates check before each row, and while matching each record.
    for action in [ImportAction::UpdateFoundSet, ImportAction::UpdateMatching] {
        let mut mem = Mem::new();
        for i in 0..10 {
            mem.add(&[(f::FIRST, &[&format!("Row {i}")])]);
        }
        mem.interrupt_after = Some(2);
        let request = import_request(action);
        let error = import::import(
            &mut mem,
            &request,
            rows.as_bytes(),
            &om_calc::Locale::en_us(),
        )
        .unwrap_err();
        assert_eq!(error.code, 1, "{action:?}");
        assert_eq!(mem.checks, 3, "{action:?}");
    }
}

/// A row the host refuses as one whose rule OpenMaker does not support
/// checking ends the import with 3 at that row (#571); a row's ordinary
/// error 3 (a lookup's invalid date, a record changed meanwhile) is a
/// skipped row, and the import goes on to the next.
#[test]
fn only_a_rule_the_host_cannot_check_ends_an_import() {
    for (error, ends) in [
        (om_transfer::TransferError::unsupported("unchecked"), true),
        (om_transfer::TransferError::new(3, "refused"), false),
    ] {
        let mut mem = Mem::new();
        mem.refuse_create = Some((2, error));
        let result = import::import(
            &mut mem,
            &import_request(ImportAction::Add),
            b"Ann\rBob\rCyd\r",
            &om_calc::Locale::en_us(),
        );
        match result {
            Err(error) => {
                assert!(ends, "{error:?}");
                assert_eq!(error.code, 3);
                assert_eq!(mem.creates, 2);
            }
            Ok(summary) => {
                assert!(!ends, "{summary:?}");
                assert_eq!(mem.creates, 3);
                assert!(summary.errors.iter().any(|e| e.row == 2 && e.code == 3));
                assert_eq!(summary.error_code(), 729);
            }
        }
    }
}

#[test]
fn an_interrupted_export_writes_nothing() {
    let options = |group_by: bool| ExportOptions {
        fields: vec![ExportField {
            field: FieldSpec::new(CONTACTS, f::FIRST),
            sum_by: FieldSpec::none(),
        }],
        group_by: if group_by {
            vec![(FieldSpec::new(CONTACTS, f::FIRST), 1)]
        } else {
            Vec::new()
        },
        formatting: false,
        charset: ExportCharset::Utf8,
        use_name_alias: Some(1),
        field_names: None,
    };
    for group_by in [false, true] {
        let mut mem = Mem::new();
        for i in 0..10 {
            mem.add(&[(f::FIRST, &[&format!("Row {i}")])]);
        }
        mem.sort_by(&[f::FIRST]);
        let request = ExportRequest::new(options(group_by), Profile::new(FileType::TabSeparated));
        assert!(export::export(&mut mem, &request).unwrap().is_some());
        mem.interrupt_after = Some(mem.checks + 4);
        let error = export::export(&mut mem, &request).unwrap_err();
        assert_eq!(error.code, 1, "group by {group_by}");
    }
}
