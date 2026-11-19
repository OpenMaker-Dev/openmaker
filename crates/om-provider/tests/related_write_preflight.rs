//! Generated validation candidates and native merge names across hosted files.
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
const LAYOUT: u32 = fixture::LAYOUT as u32;

fn autoenter_host(tag: &str) -> Host {
    let mut root = fixture::tree(&[], true);
    fixture::add_field(&mut root, TABLE, 99, "Extra", 1, 1, 0, 0, 0);
    reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    // Code automatically follows Name and validates Always against a related field.
    fixture::set_field_options(&mut root, TABLE, CODE, 0x88);
    fixture::set_field_validation(&mut root, TABLE, CODE, 0x500);
    fixture::add_formula(
        &mut root,
        &fixture::names(),
        (TABLE, OCCURRENCE),
        CODE,
        5,
        "Name",
    );
    root.put(
        &[&uint_segment(TABLE), &[3], &[5], &uint_segment(CODE), &[7]],
        OwnedNode::leaf(
            Vec::new(),
            vec![2, 1, 5, 0x90, 0, 0, 2, 1, 0, 0, 0, 0, 0xff],
        ),
    );
    root.put(
        &[&[0x21], &[5], &[5], &[0x10]],
        om_scripts::text_node(&[], "Related names"),
    );
    let mut list = vec![
        1, 0, 0, 4, 3, 0xd0, 0, 0x40, 2, 1, 1, 0, 1, 0, 0, 0, 0, 0, 0, 0,
    ];
    list.extend(om_scsu::encode_counted(" "));
    root.put(
        &[&[0x21], &[5], &[5], &[2]],
        OwnedNode::leaf(Vec::new(), list),
    );
    Host::new(
        tag,
        vec![
            ("Root.fmp12", root),
            ("Related.fmp12", fixture::tree(&[], true)),
        ],
    )
}

fn effects(host: &Host) -> [usize; 4] {
    [
        Opcode::Lock,
        Opcode::HostCreateRecord,
        Opcode::QueryProgram,
        Opcode::HostCommit,
    ]
    .map(|op| host.captures.with_opcode(op).len())
}

#[test]
fn autoentered_list_edit_refuses_before_lock() {
    let host = autoenter_host("regression-auto-edit");
    let mut provider = host.open("Root.fmp12");
    provider.calc_schema().unwrap();
    let before = effects(&host);
    let result = provider.update_record(
        LAYOUT,
        1,
        FieldChanges::from([("Name".into(), Some("Ship".into()))]),
    );
    assert_eq!(result.unwrap_err().code, Some(3));
    assert_eq!(
        effects(&host),
        before,
        "auto-entered related-list validation must precede locks and queries"
    );
}

#[test]
fn autoentered_list_transaction_create_refuses_before_reserving_id() {
    let host = autoenter_host("regression-auto-create");
    let mut provider = host.open("Root.fmp12");
    provider.calc_schema().unwrap();
    provider.begin_transaction(Default::default()).unwrap();
    let before = effects(&host);
    let created = provider.create_record(
        LAYOUT,
        FieldChanges::from([("Name".into(), Some("Ship".into()))]),
    );
    assert_eq!(created.unwrap_err().code, Some(3));
    provider.revert_transaction().unwrap();
    assert_eq!(
        effects(&host),
        before,
        "an unsupported generated value must not consume an id first"
    );
}

#[test]
fn autoentered_list_import_refuses_before_reserving_id() {
    let host = autoenter_host("regression-auto-import");
    let mut provider = host.open("Root.fmp12");
    provider.calc_schema().unwrap();
    provider.begin_transaction(Default::default()).unwrap();
    let before = effects(&host);
    let result = provider.import_record(LAYOUT, None, &[(NAME, Some("Ship".into()))].into(), true);
    assert_eq!(result.unwrap_err().code, Some(3));
    provider.revert_transaction().unwrap();
    assert_eq!(
        effects(&host),
        before,
        "auto-entered imported cells must preflight before reservation"
    );
}

#[test]
fn autoentered_list_script_edit_refuses_before_lock() {
    let host = autoenter_host("regression-auto-script");
    let mut provider = host.open("Root.fmp12");
    provider.calc_schema().unwrap();
    let before = effects(&host);
    let result = provider.write_record(&ScriptWrite {
        layout: LAYOUT,
        table: None,
        record: Some(1),
        cells: [(NAME, Some("Ship".into()))].into(),
        fields: Default::default(),
        typed: Default::default(),
        skip_validation: false,
        privilege_set: None,
        record_number: None,
        prepared: None,
    });
    assert_eq!(result.unwrap_err().code, Some(3));
    assert_eq!(
        effects(&host),
        before,
        "script auto-entry must preflight before lock"
    );
}

#[test]
fn direct_create_generated_list_already_preflights() {
    let host = autoenter_host("regression-auto-control");
    let mut provider = host.open("Root.fmp12");
    provider.calc_schema().unwrap();
    let before = effects(&host);
    assert_eq!(
        provider
            .create_record(
                LAYOUT,
                FieldChanges::from([("Name".into(), Some("Ship".into()))])
            )
            .unwrap_err()
            .code,
        Some(3)
    );
    assert_eq!(effects(&host), before);
}

