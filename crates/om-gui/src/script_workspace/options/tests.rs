//! The options dialog's edits: OK with no change answers the step byte for
//! byte, and each changed row re-encodes and decodes back.

use super::*;
use om_data::scripts::specs::{SortEntry, SpecField};

fn calc(text: &str) -> Calculation {
    calculation::compile(text, None).unwrap()
}

/// A stored step's data key, with the stale filler files carry, and its
/// `KUniqueID`.
fn stored(mut step: ScriptStep, key: u8) -> ScriptStep {
    step.data_key = StoredKey([2, 1, key, 0, 0x6f, 0]);
    step.set_unique_ids([key; 16], Some([key ^ 0xff; 16]));
    step
}

fn contacts_field(field: u64) -> FieldSpec {
    // The occurrence key as files store it, with stale filler.
    let mut spec = FieldSpec::new(
        StoredKey([4, 3, 0xd0, 0, 2, 0x6f]),
        StoredKey::from_id(field).unwrap(),
        1,
    );
    spec.0[9] = 0x6f;
    spec
}

/// Contacts template "Sort by Name" (11.1.1), keys as the segment `00`.
const SORT_BY_NAME: [u8; 28] = [
    0x00, 0x00, 0x00, 0x01, 0x04, 0x03, 0xd0, 0x00, 0x02, 0x02, 0x01, 0x08, 0x00, 0x01, 0x02, 0x01,
    0x00, 0x02, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

/// query-program.md 10.2's form: absent keys as `00`, which a rewrite
/// would spell `02 01 00`.
const SHORT_SORT: [u8; 23] = [
    0x00, 0x03, 0x00, 0x01, 0x03, 0x02, 0x80, 0x02, 0x02, 0x01, 0x02, 0x00, 0x01, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x19,
];

/// A request set as FileMaker Pro leaves it after an edit: a `fc`
/// modification count on the row.
fn edited_requests() -> OwnedNode {
    let mut node = specs::encode_requests(&[StoredRequest {
        omit: false,
        criteria: vec![(
            SpecField {
                occurrence: Some(0xd00002),
                field: 4,
                repetition: 1,
            },
            "In Progress".into(),
        )],
    }]);
    let rows = node.child_mut(&[0x06]).unwrap();
    rows.children[1]
        .children
        .push(OwnedNode::leaf([0xfc], [1, 2]));
    node
}

fn steps() -> Vec<ScriptStep> {
    let mut go_to_object = ScriptStep::new(145);
    go_to_object.set_calculation(0, Some(&calc("Get ( ScriptParameter )")));

    let mut set_variable = ScriptStep::new(141);
    set_variable.set_text(0, Some("$count"));
    set_variable.set_calculation(1, Some(&calc("$count + 1")));

    let mut set_field = ScriptStep::new(76);
    set_field.set_field_spec(0, Some(contacts_field(1)));
    set_field.set_calculation(0, Some(&calc("\"Ada\"")));

    let mut go_to_layout = ScriptStep::new(6);
    go_to_layout
        .set_parameter("Layout", ParamValue::Choice(5))
        .unwrap();
    go_to_layout.set_member_key(Some(StoredKey([2, 1, 3, 0, 0x6f, 0])));
    go_to_layout.set_list_value(6, Some(0));

    let mut perform_script = ScriptStep::new(1);
    perform_script.set_script_key(StoredKey::from_serial_id(3));
    perform_script.set_calculation(0, Some(&calc("\"p\"")));

    let mut sort = ScriptStep::new(39);
    sort.set_flag(flag::RESTORE | flag::NO_INTERACT, true);
    sort.set_data_leaf(
        StepData::SORT_ORDER,
        Some(bytes_node(&[StepData::SORT_ORDER], &SORT_BY_NAME)),
    );

    let mut short_sort = ScriptStep::new(39);
    short_sort.set_flag(flag::RESTORE, true);
    short_sort.set_data_leaf(
        StepData::SORT_ORDER,
        Some(bytes_node(&[StepData::SORT_ORDER], &SHORT_SORT)),
    );

    let mut find = ScriptStep::new(28);
    find.set_flag(flag::RESTORE, true);
    find.set_data_leaf(StepData::REQUESTS, Some(edited_requests()));

    let mut commit = ScriptStep::new(75);
    commit.set_flag(flag::OPTION | flag::NO_INTERACT, true);

    let mut insert = ScriptStep::new(160);
    insert.set_flag(flag::VERIFY_SSL_CERTIFICATES | flag::SELECT_ALL, true);
    insert.set_variable_target(0, "$out");
    insert.set_calculation(0, Some(&calc("\"https://example.com\"")));
    insert.set_calculation(1, Some(&calc("\"-X POST\"")));

    // Export Records as a new step has it (no type, no order), and with
    // a path list, a type and an order stored.
    let export_new = ScriptStep::new(36);
    let mut export = ScriptStep::new(36);
    export.set_path_list("file:Contacts.csv").unwrap();
    let csv = om_transfer::Profile::new(om_transfer::FileType::CommaSeparated);
    export.set_data_leaf(
        StepData::PROFILE,
        Some(bytes_node(&[StepData::PROFILE], &csv.encode())),
    );
    export.set_flag(flag::RESTORE, true);
    export.set_data_leaf(
        StepData::EXPORT_ORDER,
        Some(bytes_node(
            &[StepData::EXPORT_ORDER],
            &super::super::export_dialog::new_order().encode(),
        )),
    );

    // Show Custom Dialog with FileMaker Pro's default buttons: OK commits
    // ([00 3, 03] = 1), Cancel does not ([00 4, 03] = 0).
    let mut dialog = ScriptStep::new(87);
    dialog.set_calculation(0, Some(&calc("\"Hello\"")));
    dialog.set_text(0, Some("OK"));
    dialog.set_text(1, Some("Cancel"));
    dialog.set_number_bool(3, Some(true));
    dialog.set_number_bool(4, Some(false));

    // Show Custom Dialog with inputs: input 1 a field with a label, input 2
    // a variable with "Use password character".
    let mut inputs = ScriptStep::new(87);
    inputs.set_field_spec(0, Some(contacts_field(1)));
    inputs.set_calculation(2, Some(&calc("\"First\"")));
    inputs.set_variable_target(1, "$secret");
    inputs.set_number_bool(1, Some(true));
    // Input 3 as FileMaker Pro stores an unused input (a Pro-made step on
    // #221): the empty key, table 0 and field 0.
    inputs.set_field_spec(2, Some(pro_unused_input()));

    // Go to Related Record into a card window, showing only the records
    // related to the whole found set.
    let mut related = ScriptStep::new(74);
    related.set_flag(flag::TURN_ON | flag::RESTORE | flag::OPTION2, true);
    related.set_number(0, Some(WindowOptions::CARD_WORD as i32));
    related.set_calculation(0, Some(&calc("\"Related\"")));
    related.set_calculation(1, Some(&calc("300")));

    // Replace Field Contents with serial numbers from 1001 by 5, updating
    // the field's entry options.
    let mut replace = ScriptStep::new(91);
    replace.set_field_spec(0, Some(contacts_field(1)));
    replace.set_number(1, Some(2));
    replace.set_text(3, Some("1001"));
    replace.set_number(2, Some(5));
    replace.set_number_bool(4, Some(true));

    [
        go_to_object,
        set_variable,
        set_field,
        go_to_layout,
        perform_script,
        sort,
        short_sort,
        find,
        commit,
        insert,
        export_new,
        export,
        dialog,
        inputs,
        related,
        replace,
    ]
    .into_iter()
    .zip(1u8..)
    .map(|(step, key)| stored(step, key))
    .collect()
}

/// The step decoded again from its descriptor and data node.
fn round_trip(step: &ScriptStep) {
    let mut back = ScriptStep::from_descriptor(&step.descriptor());
    back.data = step.data_node().map(|n| StepData::decode(&n));
    assert_eq!(&back, step);
}

fn edit<'a>(options: &'a mut StepOptions, label: &str) -> &'a mut Edit {
    let labels: Vec<String> = options.rows.iter().map(|r| r.label.clone()).collect();
    &mut options
        .rows
        .iter_mut()
        .find(|r| r.label == label)
        .unwrap_or_else(|| panic!("no row {label} in {labels:?}"))
        .edit
}

