//! Manage Database is driven through the same background-provider actions as
//! native files; these tests exercise the actual mutable sample catalog, and
//! the native save of a new field on a file an OpenMaker Server hosts on
//! loopback. Manage Value Lists is checked over a hosted file through the
//! native provider, since the sample provider has no value-list catalog.
mod common;
#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
mod hosted;

use std::path::Path;

use common::{click, connect_to_mock, mock, open_menu, text_field, wait_for_label, wait_until};
use egui::{Key, Modifiers, Role};
use egui_kittest::kittest::{NodeT as _, Queryable};

fn open_database(app: &mut common::App) {
    app.key_press_modifiers(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::D);
    wait_for_label(app, "4 fields");
}

fn replace(app: &mut common::App, label: &str, value: &str) {
    text_field(app, label).focus();
    app.step();
    app.key_press_modifiers(Modifiers::COMMAND, Key::A);
    text_field(app, label).type_text(value);
    app.run_steps(2);
}

/// Click the dialog's button `label` (Create, Change, Delete, OK, ...).
fn press(app: &mut common::App, label: &str) {
    app.get_by_role_and_label(Role::Button, label).click();
    app.run_steps(3);
}

/// Click the confirmation's button `label`: the last of that name, as the
/// confirmation is drawn over the dialog's own buttons.
fn confirm(app: &mut common::App, label: &str) {
    app.run_steps(2);
    app.query_all_by_role_and_label(Role::Button, label)
        .last()
        .expect("the confirmation's button")
        .click();
    app.run_steps(3);
}

/// Select the list row `label`.
fn row(app: &mut common::App, label: &str) {
    app.get_by_role_and_label(Role::Button, label).click();
    app.run_steps(2);
}

fn tab(app: &mut common::App, label: &str) {
    app.get_by_role_and_label(Role::Button, label).click();
    app.run_steps(2);
}

#[test]
fn staged_field_changes_are_kept_only_by_ok() {
    let mut app = mock();
    connect_to_mock(&mut app);
    open_database(&mut app);
    app.get_by_label("First Name");
    replace(&mut app, "Field Name", "Priority");
    replace(&mut app, "Comment", "How soon");
    press(&mut app, "Create");
    wait_for_label(&mut app, "5 fields");
    app.get_by_label("Priority");
    row(&mut app, "Priority");
    replace(&mut app, "Field Name", "Ranking");
    press(&mut app, "Change");
    wait_for_label(&mut app, "Ranking");
    assert!(app.query_by_label("Priority").is_none());
    // Cancel drops every staged change.
    press(&mut app, "Cancel");
    open_database(&mut app);
    assert!(app.query_by_label("Ranking").is_none());
    // OK keeps them.
    replace(&mut app, "Field Name", "Ranking");
    press(&mut app, "Create");
    wait_for_label(&mut app, "5 fields");
    row(&mut app, "Ranking");
    press(&mut app, "Delete");
    app.get_by_label("Permanently delete the selected 1 field(s) and ALL of their field data?");
    confirm(&mut app, "Delete");
    wait_for_label(&mut app, "4 fields");
    replace(&mut app, "Field Name", "Kept");
    press(&mut app, "Create");
    wait_for_label(&mut app, "5 fields");
    press(&mut app, "OK");
    wait_until(&mut app, "the dialog closed", |app| {
        app.query_by_label("5 fields").is_none()
    });
    app.key_press_modifiers(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::D);
    wait_for_label(&mut app, "5 fields");
    app.get_by_label("Kept");
    assert!(app.query_by_label("Ranking").is_none());
}

#[test]
fn manager_edits_tables_and_selects_their_actual_fields() {
    let mut app = mock();
    connect_to_mock(&mut app);
    open_database(&mut app);
    tab(&mut app, "Tables");
    replace(&mut app, "Table Name", "Tasks");
    press(&mut app, "Create");
    wait_for_label(&mut app, "2 tables defined in this file");
    row(&mut app, "Tasks");
    replace(&mut app, "Table Name", "Work Items");
    press(&mut app, "Change");
    wait_for_label(&mut app, "Work Items");
    tab(&mut app, "Fields");
    app.get_by_label("0 fields");
    let table = app.get_by_role_and_label(Role::ComboBox, "Database Table");
    assert_eq!(table.value().as_deref(), Some("Work Items"));
    replace(&mut app, "Field Name", "Summary");
    press(&mut app, "Create");
    wait_for_label(&mut app, "1 field");
    app.get_by_label("Summary");
    click(&mut app, "Database Table");
    click(&mut app, "Contacts");
    app.get_by_label("4 fields");
    app.get_by_label("First Name");
    assert!(app.query_by_label("Summary").is_none());
}

#[test]
fn rejected_definition_keeps_the_prior_schema_and_shows_the_provider_error() {
    let mut app = mock();
    connect_to_mock(&mut app);
    open_database(&mut app);
    replace(&mut app, "Field Name", "first name");
    press(&mut app, "Create");
    wait_for_label(&mut app, "That name is already in use.");
    app.get_by_label("4 fields");
    app.get_by_label("First Name");
    assert!(!app.get_by_label("OK").accesskit_node().is_disabled());
}

/// With `OM_SCREENSHOTS` set to a folder, render the window there.
fn shot(app: &mut common::App, name: &str) {
    let Some(dir) = std::env::var_os("OM_SCREENSHOTS") else {
        return;
    };
    app.run_steps(2);
    app.set_render_cursor(false);
    let image = app.render().expect("offscreen render");
    image
        .save(std::path::Path::new(&dir).join(format!("{name}.png")))
        .expect("write png");
}

/// The Tasks file of om-server's tests with each table's field catalog
/// indexed as FileMaker indexes it (schema-path.md 5.1, 5.5), which adding
/// a field needs: the last issued id, the name lists, the display order and
/// the count, as om-author's tests build it.
fn indexed_tasks() -> om_hbam::OwnedNode {
    use om_data::field_catalog;
    use om_hbam::{serial, slot::uint_segment, OwnedNode};
    let key_string = |key: &[u8]| {
        let mut out = vec![key.len() as u8];
        out.extend(key);
        out
    };
    let mut root = fixture::tree(&[fixture::script(1, "Capture", Vec::new())], true);
    for table in [fixture::TABLE, fixture::NOTES] {
        let t = uint_segment(table);
        let catalog = root.at(&[&t, &[3]]).cloned().expect("a field catalog");
        let mut index = OwnedNode::list(vec![1], Vec::new());
        let mut last = 0;
        for (n, member) in field_catalog::members(&catalog).iter().enumerate() {
            last = last.max(serial::decode(&member.segment).expect("a serial key"));
            let name = field_catalog::member_name(member).expect("a name");
            let definition = field_catalog::member_definition(member).expect("a definition");
            let key = field_catalog::name_key(&name);
            let id = key_string(&member.segment);
            let mut listed = key.clone();
            listed.extend([0, 0, 0]);
            let mut typed = vec![definition[0], definition[1] << 2];
            typed.extend(&key);
            typed.extend([0, 0]);
            index.put(&[&[1], &listed], OwnedNode::leaf(Vec::new(), id.clone()));
            index.put(&[&[6], &typed], OwnedNode::leaf(Vec::new(), id.clone()));
            index.put(
                &[&[3], &(n as u32 + 1).to_be_bytes()],
                OwnedNode::leaf(Vec::new(), id),
            );
        }
        let count = field_catalog::members(&catalog).len() as u32;
        index.set_child(OwnedNode::leaf(vec![0], key_string(&serial::encode(last))));
        index.set_child(OwnedNode::leaf(vec![4], count.to_be_bytes().to_vec()));
        index.set_child(OwnedNode::leaf(vec![0xd8], vec![7; 16]));
        root.put(&[&t, &[3], &[1]], index);
    }
    root
}

/// Index the table and occurrence catalogs `[3, 16]`, `[3, 17]` as
/// FileMaker indexes them (schema-path.md 5.1): the last issued id, the
/// name list, the display order and the count; and give each table the
/// dependency index entry FileMaker gives a new table (5.8, E1), without
/// the fixture's own evaluated fields.
fn index_schema_catalogs(root: &mut om_hbam::OwnedNode) {
    use om_data::field_catalog;
    use om_hbam::{serial, OwnedNode};
    for catalog in [0x10u8, 0x11] {
        let members = root
            .at(&[&[3], &[catalog], &[5]])
            .cloned()
            .expect("the catalog's members");
        let mut index = OwnedNode::list(vec![1], Vec::new());
        let mut last = 0;
        for (n, member) in members.children.iter().enumerate() {
            last = last.max(serial::decode(&member.segment).expect("a serial key"));
            let name = field_catalog::member_name(member).expect("a name");
            let mut id = vec![member.segment.len() as u8];
            id.extend(&member.segment);
            let mut listed = field_catalog::name_key(&name);
            listed.extend([0, 0, 0]);
            index.put(&[&[1], &listed], OwnedNode::leaf(Vec::new(), id.clone()));
            index.put(
                &[&[3], &(n as u32 + 1).to_be_bytes()],
                OwnedNode::leaf(Vec::new(), id),
            );
        }
        let last = serial::encode(last);
        let mut value = vec![last.len() as u8];
        value.extend(last);
        index.set_child(OwnedNode::leaf(vec![0], value));
        index.set_child(OwnedNode::leaf(
            vec![4],
            (members.children.len() as u32).to_be_bytes().to_vec(),
        ));
        root.put(&[&[3], &[catalog], &[1]], index);
    }
    for table in [fixture::TABLE, fixture::NOTES] {
        let t = om_hbam::slot::uint_segment(table);
        let (order, marker): (&[u8], &[u8]) = (&[0xff], &[0xff, 0x03]);
        root.ensure(&[&[3], &[0x11], &[1], &[0x0e], &t, order]);
        root.put(
            &[&[3], &[0x11], &[1], &[0x0e], &t, marker],
            OwnedNode::leaf(Vec::new(), vec![2, 0, 0x0a]),
        );
    }
}

