//! Held validation, calculation deferral and owning-file metadata regressions.
//! Synthetic loopback fixtures only.
#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
#[path = "common/related.rs"]
mod related;
use fixture::{CODE, NAME, OCCURRENCE, TABLE};
use om_data::{DataProvider, FieldChanges, ScriptWrite};
use om_hbam::{slot::uint_segment, OwnedNode};
use om_protocol::Opcode;
use related::{reference, Host};
const REMOTE: u64 = 0xd0_0040;
const EXTRA: u64 = 99;
const LAYOUT: u32 = fixture::LAYOUT as u32;
fn tree(
    options: u32,
    validation: u32,
    expression: Option<&str>,
    existing: Option<&str>,
) -> OwnedNode {
    let mut r = fixture::tree(&[], true);
    fixture::add_field(&mut r, TABLE, EXTRA, "Extra", 1, 1, 0, 0, 0);
    reference(&mut r, (REMOTE, "Remote Tasks"), 1, "Related");
    fixture::set_field_options(&mut r, TABLE, CODE, options);
    fixture::set_field_validation(&mut r, TABLE, CODE, validation);
    let mut names = fixture::names();
    names.add_field(TABLE, EXTRA, "Extra");
    if let Some(e) = expression {
        fixture::add_formula(&mut r, &names, (TABLE, OCCURRENCE), CODE, 5, e);
    }
    r.put(
        &[&uint_segment(TABLE), &[3], &[5], &uint_segment(CODE), &[7]],
        OwnedNode::leaf(
            Vec::new(),
            vec![2, 1, 5, 0x90, 0, 0, 2, 1, 0, 0, 0, 0, 0xff],
        ),
    );
    r.put(
        &[&[0x21], &[5], &[5], &[0x10]],
        om_scripts::text_node(&[], "Related names"),
    );
    let mut list = vec![
        1, 0, 0, 4, 3, 0xd0, 0, 0x40, 2, 1, 1, 0, 1, 0, 0, 0, 0, 0, 0, 0,
    ];
    list.extend(om_scsu::encode_counted(" "));
    r.put(
        &[&[0x21], &[5], &[5], &[2]],
        OwnedNode::leaf(Vec::new(), list),
    );
    if let Some(e) = existing {
        cell(&mut r, CODE, e);
    }
    r
}
fn cell(r: &mut OwnedNode, field: u64, value: &str) {
    r.put(
        &[
            &uint_segment(TABLE),
            &[5],
            &om_hbam::serial::encode(1),
            &uint_segment(field),
        ],
        om_scripts::text_node(&[], value),
    );
}
fn host(tag: &str, r: OwnedNode) -> Host {
    Host::new(
        tag,
        vec![
            ("Root.fmp12", r),
            ("Related.fmp12", fixture::tree(&[], true)),
        ],
    )
}
fn effects(h: &Host) -> [usize; 4] {
    [
        Opcode::Lock,
        Opcode::HostCreateRecord,
        Opcode::QueryProgram,
        Opcode::HostCommit,
    ]
    .map(|o| h.captures.with_opcode(o).len())
}
fn changes(field: &str, value: Option<&str>) -> FieldChanges {
    [(field.into(), value.map(str::to_owned))].into()
}
#[path = "common/privileges.rs"]
mod privileges;
use om_data::RecordPage;
use std::collections::BTreeMap;
type Cells = BTreeMap<u64, Option<String>>;
fn cells(values: &[(u64, Option<&str>)]) -> Cells {
    values
        .iter()
        .map(|(k, v)| (*k, v.map(str::to_owned)))
        .collect()
}
fn write(
    p: &mut impl DataProvider,
    id: Option<i64>,
    values: &[(u64, Option<&str>)],
) -> Result<om_data::Record, om_data::DataError> {
    p.write_record(&ScriptWrite {
        layout: LAYOUT,
        table: None,
        record: id,
        cells: cells(values),
        fields: Default::default(),
        typed: Default::default(),
        skip_validation: false,
        privilege_set: None,
        record_number: None,
        prepared: None,
        single_field_step: false,
        assignments: None,
    })
}
fn text(page: &RecordPage, id: i64, field: &str) -> Option<String> {
    let at = page.fields.iter().position(|f| f.name == field).unwrap();
    page.records.iter().find(|r| r.id == id).unwrap().values[at].clone()
}
fn value(p: &mut impl DataProvider, id: i64, field: &str) -> Option<String> {
    text(&p.records(LAYOUT, 0, 100).unwrap(), id, field)
}
#[test]
fn required_validation_waits_until_commit_and_can_be_completed() {
    let mut r = tree(0xa8, 0x500, Some("\"\""), None);
    fixture::set_field_validation(&mut r, TABLE, EXTRA, 0x408);
    let h = host("review-deferred-fix", r);
    let mut p = h.open("Root.fmp12");
    p.begin_transaction(Default::default()).unwrap();
    let id = p
        .create_record(LAYOUT, changes("Name", Some("Partial")))
        .expect("required Extra defers to commit")
        .id;
    p.update_record(LAYOUT, id, changes("Extra", Some("Done")))
        .unwrap();
    p.commit_transaction(false).unwrap();
    assert_eq!(value(&mut p, id, "Extra").as_deref(), Some("Done"));
    assert_eq!(effects(&h)[2], 0);
}
#[test]
fn required_validation_still_aborts_incomplete_commit() {
    let mut r = tree(0xa8, 0x500, Some("\"\""), None);
    fixture::set_field_validation(&mut r, TABLE, EXTRA, 0x408);
    let h = host("review-deferred-reject", r);
    let mut p = h.open("Root.fmp12");
    p.begin_transaction(Default::default()).unwrap();
    let id = p
        .create_record(LAYOUT, changes("Name", Some("Partial")))
        .unwrap()
        .id;
    assert_eq!(p.commit_transaction(false).unwrap_err().code, Some(509));
    assert_eq!(effects(&h)[3], 0);
    assert!(!p
        .records(LAYOUT, 0, 100)
        .unwrap()
        .records
        .iter()
        .any(|r| r.id == id));
}
#[test]
fn calculation_validation_is_deferred_by_preview() {
    let mut r = tree(0xa8, 0x500, Some("\"\""), None);
    let mut names = fixture::names();
    names.add_field(TABLE, EXTRA, "Extra");
    fixture::set_field_validation(&mut r, TABLE, EXTRA, 0x401);
    fixture::add_formula(
        &mut r,
        &names,
        (TABLE, OCCURRENCE),
        EXTRA,
        0x19,
        "Extra = \"Done\"",
    );
    let h = host("review-deferred-calc", r);
    let mut p = h.open("Root.fmp12");
    p.begin_transaction(Default::default()).unwrap();
    p.update_record(LAYOUT, 1, changes("Extra", Some("Partial")))
        .expect("calculation validation defers");
    p.update_record(LAYOUT, 1, changes("Extra", Some("Done")))
        .unwrap();
    p.commit_transaction(false).unwrap();
    assert_eq!(value(&mut p, 1, "Extra").as_deref(), Some("Done"));
}
#[test]
fn script_new_explicit_empty_suppresses_generation_through_later_user_edit() {
    let h = host(
        "review-new-script-empty",
        tree(0xa8, 0x500, Some("\"Ship\""), None),
    );
    let mut p = h.open("Root.fmp12");
    p.begin_transaction(Default::default()).unwrap();
    let id = write(&mut p, None, &[(NAME, Some("New")), (CODE, None)])
        .unwrap()
        .id;
    p.update_record(LAYOUT, id, changes("Extra", Some("Later")))
        .unwrap();
    p.commit_transaction(false).unwrap();
    assert_eq!(value(&mut p, id, "Code"), None);
    assert_eq!(effects(&h)[2], 0);
}
#[test]
fn user_new_explicit_empty_does_not_suppress_generation() {
    let h = host(
        "review-new-user-empty",
        tree(0xa8, 0x500, Some("\"Ship\""), None),
    );
    let mut p = h.open("Root.fmp12");
    p.begin_transaction(Default::default()).unwrap();
    let before = effects(&h);
    let result = p.create_record(
        LAYOUT,
        [("Name".into(), Some("New".into())), ("Code".into(), None)].into(),
    );
    assert_eq!(result.unwrap_err().code, Some(3));
    assert_eq!(effects(&h), before);
    p.revert_transaction().unwrap();
}
#[test]
fn existing_script_identity_survives_later_user_edit() {
    let h = host(
        "review-sticky-script",
        tree(0x88, 0x100, Some("Name"), None),
    );
    let mut p = h.open("Root.fmp12");
    p.begin_transaction(Default::default()).unwrap();
    write(&mut p, Some(1), &[(NAME, Some("New"))]).unwrap();
    p.update_record(LAYOUT, 1, changes("Extra", Some("Later")))
        .unwrap();
    p.commit_transaction(false).unwrap();
    assert_eq!(value(&mut p, 1, "Code").as_deref(), Some("New"));
    assert_eq!(effects(&h)[2], 0);
}
#[test]
fn imported_new_later_edit_uses_update_mode() {
    let h = host(
        "review-imported-new-mode",
        tree(0xa8, 0x500, Some("\"Ship\""), None),
    );
    let mut p = h.open("Root.fmp12");
    p.begin_transaction(Default::default()).unwrap();
    let id = p
        .import_record(LAYOUT, None, &cells(&[(NAME, Some("Imported"))]), false)
        .unwrap();
    p.update_record(LAYOUT, id, changes("Extra", Some("Later")))
        .unwrap();
    p.commit_transaction(false).unwrap();
    assert_eq!(value(&mut p, id, "Code"), None);
    assert_eq!(effects(&h)[2], 0);
}
#[test]
fn imported_existing_fill_only_reads_imported_base() {
    let h = host(
        "review-imported-fill",
        tree(8, 0x500, Some("Name"), Some("Ship")),
    );
    let mut p = h.open("Root.fmp12");
    p.begin_transaction(Default::default()).unwrap();
    p.import_record(LAYOUT, Some(1), &cells(&[(NAME, Some("Imported"))]), false)
        .unwrap();
    p.update_record(LAYOUT, 1, changes("Name", Some("Later")))
        .unwrap();
    p.commit_transaction(false).unwrap();
    assert_eq!(value(&mut p, 1, "Code").as_deref(), Some("Ship"));
    assert_eq!(value(&mut p, 1, "Name").as_deref(), Some("Later"));
    assert_eq!(effects(&h)[2], 0);
}
#[test]
fn imported_later_generated_value_refuses_before_any_additional_effect() {
    let h = host(
        "review-import-later-refuse",
        tree(0x8000_0088, 0x510, Some("Name"), None),
    );
    let mut p = h.open("Root.fmp12");
    p.begin_transaction(Default::default()).unwrap();
    p.import_record(LAYOUT, Some(1), &cells(&[(EXTRA, Some("Imported"))]), false)
        .unwrap();
    let before = effects(&h);
    let result = write(&mut p, Some(1), &[(NAME, Some("Later"))]);
    assert_eq!(result.unwrap_err().code, Some(3));
    assert_eq!(effects(&h), before);
    p.revert_transaction().unwrap();
}
#[test]
fn imported_later_script_identity_survives_user_edit() {
    let h = host(
        "review-import-script",
        tree(0x88, 0x100, Some("Name"), None),
    );
    let mut p = h.open("Root.fmp12");
    p.begin_transaction(Default::default()).unwrap();
    p.import_record(LAYOUT, Some(1), &cells(&[(EXTRA, Some("Imported"))]), false)
        .unwrap();
    write(&mut p, Some(1), &[(NAME, Some("Script"))]).unwrap();
    p.update_record(LAYOUT, 1, changes("Extra", Some("Later")))
        .unwrap();
    p.commit_transaction(false).unwrap();
    assert_eq!(value(&mut p, 1, "Code").as_deref(), Some("Script"));
    assert_eq!(effects(&h)[2], 0);
}
#[test]
fn repeated_import_on_imported_new_does_not_rerun_creation_constant() {
    let h = host(
        "review-repeat-import",
        tree(0xa8, 0x500, Some("\"Ship\""), None),
    );
    let mut p = h.open("Root.fmp12");
    p.begin_transaction(Default::default()).unwrap();
    let id = p
        .import_record(LAYOUT, None, &cells(&[(NAME, Some("Imported"))]), false)
        .unwrap();
    p.import_record(LAYOUT, Some(id), &cells(&[(EXTRA, Some("Next row"))]), true)
        .unwrap();
    p.commit_transaction(false).unwrap();
    assert_eq!(value(&mut p, id, "Code"), None);
    assert_eq!(effects(&h)[2], 0);
}
#[test]
fn nonempty_generation_refuses_unheld_import_before_reservation_or_lock() {
    for record in [None, Some(1)] {
        let h = host(
            "review-import-unheld",
            tree(0x8000_00a8, 0x510, Some("Name"), None),
        );
        let mut p = h.open("Root.fmp12");
        p.begin_transaction(Default::default()).unwrap();
        let before = effects(&h);
        let result = p.import_record(LAYOUT, record, &cells(&[(NAME, Some("Imported"))]), true);
        assert_eq!(result.unwrap_err().code, Some(3));
        assert_eq!(effects(&h), before);
        p.revert_transaction().unwrap();
    }
}
#[test]
fn no_op_direct_related_list_value_is_supported() {
    let h = host("review-explicit-noop", tree(0, 0x500, None, Some("Ship")));
    let mut p = h.open("Root.fmp12");
    p.update_record(LAYOUT, 1, changes("Code", Some("Ship")))
        .unwrap();
    assert_eq!(effects(&h)[2], 0);
}
#[test]
fn generated_unchanged_value_is_supported() {
    let h = host(
        "review-generated-noop",
        tree(0x88, 0x500, Some("Left ( Name ; 4 )"), Some("Ship")),
    );
    let mut p = h.open("Root.fmp12");
    p.update_record(LAYOUT, 1, changes("Name", Some("Ship more")))
        .unwrap();
    assert_eq!(effects(&h)[2], 0);
}
#[test]
fn repeating_import_refuses_only_nonempty_changed_repetitions() {
    let mut r = tree(0, 0x500, None, None);
    fixture::set_field_repetitions(&mut r, TABLE, CODE, 3);
    let h = host("review-repeating-import", r);
    let mut p = h.open("Root.fmp12");
    p.begin_transaction(Default::default()).unwrap();
    p.import_record(LAYOUT, Some(1), &cells(&[(CODE * 256 + 3, None)]), false)
        .unwrap();
    let before = effects(&h);
    let result = p.import_record(
        LAYOUT,
        Some(1),
        &cells(&[(CODE * 256 + 3, Some("Ship"))]),
        false,
    );
    assert_eq!(result.unwrap_err().code, Some(3));
    assert_eq!(effects(&h), before);
    p.revert_transaction().unwrap();
}
#[test]
fn displayed_stale_snapshot_is_refused_and_competing_values_survive() {
    let h = host("review-stale", tree(8, 0x500, Some("Name"), Some("Ship")));
    let mut p = h.open("Root.fmp12");
    let mut other = h.open("Root.fmp12");
    p.records(LAYOUT, 0, 100).unwrap();
    other
        .update_record(LAYOUT, 1, changes("Extra", Some("Other")))
        .unwrap();
    let before = effects(&h);
    assert_eq!(
        p.update_record(LAYOUT, 1, changes("Name", Some("Stale")))
            .unwrap_err()
            .code,
        Some(306)
    );
    assert_eq!(effects(&h)[3], before[3]);
    let mut check = h.open("Root.fmp12");
    assert_eq!(value(&mut check, 1, "Name").as_deref(), Some("Write tests"));
    assert_eq!(value(&mut check, 1, "Extra").as_deref(), Some("Other"));
}
#[test]
fn locked_supported_snapshot_is_refused_and_lock_is_released() {
    let h = host("review-lock", tree(8, 0x500, Some("Name"), Some("Ship")));
    let mut p = h.open("Root.fmp12");
    let mut other = h.open("Root.fmp12");
    other.begin_transaction(Default::default()).unwrap();
    other
        .update_record(LAYOUT, 1, changes("Extra", Some("Other")))
        .unwrap();
    let before = effects(&h);
    assert_eq!(
        p.update_record(LAYOUT, 1, changes("Name", Some("Locked")))
            .unwrap_err()
            .code,
        Some(301)
    );
    assert_eq!(effects(&h)[3], before[3]);
    other.revert_transaction().unwrap();
    p.update_record(LAYOUT, 1, changes("Name", Some("After release")))
        .unwrap();
}
#[test]
fn main_integration_preserves_root_privileges_and_related_hydration_together() {
    let mut r = privileges::tree(&[], privileges::CUSTOM_KEY);
    reference(&mut r, (REMOTE, "Remote Tasks"), 1, "Related");
    let h = host("review-main70", r);
    let mut p = h.open("Root.fmp12");
    assert_eq!(p.account_privileges().unwrap().account_set_name, "Office");
    let schema = p.calc_schema().unwrap().unwrap();
    assert!(schema.table_of(REMOTE).is_some());
    assert_eq!(h.captures.opens_and_logins().0.len(), 2);
    assert_eq!(p.account_privileges().unwrap().extended, ["zeta", "alpha"]);
    assert_eq!(
        p.full_access_privileges().unwrap().extended,
        ["fmapp", "full-only"]
    );
    p.close();
    assert!(p.account_privileges().is_none());
    assert!(p.full_access_privileges().is_none());
}
#[test]
fn whitespace_and_unicode_generated_values_are_nonempty_for_preflight() {
    for formula in ["\" \"", "Char ( 13 )", "\"船\""] {
        let h = host("review-nonempty", tree(0xa8, 0x500, Some(formula), None));
        let mut p = h.open("Root.fmp12");
        p.begin_transaction(Default::default()).unwrap();
        let before = effects(&h);
        assert_eq!(
            p.create_record(LAYOUT, changes("Name", Some("New")))
                .unwrap_err()
                .code,
            Some(3),
            "{formula}"
        );
        assert_eq!(effects(&h), before);
        p.revert_transaction().unwrap();
    }
}
#[test]
fn typed_script_cell_keeps_data_entry_validation_and_refuses_before_effects() {
    let h = host("review-typed-entry", tree(0, 0x100, None, None));
    let mut p = h.open("Root.fmp12");
    p.begin_transaction(Default::default()).unwrap();
    let before = effects(&h);
    let result = p.write_record(&ScriptWrite {
        layout: LAYOUT,
        table: None,
        record: Some(1),
        cells: cells(&[(CODE, Some("Ship"))]),
        fields: Default::default(),
        typed: [CODE].into(),
        skip_validation: true,
        privilege_set: None,
        record_number: None,
        prepared: None,
        single_field_step: false,
        assignments: None,
    });
    assert_eq!(result.unwrap_err().code, Some(3));
    assert_eq!(effects(&h), before);
    p.revert_transaction().unwrap();
}
fn temporary_error_tree(related_validation: bool) -> OwnedNode {
    let mut r = tree(0, if related_validation { 0x500 } else { 0 }, None, None);
    fixture::add_field(&mut r, TABLE, EXTRA, "Extra", 2, 2, 0, 0, 0);
    fixture::add_formula(
        &mut r,
        &fixture::names(),
        (TABLE, OCCURRENCE),
        EXTRA,
        5,
        "1 / Count",
    );
    r
}
fn temporary_error_transaction(related_validation: bool, create: bool) {
    let h = host(
        "review-temporary-compute",
        temporary_error_tree(related_validation),
    );
    let mut p = h.open("Root.fmp12");
    p.begin_transaction(Default::default()).unwrap();
    let held = write(
        &mut p,
        if create { None } else { Some(1) },
        &[(NAME, Some("New")), (fixture::COUNT, Some("0"))],
    );
    assert!(held.is_ok(), "stored calculation must run at commit, after later held inputs are complete: {held:?}; effects={:?}", effects(&h));
    let id = held.unwrap().id;
    write(&mut p, Some(id), &[(fixture::COUNT, Some("2"))]).unwrap();
    p.commit_transaction(false).unwrap();
    assert_eq!(value(&mut p, id, "Extra").as_deref(), Some(".5"));
    assert_eq!(value(&mut p, id, "Code"), None);
    assert_eq!(effects(&h)[2], 0);
}
#[test]
fn temporary_calculation_error_control_without_related_validation() {
    temporary_error_transaction(false, false);
    temporary_error_transaction(false, true);
}
#[test]
fn temporary_calculation_error_with_untouched_related_list_in_held_edit() {
    temporary_error_transaction(true, false);
}
#[test]
fn temporary_calculation_error_with_untouched_related_list_in_held_new() {
    temporary_error_transaction(true, true);
}
#[test]
fn temporary_calculation_error_is_refused_at_commit_control() {
    let h = host("review-compute-commit", temporary_error_tree(false));
    let mut p = h.open("Root.fmp12");
    p.begin_transaction(Default::default()).unwrap();
    write(&mut p, Some(1), &[(fixture::COUNT, Some("0"))]).unwrap();
    assert_eq!(p.commit_transaction(false).unwrap_err().code, Some(3));
    assert_eq!(effects(&h)[3], 0);
}