fn ok(options: &StepOptions) -> ScriptStep {
    let step = options.result().unwrap();
    round_trip(&step);
    step
}

#[test]
fn ok_without_changes_answers_the_step_byte_for_byte() {
    for step in steps() {
        let options = StepOptions::new(step.clone(), None);
        let answer = options.result().unwrap();
        assert_eq!(answer.descriptor(), step.descriptor(), "{}", step.name());
        assert_eq!(answer.data_node(), step.data_node(), "{}", step.name());
        assert_eq!(answer, step);
    }
}

#[test]
fn show_custom_dialogs_buttons_are_labelled_and_commit_in_their_slots() {
    let mut options = StepOptions::new(stored(ScriptStep::new(87), 30), None);
    // Without stored bytes, the default button commits and the others do
    // not, as the runtime reads them.
    let shown = |options: &mut StepOptions, label: &str| match edit(options, label) {
        Edit::Flag { on, .. } => *on,
        _ => unreachable!(),
    };
    assert!(shown(&mut options, "Default Button commits data"));
    assert!(!shown(&mut options, "Button 3 commits data"));
    for (label, text) in [("Default Button", "Yes"), ("Button 3", "Later")] {
        if let Edit::Text { text: t, .. } = edit(&mut options, label) {
            *t = text.into();
        }
    }
    if let Edit::Flag { on, .. } = edit(&mut options, "Button 3 commits data") {
        *on = true;
    }
    let answer = ok(&options);
    assert_eq!(answer.text(0).as_deref(), Some("Yes"));
    assert_eq!(answer.text(1), None);
    assert_eq!(answer.text(2).as_deref(), Some("Later"));
    // The commit flags are [00 3..5, 03] (script-steps.md, Show Custom
    // Dialog inputs): one byte, written only when its checkbox changed.
    // Each labelled button gets its byte, as FileMaker Pro stores one: the
    // default button's shown (unchanged) "commits", Button 3's checked one.
    assert_eq!(
        (answer.number(3), answer.number(4), answer.number(5)),
        (Some(1), None, Some(1))
    );
    let mut again = StepOptions::new(answer, None);
    assert!(matches!(edit(&mut again, "Button 3"), Edit::Text { text, .. } if text == "Later"));
    if let Edit::Flag { on, .. } = edit(&mut again, "Default Button commits data") {
        *on = false;
    }
    assert_eq!(ok(&again).number(3), Some(0));

    // A calculated label is what the dialog shows: its text label is kept.
    let mut calculated = ScriptStep::new(87);
    calculated.set_text(0, Some("OK"));
    calculated.set_calculation(5, Some(&calc("\"Continue\"")));
    let mut options = StepOptions::new(stored(calculated, 31), None);
    assert!(matches!(edit(&mut options, "Default Button"), Edit::Kept));
    assert!(matches!(edit(&mut options, "Button 2"), Edit::Text { .. }));
}

/// The field spec FileMaker Pro 26.0.3 stores at `[00 1, 10]` and
/// `[00 2, 10]` of a new Show Custom Dialog for its unused inputs.
fn pro_unused_input() -> FieldSpec {
    FieldSpec::from_bytes(&[
        0x02, 0x01, 0x00, 0x00, 0xf0, 0x4a, 0x02, 0x01, 0x00, 0x00, 0x79, 0x00, 0x00, 0x01,
    ])
    .unwrap()
}

#[test]
fn show_custom_dialogs_unused_inputs_as_pro_stores_them_show_no_target() {
    let mut step = ScriptStep::new(87);
    step.set_field_spec(1, Some(pro_unused_input()));
    let mut options = StepOptions::new(stored(step, 33), None);
    assert!(matches!(
        edit(&mut options, "Input 2"),
        Edit::Field { value: None, .. }
    ));
}

#[test]
fn show_custom_dialogs_inputs_take_a_target_a_label_and_a_password_flag() {
    let mut options = StepOptions::new(stored(ScriptStep::new(87), 32), None);
    if let Edit::Field { value, .. } = edit(&mut options, "Input 1") {
        *value = Some(FieldTarget::Field {
            occurrence: 0xd00002,
            field: 2,
            repetition: 1,
        });
    }
    if let Edit::Calculation { text, .. } = edit(&mut options, "Input 1 label") {
        *text = "\"Last name\"".into();
    }
    if let Edit::Field { value, .. } = edit(&mut options, "Input 2") {
        *value = Some(FieldTarget::Variable("$pin".into()));
    }
    if let Edit::Flag { on, .. } = edit(&mut options, "Input 2 uses password character") {
        *on = true;
    }
    let answer = ok(&options);
    // Input 1 in mReserved with kFieldSpec, its label in slot 2.
    let spec = answer.slot_field_spec(0).unwrap();
    assert_eq!(
        (spec.table().id(), spec.field().id()),
        (Some(0xd00002), Some(2))
    );
    assert!(answer.has_flag(flag::FIELD_SPEC));
    assert!(answer.calculation(2).is_some());
    // Input 2 a variable at [00 1, 10], its name at slot 0x21, and its
    // password flag at [00 1, 03].
    assert!(answer.slot_field_spec(1).unwrap().is_variable());
    assert_eq!(answer.text(VARIABLE_NAME_SLOT + 1).as_deref(), Some("$pin"));
    assert_eq!(answer.number(1), Some(1));
    // Input 3 untouched: nothing stored for it.
    assert!(answer.slot_field_spec(2).is_none() && answer.calculation(4).is_none());
    // Opened again, the rows read them back.
    let mut again = StepOptions::new(answer, None);
    assert!(matches!(
        edit(&mut again, "Input 2"),
        Edit::Field { value: Some(FieldTarget::Variable(name)), .. } if name == "$pin"
    ));
    assert!(matches!(
        edit(&mut again, "Input 2 uses password character"),
        Edit::Flag { on: true, .. }
    ));
}