/// Manage Database on a file an OpenMaker Server hosts on loopback (#222):
/// a new table and field, a new field, a renamed and a deleted field of
/// Tasks, staged in the dialog and saved by OK with FileMaker Pro's schema
/// transaction. While the dialog is open the schema is locked to everyone
/// else and nothing is saved; a fresh connection then reads the changes
/// back, with the records' values as they were but the deleted field's.
/// `OM_SCREENSHOTS` keeps the dialog's tabs.
#[test]
fn manager_saves_tables_and_fields_on_an_openmaker_server() {
    use om_data::{DataProvider, FieldKind, SchemaFieldKind};
    let mut root = indexed_tasks();
    index_schema_catalogs(&mut root);
    let table_key = om_hbam::slot::uint_segment(fixture::TABLE);
    let name_key = om_hbam::slot::uint_segment(fixture::NAME);
    root.put(
        &[&table_key, &[3], &[5], &name_key, &[0x29]],
        om_hbam::OwnedNode::leaf(Vec::new(), 2u32.to_be_bytes().to_vec()),
    );
    root.put(
        &[&table_key, &[3], &[5], &name_key, &[0x30]],
        om_scripts::text_node(&[], "Task title"),
    );
    let file = fixture::TempFile::from_tree("gui-database-save", &root);
    let hosted = hosted::host_file(
        "database-save",
        std::path::Path::new(&file.0),
        fixture::FILE,
        (fixture::ACCOUNT, fixture::PASSWORD),
    );
    let layout = u32::try_from(fixture::LAYOUT).unwrap();
    let mut before = hosted.opened();
    let records = before.records(layout, 0, 100).unwrap();
    before.close();

    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    click(&mut app, "Open");
    wait_until(&mut app, "the hosted file", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled()
    });
    app.key_press_modifiers(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::D);
    wait_for_label(&mut app, "7 fields");
    // The third column lists each field's options as FileMaker Pro's Fields
    // tab does (client-ui.md): Setting "Global", Doubled "= Count * 2",
    // Creator "Creation Account Name, Can't Modify Auto", Code "Always
    // Validate, By Calculation". Its title steps it to the comments, the
    // advanced options and back.
    shot(&mut app, "manage-database-fields");
    press(&mut app, om_data::FieldColumn::TITLE);
    shot(&mut app, "manage-database-fields-comments");
    press(&mut app, om_data::FieldColumn::TITLE);
    shot(&mut app, "manage-database-fields-advanced");
    press(&mut app, om_data::FieldColumn::TITLE);
    // Tasks: a new timestamp field with a comment, Count renamed, Due
    // deleted.
    replace(&mut app, "Field Name", "Finished");
    replace(&mut app, "Comment", "When the task was done");
    click(&mut app, "Database Field Type");
    click(&mut app, "Timestamp");
    press(&mut app, "Create");
    wait_for_label(&mut app, "8 fields");
    row(&mut app, "Count");
    replace(&mut app, "Field Name", "Quantity");
    press(&mut app, "Change");
    wait_for_label(&mut app, "Quantity");
    row(&mut app, "Due");
    press(&mut app, "Delete");
    confirm(&mut app, "Delete");
    wait_for_label(&mut app, "7 fields");
    // A new table, its occurrence and a field.
    tab(&mut app, "Tables");
    replace(&mut app, "Table Name", "Projects");
    replace(&mut app, "Comment", "Work grouped by client");
    press(&mut app, "Create");
    wait_for_label(&mut app, "3 tables defined in this file");
    shot(&mut app, "manage-database-tables");
    tab(&mut app, "Fields");
    click(&mut app, "Database Table");
    click(&mut app, "Projects");
    wait_for_label(&mut app, "0 fields");
    replace(&mut app, "Field Name", "Title");
    press(&mut app, "Create");
    wait_for_label(&mut app, "1 field");

    // Nothing is saved yet, and the schema is in use.
    let mut other = hosted.opened();
    let read = other.database_schema().unwrap();
    assert_eq!(read.tables.len(), 2);
    let in_use = other.begin_schema_edit().unwrap_err();
    assert_eq!(in_use.code, Some(303), "{in_use}");
    other.close();

    press(&mut app, "OK");
    wait_until(&mut app, "the save", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.query_by_label("Manage Database for “Tasks.fmp12”")
            .is_none()
    });

    let mut fresh = hosted.opened();
    let schema = fresh.database_schema().unwrap();
    let tasks = schema.table(fixture::TABLE).unwrap();
    let name = tasks.fields.iter().find(|f| f.id == fixture::NAME).unwrap();
    assert_eq!(
        schema.field_column_text(tasks, name, om_data::FieldColumn::Advanced),
        "Display Names, Task title"
    );
    let finished = tasks
        .fields
        .iter()
        .find(|f| f.name == "Finished")
        .expect("Finished read back");
    assert_eq!(
        (finished.kind, finished.data_type, finished.comment.as_str()),
        (
            SchemaFieldKind::Normal,
            FieldKind::Timestamp,
            "When the task was done"
        )
    );
    let quantity = tasks.fields.iter().find(|f| f.name == "Quantity").unwrap();
    assert_eq!(quantity.id, fixture::COUNT);
    assert!(!tasks.fields.iter().any(|f| f.id == fixture::DUE));
    let projects = schema
        .tables
        .iter()
        .find(|t| t.name == "Projects")
        .expect("Projects read back");
    assert_eq!(projects.comment, "Work grouped by client");
    assert_eq!(
        projects
            .fields
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>(),
        ["Title"]
    );
    assert!(schema
        .occurrences
        .iter()
        .any(|o| o.name == "Projects" && o.table_id == Some(projects.id)));
    // The records keep every value but Due's.
    let after = fresh.records(layout, 0, 100).unwrap();
    assert_eq!(after.total, records.total);
    for record in &records.records {
        let again = after
            .records
            .iter()
            .find(|r| r.id == record.id)
            .expect("the record kept");
        for (field, value) in records.fields.iter().zip(&record.values) {
            let name = match field.name.as_str() {
                "Due" => continue,
                "Count" => "Quantity",
                other => other,
            };
            let at = after
                .fields
                .iter()
                .position(|f| f.name == name)
                .expect("the field kept");
            assert_eq!(&again.values[at], value, "{name} of {}", record.id);
        }
    }
    fresh.close();
}

/// Options for Field on a file an OpenMaker Server hosts on loopback
/// (#431): Tasks::Name gets auto-entered data, Not empty and full
/// indexing; a new number field a serial number from 10 by 2, a range, a
/// validation calculation and a custom message; a new text field a
/// calculated value written in Specify Calculation. Each is staged by the
/// dialog's OK and saved by Manage Database's. A fresh connection reads
/// the options back, and the records keep their values.
/// Specify Calculation's formula editor (a multiline text field).
fn formula(app: &common::App) -> egui_kittest::Node<'_> {
    app.get_by_role_and_label(Role::MultilineTextInput, "Calculation")
}

/// Manage Database makes a calculation field and then changes its formula
/// on a file an OpenMaker Server hosts on loopback (#432): Type
/// Calculation, Create, Specify Calculation's formula and OK; then Options…
/// on the field and a new formula. OK saves both with FileMaker Pro's
/// schema transaction, and a fresh connection reads the field's formula
/// back while the server has computed its value in every record.
/// `OM_SCREENSHOTS` keeps the dialogs.
#[test]
fn a_calculation_field_is_made_and_changed_on_an_openmaker_server() {
    use om_data::{DataProvider, FieldKind, SchemaFieldKind};
    let mut root = indexed_tasks();
    index_schema_catalogs(&mut root);
    let file = fixture::TempFile::from_tree("gui-database-calculation", &root);
    let hosted = hosted::host_file(
        "database-calculation",
        std::path::Path::new(&file.0),
        fixture::FILE,
        (fixture::ACCOUNT, fixture::PASSWORD),
    );
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    click(&mut app, "Open");
    wait_until(&mut app, "the hosted file", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled()
    });
    app.key_press_modifiers(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::D);
    wait_for_label(&mut app, "7 fields");
    replace(&mut app, "Field Name", "Shout");
    click(&mut app, "Database Field Type");
    shot(&mut app, "manage-database-field-type-calculation");
    click(&mut app, "Calculation");
    press(&mut app, "Create");
    wait_for_label(&mut app, "Specify Calculation “Shout”");
    formula(&app).focus();
    app.run_steps(2);
    formula(&app).type_text("Name & \"!\"");
    app.run_steps(2);
    shot(&mut app, "manage-database-specify-calculation");
    confirm(&mut app, "Storage Options…");
    wait_for_label(&mut app, "Storage Options for Field “Shout”");
    shot(&mut app, "manage-database-calculation-storage-options");
    confirm(&mut app, "OK");
    confirm(&mut app, "OK");
    wait_until(&mut app, "the calculation staged", |app| {
        app.query_by_label("Specify Calculation “Shout”").is_none()
            && app.state().is_settled()
            && app.query_by_label("8 fields").is_some()
    });
    shot(&mut app, "manage-database-calculation-field");
    // Options… on the new field opens Specify Calculation on its formula.
    press(&mut app, "Options…");
    wait_for_label(&mut app, "Specify Calculation “Shout”");
    assert_eq!(formula(&app).value().as_deref(), Some("Name & \"!\""));
    formula(&app).focus();
    app.run_steps(2);
    app.key_press_modifiers(Modifiers::COMMAND, Key::A);
    formula(&app).type_text("Upper ( Name ) & Count");
    app.run_steps(2);
    confirm(&mut app, "OK");
    wait_until(&mut app, "the change staged", |app| {
        app.query_by_label("Specify Calculation “Shout”").is_none() && app.state().is_settled()
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    press(&mut app, "OK");
    wait_until(&mut app, "the save", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.query_by_label("Manage Database for “Tasks.fmp12”")
            .is_none()
    });

    let mut fresh = hosted.opened();
    let schema = fresh.database_schema().unwrap();
    let shout = schema
        .table(fixture::TABLE)
        .unwrap()
        .fields
        .iter()
        .find(|f| f.name == "Shout")
        .cloned()
        .expect("the new field");
    assert_eq!(shout.kind, SchemaFieldKind::Calculation);
    assert_eq!(shout.data_type, FieldKind::Text);
    let calculation = shout.calculation.expect("its formula reads back");
    assert_eq!(calculation.formula, "Upper ( Name ) & Count");
    assert_eq!(calculation.context, fixture::OCCURRENCE);
    // The server computed the stored values from the saved formula.
    let records = fresh
        .records(u32::try_from(fixture::LAYOUT).unwrap(), 0, 100)
        .unwrap();
    assert!(!records.records.is_empty());
    let saved = om_hbam::HbamFile::open(hosted.file()).unwrap();
    let tree = saved.key_tree().unwrap();
    let t = om_hbam::slot::uint_segment(fixture::TABLE);
    let value = |record: i64, field: u64| {
        let r = om_hbam::slot::uint_segment(u64::try_from(record).unwrap());
        tree.node(&[&t, &[5], &r, &om_hbam::slot::uint_segment(field)])
            .and_then(|n| om_hbam::Cell::from_node(n).text())
            .and_then(Result::ok)
    };
    let mut checked = 0;
    for record in &records.records {
        let name = value(record.id, fixture::NAME).unwrap_or_default();
        let count = value(record.id, fixture::COUNT).unwrap_or_default();
        if name.is_empty() && count.is_empty() {
            continue;
        }
        checked += 1;
        assert_eq!(
            value(record.id, shout.id).unwrap_or_default(),
            format!("{}{count}", name.to_uppercase()),
            "record {}",
            record.id
        );
    }
    assert!(checked > 0, "records with values were checked");
}

