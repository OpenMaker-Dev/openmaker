//! Read-only verification against an owned, local Contacts template. No
//! proprietary file, records or calculation bodies are checked in.
use om_hbam::{EntryVariable, FieldKind, HbamFile, SimpleGetSelector};

#[test]
#[ignore = "requires OPENMAKER_CONTACTS_TEMPLATE pointing at a local Contacts .fmp12"]
fn contacts_auto_entry_and_validation_metadata() {
    let path =
        std::env::var_os("OPENMAKER_CONTACTS_TEMPLATE").expect("set OPENMAKER_CONTACTS_TEMPLATE");
    let file = HbamFile::open(path).unwrap();
    let tree = file.key_tree().unwrap();
    let catalog = om_hbam::Catalog::from_tree(&tree).unwrap();
    // Every observed definition is recognized without dropping unknown bits.
    for table in &catalog.tables {
        for field in &table.fields {
            assert_eq!(
                field.decoded_options().unwrap().raw.as_slice(),
                field.options
            );
        }
    }
    let contacts = catalog
        .tables
        .iter()
        .find(|table| table.name == "Contacts")
        .unwrap();
    assert_eq!(contacts.id, 0x8002);
    assert_eq!(contacts.fields.len(), 13);
    let field = |id| contacts.fields.iter().find(|field| field.id == id).unwrap();
    for id in 1..=7 {
        let options = field(id).decoded_options().unwrap();
        assert_eq!(options.auto_entry_variable(), None);
        assert!(!options.auto_entry_calculation());
        assert!(!options.auto_entry_constant());
        assert!(!options.auto_entry_serial());
    }
    let initial = field(8);
    assert_eq!(initial.name, "Initial");
    assert_eq!(initial.kind, FieldKind::Calculation);
    assert_eq!(initial.decoded_options().unwrap().options & 0x20000, 0);
    let primary = field(9);
    assert_eq!(primary.name, "PrimaryKey");
    let options = primary.decoded_options().unwrap();
    assert_eq!(options.options, 0x8000_0188);
    assert_eq!(options.validation_options, 0x1c);
    assert!(options.auto_entry_calculation());
    assert!(options.calculation_replaces_existing());
    assert!(options.validation_unique());
    let code = tree
        .at(&[contacts.id, 3, 5, 9, 5, 6])
        .unwrap()
        .assembled()
        .unwrap();
    assert_eq!(code.as_ref(), &[0x9b, 0x65]);
    assert_eq!(
        SimpleGetSelector::decode(&code),
        Some(SimpleGetSelector::Uuid)
    );
    for (id, name, variable, validation) in [
        (10, "CreatedBy", EntryVariable::CreationAccountName, 0xc),
        (
            11,
            "ModifiedBy",
            EntryVariable::ModificationAccountName,
            0xc,
        ),
        (
            12,
            "CreationTimestamp",
            EntryVariable::CreationTimestamp,
            0x200c,
        ),
        (
            13,
            "ModificationTimestamp",
            EntryVariable::ModificationTimestamp,
            0x200c,
        ),
    ] {
        let field = field(id);
        assert_eq!(field.name, name);
        let options = field.decoded_options().unwrap();
        assert_eq!(options.options, 0x101);
        assert_eq!(options.validation_options, validation);
        assert_eq!(options.auto_entry_variable(), Some(variable));
        assert!(!options.auto_entry_calculation());
        assert!(!options.validation_unique());
        assert!(tree.at(&[contacts.id, 3, 5, id, 5]).is_none());
    }
    for id in 9..=13 {
        let options = field(id).decoded_options().unwrap();
        assert!(options.auto_entry_editing_prohibited());
        assert!(options.validation_not_empty());
        assert!(options.validation_strict());
    }
}