#[test]
fn go_to_related_records_options_set_its_flags_window_and_found_set() {
    let mut options = StepOptions::new(stored(ScriptStep::new(74), 34), None);
    // Without "Show in new window", its rows are not shown or written.
    if let Edit::Calculation { text, .. } = edit(&mut options, "Window Name") {
        *text = "\"Ignored\"".into();
    }
    assert!(ok(&options).calculation(0).is_none());
    for label in [
        "Show in new window",
        "Show only related records",
        "Match all records in current found set",
    ] {
        if let Edit::Flag { on, .. } = edit(&mut options, label) {
            *on = true;
        }
    }
    if let Edit::Calculation { text, .. } = edit(&mut options, "Width") {
        *text = "500".into();
    }
    if let Edit::WindowStyle { value, .. } = edit(&mut options, "Window Style") {
        *value = WindowOptions::DIALOG_WORD;
    }
    let answer = ok(&options);
    assert!(answer.has_flag(flag::TURN_ON | flag::RESTORE | flag::OPTION2));
    assert!(!answer.has_flag(flag::OPTION));
    // The window's name typed while hidden is written once it shows.
    assert!(answer.calculation(0).is_some() && answer.calculation(2).is_some());
    assert_eq!(
        answer.number(0).map(|w| w as u32),
        Some(WindowOptions::DIALOG_WORD)
    );
    // Opened again, the rows read them back.
    let mut again = StepOptions::new(answer, None);
    assert!(matches!(
        edit(&mut again, "Match all records in current found set"),
        Edit::Flag { on: true, .. }
    ));
    // Table by Calculation (26.1, `0x400` and slot 8): no picker, so no
    // stored occurrence beside the calculation the runtime follows.
    let mut by_calc = stored(ScriptStep::new(74), 35);
    by_calc.set_flag(flag::OPTION3, true);
    by_calc.set_calculation(8, Some(&calc("\"Tasks\"")));
    let mut options = StepOptions::new(by_calc, None);
    assert!(matches!(edit(&mut options, "Related Table"), Edit::Kept));
    let answer = ok(&options);
    assert!(answer.calculation(8).is_some());
    assert!(answer.data.as_ref().and_then(|d| d.slot(0)).is_none());
}

#[test]
fn go_to_object_shows_35bs_rows_and_compiles_the_typed_text() {
    let step = steps().remove(0);
    let mut options = StepOptions::new(step, None);
    assert_eq!(options.title, "\"Go to Object\" Options");
    assert!(options.description.starts_with("Specify an object"));
    let rows: Vec<(&str, &str)> = options
        .rows
        .iter()
        .map(|r| match &r.edit {
            Edit::Calculation { text, .. } => (r.label.as_str(), text.as_str()),
            _ => (r.label.as_str(), ""),
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("Object Name", "Get ( ScriptParameter )"),
            ("Repetition", "1")
        ]
    );
    if let Edit::Calculation { text, .. } = edit(&mut options, "Object Name") {
        *text = "\"First Name\"".into();
    }
    let answer = ok(&options);
    assert_eq!(answer.argument_text(0).as_deref(), Some("First Name"));
    // The repetition shown as the default 1 is not written.
    assert!(answer.calculation(1).is_none());
    // A formula that does not compile keeps the dialog open with
    // FileMaker's message.
    if let Edit::Calculation { text, .. } = edit(&mut options, "Object Name") {
        *text = "1 +".into();
    }
    assert!(options.result().is_err());
}

#[test]
fn specified_calculations_are_stored_as_answered() {
    let step = steps().remove(1);
    let mut options = StepOptions::new(step, None);
    let answered = calc("$count * 2");
    if let Edit::Calculation {
        text, specified, ..
    } = edit(&mut options, "Value")
    {
        *text = "$count * 2".into();
        *specified = Some(answered.clone());
    }
    if let Edit::Text { text, .. } = edit(&mut options, "Name") {
        *text = "$total".into();
    }
    let answer = ok(&options);
    assert_eq!(answer.calculation(1), Some(answered));
    assert_eq!(answer.text(0).as_deref(), Some("$total"));
}

#[test]
fn fields_targets_layouts_and_scripts_are_set_by_their_pickers() {
    let all = steps();
    // Set Field: another field of the same occurrence.
    let mut options = StepOptions::new(all[2].clone(), None);
    assert!(matches!(
        edit(&mut options, "Field"),
        Edit::Field {
            value: Some(FieldTarget::Field { field: 1, .. }),
            ..
        }
    ));
    if let Edit::Field { value, .. } = edit(&mut options, "Field") {
        *value = Some(FieldTarget::Field {
            occurrence: 0xd00002,
            field: 2,
            repetition: 1,
        });
    }
    let answer = ok(&options);
    let spec = answer.field_spec().unwrap();
    assert_eq!(
        (spec.table().id(), spec.field().id()),
        (Some(0xd00002), Some(2))
    );
    assert_eq!(answer.calculation(0), all[2].calculation(0));

    // Go to Layout: another layout.
    let mut options = StepOptions::new(all[3].clone(), None);
    if let Edit::Layout { value, .. } = edit(&mut options, "Specified Layout") {
        *value = Some(7);
    }
    let answer = ok(&options);
    assert_eq!(answer.member_key().and_then(|k| k.id()), Some(7));
    assert_eq!((answer.value, answer.list_value(6)), (5, Some(0)));

    // Perform Script: another script, then a calculated name.
    let mut options = StepOptions::new(all[4].clone(), None);
    if let Edit::Script { value, .. } = edit(&mut options, "Script") {
        *value = Some(141);
    }
    assert_eq!(ok(&options).script_id(), Some(141));
    if let Edit::ScriptBy { by_name, .. } = edit(&mut options, "Specified") {
        *by_name = true;
    }
    if let Edit::Calculation { text, .. } = edit(&mut options, "Script Name") {
        *text = "\"Other\"".into();
    }
    let answer = ok(&options);
    assert_eq!(answer.list_value(1), Some(1));
    assert_eq!(answer.argument_text(2).as_deref(), Some("Other"));
    assert!(answer.has_flag(BY_NAME_FLAGS));

    // Insert from URL: the variable target becomes a field.
    let mut options = StepOptions::new(all[9].clone(), None);
    assert!(matches!(
        edit(&mut options, "Target"),
        Edit::Field {
            value: Some(FieldTarget::Variable(name)),
            target: true,
            ..
        } if name == "$out"
    ));
    if let Edit::Field { value, .. } = edit(&mut options, "Target") {
        *value = Some(FieldTarget::Field {
            occurrence: 0xd00002,
            field: 1,
            repetition: 1,
        });
    }
    if let Edit::Flag { on, .. } = edit(&mut options, "With dialog") {
        *on = false;
    }
    let answer = ok(&options);
    assert!(!answer.field_spec().unwrap().is_variable());
    assert_eq!(answer.text(VARIABLE_NAME_SLOT), None);
    assert!(answer.has_flag(flag::NO_INTERACT));
    assert_eq!(answer.calculation(1), all[9].calculation(1));
}