/// Manage Database makes a summary field on a file an OpenMaker Server
/// hosts on loopback (#432): Type Summary, Create, Options for Summary
/// Field (Total of Count, a running total), OK; Manage Database's OK saves
/// it with FileMaker Pro's schema transaction, and a fresh connection
/// reads its definition back. `OM_SCREENSHOTS` keeps the dialog.
#[test]
fn a_summary_field_is_made_on_an_openmaker_server() {
    use om_data::meta::{summary_flag, SummaryKind};
    use om_data::{DataProvider, FieldKind, SchemaFieldKind};
    let mut root = indexed_tasks();
    index_schema_catalogs(&mut root);
    let file = fixture::TempFile::from_tree("gui-database-summary", &root);
    let hosted = hosted::host_file(
        "database-summary",
        std::path::Path::new(&file.0),
        fixture::FILE,
        (fixture::ACCOUNT, fixture::PASSWORD),
    );
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    click(&mut app, "Open");
    wait_until(&mut app, "the hosted file", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled()
    });
    app.key_press_modifiers(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::D);
    wait_for_label(&mut app, "7 fields");
    replace(&mut app, "Field Name", "Running Count");
    click(&mut app, "Database Field Type");
    click(&mut app, "Summary");
    press(&mut app, "Create");
    wait_for_label(&mut app, "Options for Summary Field “Running Count”");
    // The dialog's field list is drawn over Manage Database's.
    confirm(&mut app, "Count");
    app.get_by_role_and_label(Role::CheckBox, "Running total")
        .click();
    app.run_steps(2);
    shot(&mut app, "manage-database-summary-options");
    confirm(&mut app, "OK");
    wait_until(&mut app, "the summary staged", |app| {
        app.query_by_label("Options for Summary Field “Running Count”")
            .is_none()
            && app.state().is_settled()
            && app.query_by_label("8 fields").is_some()
    });
    shot(&mut app, "manage-database-summary-field");
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    press(&mut app, "OK");
    wait_until(&mut app, "the save", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.query_by_label("Manage Database for “Tasks.fmp12”")
            .is_none()
    });

    let mut fresh = hosted.opened();
    let schema = fresh.database_schema().unwrap();
    let field = schema
        .table(fixture::TABLE)
        .unwrap()
        .fields
        .iter()
        .find(|f| f.name == "Running Count")
        .cloned()
        .expect("the new field");
    assert_eq!(field.kind, SchemaFieldKind::Summary);
    assert_eq!(field.data_type, FieldKind::Number);
    let summary = field.summary.expect("its definition reads back");
    assert_eq!(summary.kind, SummaryKind::Total);
    assert_eq!(summary.flags, summary_flag::RUNNING);
    assert_eq!(summary.field.map(|f| f.field), Some(fixture::COUNT));
}

/// A looked-up value set in Options for Field on a file an OpenMaker
/// Server hosts on loopback (#431): with Tasks::Name = Notes::Body related
/// first, a new text field of Tasks gets "Looked-up value" (Lookup for
/// Field: from Notes, copying ::Body, "do not copy", "Don't copy contents
/// if empty"). Manage Database's OK saves it, a fresh connection reads the
/// lookup back, and a write of Name through OpenMaker performs it.
/// `OM_SCREENSHOTS` keeps the dialogs.
#[test]
fn a_lookup_is_set_on_an_openmaker_server() {
    use om_data::field_options::{AutoEnterValue, Lookup, NoMatch};
    use om_data::{DataProvider, RelationshipMatch, RelationshipSide, SchemaChange};
    let mut root = indexed_tasks();
    index_schema_catalogs(&mut root);
    let file = fixture::TempFile::from_tree("gui-database-lookup", &root);
    let hosted = hosted::host_file(
        "database-lookup",
        std::path::Path::new(&file.0),
        fixture::FILE,
        (fixture::ACCOUNT, fixture::PASSWORD),
    );
    // The relationship the lookup follows, saved first.
    let mut setup = hosted.opened();
    setup.begin_schema_edit().unwrap();
    setup
        .apply_schema_change(SchemaChange::CreateRelationship {
            left_occurrence: fixture::OCCURRENCE,
            right_occurrence: fixture::NOTES_OCCURRENCE,
            matches: vec![RelationshipMatch {
                left_field: fixture::NAME,
                operator: 0,
                right_field: fixture::BODY,
            }],
            left: RelationshipSide::default(),
            right: RelationshipSide::default(),
        })
        .unwrap();
    setup.commit_schema_changes().unwrap();
    setup.close();

    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    click(&mut app, "Open");
    wait_until(&mut app, "the hosted file", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled()
    });
    app.key_press_modifiers(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::D);
    wait_for_label(&mut app, "7 fields");
    replace(&mut app, "Field Name", "First Note");
    press(&mut app, "Create");
    wait_for_label(&mut app, "8 fields");
    row(&mut app, "First Note");
    press(&mut app, "Options…");
    wait_for_label(&mut app, "Options for Field “First Note”");
    // Checking "Looked-up value" opens Lookup for Field; its OK checks it.
    app.get_by_role_and_label(Role::CheckBox, "Looked-up value")
        .click();
    app.run_steps(3);
    wait_for_label(&mut app, "Lookup for Field “First Note”");
    click(&mut app, "Lookup Related Table");
    app.get_by_role_and_label(Role::Button, "Notes").click();
    app.run_steps(2);
    app.get_by_role_and_label(Role::Button, "::Body").click();
    app.run_steps(2);
    shot(&mut app, "manage-database-lookup-for-field");
    confirm(&mut app, "OK");
    wait_until(&mut app, "the lookup chosen", |app| {
        app.query_by_label("Lookup for Field “First Note”")
            .is_none()
    });
    shot(&mut app, "manage-database-field-options-looked-up-value");
    confirm(&mut app, "OK");
    wait_until(&mut app, "the lookup staged", |app| {
        app.query_by_label("Options for Field “First Note”")
            .is_none()
            && app.state().is_settled()
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    press(&mut app, "OK");
    wait_until(&mut app, "the save", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.query_by_label("Manage Database for “Tasks.fmp12”")
            .is_none()
    });

    let mut fresh = hosted.opened();
    let schema = fresh.database_schema().unwrap();
    let field = schema
        .table(fixture::TABLE)
        .unwrap()
        .fields
        .iter()
        .find(|f| f.name == "First Note")
        .cloned()
        .expect("the new field");
    assert_eq!(
        field.options.clone().expect("its options").auto_enter.value,
        AutoEnterValue::LookedUp(Some(Lookup {
            from: fixture::OCCURRENCE,
            to: fixture::NOTES_OCCURRENCE,
            copied: fixture::BODY,
            copy_empty: false,
            no_match: NoMatch::DoNotCopy,
        }))
    );
    // The table stays writable to OpenMaker, whose record rules perform
    // the lookup: Name "Hi" matches the note "Hi" and copies its body.
    let layout = u32::try_from(fixture::LAYOUT).unwrap();
    let change = [("Name".to_owned(), Some("Hi".to_owned()))]
        .into_iter()
        .collect();
    fresh.update_record(layout, 1, change).unwrap();
    fresh.close();
    let saved = om_hbam::HbamFile::open(hosted.file()).unwrap();
    let tree = saved.key_tree().unwrap();
    let t = om_hbam::slot::uint_segment(fixture::TABLE);
    let cell = tree
        .node(&[&t, &[5], &[1], &om_hbam::slot::uint_segment(field.id)])
        .and_then(|n| om_hbam::Cell::from_node(n).text())
        .and_then(Result::ok);
    assert_eq!(cell.as_deref(), Some("Hi"));
}
/// Duplicate in Manage Database's Fields tab on a file an OpenMaker Server
/// hosts on loopback (#433): the calculation Doubled (`Count * 2`) becomes
/// "Doubled Copy", which the save keeps with its formula, and which the
/// server computes in every record as it does a new calculation field; the
/// global Setting, holding "on", becomes a global "Setting Copy" without a
/// value, as FileMaker copies no data.
#[test]
fn a_field_is_duplicated_on_an_openmaker_server() {
    use om_data::{DataProvider, SchemaFieldKind};
    let mut root = indexed_tasks();
    index_schema_catalogs(&mut root);
    // FileMaker writes increment 1 on every field, which OpenMaker's copy
    // must match byte for byte (schema-path.md 5.14).
    let table = om_hbam::slot::uint_segment(fixture::TABLE);
    for field in [fixture::DOUBLED, fixture::SETTING] {
        fixture::set_field_serial_increment(&mut root, fixture::TABLE, field, 1);
    }
    root.put(
        &[&table, &[8], &om_hbam::slot::uint_segment(fixture::SETTING)],
        om_data::scripts::text_node(&[], "on"),
    );
    let file = fixture::TempFile::from_tree("gui-database-duplicate", &root);
    let hosted = hosted::host_file(
        "database-duplicate",
        std::path::Path::new(&file.0),
        fixture::FILE,
        (fixture::ACCOUNT, fixture::PASSWORD),
    );
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    click(&mut app, "Open");
    wait_until(&mut app, "the hosted file", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled()
    });
    app.key_press_modifiers(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::D);
    wait_for_label(&mut app, "7 fields");
    row(&mut app, "Doubled");
    press(&mut app, "Duplicate");
    wait_for_label(&mut app, "8 fields");
    wait_for_label(&mut app, "Doubled Copy");
    shot(&mut app, "manage-database-duplicated-field");
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    row(&mut app, "Setting");
    press(&mut app, "Duplicate");
    wait_for_label(&mut app, "9 fields");
    wait_for_label(&mut app, "Setting Copy");
    press(&mut app, "OK");
    wait_until(&mut app, "the save", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.query_by_label("Manage Database for “Tasks.fmp12”")
            .is_none()
    });

    let mut fresh = hosted.opened();
    let schema = fresh.database_schema().unwrap();
    let field = |name: &str| {
        schema
            .table(fixture::TABLE)
            .unwrap()
            .fields
            .iter()
            .find(|f| f.name == name)
            .cloned()
            .unwrap_or_else(|| panic!("{name}"))
    };
    let copy = field("Doubled Copy");
    assert_eq!(copy.kind, SchemaFieldKind::Calculation);
    let calculation = copy.calculation.expect("its formula reads back");
    assert_eq!(calculation.formula, "Count * 2");
    let global = field("Setting Copy");
    assert!(global.options.expect("its options").storage.global);
    fresh.close();
    let saved = om_hbam::HbamFile::open(hosted.file()).unwrap();
    let tree = saved.key_tree().unwrap();
    let t = table;
    let globals = |field: u64| {
        tree.node(&[&t, &[8], &om_hbam::slot::uint_segment(field)])
            .is_some()
    };
    assert!(globals(fixture::SETTING), "the original keeps its value");
    assert!(!globals(global.id), "the copy holds no value");
    let records = tree.node(&[&t, &[5]]).expect("the records");
    let value = |record: &om_hbam::Node<'_>, field: u64| {
        record
            .child(&om_hbam::slot::uint_segment(field))
            .and_then(|n| om_hbam::Cell::from_node(n).text())
            .and_then(Result::ok)
            .unwrap_or_default()
    };
    let mut checked = 0;
    for record in &records.children {
        let count = value(record, fixture::COUNT);
        if count.is_empty() {
            continue;
        }
        checked += 1;
        assert_eq!(value(record, copy.id), value(record, fixture::DOUBLED));
    }
    assert!(checked > 0, "records with a count were checked");
}
/// Global storage turned off and on for fields that hold values, on a
/// file an OpenMaker Server hosts on loopback (#431): Tasks::Setting, a
/// global holding "on", becomes a stored field and Tasks::Name, stored in
/// every record, a global one, both in Options for Field's Storage tab.
/// Manage Database's OK saves them, and the server does the deferred work
/// FileMaker Server does (schema-path.md 7.2): Setting's global value and
/// Name's record values are deleted. `OM_SCREENSHOTS` keeps the tab.
#[test]
fn global_storage_is_changed_on_an_openmaker_server() {
    use om_data::DataProvider;
    let mut root = indexed_tasks();
    index_schema_catalogs(&mut root);
    let table = om_hbam::slot::uint_segment(fixture::TABLE);
    let setting = om_hbam::slot::uint_segment(fixture::SETTING);
    root.put(
        &[&table, &[8], &setting],
        om_data::scripts::text_node(&[], "on"),
    );
    // A second repetition beside it, `[T, 08, F 02]` (calc-values.md).
    let second = [setting.as_slice(), &[2]].concat();
    root.put(
        &[&table, &[8], &second],
        om_data::scripts::text_node(&[], "two"),
    );
    let file = fixture::TempFile::from_tree("gui-database-global", &root);
    let hosted = hosted::host_file(
        "database-global",
        std::path::Path::new(&file.0),
        fixture::FILE,
        (fixture::ACCOUNT, fixture::PASSWORD),
    );
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    click(&mut app, "Open");
    wait_until(&mut app, "the hosted file", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled()
    });
    app.key_press_modifiers(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::D);
    wait_for_label(&mut app, "7 fields");
    for (field, title) in [
        ("Setting", "Options for Field “Setting”"),
        ("Name", "Options for Field “Name”"),
    ] {
        row(&mut app, field);
        press(&mut app, "Options…");
        wait_for_label(&mut app, title);
        tab(&mut app, "Storage");
        app.get_by_role_and_label(
            Role::CheckBox,
            "Use global storage (one value for all records)",
        )
        .click();
        app.run_steps(2);
        if field == "Name" {
            shot(&mut app, "manage-database-field-options-global-storage");
        }
        confirm(&mut app, "OK");
        wait_until(&mut app, "the storage staged", |app| {
            app.query_by_label(title).is_none() && app.state().is_settled()
        });
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
    }
    press(&mut app, "OK");
    wait_until(&mut app, "the save", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.query_by_label("Manage Database for “Tasks.fmp12”")
            .is_none()
    });

    let mut fresh = hosted.opened();
    let schema = fresh.database_schema().unwrap();
    let tasks = schema.table(fixture::TABLE).unwrap();
    let global = |name: &str| {
        tasks
            .fields
            .iter()
            .find(|f| f.name == name)
            .and_then(|f| f.options.as_ref())
            .unwrap_or_else(|| panic!("{name}'s options"))
            .storage
            .global
    };
    assert!(!global("Setting"));
    assert!(global("Name"));
    fresh.close();
    let saved = om_hbam::HbamFile::open(hosted.file()).unwrap();
    let tree = saved.key_tree().unwrap();
    assert!(
        tree.node(&[&table, &[8], &setting]).is_none(),
        "Setting's global value"
    );
    assert!(
        tree.node(&[&table, &[8], &second]).is_none(),
        "its second repetition"
    );
    let name = om_hbam::slot::uint_segment(fixture::NAME);
    let records = tree.node(&[&table, &[5]]).expect("the records");
    assert!(!records.children.is_empty());
    for record in &records.children {
        assert!(
            record.child(&name).is_none(),
            "record {:02x?} kept Name",
            record.segment
        );
    }
}