fn dependent_temporary_error_tree() -> OwnedNode {
    let mut root = temporary_error_tree(true);
    fixture::set_field_options(&mut root, TABLE, CODE, 0xa8);
    let mut names = fixture::names();
    names.add_field(TABLE, EXTRA, "Extra");
    fixture::add_formula(
        &mut root,
        &names,
        (TABLE, OCCURRENCE),
        CODE,
        5,
        "If ( Extra < 1 ; \"\" ; \"Ship\" )",
    );
    // A failed recomputation must not reuse this stale value to generate Code.
    cell(&mut root, EXTRA, "2");
    root
}

#[test]
fn pending_dependency_does_not_use_its_stale_value_for_related_validation() {
    for create in [false, true] {
        let h = host("held-pending-dependency", dependent_temporary_error_tree());
        let mut p = h.open("Root.fmp12");
        p.begin_transaction(Default::default()).unwrap();
        let held = write(
            &mut p,
            if create { None } else { Some(1) },
            &[(NAME, Some("New")), (fixture::COUNT, Some("0"))],
        )
        .expect("an unresolved dependent calculation is deferred");
        write(&mut p, Some(held.id), &[(fixture::COUNT, Some("2"))]).unwrap();
        p.commit_transaction(false).unwrap();
        assert_eq!(value(&mut p, held.id, "Extra").as_deref(), Some(".5"));
        assert_eq!(value(&mut p, held.id, "Code"), None);
        assert_eq!(effects(&h)[2], 0);
    }
}