#[test]
fn reverse_double_extension_and_failed_rebuild() {
    let mut root = fixture::tree(&[], true);
    reference(&mut root, (REMOTE, "Long name"), 1, "Root.fmp12.fmp12");
    reference(&mut root, (REMOTE + 1, "Missing"), 2, "Absent");
    let mut child = fixture::tree(&[], true);
    reference(&mut child, (REMOTE, "Back"), 1, "Root");
    let host = Host::new(
        "regression-reverse",
        vec![("Root.fmp12", root), ("Root.fmp12.fmp12", child)],
    );
    let mut provider = host.open("Root.fmp12");
    let schema = provider.calc_schema().unwrap().unwrap();
    let child = schema.table_of(REMOTE).unwrap().id;
    assert_ne!(child, TABLE);
    let opens = host.captures.opens_and_logins();
    provider.database_schema().unwrap();
    let rebuilt = provider.calc_schema().unwrap().unwrap();
    assert_eq!(rebuilt.table_of(REMOTE).unwrap().id, child);
    assert_eq!(host.captures.opens_and_logins(), opens);
    assert_eq!(opens.0.len(), 3);
    provider.close();
}

#[test]
fn native_related_merge_field_resolves_after_hydration() {
    let mut root = fixture::tree(&[], true);
    reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    fixture::add_relationship(&mut root, 9, (OCCURRENCE, NAME), 0, (REMOTE, NAME));
    // Two ordinary text objects: local and related field merge tokens.
    root.put(
        &[&[4], &[5], &[1], &[7]],
        OwnedNode::leaf(
            Vec::new(),
            vec![8, 1, 18, 4, 8, 2, 16, 1, 8, 2, 18, 4, 8, 2, 16, 2],
        ),
    );
    for (object, token) in [
        (1u8, "<<\u{1}\u{1}\u{e}0003D000010101>>"),
        (2u8, "<<\u{1}\u{1}\u{e}0003D000400101>>"),
    ] {
        let text = om_scsu::encode_scsu(token);
        let literal = [vec![0x13, text.len() as u8], text, vec![0]].concat();
        root.put(
            &[&[4], &[5], &[1], &[5], &[object], &[2], &[6]],
            OwnedNode::leaf(Vec::new(), literal),
        );
    }
    let mut child = fixture::tree(&[], true);
    child.put(
        &[&uint_segment(TABLE), &[3], &[5], &uint_segment(NAME), &[16]],
        om_scripts::text_node(&[], "Child Name"),
    );
    let host = Host::new(
        "regression-merge",
        vec![("Root.fmp12", root), ("Related.fmp12", child)],
    );
    let mut provider = host.open("Root.fmp12");
    let schema = provider.calc_schema().unwrap().unwrap();
    assert_eq!(schema.field(REMOTE, NAME).unwrap().name, "Child Name");
    let rendered = provider.render_layout(LAYOUT).unwrap().unwrap();
    let local = rendered.layout.objects.get(1).unwrap();
    let remote = rendered.layout.objects.get(2).unwrap();
    assert_eq!(
        local.text.as_deref(),
        Some("<<Name>>"),
        "local control must resolve"
    );

    assert_eq!(
        remote.text.as_deref(),
        Some("<<Remote Tasks::Child Name>>"),
        "hydrated related field must resolve to its owning-file field name"
    );
}

#[test]
fn unrelated_edits_do_not_preflight_an_unchanged_generated_field() {
    let host = autoenter_host("unrelated-auto-edit");
    let mut provider = host.open("Root.fmp12");
    provider.calc_schema().unwrap();
    provider
        .update_record(
            LAYOUT,
            1,
            FieldChanges::from([("Extra".into(), Some("Unrelated".into()))]),
        )
        .unwrap();
}

#[test]
fn an_import_skipping_auto_entry_can_write_without_the_generated_list() {
    let host = autoenter_host("skip-auto-import");
    let mut provider = host.open("Root.fmp12");
    provider.calc_schema().unwrap();
    provider.begin_transaction(Default::default()).unwrap();
    let id = provider
        .import_record(LAYOUT, None, &[(NAME, Some("Ship".into()))].into(), false)
        .unwrap();
    assert!(id > 0);
    provider.revert_transaction().unwrap();
}

#[test]
fn a_generated_list_in_a_held_edit_refuses_before_locking() {
    let host = autoenter_host("held-auto-edit");
    let mut provider = host.open("Root.fmp12");
    provider.calc_schema().unwrap();
    provider.begin_transaction(Default::default()).unwrap();
    let before = effects(&host);
    let result = provider.update_record(
        LAYOUT,
        1,
        FieldChanges::from([("Name".into(), Some("Ship".into()))]),
    );
    assert_eq!(result.unwrap_err().code, Some(3));
    assert_eq!(effects(&host), before);
    provider.revert_transaction().unwrap();
}
