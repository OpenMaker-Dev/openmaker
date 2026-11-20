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

/// The hosted Tasks file with `scripts`, Status Open for odd ids and Done
/// for even ones, opened in the desktop client.
fn open_tasks(
    name: &str,
    scripts: &[om_scripts::Script],
) -> (common::App, hosted::Hosted, fixture::TempFile) {
    let mut root = fixture::tree(scripts, true);
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
    let file = fixture::TempFile::from_tree(name, &root);
    let hosted = hosted::host_file(
        name,
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
    (app, hosted, file)
}

/// Run script `name` from the Scripts menu up to its dialog `label`.
fn run(app: &mut common::App, name: &str, label: &str) {
    common::open_menu(app, "Scripts");
    common::click(app, name);
    common::wait_for_label(app, label);
    common::click(app, "OK");
    common::wait_until(app, "the script to end", |app| {
        !app.state().script_running()
    });
}

/// The file an export wrote to the temporary folder as `name`, removed.
fn take_export(name: &str) -> String {
    let path = std::env::temp_dir()
        .join(format!("OpenMaker-S{}", std::process::id()))
        .join(name);
    let written = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    written
}

fn export_to(name: &str) -> ScriptStep {
    set_variable(
        "$p",
        &format!("\"file:\" & Get ( TemporaryPath ) & \"{name}\""),
    )
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
            export_to(&name),
            export_grouped(),
            show("\"exported \" & Get ( LastError )"),
        ],
    )];
    let (mut app, _hosted, _file) = open_tasks("grouped-export", &scripts);
    run(&mut app, "Export groups", "exported 0");
    // Done: Ship 5, Task 4 4, Task 6 6; Open: Task 3 3, Task 5 5, Write
    // tests 3.
    assert_eq!(
        take_export(&name),
        "Done\tShip\t15\t15\t1\t26\rOpen\tTask 3\t11\t11\t1\t\r"
    );
}

/// A found set sorted by one script and saved as Excel by another (as
/// Records > Sort and then Share > Save as Excel run): Save Records as
/// Excel [Restore; Records being browsed] restarts the running total and
/// subtotals the fraction at each Status, as the window shows them. Once
/// reported as unsorted values
/// ([#264](https://github.com/OpenMaker-Dev/openmaker/issues/264),
/// [#265](https://github.com/OpenMaker-Dev/openmaker/issues/265)).
#[test]
fn save_records_as_excel_after_a_sort_keeps_the_sorts_groups() {
    let name = format!("om-gui-sorted-{}.xlsx", std::process::id());
    let at = |field| FieldSpec::new(OCCURRENCE, field);
    let column = |field| ExportField {
        field: at(field),
        sum_by: FieldSpec::none(),
    };
    let options = ExportOptions {
        fields: vec![
            column(STATUS),
            column(fixture::NAME),
            column(RUNNING),
            column(SHARE),
        ],
        group_by: Vec::new(),
        formatting: false,
        charset: ExportCharset::Utf8,
        use_name_alias: Some(1),
        field_names: None,
    };
    let mut excel = ScriptStep::new(143);
    excel.set_data_leaf(0x85, Some(OwnedNode::leaf([0x85], options.encode())));
    // Slot 0 one byte and slot 1 two, the output path in slot 0's path
    // list, as FileMaker Pro stores them (script-steps.md 11.7).
    excel.set_number_bool(0, Some(false));
    excel.set_number_u16(1, Some(1));
    excel.flags |= flag::NO_INTERACT | flag::RESTORE;
    excel.set_path_list("file:$q").unwrap();
    let scripts = vec![
        fixture::script(
            1,
            "Sort tasks",
            vec![sort(&[STATUS, fixture::NAME]), show("\"sorted\"")],
        ),
        fixture::script(
            2,
            "Save rows",
            vec![
                fixture::on(86, true),
                set_variable("$q", &format!("Get ( TemporaryPath ) & \"{name}\"")),
                excel,
                show("\"exported \" & Get ( LastError )"),
            ],
        ),
    ];
    let (mut app, _hosted, _file) = open_tasks("sorted-excel", &scripts);
    run(&mut app, "Sort tasks", "sorted");
    run(&mut app, "Save rows", "exported 0");
    let path = std::env::temp_dir()
        .join(format!("OpenMaker-S{}", std::process::id()))
        .join(&name);
    let bytes = std::fs::read(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    let rows = om_transfer::import::read::workbook(&bytes, 0, "").unwrap();
    let cells: Vec<String> = rows
        .iter()
        .map(|row| row.iter().map(|c| c.concat()).collect::<Vec<_>>().join("|"))
        .collect();
    assert_eq!(
        cells,
        [
            "Status|Name|Running|Share",
            "Done|Ship|5|.3333333333333333",
            "Done|Task 4|9|.2666666666666667",
            "Done|Task 6|15|.4",
            "Open|Task 3|3|.2727272727272727",
            "Open|Task 5|8|.4545454545454545",
            "Open|Write tests|11|.2727272727272727",
        ]
    );
}