#[test]
fn flags_toggle_their_bits_only() {
    let step = steps().remove(8);
    let mut options = StepOptions::new(step.clone(), None);
    if let Edit::Flag { on, .. } = edit(&mut options, "Skip data entry validation") {
        assert!(*on);
        *on = false;
    }
    let answer = ok(&options);
    assert_eq!(answer.flags, step.flags & !flag::OPTION);
}

#[test]
fn sort_orders_are_rewritten_only_when_edited() {
    let all = steps();
    // The short form decodes, and OK keeps its bytes.
    let options = StepOptions::new(all[6].clone(), None);
    assert!(matches!(
        &options.rows[1].edit,
        Edit::Sort {
            editable: true,
            stored: Some(_),
            ..
        }
    ));
    assert_eq!(options.result().unwrap(), all[6]);

    let mut options = StepOptions::new(all[5].clone(), None);
    let mut spec = SortSpec::decode(&SORT_BY_NAME).unwrap();
    spec.fields[0].descending = true;
    spec.fields.push(SortEntry {
        field: SpecField {
            occurrence: Some(0xd00002),
            field: 1,
            repetition: 1,
        },
        summary: None,
        value_list: None,
        descending: false,
        locale: 25,
    });
    if let Edit::Sort { edited, .. } = edit(&mut options, "Specify sort order") {
        *edited = Some(spec.clone());
    }
    let answer = ok(&options);
    let stored = answer.data_leaf(StepData::SORT_ORDER).unwrap();
    assert_eq!(SortSpec::from_node(stored), Some(spec));
    assert!(answer.has_flag(flag::RESTORE | flag::EXTENDED_DATA));

    // Unchecking "Specify sort order" clears Restore and keeps the order.
    let mut options = StepOptions::new(all[5].clone(), None);
    if let Edit::Sort { restore, .. } = edit(&mut options, "Specify sort order") {
        *restore = false;
    }
    let answer = ok(&options);
    assert!(!answer.has_flag(flag::RESTORE));
    assert_eq!(
        answer.data_leaf(StepData::SORT_ORDER),
        all[5].data_leaf(StepData::SORT_ORDER)
    );
}

#[test]
fn find_requests_are_rewritten_only_when_edited() {
    let step = steps().remove(7);
    let mut options = StepOptions::new(step.clone(), None);
    let Edit::Find {
        stored: Some(stored),
        ..
    } = edit(&mut options, "Specify find requests")
    else {
        panic!("the requests decode");
    };
    let mut requests = stored.clone();
    requests.push(StoredRequest {
        omit: true,
        criteria: vec![(
            SpecField {
                occurrence: Some(0xd00002),
                field: 1,
                repetition: 2,
            },
            "==$name".into(),
        )],
    });
    if let Edit::Find { edited, .. } = edit(&mut options, "Specify find requests") {
        *edited = Some(requests.clone());
    }
    let answer = ok(&options);
    let node = answer.data_leaf(StepData::REQUESTS).unwrap();
    assert_eq!(specs::requests(node), Some(requests));
}

#[test]
fn rows_follow_the_schema_and_keep_what_they_do_not_edit() {
    // Show Custom Dialog: title, message, buttons and inputs edit; a
    // calculated button label is kept as stored.
    let mut dialog = ScriptStep::new(87);
    dialog.set_calculation(0, Some(&calc("\"Title\"")));
    dialog.set_text(0, Some("OK"));
    dialog.set_calculation(5, Some(&calc("\"Go\"")));
    let options = StepOptions::new(stored(dialog.clone(), 9), None);
    let labels: Vec<(&str, bool)> = options
        .rows
        .iter()
        .map(|r| (r.label.as_str(), matches!(r.edit, Edit::Kept)))
        .collect();
    assert_eq!(
        labels,
        [
            ("Title", false),
            ("Message", false),
            ("Default Button", true),
            ("Default Button commits data", false),
            ("Button 2", false),
            ("Button 2 commits data", false),
            ("Button 3", false),
            ("Button 3 commits data", false),
            ("Input 1", false),
            ("Input 1 label", false),
            ("Input 1 uses password character", false),
            ("Input 2", false),
            ("Input 2 label", false),
            ("Input 2 uses password character", false),
            ("Input 3", false),
            ("Input 3 label", false),
            ("Input 3 uses password character", false),
            ("Button Label Calculations", true)
        ]
    );
    // Go to Record: Exit after last only with Previous and Next.
    let mut record = ScriptStep::new(16);
    record
        .set_parameter("Record/Request/Page", ParamValue::Choice(4))
        .unwrap();
    let options = StepOptions::new(record, None);
    let shown: Vec<&str> = (0..options.rows.len())
        .filter(|i| options.visible(*i))
        .map(|i| options.rows[i].label.as_str())
        .collect();
    assert_eq!(shown, ["Record/Request/Page", "Exit after last"]);
    assert_eq!(title_case("cURL options"), "cURL Options");
    assert_eq!(title_case("Interval (seconds)"), "Interval (seconds)");
}

#[test]
fn compound_ai_options_keep_sibling_settings_and_refuse_inline_clearing() {
    let mut semantic = ScriptStep::new(218);
    semantic.set_list_value(8, Some(1));
    semantic.set_calculation(2, Some(&calc("\"Text query\"")));
    semantic.set_list_value(6, Some(1));
    semantic.set_calculation(5, Some(&calc("0.8")));
    semantic.set_flag(0x10 | 0x400, true);
    let semantic = stored(semantic, 44);
    let options = StepOptions::new(semantic.clone(), None);
    for name in ["Text/Image/Vector", "Cosine similarity"] {
        assert!(options
            .rows
            .iter()
            .any(|row| { row.param == name && matches!(row.edit, Edit::Kept) }));
    }
    assert_eq!(ok(&options), semantic);
    round_trip(&semantic);

    for id in [214, 220] {
        let mut step = ScriptStep::new(id);
        step.set_calculation(20, Some(&calc("\"Web script name\"")));
        step.set_calculation(21, Some(&calc("\"Function reference\"")));
        step.set_number_u16(19, Some(1));
        step.set_text(99, Some("unrecognized parameter data"));
        step.set_flag(0x2000000, true);
        let mut step = stored(step, 45);
        let before = step.clone();
        let options = StepOptions::new(step.clone(), None);
        assert!(options
            .rows
            .iter()
            .any(|row| { row.param == "Web script" && matches!(row.edit, Edit::Kept) }));
        assert_eq!(ok(&options), before);
        let parameter = step
            .schema()
            .unwrap()
            .params
            .iter()
            .position(|p| p.name == "Web script")
            .unwrap();
        assert!(super::super::CalculationTarget::for_option(&step, parameter, Some(20)).is_none());
        let line = om_data::scripts::display::step_line(&step, &NoStepNames);
        assert!(line
            .options
            .iter()
            .all(|span| { span.parameter != Some(parameter) || span.calculation_slot.is_none() }));
        let target = super::super::CalculationTarget {
            parameter,
            slot: 20,
        };
        assert!(super::super::set_parameter_calculation(&mut step, target, None).is_err());
        assert_eq!(step, before);
        round_trip(&step);
    }
}