/// A calculated value over a related field, set in Options for Field on a
/// file an OpenMaker Server hosts on loopback (#431): with Tasks::Name =
/// Notes::Body related first, a new text field of Tasks gets the
/// calculated value `Notes::Body` written in Specify Calculation. Manage
/// Database's OK saves it, a fresh connection reads it back, and a write
/// of Name through OpenMaker evaluates it on the related note; then a
/// validation by `Notes::Body ≠ ""` refuses a Name matching no note
/// (auto-entry.md, "Calculations over related fields").
#[test]
fn a_related_calculated_value_is_set_on_an_openmaker_server() {
    use om_data::field_options::{AutoCalculation, AutoEnterValue, OptionFormula};
    use om_data::{DataProvider, RelationshipMatch, RelationshipSide, SchemaChange};
    let mut root = indexed_tasks();
    index_schema_catalogs(&mut root);
    let file = fixture::TempFile::from_tree("gui-database-related-formula", &root);
    let hosted = hosted::host_file(
        "database-related-formula",
        std::path::Path::new(&file.0),
        fixture::FILE,
        (fixture::ACCOUNT, fixture::PASSWORD),
    );
    let mut setup = hosted.opened();
    setup.begin_schema_edit().unwrap();
    setup
        .apply_schema_change(SchemaChange::CreateRelationship {
            left_occurrence: fixture::OCCURRENCE,
            right_occurrence: fixture::NOTES_OCCURRENCE,
            matches: vec![RelationshipMatch {
                left_field: fixture::NAME,
                operator: 0,
                right_field: fixture::BODY,
            }],
            left: RelationshipSide::default(),
            right: RelationshipSide::default(),
        })
        .unwrap();
    setup.commit_schema_changes().unwrap();
    setup.close();

    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    click(&mut app, "Open");
    wait_until(&mut app, "the hosted file", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled()
    });
    app.key_press_modifiers(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::D);
    wait_for_label(&mut app, "7 fields");
    replace(&mut app, "Field Name", "Note Copy");
    press(&mut app, "Create");
    wait_for_label(&mut app, "8 fields");
    row(&mut app, "Note Copy");
    press(&mut app, "Options…");
    wait_for_label(&mut app, "Options for Field “Note Copy”");
    app.get_by_role_and_label(Role::CheckBox, "Calculated value")
        .click();
    app.run_steps(3);
    wait_for_label(&mut app, "Specify Calculation “Note Copy”");
    formula(&app).focus();
    app.run_steps(2);
    formula(&app).type_text("Notes::Body");
    app.run_steps(2);
    shot(&mut app, "manage-database-related-calculated-value");
    confirm(&mut app, "OK");
    wait_until(&mut app, "the calculated value", |app| {
        app.query_by_label("Specify Calculation “Note Copy”")
            .is_none()
    });
    confirm(&mut app, "OK");
    wait_until(&mut app, "the calculated value staged", |app| {
        app.query_by_label("Options for Field “Note Copy”")
            .is_none()
            && app.state().is_settled()
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    // Closing Options starts asynchronous staging; document settle does not
    // include Manage Database's busy state.
    wait_until(&mut app, "Manage Database ready to save", |app| {
        app.query_by_role_and_label(Role::Button, "OK")
            .is_some_and(|button| !button.accesskit_node().is_disabled())
    });
    press(&mut app, "OK");
    wait_until(&mut app, "the save", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.query_by_label("Manage Database for “Tasks.fmp12”")
            .is_none()
    });

    let mut fresh = hosted.opened();
    let schema = fresh.database_schema().unwrap();
    let field = schema
        .table(fixture::TABLE)
        .unwrap()
        .fields
        .iter()
        .find(|f| f.name == "Note Copy")
        .cloned()
        .expect("the new field");
    assert_eq!(
        field.options.clone().expect("its options").auto_enter.value,
        AutoEnterValue::Calculated(AutoCalculation::new(OptionFormula::new(
            "Notes::Body",
            fixture::OCCURRENCE
        )))
    );
    // A validation over the related note, checked at every write: Name
    // matching no note is refused (507), one matching a note is kept.
    fresh.begin_schema_edit().unwrap();
    let mut options = field.options.clone().unwrap();
    options.auto_enter.value = AutoEnterValue::None;
    options.validation.calculation = Some(om_data::field_options::ValidationCalculation {
        formula: OptionFormula::new("Notes::Body ≠ \"\"", fixture::OCCURRENCE),
        only_if_modified: false,
    });
    let check = |name: &str| {
        [("Name".to_owned(), Some(name.to_owned()))]
            .into_iter()
            .collect()
    };
    let layout = u32::try_from(fixture::LAYOUT).unwrap();
    fresh.update_record(layout, 1, check("Hi")).unwrap();
    fresh
        .apply_schema_change(SchemaChange::SetFieldOptions {
            table_id: fixture::TABLE,
            field_id: field.id,
            options: Box::new(options),
        })
        .unwrap();
    fresh.commit_schema_changes().unwrap();
    let refused = fresh
        .update_record(layout, 1, check("no such note"))
        .unwrap_err();
    assert_eq!(refused.code, Some(507), "{}", refused.message);
    fresh.update_record(layout, 1, check("Hi")).unwrap();
    fresh.close();
    let saved = om_hbam::HbamFile::open(hosted.file()).unwrap();
    let tree = saved.key_tree().unwrap();
    let t = om_hbam::slot::uint_segment(fixture::TABLE);
    let cell = |field: u64| {
        tree.node(&[&t, &[5], &[1], &om_hbam::slot::uint_segment(field)])
            .and_then(|n| om_hbam::Cell::from_node(n).text())
            .and_then(Result::ok)
    };
    // The calculated value copied the note when Name was first set.
    assert_eq!(cell(field.id).as_deref(), Some("Hi"));
    assert_eq!(cell(fixture::NAME).as_deref(), Some("Hi"));
}