#[test]
fn unresolved_dependency_still_refuses_commit_without_index_or_write_effects() {
    let h = host("held-pending-commit", dependent_temporary_error_tree());
    let mut p = h.open("Root.fmp12");
    p.begin_transaction(Default::default()).unwrap();
    write(&mut p, Some(1), &[(fixture::COUNT, Some("0"))]).unwrap();
    let before = effects(&h);
    let error = p.commit_transaction(false).unwrap_err();
    assert_eq!(error.code, Some(3));
    assert!(error.message.contains("calculation failed"), "{error:?}");
    assert_eq!(effects(&h)[2..], before[2..]);
    assert_eq!(value(&mut p, 1, "Extra").as_deref(), Some("2"));
}

#[test]
fn unrelated_calculation_failure_does_not_hide_known_generated_related_value() {
    for create in [false, true] {
        let mut root = temporary_error_tree(true);
        fixture::set_field_options(&mut root, TABLE, CODE, 0x88);
        fixture::add_formula(
            &mut root,
            &fixture::names(),
            (TABLE, OCCURRENCE),
            CODE,
            5,
            "Name",
        );
        let h = host("held-independent-generated", root);
        let mut p = h.open("Root.fmp12");
        p.begin_transaction(Default::default()).unwrap();
        let before = effects(&h);
        let error = write(
            &mut p,
            if create { None } else { Some(1) },
            &[(NAME, Some("Ship")), (fixture::COUNT, Some("0"))],
        )
        .unwrap_err();
        assert_eq!(error.code, Some(3));
        assert!(error.message.contains("value list"), "{error:?}");
        assert_eq!(effects(&h), before);
        p.revert_transaction().unwrap();
    }
}

#[test]
fn immediate_script_edits_and_imports_keep_strict_calculation_errors() {
    for operation in [0, 1, 2] {
        let h = host("strict-immediate-calculation", temporary_error_tree(true));
        let mut p = h.open("Root.fmp12");
        let before = effects(&h);
        let error = match operation {
            0 => write(&mut p, Some(1), &[(fixture::COUNT, Some("0"))]).map(|_| ()),
            1 => p
                .import_record(
                    LAYOUT,
                    Some(1),
                    &cells(&[(fixture::COUNT, Some("0"))]),
                    true,
                )
                .map(|_| ()),
            _ => p
                .import_record(LAYOUT, None, &cells(&[(fixture::COUNT, Some("0"))]), true)
                .map(|_| ()),
        }
        .unwrap_err();
        assert_eq!(error.code, Some(3));
        assert!(error.message.contains("calculation failed"), "{error:?}");
        assert_eq!(effects(&h), before);
    }
}