#[test]
fn selected_layouts_and_tables_set_the_native_discriminator() {
    for id in [6, 122, 182] {
        let mut step = ScriptStep::new(id);
        // An unrelated stored option must survive.
        step.set_calculation(0, Some(&calc("123")));
        let step = stored(step, 41);
        let mut options = StepOptions::new(step.clone(), None);
        assert_eq!(options.result().unwrap(), step);
        let label = match id {
            6 => "Specified Layout",
            122 => "Layout",
            _ => "Table",
        };
        match edit(&mut options, label) {
            Edit::Layout { value, .. } | Edit::Table { value, .. } => *value = Some(0x8002),
            _ => panic!("expected a picker"),
        }
        let answer = ok(&options);
        assert_eq!(answer.member_key().and_then(|k| k.id()), Some(0x8002));
        assert_eq!(answer.value, if id == 182 { 1 } else { 5 });
        assert_eq!(answer.has_flag(flag::MENU_SELECTION), id != 122);
        assert_eq!(answer.calculation(0), step.calculation(0));
        assert_eq!(ok(&StepOptions::new(answer.clone(), None)), answer);
    }
}

/// Go to Related Record's layout (#770): a new step starts on `<Current
/// Layout>` (step value 1), a named layout goes back to it with value 1 and
/// no key, a calculated layout (value 3 or 4) stays as stored, and "Use
/// external table's layouts" takes input only for another file's table.
#[test]
fn related_records_layout_starts_on_and_returns_to_the_current_layout() {
    // No animation stored reads as None, and OK keeps nothing stored (#785).
    let mut options = StepOptions::new(stored(ScriptStep::new(74), 45), None);
    assert!(matches!(
        edit(&mut options, "Animation"),
        Edit::Choice { value: Some(0), .. }
    ));
    assert_eq!(ok(&options).list_value(6), None);

    let new = stored(super::super::editor::new_step(74), 42);
    assert_eq!(new.value, 1);
    let mut options = StepOptions::new(new.clone(), None);
    assert!(matches!(
        edit(&mut options, "Layout"),
        Edit::Layout { value: None, .. }
    ));
    assert_eq!(ok(&options), new);

    let mut named = ScriptStep::new(74);
    named.set_member_key(StoredKey::from_id(0x8002));
    named
        .set_parameter("Options", ParamValue::Choice(5))
        .unwrap();
    let named = stored(named, 43);
    // Specify Layout's <Current Layout>, also for a step that read as no
    // layout before (value 0 without a key, as OpenMaker made new steps).
    let pick_current = |step: &ScriptStep| {
        let mut options = StepOptions::new(step.clone(), None);
        if let Edit::Layout { value, picked, .. } = edit(&mut options, "Layout") {
            *value = None;
            *picked = true;
        }
        ok(&options)
    };
    for step in [named.clone(), stored(ScriptStep::new(74), 44)] {
        let answer = pick_current(&step);
        assert_eq!((answer.value, answer.member_key()), (1, None));
        assert_eq!(ok(&StepOptions::new(answer.clone(), None)), answer);
    }

    let mut by_name = named.clone();
    by_name.value = 3;
    by_name.set_calculation(5, Some(&calc("\"Assignee Details\"")));
    let mut options = StepOptions::new(by_name.clone(), None);
    assert!(matches!(edit(&mut options, "Layout"), Edit::Kept));
    assert_eq!(ok(&options), by_name);

    let mut schema = CalcSchema::new("Tasks", om_data::calc::Locale::en_us());
    schema.add_table(0x8001, "Tasks");
    schema.add_occurrence(0xd00001, "Tasks", 0x8001);
    schema.add_external_occurrence(om_data::host::ExternalOccurrence {
        id: 0xd00002,
        name: "Parts".into(),
        source: "Inventory".into(),
    });
    let schema = Arc::new(schema);
    for (occurrence, external) in [(0xd00001, false), (0xd00002, true)] {
        let mut options = StepOptions::new(new.clone(), Some(schema.clone()));
        if let Edit::Table { value, .. } = edit(&mut options, "Related Table") {
            *value = Some(occurrence);
        }
        let row = options
            .rows
            .iter()
            .position(|r| r.label == "Use external table's layouts")
            .unwrap();
        assert_eq!(options.enabled(row), external, "{occurrence:#x}");
        // Checked, it stays clearable whatever the table.
        if let Edit::Flag { on, .. } = &mut options.rows[row].edit {
            *on = true;
        }
        assert!(options.enabled(row));
    }
}

/// FileMaker Pro 26.1.1's steps by name (script-steps.md 12.4, #349):
/// Set Variable by Name's name is the calculation in slot 3, and Replace
/// Field Contents by Name's field the calculation in slot 1 with
/// `kFieldSpecByCalc`, beside Replace Field Contents' choice in the same
/// slot; both read back as written.
#[test]
fn steps_by_name_edit_their_name_calculations() {
    let mut options = StepOptions::new(stored(ScriptStep::new(250), 47), None);
    for (label, typed) in [("Name", "\"$x\""), ("Value", "1")] {
        if let Edit::Calculation { text, .. } = edit(&mut options, label) {
            *text = typed.into();
        }
    }
    let set = ok(&options);
    assert!(set.calculation(3).is_some() && set.calculation(1).is_some());
    assert_eq!(ok(&StepOptions::new(set.clone(), None)), set);

    let mut options = StepOptions::new(stored(ScriptStep::new(249), 48), None);
    if let Edit::Calculation { text, .. } = edit(&mut options, "Target Field by Name") {
        *text = "\"People::Name\"".into();
    }
    if let Edit::Choice { value, .. } = edit(&mut options, "Replace with") {
        *value = Some(3);
    }
    if let Edit::Calculation { text, .. } = edit(&mut options, "Calculated result") {
        *text = "\"x\"".into();
    }
    let replace = ok(&options);
    assert!(replace.calculation(1).is_some() && replace.calculation(0).is_some());
    assert_eq!(replace.number(1), Some(3));
    assert!(replace.has_flag(0x0800_0000));
    assert_eq!(ok(&StepOptions::new(replace.clone(), None)), replace);
}

