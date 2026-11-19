//! A grouped export in the desktop client end to end (the import review's
//! C5): a script on the provider worker sorts the hosted Tasks file of
//! `om-server/tests/fixtures/tasks.rs` by Status and Name and exports it
//! grouped by Status, with summaries by Status (each group's computed once
//! over the sorted found set) and a grand total on the first row only;
//! the native provider against an in-process `om-server`.
mod common;
#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
mod hosted;

use std::path::Path;

use fixture::{OCCURRENCE, RUNNING, SHARE, STATUS, TABLE, TOTAL};
use om_hbam::slot::uint_segment;
use om_hbam::{serial, OwnedNode};
use om_scripts::specs::{SortEntry, SortSpec, SpecField};
use om_scripts::{flag, ScriptStep};
use om_transfer::spec::{ExportField, ExportOptions, FieldSpec};
use om_transfer::{ExportCharset, FileType, Profile};

fn sort(fields: &[u64]) -> ScriptStep {
    let spec = SortSpec {
        keep_sorted: true,
        blanks_last: false,
        fields: fields
            .iter()
            .map(|&field| SortEntry {
                field: SpecField {
                    occurrence: Some(OCCURRENCE),
                    field,
                    repetition: 1,
                },
                summary: None,
                value_list: None,
                descending: false,
                locale: 0,
            })
            .collect(),
    };
    let mut step = ScriptStep::new(39);
    step.set_data_leaf(0x0a, Some(OwnedNode::leaf([0x0a], spec.encode())));
    step.set_flag(flag::RESTORE, true);
    step.flags |= flag::MENU_SELECTION | flag::NO_INTERACT;
    step
}

/// Export Records [No dialog; tab-separated UTF-8; grouped by Status] to
/// `$p`.
fn export_grouped() -> ScriptStep {
    let at = |field| FieldSpec::new(OCCURRENCE, field);
    let column = |field, by: Option<u64>| ExportField {
        field: at(field),
        sum_by: by.map_or_else(FieldSpec::none, at),
    };
    let options = ExportOptions {
        fields: vec![
            column(STATUS, None),
            column(fixture::NAME, None),
            column(TOTAL, Some(STATUS)),
            column(RUNNING, Some(STATUS)),
            column(SHARE, Some(STATUS)),
            column(TOTAL, None),
        ],
        group_by: vec![(at(STATUS), 1)],
        formatting: false,
        charset: ExportCharset::Utf8,
        use_name_alias: Some(1),
        field_names: None,
    };
    let mut step = ScriptStep::new(36);
    step.set_data_leaf(0x85, Some(OwnedNode::leaf([0x85], options.encode())));
    step.set_data_leaf(
        0x86,
        Some(OwnedNode::leaf(
            [0x86],
            Profile::new(FileType::TabSeparated).encode(),
        )),
    );
    step.flags |= flag::NO_INTERACT | flag::RESTORE | flag::EXTENDED_DATA | flag::FILE_PATH;
    step.set_data_leaf(0x82, Some(om_scripts::text_node(&[0x82], "$p")));
    step
}

/// Show Custom Dialog [ "Export" ; message ] with an OK button.
fn show(message: &str) -> ScriptStep {
    let mut step = ScriptStep::new(87);
    step.set_calculation(0, Some(&fixture::calculated_calc("\"Export\"")));
    step.set_calculation(1, Some(&fixture::calculated_calc(message)));
    step.set_text(0, Some("OK"));
    step
}

fn set_variable(name: &str, text: &str) -> ScriptStep {
    let mut step = ScriptStep::new(141);
    step.set_text(0, Some(name));
    step.set_calculation(1, Some(&fixture::calculated_calc(text)));
    step
}

#[test]
fn a_grouped_export_summarizes_each_group() {
    let name = format!("om-gui-grouped-{}.tab", std::process::id());
    let scripts = vec![fixture::script(
        1,
        "Export groups",
        vec![
            fixture::on(86, true),
            sort(&[STATUS, fixture::NAME]),
            set_variable(
                "$p",
                &format!("\"file:\" & Get ( TemporaryPath ) & \"{name}\""),
            ),
            export_grouped(),
            show("\"exported \" & Get ( LastError )"),
        ],
    )];
    let mut root = fixture::tree(&scripts, true);
    fixture::add_tasks(&mut root, 3..=6);
    fixture::add_calculated(&mut root);
    let t = uint_segment(TABLE);
    for id in 1..=6u64 {
        let status = if id % 2 == 1 { "Open" } else { "Done" };
        root.put(
            &[&t, &[5], &serial::encode(id), &uint_segment(STATUS)],
            om_scripts::text_node(&[], status),
        );
    }
    let file = fixture::TempFile::from_tree("gui-grouped-export", &root);
    let hosted = hosted::host_file(
        "grouped-export",
        Path::new(&file.0),
        fixture::FILE,
        (fixture::ACCOUNT, fixture::PASSWORD),
    );
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "hosted file", |app| {
        assert!(app.state().error().is_none(), "native connection failed");
        app.state().is_settled()
    });
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Export groups");
    common::wait_for_label(&mut app, "exported 0");
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "the script to end", |app| {
        !app.state().script_running()
    });
    let path = std::env::temp_dir()
        .join(format!("OpenMaker-S{}", std::process::id()))
        .join(&name);
    let written = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    // Done: Ship 5, Task 4 4, Task 6 6; Open: Task 3 3, Task 5 5, Write
    // tests 3.
    assert_eq!(
        written,
        "Done\tShip\t15\t15\t1\t26\rOpen\tTask 3\t11\t11\t1\t\r"
    );
}