/// Formulas over related fields read each relationship on its own and are
/// evaluated by their own reads (#431, auto-entry.md "Calculations over
/// related fields"), through OpenMaker's record rules on a file an
/// OpenMaker Server hosts on loopback: Tasks relates Notes on Name = Body
/// and a second occurrence "Other notes" on Code = Body. "Both" auto-enters
/// `Notes::Body & "|" & Other notes::Body`, "Kept" auto-enters "default"
/// and is validated by `Notes::Body ≠ ""`. One note related twice is read
/// whole through both occurrences, and a change of Name evaluates "Both"
/// but not "Kept", whose validation alone reads through Name.
#[test]
fn related_formulas_read_each_relationship_on_its_own() {
    use om_data::field_options::{
        AutoCalculation, AutoEnterValue, FieldOptions, OptionFormula, ValidationCalculation,
    };
    use om_data::{DataProvider, FieldKind, RelationshipMatch, RelationshipSide, SchemaChange};
    let mut root = indexed_tasks();
    index_schema_catalogs(&mut root);
    let file = fixture::TempFile::from_tree("gui-database-related-reads", &root);
    let hosted = hosted::host_file(
        "database-related-reads",
        std::path::Path::new(&file.0),
        fixture::FILE,
        (fixture::ACCOUNT, fixture::PASSWORD),
    );
    let mut provider = hosted.opened();
    provider.begin_schema_edit().unwrap();
    let schema = provider
        .apply_schema_change(SchemaChange::CreateOccurrence {
            table_id: fixture::NOTES,
            name: "Other notes".into(),
        })
        .unwrap();
    let other = schema
        .occurrences
        .iter()
        .find(|o| o.name == "Other notes")
        .unwrap()
        .id;
    for (right, left_field) in [
        (fixture::NOTES_OCCURRENCE, fixture::NAME),
        (other, fixture::CODE),
    ] {
        provider
            .apply_schema_change(SchemaChange::CreateRelationship {
                left_occurrence: fixture::OCCURRENCE,
                right_occurrence: right,
                matches: vec![RelationshipMatch {
                    left_field,
                    operator: 0,
                    right_field: fixture::BODY,
                }],
                left: RelationshipSide::default(),
                right: RelationshipSide::default(),
            })
            .unwrap();
    }
    let mut schema = provider.database_schema().unwrap();
    for name in ["Both", "Kept"] {
        schema = provider
            .apply_schema_change(SchemaChange::CreateField {
                table_id: fixture::TABLE,
                name: name.into(),
                data_type: FieldKind::Text,
                comment: String::new(),
            })
            .unwrap();
    }
    let id = |name: &str| {
        schema
            .table(fixture::TABLE)
            .unwrap()
            .fields
            .iter()
            .find(|f| f.name == name)
            .unwrap()
            .id
    };
    let (both, kept) = (id("Both"), id("Kept"));
    let formula = |text: &str| OptionFormula::new(text, fixture::OCCURRENCE);
    let mut options = FieldOptions::default();
    options.auto_enter.value = AutoEnterValue::Calculated(AutoCalculation {
        replace_existing: true,
        ..AutoCalculation::new(formula("Notes::Body & \"|\" & Other notes::Body"))
    });
    provider
        .apply_schema_change(SchemaChange::SetFieldOptions {
            table_id: fixture::TABLE,
            field_id: both,
            options: Box::new(options),
        })
        .unwrap();
    let mut options = FieldOptions::default();
    options.auto_enter.value = AutoEnterValue::Calculated(AutoCalculation {
        replace_existing: true,
        ..AutoCalculation::new(formula("\"default\""))
    });
    options.validation.calculation = Some(ValidationCalculation {
        formula: formula("Notes::Body ≠ \"\""),
        only_if_modified: false,
    });
    provider
        .apply_schema_change(SchemaChange::SetFieldOptions {
            table_id: fixture::TABLE,
            field_id: kept,
            options: Box::new(options),
        })
        .unwrap();
    provider.commit_schema_changes().unwrap();

    let notes = u32::try_from(fixture::NOTES_LAYOUT).unwrap();
    let body = |text: &str| {
        [("Body".to_owned(), Some(text.to_owned()))]
            .into_iter()
            .collect()
    };
    provider.create_record(notes, body("Yo")).unwrap();
    let tasks = u32::try_from(fixture::LAYOUT).unwrap();
    let set = |pairs: &[(&str, &str)]| {
        pairs
            .iter()
            .map(|(f, v)| ((*f).to_owned(), Some((*v).to_owned())))
            .collect()
    };
    let stored = |field: u64| {
        let saved = om_hbam::HbamFile::open(hosted.file()).unwrap();
        let tree = saved.key_tree().unwrap();
        let t = om_hbam::slot::uint_segment(fixture::TABLE);
        tree.node(&[&t, &[5], &[1], &om_hbam::slot::uint_segment(field)])
            .and_then(|n| om_hbam::Cell::from_node(n).text())
            .and_then(Result::ok)
    };
    // The note "Hi" related through both occurrences is read whole.
    provider
        .update_record(tasks, 1, set(&[("Name", "Hi"), ("Code", "Hi")]))
        .unwrap();
    provider.close();
    assert_eq!(stored(both).as_deref(), Some("Hi|Hi"));
    let mut provider = hosted.opened();
    provider
        .update_record(tasks, 1, set(&[("Kept", "manual")]))
        .unwrap();
    provider
        .update_record(tasks, 1, set(&[("Name", "Yo")]))
        .unwrap();
    provider.close();
    // Name evaluates "Both" through Notes and leaves "Kept", whose
    // validation alone reads through it.
    assert_eq!(stored(both).as_deref(), Some("Yo|Hi"));
    assert_eq!(stored(kept).as_deref(), Some("manual"));
}