/// Replace Field Contents' "Replace with" (11.8): the choice at `[00 1,
/// 03]`, serial numbers' initial value, increment and entry-options byte,
/// and a calculated result, which needs its calculation.
#[test]
fn replace_field_contents_sets_serial_numbers_or_a_calculated_result() {
    let new = stored(ScriptStep::new(91), 46);
    let mut options = StepOptions::new(new.clone(), None);
    assert!(matches!(
        edit(&mut options, "Replace with"),
        Edit::Choice { value: Some(1), .. }
    ));
    assert_eq!(ok(&options), new);

    if let Edit::Choice { value, .. } = edit(&mut options, "Replace with") {
        *value = Some(2);
    }
    for (label, typed) in [("Initial value", "1001"), ("Increment by", "5")] {
        if let Edit::Text { text, .. } | Edit::Number { text, .. } = edit(&mut options, label) {
            *text = typed.into();
        }
    }
    // "Update serial number in Entry Options" and "Perform auto-enter
    // options for fields" take no input and are never written.
    for label in [
        "Update serial number in Entry Options",
        "Perform auto-enter options for fields",
    ] {
        let row = options.rows.iter().position(|r| r.label == label).unwrap();
        assert!(!options.enabled(row), "{label}");
        if let Edit::Flag { on, .. } = &mut options.rows[row].edit {
            *on = !*on;
        }
    }
    // Typed while hidden, the calculation is not written.
    if let Edit::Calculation { text, .. } = edit(&mut options, "Calculated result") {
        *text = "\"x\"".into();
    }
    let serial = ok(&options);
    assert_eq!(serial.number(1), Some(2));
    assert_eq!(serial.text(3).as_deref(), Some("1001"));
    assert_eq!(serial.number(2), Some(5));
    assert_eq!(serial.number(4), None);
    assert!(!serial.has_flag(flag::RESTORE) && serial.number(6).is_none());
    assert!(serial.calculation(0).is_none());
    assert_eq!(ok(&StepOptions::new(serial.clone(), None)), serial);

    let mut options = StepOptions::new(new.clone(), None);
    if let Edit::Choice { value, .. } = edit(&mut options, "Replace with") {
        *value = Some(3);
    }
    assert!(options.result().is_err());
    if let Edit::Calculation { text, .. } = edit(&mut options, "Calculated result") {
        *text = "\"x\"".into();
    }
    let calculated = ok(&options);
    assert_eq!(calculated.number(1), Some(3));
    assert!(calculated.calculation(0).is_some());

    // A choice FileMaker is not known to store stays as stored, and so do
    // serial numbers from the field's Entry Options, which would override
    // the initial value and increment.
    let mut other = new.clone();
    other.set_number(1, Some(7));
    let mut entry = serial.clone();
    entry.set_number_bool(5, Some(true));
    for step in [other, entry] {
        let mut options = StepOptions::new(step.clone(), None);
        assert!(matches!(edit(&mut options, "Replace With"), Edit::Kept));
        assert_eq!(ok(&options), step);
    }
}
#[test]
fn psos_by_name_and_from_list_keep_the_parameter_and_runtime_selection() {
    use om_data::scripts::display::step_line;
    let mut step = ScriptStep::new(164);
    step.set_list_value(1, Some(1));
    step.set_flag(BY_NAME_FLAGS, true);
    step.set_calculation(2, Some(&calc("\"Named target\"")));
    step.set_calculation(0, Some(&calc("\"parameter\"")));
    let step = stored(step, 42);
    let mut options = StepOptions::new(step.clone(), None);
    assert!(matches!(
        edit(&mut options, "Specified"),
        Edit::ScriptBy { by_name: true, .. }
    ));
    assert_eq!(ok(&options), step);
    if let Edit::Calculation { text, .. } = edit(&mut options, "Script Name") {
        *text = "\"Changed target\"".into();
    }
    let answer = ok(&options);
    assert_eq!(answer.argument_text(2).as_deref(), Some("Changed target"));
    assert!(step_line(&answer, &NoStepNames)
        .text
        .contains("Specified: By name ; \"Changed target\""));
    // The edited name becomes hidden when the user chooses From list:
    // it must not undo that choice when OK applies the dialog's rows.
    if let Edit::ScriptBy { by_name, .. } = edit(&mut options, "Specified") {
        *by_name = false;
    }
    if let Edit::Script { value, .. } = edit(&mut options, "Script") {
        *value = Some(7);
    }
    let answer = ok(&options);
    assert_eq!(answer.list_value(1), Some(0));
    assert!(!answer.has_flag(BY_NAME_FLAGS));
    assert_eq!(answer.script_id(), Some(7));
    assert_eq!(answer.calculation(0), step.calculation(0));
    assert_eq!(answer.calculation(2), step.calculation(2));
    assert!(step_line(&answer, &NoStepNames)
        .text
        .contains("Specified: From list"));
    assert_eq!(ok(&StepOptions::new(answer.clone(), None)), answer);
}

#[test]
fn legacy_psos_names_open_unchanged_and_migrate_only_when_edited() {
    let mut step = ScriptStep::new(164);
    step.set_calculation(1, Some(&calc("\"Legacy\"")));
    let step = stored(step, 43);
    let mut options = StepOptions::new(step.clone(), None);
    assert_eq!(ok(&options), step);
    if let Edit::Calculation { text, .. } = edit(&mut options, "Script Name") {
        *text = "\"New\"".into();
    }
    let answer = ok(&options);
    assert_eq!(answer.list_value(1), Some(1));
    assert_eq!(answer.argument_text(2).as_deref(), Some("New"));
    if let Edit::ScriptBy { by_name, .. } = edit(&mut options, "Specified") {
        *by_name = false;
    }
    if let Edit::Script { value, .. } = edit(&mut options, "Script") {
        *value = Some(7);
    }
    let answer = ok(&options);
    assert_eq!(answer.script_id(), Some(7));
    assert!(answer.calculation(1).is_none());
    assert_eq!(answer.list_value(1), Some(0));
}

#[test]
fn new_windows_size_style_and_the_state_steps_are_set_in_their_options() {
    // New Window: the size typed, Card chosen; the position stays empty.
    let mut options = StepOptions::new(stored(ScriptStep::new(122), 30), None);
    for (label, text) in [("Height", "584"), ("Width", "668")] {
        if let Edit::Calculation { text: t, .. } = edit(&mut options, label) {
            *t = text.into();
        }
    }
    if let Edit::WindowStyle { value, .. } = edit(&mut options, "Window Style") {
        *value = WindowOptions::CARD_WORD;
    }
    let answer = ok(&options);
    // The word FileMaker stores at [00 0, 03], as Tasks' Add Related Record has it.
    assert_eq!(
        answer.number(0).map(|w| w as u32),
        Some(WindowOptions::CARD_WORD)
    );
    assert_eq!(
        om_data::scripts::display::step_line(&answer, &om_data::scripts::display::NoStepNames).text,
        "New Window [ Style: Card ; Height: 584 ; Width: 668 ]"
    );
    assert!(answer.calculation(1).is_some() && answer.calculation(2).is_some());
    assert!(answer.calculation(3).is_none() && answer.calculation(4).is_none());
    // Opened again, the card shows as Card.
    let mut again = StepOptions::new(answer, None);
    assert!(matches!(
        edit(&mut again, "Window Style"),
        Edit::WindowStyle { value, .. } if *value == WindowOptions::CARD_WORD
    ));

    // Set Error Capture: On is kTurnOn; kSetOnOff stays as stored.
    let mut options = StepOptions::new(stored(ScriptStep::new(86), 31), None);
    if let Edit::Flag { on, .. } = edit(&mut options, "State") {
        *on = true;
    }
    let answer = ok(&options);
    assert!(answer.has_flag(flag::TURN_ON) && !answer.has_flag(flag::SET_ON_OFF));
    let mut both = ScriptStep::new(86);
    both.set_flag(flag::TURN_ON | flag::SET_ON_OFF, true);
    let mut options = StepOptions::new(stored(both, 32), None);
    if let Edit::Flag { on, .. } = edit(&mut options, "State") {
        *on = false;
    }
    let answer = ok(&options);
    assert!(!answer.has_flag(flag::TURN_ON) && answer.has_flag(flag::SET_ON_OFF));

    // Move/Resize Window: the same four rows, slots 1-4.
    let mut options = StepOptions::new(stored(ScriptStep::new(119), 33), None);
    for (label, text) in [("Height", "400"), ("Left", "10")] {
        if let Edit::Calculation { text: t, .. } = edit(&mut options, label) {
            *t = text.into();
        }
    }
    let answer = ok(&options);
    assert!(answer.calculation(1).is_some() && answer.calculation(4).is_some());
    assert!(answer.calculation(2).is_none() && answer.calculation(3).is_none());
    assert_eq!(ok(&StepOptions::new(answer.clone(), None)), answer);
}

