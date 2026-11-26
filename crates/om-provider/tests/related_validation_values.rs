//! Related validation precision, held-import state, and native merge regressions.
//! Synthetic loopback fixtures only.
#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
#[path = "common/related.rs"]
mod related;
use fixture::{CODE, NAME, OCCURRENCE, TABLE};
use om_data::runtime::TransactionOptions;
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
fn script(
    p: &mut impl DataProvider,
    field: u64,
    value: Option<&str>,
) -> Result<om_data::Record, om_data::DataError> {
    p.write_record(&ScriptWrite {
        layout: LAYOUT,
        table: None,
        record: Some(1),
        cells: [(field, value.map(str::to_owned))].into(),
        fields: Default::default(),
        typed: Default::default(),
        skip_validation: false,
        privilege_set: None,
        record_number: None,
        prepared: None,
        single_field_step: false,
    })
}
#[test]
fn precision_fill_only_keeps_existing_value() {
    let h = host("final-fill", tree(8, 0x500, Some("Name"), Some("Ship")));
    let mut p = h.open("Root.fmp12");
    let result = p.update_record(LAYOUT, 1, changes("Name", Some("Changed")));
    assert!(
        result.is_ok(),
        "fill-only Code already has a value: no related-list lookup is needed"
    );
}
#[test]
fn precision_no_op_dependency_does_not_generate_values() {
    let h = host("final-noop", tree(0x88, 0x500, Some("Name"), None));
    let mut p = h.open("Root.fmp12");
    let result = p.update_record(LAYOUT, 1, changes("Name", Some("Write tests")));
    assert!(
        result.is_ok(),
        "unchanged Name returns before generating or validating Code"
    );
}
#[test]
fn precision_clearing_last_reference_skips_auto_entry() {
    let h = host("final-clear", tree(0x88, 0x500, Some("Name"), Some("Ship")));
    let mut p = h.open("Root.fmp12");
    let result = p.update_record(LAYOUT, 1, changes("Name", None));
    assert!(
        result.is_ok(),
        "all referenced fields empty skips auto-entry and preserves Code"
    );
}
#[test]
fn precision_fill_only_bridge_does_not_dirty_its_dependents() {
    let mut r = tree(0x88, 0x500, Some("Extra"), Some("Ship"));
    fixture::set_field_options(&mut r, TABLE, EXTRA, 8);
    let mut names = fixture::names();
    names.add_field(TABLE, EXTRA, "Extra");
    fixture::add_formula(&mut r, &names, (TABLE, OCCURRENCE), EXTRA, 5, "Name");
    cell(&mut r, EXTRA, "Ship");
    let h = host("final-fill-chain", r);
    let mut p = h.open("Root.fmp12");
    let result = p.update_record(LAYOUT, 1, changes("Name", Some("Changed")));
    assert!(
        result.is_ok(),
        "unchanged fill-only Extra must not propagate dirtiness into Code: {result:?}"
    );
}
#[test]
fn precision_empty_generated_held_create_is_supported() {
    let h = host("final-empty-held", tree(0xa8, 0x500, Some("\"\""), None));
    let mut p = h.open("Root.fmp12");
    p.begin_transaction(Default::default()).unwrap();
    let result = p.create_record(LAYOUT, changes("Name", Some("New")));
    p.revert_transaction().unwrap();
    assert!(
        result.is_ok(),
        "generated empty values do not need a list lookup"
    );
}
#[test]
fn control_empty_generated_direct_create_is_supported() {
    let h = host("final-empty-direct", tree(0xa8, 0x500, Some("\"\""), None));
    let mut p = h.open("Root.fmp12");
    p.create_record(LAYOUT, changes("Name", Some("New")))
        .unwrap();
    assert_eq!(effects(&h)[2], 0);
}
#[test]
fn precision_empty_constant_held_create_is_supported() {
    let mut r = tree(4, 0x500, None, None);
    r.put(
        &[
            &uint_segment(TABLE),
            &[3],
            &[5],
            &uint_segment(CODE),
            &[0x0d],
        ],
        om_scripts::text_node(&[], ""),
    );
    let h = host("final-empty-constant", r);
    let mut p = h.open("Root.fmp12");
    p.begin_transaction(Default::default()).unwrap();
    let result = p.create_record(LAYOUT, changes("Name", Some("New")));
    p.revert_transaction().unwrap();
    assert!(
        result.is_ok(),
        "empty constant requires no related-list lookup: {result:?}"
    );
}
#[test]
fn completeness_constant_refuses_before_create() {
    let mut r = tree(4, 0x500, None, None);
    r.put(
        &[
            &uint_segment(TABLE),
            &[3],
            &[5],
            &uint_segment(CODE),
            &[0x0d],
        ],
        om_scripts::text_node(&[], "Ship"),
    );
    let h = host("final-constant", r);
    let mut p = h.open("Root.fmp12");
    p.begin_transaction(Default::default()).unwrap();
    let before = effects(&h);
    let result = p.create_record(LAYOUT, changes("Name", Some("New")));
    assert_eq!(result.unwrap_err().code, Some(3));
    assert_eq!(effects(&h), before);
    p.revert_transaction().unwrap();
}
#[test]
fn completeness_modification_variable_refuses_before_lock() {
    let mut r = tree(1, 0x500, None, None);
    fixture::add_field(&mut r, TABLE, CODE, "Code", 1, 1, 9, 1, 0x500);
    let h = host("final-modvar", r);
    let mut p = h.open("Root.fmp12");
    p.calc_schema().unwrap();
    let before = effects(&h);
    let result = p.update_record(LAYOUT, 1, changes("Extra", Some("New")));
    assert_eq!(result.unwrap_err().code, Some(3));
    assert_eq!(effects(&h), before);
}
#[test]
fn control_creation_variable_does_not_taint_update() {
    let mut r = tree(1, 0x500, None, Some("Ship"));
    fixture::add_field(&mut r, TABLE, CODE, "Code", 1, 1, 4, 1, 0x500);
    let h = host("final-createvar", r);
    let mut p = h.open("Root.fmp12");
    p.update_record(LAYOUT, 1, changes("Extra", Some("New")))
        .unwrap();
    assert_eq!(effects(&h)[2], 0);
}
#[test]
fn completeness_stored_chain_refuses_before_lock() {
    let mut r = tree(0x88, 0x500, Some("Extra"), None);
    fixture::add_field(&mut r, TABLE, EXTRA, "Extra", 2, 1, 0, 0, 0);
    fixture::add_formula(
        &mut r,
        &fixture::names(),
        (TABLE, OCCURRENCE),
        EXTRA,
        5,
        "Name",
    );
    let h = host("final-stored-chain", r);
    let mut p = h.open("Root.fmp12");
    p.calc_schema().unwrap();
    let before = effects(&h);
    let result = p.update_record(LAYOUT, 1, changes("Name", Some("New")));
    assert_eq!(result.unwrap_err().code, Some(3));
    assert_eq!(effects(&h), before);
}
#[test]
fn completeness_stored_calculation_survives_skip_auto_entry() {
    let mut r = tree(0, 0x500, Some("Name"), None);
    fixture::add_field(&mut r, TABLE, CODE, "Code", 2, 1, 0, 0, 0x500);
    let h = host("final-stored-skip", r);
    let mut p = h.open("Root.fmp12");
    p.begin_transaction(TransactionOptions {
        skip_auto_enter: true,
        ..Default::default()
    })
    .unwrap();
    p.calc_schema().unwrap();
    let before = effects(&h);
    let result = p.update_record(LAYOUT, 1, changes("Name", Some("New")));
    assert_eq!(result.unwrap_err().code, Some(3));
    assert_eq!(effects(&h), before);
    p.revert_transaction().unwrap();
}
#[test]
fn completeness_repeating_source_marks_base_dependency() {
    let mut r = tree(0x88, 0x500, Some("Last ( Name )"), None);
    fixture::set_field_repetitions(&mut r, TABLE, NAME, 3);
    let h = host("final-repeat-source", r);
    let mut p = h.open("Root.fmp12");
    p.calc_schema().unwrap();
    let before = effects(&h);
    let result = script(&mut p, NAME * 256 + 3, Some("Ship"));
    assert_eq!(result.unwrap_err().code, Some(3));
    assert_eq!(effects(&h), before);
}
#[test]
fn completeness_repeating_validated_cell_refuses_before_lock() {
    let mut r = tree(0, 0x500, None, None);
    fixture::set_field_repetitions(&mut r, TABLE, CODE, 3);
    let h = host("final-repeat-target", r);
    let mut p = h.open("Root.fmp12");
    p.calc_schema().unwrap();
    let before = effects(&h);
    let result = script(&mut p, CODE * 256 + 3, Some("Ship"));
    assert_eq!(result.unwrap_err().code, Some(3));
    assert_eq!(effects(&h), before);
}
#[test]
fn control_script_data_entry_only_validation_is_skipped() {
    let h = host(
        "final-dataentry-script",
        tree(0x88, 0x100, Some("Name"), None),
    );
    let mut p = h.open("Root.fmp12");
    script(&mut p, NAME, Some("Ship")).unwrap();
    assert_eq!(effects(&h)[2], 0);
}
#[test]
fn control_transaction_skip_validation_respects_data_entry_only() {
    let h = host(
        "final-dataentry-skip",
        tree(0x88, 0x100, Some("Name"), None),
    );
    let mut p = h.open("Root.fmp12");
    p.begin_transaction(TransactionOptions {
        skip_validation: true,
        ..Default::default()
    })
    .unwrap();
    p.update_record(LAYOUT, 1, changes("Name", Some("Ship")))
        .unwrap();
    p.commit_transaction(false).unwrap();
    assert_eq!(effects(&h)[2], 0);
}
#[test]
fn control_import_keeps_explicit_empty_auto_enter_cell() {
    let h = host("final-import-empty", tree(0x88, 0x500, Some("Name"), None));
    let mut p = h.open("Root.fmp12");
    p.begin_transaction(Default::default()).unwrap();
    p.import_record(
        LAYOUT,
        None,
        &[(NAME, Some("New".into())), (CODE, None)].into(),
        true,
    )
    .unwrap();
    p.commit_transaction(false).unwrap();
    assert_eq!(effects(&h)[2], 0);
}
#[test]
fn completeness_import_must_preflight_merged_held_changes() {
    let h = host(
        "final-import-held",
        tree(0x8000_0088, 0x510, Some("Name"), None),
    );
    let mut p = h.open("Root.fmp12");
    p.begin_transaction(TransactionOptions {
        skip_auto_enter: true,
        ..Default::default()
    })
    .unwrap();
    p.update_record(LAYOUT, 1, changes("Name", Some("New unique")))
        .unwrap();
    let before = effects(&h);
    let result = p.import_record(
        LAYOUT,
        Some(1),
        &[(EXTRA, Some("More".into()))].into(),
        true,
    );
    let after = effects(&h);
    p.revert_transaction().unwrap();
    assert_eq!(result.unwrap_err().code, Some(3));
    assert_eq!(after,before,"the import must include held Name when finding generated Code, before unique-index locks/queries");
}
#[test]
fn completeness_import_of_held_new_must_use_create_mode() {
    let h = host(
        "final-import-held-new",
        tree(0x8000_00a8, 0x510, Some("\"Ship\""), None),
    );
    let mut p = h.open("Root.fmp12");
    p.begin_transaction(TransactionOptions {
        skip_auto_enter: true,
        ..Default::default()
    })
    .unwrap();
    let id = p
        .create_record(LAYOUT, changes("Name", Some("New")))
        .unwrap()
        .id;
    let before = effects(&h);
    let result = p.import_record(
        LAYOUT,
        Some(id),
        &[(EXTRA, Some("More".into()))].into(),
        true,
    );
    let after = effects(&h);
    p.revert_transaction().unwrap();
    assert_eq!(result.unwrap_err().code, Some(3));
    assert_eq!(
        after, before,
        "a held new row computes in Create mode even when record id is Some"
    );
}
fn merge_object(r: &mut OwnedNode, token: &str) {
    r.put(
        &[&[4], &[5], &[1], &[7]],
        OwnedNode::leaf(Vec::new(), vec![8, 1, 18, 4, 8, 2, 16, 1]),
    );
    let text = om_scsu::encode_scsu(token);
    let literal = [vec![0x13, text.len() as u8], text, vec![0]].concat();
    r.put(
        &[&[4], &[5], &[1], &[5], &[1], &[2], &[6]],
        OwnedNode::leaf(Vec::new(), literal),
    );
}
#[test]
fn merge_mixed_local_remote_high_ids_repetitions_and_owning_names() {
    let mut r = fixture::tree(&[], true);
    reference(&mut r, (REMOTE, "Remote Tasks"), 1, "Related");
    fixture::add_field(&mut r, TABLE, 0x8123, "Root high", 1, 1, 0, 0, 0);
    merge_object(
        &mut r,
        "L=<<\u{1}\u{1}\u{10}0003D00001028123>> R=<<\u{1}\u{2}\u{10}0003D00040028123>>",
    );
    let mut child = fixture::tree(&[], true);
    fixture::add_field(&mut child, TABLE, 0x8123, "Child high", 1, 1, 0, 0, 0);
    fixture::set_field_repetitions(&mut child, TABLE, 0x8123, 3);
    let h = Host::new(
        "final-merge-high",
        vec![("Root.fmp12", r), ("Related.fmp12", child)],
    );
    let mut p = h.open("Root.fmp12");
    let rendered = p.render_layout(LAYOUT).unwrap().unwrap();
    assert_eq!(
        rendered.layout.objects.get(1).unwrap().text.as_deref(),
        Some("L=<<Root high>> R=<<Remote Tasks::Child high>>")
    );
    assert_eq!(
        h.captures.opens_and_logins().0.len(),
        2,
        "render must hydrate without a prior explicit schema request"
    );
}
#[test]
fn merge_unavailable_reference_preserves_entire_stored_object() {
    let mut r = fixture::tree(&[], true);
    reference(&mut r, (REMOTE, "Missing"), 1, "Absent");
    let token = "L=<<\u{1}\u{1}\u{e}0003D000010101>> R=<<\u{1}\u{1}\u{e}0003D000400101>>";
    merge_object(&mut r, token);
    let h = Host::new("final-merge-missing", vec![("Root.fmp12", r)]);
    let mut p = h.open("Root.fmp12");
    assert_eq!(
        p.render_layout(LAYOUT)
            .unwrap()
            .unwrap()
            .layout
            .objects
            .get(1)
            .unwrap()
            .text
            .as_deref(),
        Some(token)
    );
}
#[test]
fn related_layout_user_writes_stay_refused_and_script_writes_are_held_in_transactions() {
    let mut r = fixture::tree(&[], true);
    reference(&mut r, (REMOTE, "Remote Tasks"), 1, "Related");
    r.put(
        &[&[4], &[5], &[1], &[2]],
        OwnedNode::leaf(Vec::new(), layout_def(REMOTE)),
    );
    let h = host("final-related-layout", r);
    let mut p = h.open("Root.fmp12");
    p.calc_schema().unwrap();
    assert!(p.render_layout(LAYOUT).unwrap().is_some());
    let capabilities = p.record_capabilities(LAYOUT).unwrap();
    assert!(capabilities.update && capabilities.delete && capabilities.create);
    let before = effects(&h);
    p.begin_transaction(Default::default()).unwrap();
    assert_eq!(
        p.create_record(LAYOUT, changes("Name", Some("New")))
            .unwrap_err()
            .code,
        Some(3)
    );
    assert_eq!(p.delete_record(LAYOUT, 1).unwrap_err().code, Some(3));
    assert_eq!(
        p.update_record(LAYOUT, 1, changes("Name", Some("Changed")))
            .unwrap_err()
            .code,
        Some(3)
    );
    p.revert_transaction().unwrap();
    assert_eq!(effects(&h), before);
    // A script's write on the layout is held in the transaction, in
    // Related's part of it, and committed by Commit Transaction (#1006).
    p.begin_transaction(Default::default()).unwrap();
    script(&mut p, NAME, Some("Held")).unwrap();
    assert_eq!(effects(&h)[3], before[3], "nothing committed yet");
    p.commit_transaction(false).unwrap();
    assert_eq!(effects(&h)[3], before[3] + 1);
    let page = h.open("Related.fmp12").records(LAYOUT, 0, 10).unwrap();
    let at = page.fields.iter().position(|f| f.name == "Name").unwrap();
    assert_eq!(page.records[0].values[at].as_deref(), Some("Held"));
}

fn layout_def(occurrence: u64) -> Vec<u8> {
    let mut alias = om_hbam::serial::decode(&uint_segment(occurrence)).unwrap();
    let mut out = vec![0x10];
    loop {
        let byte = (alias & 0x7f) as u8;
        alias >>= 7;
        if alias == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
    out
}