#[test]
fn options_for_field_are_saved_on_an_openmaker_server() {
    use om_data::field_options::{
        AutoCalculation, AutoEnterValue, Indexing, Message, OptionFormula, Serial,
        ValidationCalculation,
    };
    use om_data::DataProvider;
    let mut root = indexed_tasks();
    index_schema_catalogs(&mut root);
    let file = fixture::TempFile::from_tree("gui-database-options", &root);
    let hosted = hosted::host_file(
        "database-options",
        std::path::Path::new(&file.0),
        fixture::FILE,
        (fixture::ACCOUNT, fixture::PASSWORD),
    );
    let layout = u32::try_from(fixture::LAYOUT).unwrap();
    let mut before = hosted.opened();
    let records = before.records(layout, 0, 100).unwrap();
    before.close();

    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    click(&mut app, "Open");
    wait_until(&mut app, "the hosted file", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled()
    });
    app.key_press_modifiers(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::D);
    wait_for_label(&mut app, "7 fields");
    row(&mut app, "Name");
    press(&mut app, "Options…");
    wait_for_label(&mut app, "Options for Field “Name”");
    shot(&mut app, "manage-database-field-options-auto-enter");
    app.get_by_role_and_label(Role::CheckBox, "Data:").click();
    app.run_steps(2);
    app.get_by_role_and_label(Role::MultilineTextInput, "Auto-Enter Data")
        .focus();
    app.step();
    app.get_by_role_and_label(Role::MultilineTextInput, "Auto-Enter Data")
        .type_text("todo");
    app.run_steps(2);
    tab(&mut app, "Validation");
    app.get_by_role_and_label(Role::CheckBox, "Not empty")
        .click();
    app.run_steps(2);
    // A custom message as a calculation, as FileMaker Pro 26 writes every
    // one (#431, #602).
    app.get_by_role_and_label(Role::CheckBox, "Display custom message if validation fails")
        .click();
    app.run_steps(2);
    click(&mut app, "Specify Message Calculation");
    wait_for_label(&mut app, "Specify Calculation “Name”");
    formula(&app).focus();
    app.run_steps(2);
    formula(&app).type_text("\"Name \" & \"is needed\"");
    app.run_steps(2);
    shot(&mut app, "manage-database-message-calculation");
    confirm(&mut app, "OK");
    wait_until(&mut app, "the message formula", |app| {
        app.query_by_label("Specify Calculation “Name”").is_none()
    });
    shot(&mut app, "manage-database-field-options-validation");
    tab(&mut app, "Storage");
    app.get_by_role_and_label(Role::RadioButton, "All").click();
    app.run_steps(2);
    shot(&mut app, "manage-database-field-options-storage");
    confirm(&mut app, "OK");
    // The document can be settled while Manage Database applies its edit.
    // Wait for its field editor before typing the next definition.
    wait_until(&mut app, "the options staged", |app| {
        app.query_by_label("Options for Field “Name”").is_none()
            && app.state().is_settled()
            && common::find_text_field(app, "Field Name")
                .is_some_and(|field| !field.accesskit_node().is_disabled())
    });
    replace(&mut app, "Field Name", "Seq");
    click(&mut app, "Database Field Type");
    click(&mut app, "Number");
    press(&mut app, "Create");
    wait_for_label(&mut app, "8 fields");
    row(&mut app, "Seq");
    press(&mut app, "Options…");
    wait_for_label(&mut app, "Options for Field “Seq”");
    app.get_by_role_and_label(Role::CheckBox, "Serial number")
        .click();
    app.run_steps(2);
    replace(&mut app, "Next Serial Value", "10");
    replace(&mut app, "Serial Increment", "2");
    tab(&mut app, "Validation");
    app.get_by_role_and_label(Role::CheckBox, "In range:")
        .click();
    app.run_steps(2);
    replace(&mut app, "Range From", "1");
    replace(&mut app, "Range To", "1000");
    // Checking "Validated by calculation" opens Specify Calculation.
    app.get_by_role_and_label(Role::CheckBox, "Validated by calculation")
        .click();
    app.run_steps(3);
    wait_for_label(&mut app, "Specify Calculation “Seq”");
    formula(&app).focus();
    app.run_steps(2);
    formula(&app).type_text("Self > 0");
    app.run_steps(2);
    shot(&mut app, "manage-database-validation-calculation");
    confirm(&mut app, "OK");
    wait_until(&mut app, "the validation formula", |app| {
        app.query_by_label("Specify Calculation “Seq”").is_none()
    });
    app.get_by_role_and_label(Role::CheckBox, "Display custom message if validation fails")
        .click();
    app.run_steps(2);
    app.get_by_role_and_label(Role::MultilineTextInput, "Custom Message")
        .focus();
    app.step();
    app.get_by_role_and_label(Role::MultilineTextInput, "Custom Message")
        .type_text("Positive only");
    app.run_steps(2);
    shot(&mut app, "manage-database-field-options-validation-rules");
    confirm(&mut app, "OK");
    wait_until(&mut app, "the serial staged", |app| {
        app.query_by_label("Options for Field “Seq”").is_none()
            && app.state().is_settled()
            && common::find_text_field(app, "Field Name")
                .is_some_and(|field| !field.accesskit_node().is_disabled())
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    replace(&mut app, "Field Name", "Label");
    click(&mut app, "Database Field Type");
    click(&mut app, "Text");
    press(&mut app, "Create");
    wait_for_label(&mut app, "9 fields");
    row(&mut app, "Label");
    press(&mut app, "Options…");
    wait_for_label(&mut app, "Options for Field “Label”");
    // Checking "Calculated value" opens Specify Calculation; its OK checks
    // the box.
    app.get_by_role_and_label(Role::CheckBox, "Calculated value")
        .click();
    app.run_steps(3);
    wait_for_label(&mut app, "Specify Calculation “Label”");
    formula(&app).focus();
    app.run_steps(2);
    formula(&app).type_text("Upper ( Name )");
    app.run_steps(2);
    shot(&mut app, "manage-database-auto-enter-calculation");
    confirm(&mut app, "OK");
    wait_until(&mut app, "the calculated value", |app| {
        app.query_by_label("Specify Calculation “Label”").is_none()
    });
    shot(&mut app, "manage-database-field-options-calculated-value");
    confirm(&mut app, "OK");
    wait_until(&mut app, "the calculated value staged", |app| {
        app.query_by_label("Options for Field “Label”").is_none() && app.state().is_settled()
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    press(&mut app, "OK");
    wait_until(&mut app, "the save", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.query_by_label("Manage Database for “Tasks.fmp12”")
            .is_none()
    });

    let mut fresh = hosted.opened();
    let schema = fresh.database_schema().unwrap();
    let tasks = schema.table(fixture::TABLE).unwrap();
    let options = |name: &str| {
        tasks
            .fields
            .iter()
            .find(|f| f.name == name)
            .and_then(|f| f.options.clone())
            .unwrap_or_else(|| panic!("{name}'s options read back"))
    };
    let name = options("Name");
    assert_eq!(name.auto_enter.value, AutoEnterValue::Data("todo".into()));
    assert!(name.validation.not_empty);
    assert_eq!(
        name.validation.message,
        Some(Message::Calculation(OptionFormula::new(
            "\"Name \" & \"is needed\"",
            fixture::OCCURRENCE
        )))
    );
    assert_eq!(name.storage.indexing, Indexing::All);
    let seq = options("Seq");
    assert_eq!(
        seq.auto_enter.value,
        AutoEnterValue::Serial(Serial {
            on_commit: false,
            next: "10".into(),
            increment: 2,
        })
    );
    let rules = &seq.validation;
    assert_eq!(rules.range, Some(("1".into(), "1000".into())));
    assert_eq!(
        rules.calculation,
        Some(ValidationCalculation {
            formula: OptionFormula::new("Self > 0", fixture::OCCURRENCE),
            only_if_modified: true,
        })
    );
    assert_eq!(rules.message, Some(Message::Text("Positive only".into())));
    assert_eq!(
        options("Label").auto_enter.value,
        AutoEnterValue::Calculated(AutoCalculation::new(OptionFormula::new(
            "Upper ( Name )",
            fixture::OCCURRENCE
        )))
    );
    let after = fresh.records(layout, 0, 100).unwrap();
    assert_eq!(after.total, records.total);
    for record in &records.records {
        let again = after.records.iter().find(|r| r.id == record.id).unwrap();
        for (field, value) in records.fields.iter().zip(&record.values) {
            let at = after
                .fields
                .iter()
                .position(|f| f.name == field.name)
                .unwrap();
            assert_eq!(&again.values[at], value, "{} of {}", field.name, record.id);
        }
    }
    // The table stays writable, and a refusal shows the calculated message,
    // not the text `18` holds for older versions.
    let first = after.records[0].id;
    let refused = fresh
        .update_record(
            layout,
            first,
            [("Name".to_owned(), Some(String::new()))]
                .into_iter()
                .collect(),
        )
        .unwrap_err();
    assert_eq!(refused.message, "Name is needed");
    fresh
        .update_record(
            layout,
            first,
            [("Name".to_owned(), Some("Renamed".to_owned()))]
                .into_iter()
                .collect(),
        )
        .unwrap();
    fresh.close();
}

#[test]
#[ignore = "renders Manage Database reference PNGs in the system temporary directory"]
fn render_database_dialogs() {
    let mut app = mock();
    connect_to_mock(&mut app);
    open_database(&mut app);
    app.set_render_cursor(false);
    for (tab, name) in [
        ("Fields", "fields"),
        ("Tables", "tables"),
        ("Relationships", "relationships"),
    ] {
        app.get_by_role_and_label(Role::Button, tab).click();
        app.run_steps(3);
        let path = std::env::temp_dir().join(format!("openmaker-manage-database-{name}.png"));
        app.render()
            .expect("render database manager")
            .save(&path)
            .expect("save database manager reference");
        eprintln!("wrote {}", path.display());
    }
}

fn hex(digits: &str) -> Vec<u8> {
    let digits: String = digits.split_whitespace().collect();
    (0..digits.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&digits[i..i + 2], 16).unwrap())
        .collect()
}

/// The Tasks file with a value-list catalog `[21]` (file-metadata.md 6):
/// list 1 "Priorities", custom values `High`, `-`, `Low`; list 2 "Names",
/// values from Tasks::Name, also showing Tasks::Count and sorted by it,
/// re-sorted by German; list 3 "Odd", of a type FileMaker does not define
/// (9). The display order is 2, 1, 3.
fn value_lists_host() -> hosted::Hosted {
    use om_hbam::OwnedNode;
    let mut root = fixture::tree(&[fixture::script(1, "Nothing", Vec::new())], true);
    let name = |text: &str| OwnedNode::leaf([0x10], om_scsu::encode_scsu(text));
    let custom = OwnedNode::list(
        [1],
        vec![
            OwnedNode::leaf([3], om_scsu::encode_scsu("High\r-\rLow")),
            name("Priorities"),
        ],
    );
    let mut definition = hex("01 0002 0403d00001 020101 0001 0403d00001 020102 0001 00 00 00 01");
    definition.push(0x7a);
    definition.extend(hex(
        "0000 0001 020100 020100 0001 020100 020100 0001 00 00 00000019",
    ));
    let field = OwnedNode::list([2], vec![OwnedNode::leaf([2], definition), name("Names")]);
    let odd = OwnedNode::list([3], vec![OwnedNode::leaf([2], hex("09 0000")), name("Odd")]);
    let order = OwnedNode::list(
        [3],
        vec![
            OwnedNode::leaf([0, 0, 0, 1], [1, 2]),
            OwnedNode::leaf([0, 0, 0, 2], [1, 1]),
            OwnedNode::leaf([0, 0, 0, 3], [1, 3]),
        ],
    );
    root.put(
        &[&[0x21]],
        OwnedNode::list(
            [0x21],
            vec![
                OwnedNode::list([1], vec![order]),
                OwnedNode::list([5], vec![custom, field, odd]),
            ],
        ),
    );
    let file = fixture::TempFile::from_tree("gui-value-lists", &root);
    hosted::host_file(
        "value-lists",
        std::path::Path::new(&file.0),
        fixture::FILE,
        (fixture::ACCOUNT, fixture::PASSWORD),
    )
}

/// File > Manage > Value Lists over a hosted file shows the catalog the
/// provider reads, in display order, with the selected list's options and
/// the read-only reason; New, Edit, Duplicate and Delete stay disabled, and
/// Escape closes it leaving the window and the file as they were.
#[test]
fn manage_value_lists_shows_the_hosted_catalog_read_only() {
    use om_data::value_lists::{ReadOnly, Unreadable};
    use om_data::DataProvider;
    let hosted = value_lists_host();
    // The catalog through the provider operation the dialog and
    // get_value_lists share: ids and the ids FileMaker shows, display order.
    let mut reader = hosted.opened();
    let before = reader.value_list_catalog().unwrap();
    reader.close();
    let lists: Vec<_> = before
        .lists
        .iter()
        .map(|list| (list.id, list.shown_id, list.name.as_deref()))
        .collect();
    assert_eq!(
        lists,
        [
            (2, Some(2), Some("Names")),
            (1, Some(1), Some("Priorities")),
            (3, Some(3), Some("Odd"))
        ]
    );
    assert_eq!(before.lists[2].content, Err(Unreadable::Type(9)));
    assert_eq!(before.read_only, Some(ReadOnly::NoWriter));

    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    click(&mut app, "Open");
    wait_until(&mut app, "the file", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled()
    });
    let window = app.state().window_name();
    let record = common::loaded_record_number(&mut app);

    open_menu(&mut app, "File");
    click(&mut app, "Manage");
    click(&mut app, "Value Lists...");
    wait_for_label(&mut app, "3 value lists");
    let tops: Vec<f32> = ["Names", "Priorities", "Odd"]
        .iter()
        .map(|list| app.get_by_label(list).rect().top())
        .collect();
    assert!(tops.windows(2).all(|pair| pair[0] < pair[1]), "{tops:?}");
    assert!(is_on(&app, "Names") && !is_on(&app, "Odd"));
    // The first list's options, below the list.
    app.get_by_label("Sort values using second field");
    app.get_by_label("Re-sort values based on German");
    assert!(app
        .query_all_by_label(ReadOnly::NoWriter.message())
        .next()
        .is_some());
    for button in ["New...", "Edit...", "Duplicate", "Delete"] {
        assert!(
            app.get_by_label(button).accesskit_node().is_disabled(),
            "{button}"
        );
    }
    shot(&mut app, "manage-value-lists-catalog");

    click(&mut app, "Odd");
    assert!(is_on(&app, "Odd") && !is_on(&app, "Names"));
    // An unreadable list has no options; its reason is drawn in the Values
    // column.
    assert!(app
        .query_by_label("Sort values using second field")
        .is_none());
    shot(&mut app, "manage-value-lists-unreadable");

    app.key_press(Key::Escape);
    app.run_steps(2);
    wait_until(&mut app, "the dialog closed", |app| {
        app.query_by_label("3 value lists").is_none()
    });
    assert_eq!(app.state().mode(), om_gui::Mode::Browse);
    assert_eq!(app.state().window_name(), window);
    assert_eq!(common::record_number(&app), record);
    assert!(app.state().error().is_none() && app.state().is_settled());
    shot(&mut app, "manage-value-lists-dismissed");
    let mut reader = hosted.opened();
    assert_eq!(reader.value_list_catalog().unwrap(), before);
    reader.close();
}