/// Pause/Resume Script: Indefinitely (0) or For duration (1, with the
/// seconds in slot 0); back to Indefinitely drops the duration, which the
/// runtime and the step text read as the choice.
#[test]
fn pause_resume_scripts_duration_is_chosen_and_dropped_with_the_choice() {
    let new = stored(ScriptStep::new(62), 47);
    let mut options = StepOptions::new(new.clone(), None);
    assert!(matches!(
        edit(&mut options, "Pause"),
        Edit::Choice { value: Some(0), .. }
    ));
    assert!(
        !options.visible(1),
        "the duration shows only for a duration"
    );
    assert_eq!(ok(&options), new);

    if let Edit::Choice { value, .. } = edit(&mut options, "Pause") {
        *value = Some(1);
    }
    assert!(options.result().is_err(), "a duration needs its seconds");
    if let Edit::Calculation { text, .. } = edit(&mut options, "Duration (seconds)") {
        *text = "5".into();
    }
    let timed = ok(&options);
    assert_eq!(timed.value, 1);
    assert!(timed.calculation(0).is_some());
    let timed_again = timed.clone();

    let mut options = StepOptions::new(timed, None);
    if let Edit::Choice { value, .. } = edit(&mut options, "Pause") {
        *value = Some(0);
    }
    let back = ok(&options);
    assert_eq!(back.value, 0);
    assert!(back.calculation(0).is_none() && !back.has_flag(flag::CALCULATION));

    // A stored choice 1 without a duration OKs unchanged; clearing a
    // stored duration is refused.
    let mut bare = new.clone();
    bare.set_parameter("Pause", ParamValue::Choice(1)).unwrap();
    assert_eq!(ok(&StepOptions::new(bare.clone(), None)), bare);
    let mut options = StepOptions::new(timed_again.clone(), None);
    if let Edit::Calculation { text, .. } = edit(&mut options, "Duration (seconds)") {
        text.clear();
    }
    assert!(options.result().is_err());

    // A duration stored at choice 0 reads as For duration, and OK keeps it.
    let mut zero = new.clone();
    zero.set_calculation(0, Some(&calc("3")));
    let mut options = StepOptions::new(zero.clone(), None);
    assert!(matches!(
        edit(&mut options, "Pause"),
        Edit::Choice { value: Some(1), .. }
    ));
    assert_eq!(ok(&options), zero);
}

/// Send Mail by the email client (script-steps.md 12.3): the addresses,
/// subject and message calculations, "One email per record", the collect
/// options shown only without it, and the attachments; a step sending by
/// SMTP keeps its options as stored.
#[test]
fn send_mail_sets_the_email_clients_addresses_message_and_attachments() {
    let new = stored(ScriptStep::new(63), 52);
    let mut options = StepOptions::new(new.clone(), None);
    assert_eq!(ok(&options), new);
    for (label, text) in [
        ("To", "\"pat@example.com\""),
        ("Subject", "\"Weekly report\""),
        ("Message", "\"See attached\""),
    ] {
        if let Edit::Calculation { text: t, .. } = edit(&mut options, label) {
            *t = text.into();
        }
    }
    if let Edit::Flag { on, .. } = edit(&mut options, "Collect To addresses across found set") {
        *on = true;
    }
    if let Edit::Paths { text, .. } = edit(&mut options, "Attachments") {
        *text = "file:report.pdf".into();
    }
    let mail = ok(&options);
    for slot in [0, 3, 4] {
        assert!(mail.calculation(slot).is_some(), "slot {slot}");
    }
    assert!(mail.calculation(1).is_none() && mail.calculation(2).is_none());
    assert_eq!(mail.number(0), Some(1));
    assert_eq!(mail.path_list().as_deref(), Some("file:report.pdf"));
    assert!(mail.has_flag(flag::FILE_PATH));
    assert_eq!(ok(&StepOptions::new(mail.clone(), None)), mail);

    // One email per record hides the collect options; one typed while
    // hidden is not written.
    let mut options = StepOptions::new(new.clone(), None);
    if let Edit::Flag { on, .. } = edit(&mut options, "One email per record") {
        *on = true;
    }
    if let Edit::Flag { on, .. } = edit(&mut options, "Collect CC addresses across found set") {
        *on = true;
    }
    let each = ok(&options);
    assert_eq!((each.number(3), each.number(1)), (Some(1), None));

    // By SMTP: kept as stored.
    let mut smtp = new.clone();
    smtp.set_number_bool(4, Some(true));
    let mut options = StepOptions::new(smtp.clone(), None);
    assert!(matches!(edit(&mut options, "Send Mail"), Edit::Kept));
    assert_eq!(ok(&options), smtp);
}

/// The records Save Records as PDF and Save Records as Excel take (11.7):
/// Records being browsed when nothing is stored; PDF in the step value,
/// Excel in slot 1 (two bytes) and only with Restore. Print's are
/// `print_writes_its_records_to_the_step_value_and_its_stored_settings`.
#[test]
fn saves_choose_the_records_being_browsed_or_the_current_record() {
    let new = stored(ScriptStep::new(144), 49);
    let mut options = StepOptions::new(new.clone(), None);
    assert!(matches!(
        edit(&mut options, "Records"),
        Edit::Choice { value: Some(1), .. }
    ));
    assert_eq!(ok(&options), new);
    if let Edit::Choice { value, .. } = edit(&mut options, "Records") {
        *value = Some(2);
    }
    let current = ok(&options);
    assert_eq!(current.value, 2);
    assert!(current.has_flag(flag::MENU_SELECTION));
    assert_eq!(ok(&StepOptions::new(current.clone(), None)), current);

    // Excel: shown with Restore, stored in slot 1 as two bytes.
    let new = stored(ScriptStep::new(143), 51);
    let mut options = StepOptions::new(new.clone(), None);
    let records = options
        .rows
        .iter()
        .position(|r| r.label == "Records")
        .unwrap();
    assert!(!options.visible(records));
    if let Edit::Flag { on, .. } = edit(&mut options, "Restore") {
        *on = true;
    }
    assert!(options.visible(records));
    if let Edit::Choice { value, .. } = edit(&mut options, "Records") {
        *value = Some(2);
    }
    let mut expected = new.clone();
    expected.set_flag(flag::RESTORE, true);
    expected.set_number_u16(1, Some(2));
    assert_eq!(ok(&options), expected);

    // Other stored options show as kept beside the records: a PDF with
    // only a calculation option, an Excel step with a calculation in slot 3.
    for (id, slot, label) in [(144, 4, "PDF Options"), (143, 3, "Excel Options")] {
        let mut other = stored(ScriptStep::new(id), 53);
        other.set_calculation(slot, Some(&calc("1")));
        let mut options = StepOptions::new(other.clone(), None);
        assert!(matches!(edit(&mut options, label), Edit::Kept), "{id}");
        assert_eq!(ok(&options), other, "{id}");
    }
}

/// Print's records (11.6, #843), as FileMaker Pro 26 writes them with
/// "Specify print options" (Restore): the step value, and the digit of
/// `printStyleSetting` in the stored settings' `M_PM` property list, one
/// byte, the record kept in the pieces it is stored in; no kMenuSelection.
/// Turning the options off clears only Restore. Without stored settings
/// the records take no choice and the options cannot be specified.
#[test]
fn print_writes_its_records_to_the_step_value_and_its_stored_settings() {
    // The common chunk, a property list long enough to be stored in
    // pieces, and a chunk OpenMaker does not read.
    let padding = "<key>com.example.pad</key><string>x</string>\n".repeat(30);
    let plist = format!(
        "<?xml version=\"1.0\"?>\n<plist version=\"1.0\">\n<dict>\n{padding}\
         \t<key>printStyleSetting</key>\n\t<integer>1</integer>\n</dict>\n</plist>\n"
    );
    let mut record = crate::printing::tests::COMMON.to_vec();
    record.extend_from_slice(b"M_PM");
    record.extend_from_slice(&(plist.len() as u32).to_be_bytes());
    record.extend_from_slice(plist.as_bytes());
    record.extend_from_slice(b"M_PD\x00\x00\x00\x02\x00\x01");
    let mut print = stored(ScriptStep::new(43), 60);
    print.value = 1;
    print.flags |= flag::RESTORE | flag::NO_INTERACT;
    print.set_data_leaf(0x88, Some(om_data::scripts::bytes_node(&[0x88], &record)));
    assert!(
        print.data_leaf(0x88).unwrap().value.is_none(),
        "stored in pieces"
    );
    let mut options = StepOptions::new(print.clone(), None);
    assert_eq!(ok(&options), print);
    if let Edit::Choice { value, .. } = edit(&mut options, "Records") {
        *value = Some(2);
    }
    let current = ok(&options);
    assert_eq!((current.value, current.flags), (2, print.flags));
    let digit = record.windows(10).position(|w| w == b"<integer>1").unwrap() + 9;
    let mut expected = record.clone();
    expected[digit] = b'2';
    let written = current.data_leaf(0x88).unwrap();
    assert_eq!(
        &*om_data::scripts::node_bytes(written).unwrap(),
        &expected[..]
    );
    assert_eq!(
        written.children.len(),
        print.data_leaf(0x88).unwrap().children.len()
    );
    // A step stored without kExtendedData keeps its flags as stored.
    let mut unflagged = print.clone();
    unflagged.flags &= !flag::EXTENDED_DATA;
    let mut options = StepOptions::new(unflagged.clone(), None);
    if let Edit::Choice { value, .. } = edit(&mut options, "Records") {
        *value = Some(2);
    }
    assert_eq!(ok(&options).flags, unflagged.flags);

    // Off: only Restore clears; the step value and the record stay.
    let mut options = StepOptions::new(current.clone(), None);
    let records = options
        .rows
        .iter()
        .position(|r| r.label == "Records")
        .unwrap();
    if let Edit::Flag { on, .. } = edit(&mut options, "Specify print options") {
        *on = false;
    }
    assert!(!options.visible(records));
    let mut off = current.clone();
    off.set_flag(flag::RESTORE, false);
    assert_eq!(ok(&options), off);

    // No stored settings: the records and the options are read-only.
    let mut bare = stored(ScriptStep::new(43), 61);
    bare.value = 2;
    bare.flags |= flag::RESTORE;
    let options = StepOptions::new(bare.clone(), None);
    let row = |label: &str| options.rows.iter().position(|r| r.label == label).unwrap();
    assert!(!options.enabled(row("Records")));
    assert!(
        options.enabled(row("Specify print options")),
        "Restore can go off"
    );
    bare.set_flag(flag::RESTORE, false);
    let options = StepOptions::new(bare.clone(), None);
    let row = |label: &str| options.rows.iter().position(|r| r.label == label).unwrap();
    assert!(!options.enabled(row("Specify print options")));
    assert_eq!(ok(&options), bare);
}

/// Every step of FileMaker's starter files can be recreated in the step
/// options: no parameter is only kept as stored, and OK without a change
/// answers each step byte for byte.
#[test]
#[ignore = "requires the shipped templates under re/extracted"]
fn the_starter_files_steps_are_all_editable_and_kept_byte_for_byte() {
    let dir = std::path::Path::new(
        "../../re/extracted/mac/FileMaker Pro 26/FileMaker Pro.app/Contents/Resources/DynamicTemplates",
    );
    let mut files = 0;
    let mut steps = 0;
    for template in std::fs::read_dir(dir).expect("the starter templates") {
        let template = template.unwrap().path();
        for file in std::fs::read_dir(&template).unwrap() {
            let path = file.unwrap().path();
            if path.extension().is_none_or(|e| e != "fmp12") {
                continue;
            }
            files += 1;
            let at = path.display().to_string();
            let hbam = om_hbam::HbamFile::open(&path).unwrap();
            let catalog = om_data::ScriptCatalog::decode(&hbam.key_tree().unwrap()).unwrap();
            for script in &catalog.scripts {
                for step in &script.steps {
                    steps += 1;
                    let options = StepOptions::new(step.clone(), None);
                    let kept: Vec<&str> = options
                        .rows
                        .iter()
                        .filter(|r| matches!(r.edit, Edit::Kept))
                        .map(|r| r.label.as_str())
                        .collect();
                    assert!(
                        kept.is_empty(),
                        "{at}, “{}”: {} keeps {kept:?}",
                        script.name,
                        step.name()
                    );
                    let answer = options.result().unwrap();
                    assert_eq!(&answer, step, "{at}, “{}”: {}", script.name, step.name());
                }
            }
        }
    }
    assert_eq!(files, 66);
    println!("{files} files, {steps} steps");
}