/// Whether the selectable row `label` is selected.
fn is_on(app: &common::App, label: &str) -> bool {
    app.get_by_label(label).accesskit_node().toggled() == Some(egui::accesskit::Toggled::True)
}

/// The Relationships tab of a hosted file, its graph read by the native
/// provider (`om_provider::schema`). The fixture's relationships 2 and 3
/// (Tasks::Key to its two other Notes occurrences) are made cartesian and
/// `<` (file-metadata.md 4.2), and relationship 3 gets a second predicate,
/// `Tasks::Name ≥ Other notes::Body`. With `OM_SCREENSHOTS` set to a
/// folder, the tab is rendered there as `relationship-operators.png`
/// before the listed predicates are checked.
#[test]
#[ignore = "renders a hosted file's Relationships tab; set OM_SCREENSHOTS to a folder to keep it"]
fn render_hosted_relationship_operators() {
    let mut root = fixture::tree(&[fixture::script(1, "Capture", Vec::new())], true);
    fixture::add_calculated(&mut root);
    let (key, task) = (fixture::KEY, fixture::TASK);
    fixture::add_relationship(
        &mut root,
        2,
        (fixture::OCCURRENCE, key),
        6,
        (fixture::NOTES_BY_KEY, task),
    );
    fixture::add_relationship(
        &mut root,
        3,
        (fixture::OCCURRENCE, key),
        2,
        (fixture::OTHER_NOTES, task),
    );
    // Predicate 2: operator 5 (≥), Tasks::Name (1), Notes::Body (1).
    root.put(
        &[&[3], &[0xfb], &[5], &[3], &[3], &[2]],
        om_hbam::OwnedNode::leaf(Vec::new(), vec![5, 2, 1, 1, 2, 1, 1]),
    );
    let file = fixture::TempFile::from_tree("gui-relationship-operators", &root);
    let hosted = hosted::host_file(
        "relationship-operators",
        Path::new(&file.0),
        fixture::FILE,
        (fixture::ACCOUNT, fixture::PASSWORD),
    );
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    click(&mut app, "Open");
    wait_until(&mut app, "the file open", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled()
    });
    app.key_press_modifiers(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::D);
    wait_until(&mut app, "Manage Database", |app| {
        app.query_by_label("Relationships").is_some()
    });
    app.get_by_role_and_label(Role::Button, "Relationships")
        .click();
    wait_for_label(&mut app, "Tasks::Name = Notes::Task");
    if let Some(dir) = std::env::var_os("OM_SCREENSHOTS") {
        app.run_steps(2);
        app.set_render_cursor(false);
        let path = Path::new(&dir).join("relationship-operators.png");
        app.render()
            .expect("render the Relationships tab")
            .save(&path)
            .expect("save the Relationships tab");
        eprintln!("wrote {}", path.display());
    }
    for line in [
        "Tasks::Name = Notes::Task",
        "Tasks::Key × Notes by key::Task",
        "Tasks::Key < Other notes::Task and Tasks::Name ≥ Other notes::Body",
    ] {
        app.get_by_label(line);
    }
}

/// Field-row drags use the same staged relationship operations as Edit
/// Relationship: Cancel loses them; OK keeps both match pairs on reopen.
#[test]
fn graph_field_drags_stage_match_pairs_and_save_them_together() {
    use om_data::DataProvider;
    let mut root = indexed_tasks();
    index_schema_catalogs(&mut root);
    let file = fixture::TempFile::from_tree("gui-graph-field-link", &root);
    let hosted = hosted::host_file(
        "graph-field-link",
        Path::new(&file.0),
        fixture::FILE,
        (fixture::ACCOUNT, fixture::PASSWORD),
    );
    let mut before = hosted.opened();
    let original = before.database_schema().unwrap();
    before.close();
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    click(&mut app, "Open");
    wait_until(&mut app, "the file open", |app| app.state().is_settled());
    let drag_fields = |app: &mut common::App, source: &str, target: &str| {
        let from = app.get_all_by_label(source).last().unwrap().rect().center();
        let to = app.get_all_by_label(target).last().unwrap().rect().center();
        app.event(egui::Event::PointerMoved(from));
        app.event(egui::Event::PointerButton {
            pos: from,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        app.step();
        for step in 1..=6 {
            app.event(egui::Event::PointerMoved(
                from + (to - from) * (step as f32 / 6.0),
            ));
            app.step();
        }
        app.event(egui::Event::PointerButton {
            pos: to,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        app.run_steps(3);
    };
    for save in [false, true] {
        app.key_press_modifiers(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::D);
        wait_for_label(&mut app, "Database Table");
        tab(&mut app, "Relationships");
        press(&mut app, "Add a table occurrence");
        row(&mut app, "Tasks");
        confirm(&mut app, "OK");
        wait_for_label(&mut app, "Match field Tasks 2::Count");
        drag_fields(
            &mut app,
            "Match field Tasks::Count",
            "Match field Tasks 2::Count",
        );
        wait_for_label(&mut app, "Tasks::Count = Tasks 2::Count");
        // A reversed drag (from the stored right side) keeps the stored
        // relationship's sides: Count on the left, Name on the right.
        drag_fields(
            &mut app,
            "Match field Tasks 2::Name",
            "Match field Tasks::Count",
        );
        wait_for_label(
            &mut app,
            "Tasks::Count = Tasks 2::Count and Tasks::Count = Tasks 2::Name",
        );
        press(&mut app, if save { "OK" } else { "Cancel" });
        wait_until(&mut app, "the schema dialog closed", |app| {
            app.query_by_label("Relationships").is_none() && app.state().is_settled()
        });
        let mut reader = hosted.opened();
        let schema = reader.database_schema().unwrap();
        reader.close();
        if save {
            let second = schema
                .occurrences
                .iter()
                .find(|o| o.name == "Tasks 2")
                .unwrap();
            assert_eq!(second.table_id, Some(fixture::TABLE));
            let relation = schema
                .relationships
                .iter()
                .find(|r| r.right_occurrence == second.id)
                .unwrap();
            assert_eq!(relation.matches.len(), 2);
            assert_eq!(
                (
                    relation.matches[0].left_field,
                    relation.matches[0].right_field
                ),
                (fixture::COUNT, fixture::COUNT)
            );
            assert_eq!(
                (
                    relation.matches[1].left_field,
                    relation.matches[1].right_field
                ),
                (fixture::COUNT, fixture::NAME)
            );
            let first = schema
                .occurrences
                .iter()
                .find(|o| o.name == "Tasks")
                .unwrap();
            assert_eq!(relation.left_occurrence, first.id);
        } else {
            assert_eq!(schema.occurrences, original.occurrences);
            assert_eq!(schema.relationships, original.relationships);
        }
    }
}

/// The graph's Duplicate (#430): Tasks and Tasks 2, related on Count and
/// selected with Shift-click, are copied as "Tasks 3" and "Tasks 2 2" with
/// the relationship between them. Cancel loses the copies; OK saves them.
#[test]
fn graph_duplicate_copies_selected_occurrences_and_their_relationship() {
    use om_data::DataProvider;
    let mut root = indexed_tasks();
    index_schema_catalogs(&mut root);
    let file = fixture::TempFile::from_tree("gui-graph-duplicate", &root);
    let hosted = hosted::host_file(
        "graph-duplicate",
        Path::new(&file.0),
        fixture::FILE,
        (fixture::ACCOUNT, fixture::PASSWORD),
    );
    let mut before = hosted.opened();
    let original = before.database_schema().unwrap();
    before.close();
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    click(&mut app, "Open");
    wait_until(&mut app, "the file open", |app| app.state().is_settled());
    let drag_fields = |app: &mut common::App, source: &str, target: &str| {
        let from = app.get_all_by_label(source).last().unwrap().rect().center();
        let to = app.get_all_by_label(target).last().unwrap().rect().center();
        app.event(egui::Event::PointerMoved(from));
        app.event(egui::Event::PointerButton {
            pos: from,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        app.step();
        for step in 1..=6 {
            app.event(egui::Event::PointerMoved(
                from + (to - from) * (step as f32 / 6.0),
            ));
            app.step();
        }
        app.event(egui::Event::PointerButton {
            pos: to,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        app.run_steps(3);
    };
    let occurrence = |app: &common::App, name: &str| {
        app.query_all_by_role_and_label(Role::ListBoxOption, name)
            .next()
            .is_some()
    };
    for save in [false, true] {
        app.key_press_modifiers(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::D);
        wait_for_label(&mut app, "Database Table");
        tab(&mut app, "Relationships");
        press(&mut app, "Add a table occurrence");
        row(&mut app, "Tasks");
        confirm(&mut app, "OK");
        wait_for_label(&mut app, "Match field Tasks 2::Count");
        drag_fields(
            &mut app,
            "Match field Tasks::Count",
            "Match field Tasks 2::Count",
        );
        wait_for_label(&mut app, "Tasks::Count = Tasks 2::Count");
        app.query_all_by_role_and_label(Role::ListBoxOption, "Tasks")
            .next()
            .unwrap()
            .click();
        app.run_steps(2);
        app.query_all_by_role_and_label(Role::ListBoxOption, "Tasks 2")
            .next()
            .unwrap()
            .click_modifiers(Modifiers::SHIFT);
        app.run_steps(2);
        press(&mut app, "Duplicate selected objects");
        wait_for_label(&mut app, "Tasks 3::Count = Tasks 2 2::Count");
        assert!(occurrence(&app, "Tasks 3") && occurrence(&app, "Tasks 2 2"));
        press(&mut app, if save { "OK" } else { "Cancel" });
        wait_until(&mut app, "the schema dialog closed", |app| {
            app.query_by_label("Relationships").is_none() && app.state().is_settled()
        });
        let mut reader = hosted.opened();
        let schema = reader.database_schema().unwrap();
        reader.close();
        if save {
            let named = |name: &str| {
                schema
                    .occurrences
                    .iter()
                    .find(|o| o.name == name)
                    .unwrap_or_else(|| panic!("{name}"))
            };
            let (left, right) = (named("Tasks 3"), named("Tasks 2 2"));
            assert_eq!(left.table_id, Some(fixture::TABLE));
            assert_eq!(right.table_id, Some(fixture::TABLE));
            let relation = schema
                .relationships
                .iter()
                .find(|r| (r.left_occurrence, r.right_occurrence) == (left.id, right.id))
                .unwrap();
            assert_eq!(relation.matches.len(), 1);
            assert_eq!(
                (
                    relation.matches[0].left_field,
                    relation.matches[0].right_field
                ),
                (fixture::COUNT, fixture::COUNT)
            );
        } else {
            assert_eq!(schema.occurrences, original.occurrences);
            assert_eq!(schema.relationships, original.relationships);
        }
    }
}

/// The graph's Arrange tools (#430): Tasks 2 and Notes 2, selected with
/// Shift-click, aligned by their top edges and then their left edges, are
/// saved at one place.
#[test]
fn graph_arrange_aligns_the_selected_occurrences() {
    use om_data::DataProvider;
    let mut root = indexed_tasks();
    index_schema_catalogs(&mut root);
    let file = fixture::TempFile::from_tree("gui-graph-arrange", &root);
    let hosted = hosted::host_file(
        "graph-arrange",
        Path::new(&file.0),
        fixture::FILE,
        (fixture::ACCOUNT, fixture::PASSWORD),
    );
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    click(&mut app, "Open");
    wait_until(&mut app, "the file open", |app| app.state().is_settled());
    app.key_press_modifiers(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::D);
    wait_for_label(&mut app, "Database Table");
    tab(&mut app, "Relationships");
    for (table, name) in [("Tasks", "Tasks 2"), ("Notes", "Notes 2")] {
        press(&mut app, "Add a table occurrence");
        row(&mut app, table);
        confirm(&mut app, "OK");
        wait_until(&mut app, name, |app| {
            app.query_all_by_role_and_label(Role::ListBoxOption, name)
                .next()
                .is_some()
        });
    }
    // Every box in view, so the clicks land on them.
    press(&mut app, "Zoom out");
    press(&mut app, "Zoom out");
    app.query_all_by_role_and_label(Role::ListBoxOption, "Tasks 2")
        .next()
        .unwrap()
        .click();
    app.run_steps(2);
    app.query_all_by_role_and_label(Role::ListBoxOption, "Notes 2")
        .next()
        .unwrap()
        .click_modifiers(Modifiers::SHIFT);
    app.run_steps(2);
    for (tool, option) in [
        ("Align objects vertically", "Align Top Edges"),
        ("Align objects", "Align Left Edges"),
    ] {
        press(&mut app, tool);
        click(&mut app, option);
        app.run_steps(5);
    }
    press(&mut app, "OK");
    wait_until(&mut app, "the schema dialog closed", |app| {
        app.query_by_label("Relationships").is_none() && app.state().is_settled()
    });
    let mut reader = hosted.opened();
    let schema = reader.database_schema().unwrap();
    reader.close();
    let place = |name: &str| {
        let occurrence = schema.occurrences.iter().find(|o| o.name == name).unwrap();
        let graph = occurrence.graph.unwrap();
        (graph.x, graph.y)
    };
    assert_eq!(place("Tasks 2"), place("Notes 2"));
}

/// The Relationships tab's Add Table and Add Relationship open FileMaker's
/// Specify Table and Edit Relationship, whose OK stages the occurrence and
/// the relationship. `OM_SCREENSHOTS` keeps both dialogs.
#[test]
fn specify_table_and_edit_relationship_stage_the_graph() {
    let mut app = mock();
    connect_to_mock(&mut app);
    open_database(&mut app);
    tab(&mut app, "Relationships");
    press(&mut app, "Add a table occurrence");
    app.run_steps(2);
    row(&mut app, "Contacts");
    assert_eq!(
        text_field(&app, "Name").value().as_deref(),
        Some("Contacts 2")
    );
    shot(&mut app, "manage-database-specify-table");
    confirm(&mut app, "OK");
    wait_until(&mut app, "the new occurrence", |app| {
        app.query_all_by_label("Contacts 2").next().is_some()
    });
    press(&mut app, "Add a relationship");
    app.run_steps(2);
    click(&mut app, "Left Table");
    app.query_all_by_label("Contacts").last().unwrap().click();
    app.run_steps(2);
    click(&mut app, "Right Table");
    app.query_all_by_label("Contacts 2").last().unwrap().click();
    app.run_steps(2);
    app.query_all_by_role_and_label(Role::Button, "Email")
        .next()
        .unwrap()
        .click();
    app.run_steps(1);
    app.query_all_by_role_and_label(Role::Button, "Email")
        .last()
        .unwrap()
        .click();
    app.run_steps(1);
    press(&mut app, "Add");
    assert!(app.query_all_by_label("Email   =   Email").next().is_some());
    // "Sort records" on the right side (drawn last) opens Sort Records on
    // that occurrence's fields; its order keeps the box checked.
    let sort_records = |app: &common::App| {
        app.query_all_by_role_and_label(Role::CheckBox, "Sort records")
            .last()
            .unwrap()
            .accesskit_node()
            .toggled()
    };
    app.query_all_by_role_and_label(Role::CheckBox, "Sort records")
        .last()
        .unwrap()
        .click();
    app.run_steps(4);
    wait_for_label(&mut app, "Sort Records");
    app.query_all_by_label("Last Name").last().unwrap().click();
    app.run_steps(2);
    press(&mut app, "Move");
    shot(&mut app, "manage-database-relationship-sort");
    confirm(&mut app, "OK");
    wait_until(&mut app, "Sort Records closed", |app| {
        app.query_by_label("Sort Records").is_none()
    });
    assert_eq!(sort_records(&app), Some(egui::accesskit::Toggled::True));
    shot(&mut app, "manage-database-edit-relationship");
    confirm(&mut app, "OK");
    // The graph draws it, and selecting and deleting it removes it.
    wait_for_label(&mut app, "Contacts::Email = Contacts 2::Email");
    shot(&mut app, "manage-database-graph-related");
    row(&mut app, "Contacts::Email = Contacts 2::Email");
    press(&mut app, "Delete selected objects");
    app.get_by_label("Delete the selected relationship?");
    confirm(&mut app, "Delete");
    wait_until(&mut app, "the relationship gone", |app| {
        app.query_by_label("Contacts::Email = Contacts 2::Email")
            .is_none()
    });
}

/// The Relationships tab of the `.fmp12` file at `OM_RELATIONSHIPS_FILE`,
/// hosted on loopback from a copy and opened as its Admin with the password
/// in `OM_RELATIONSHIPS_PASSWORD` (empty when unset), rendered into the folder
/// `OM_SCREENSHOTS` as `relationships-100.png`, then with the first
/// occurrence selected and zoomed out as `relationships-50.png`: the
/// comparison with FileMaker Pro's own graph of the same file.
#[test]
#[ignore = "requires OM_RELATIONSHIPS_FILE (an .fmp12 file whose Admin's password is OM_RELATIONSHIPS_PASSWORD, empty when unset) and OM_SCREENSHOTS (a folder)"]
fn render_relationships_of_a_file() {
    let source = std::env::var_os("OM_RELATIONSHIPS_FILE").expect("OM_RELATIONSHIPS_FILE");
    let dir = std::env::var_os("OM_SCREENSHOTS").expect("OM_SCREENSHOTS");
    let source = Path::new(&source);
    let name = source.file_name().unwrap().to_string_lossy();
    let password = std::env::var("OM_RELATIONSHIPS_PASSWORD").unwrap_or_default();
    let hosted = hosted::host_file(
        "relationships-of-a-file",
        source,
        &name,
        ("Admin", &password),
    );
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    click(&mut app, "Open");
    wait_until(&mut app, "the file open", |app| {
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        app.state().is_settled()
    });
    app.key_press_modifiers(Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::D);
    wait_until(&mut app, "Manage Database", |app| {
        app.query_by_label("Relationships").is_some()
    });
    app.get_by_role_and_label(Role::Button, "Relationships")
        .click();
    wait_until(&mut app, "the graph", |app| {
        app.query_all_by_role(Role::ListBoxOption).next().is_some()
    });
    let save = |app: &mut common::App, file: &str| {
        app.run_steps(2);
        app.set_render_cursor(false);
        let path = Path::new(&dir).join(file);
        app.render()
            .expect("render the Relationships tab")
            .save(&path)
            .expect("save the Relationships tab");
        eprintln!("wrote {}", path.display());
    };
    save(&mut app, "relationships-100.png");
    app.query_all_by_role(Role::ListBoxOption)
        .next()
        .unwrap()
        .click();
    for _ in 0..2 {
        click(&mut app, "Zoom out");
    }
    save(&mut app, "relationships-50.png");
}
