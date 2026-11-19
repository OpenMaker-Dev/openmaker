//! End to end: finds in server scripts on fields that have no index
//! (script-runtime.md 13.19): a criterion on an unstored calculation is
//! computed on each candidate with the find operators; summary fields take
//! no criterion (Set Field into one in Find mode is 201) and global fields
//! are set, not searched.
//!
//! The Tasks file of `fixtures/tasks.rs` with `add_calculated`: records 1
//! "Write tests" (Count 3), 2 "Ship" (5), 3 "alpha" (7), 4 without a name
//! (1) and 5 with neither (Status "Open"); Label = Name & " (" & Count &
//! ")", which does not evaluate when both are empty.

#[path = "fixtures/tasks.rs"]
mod fixture;
#[path = "fixtures/server.rs"]
mod server;

use fixture::{
    calculated_calc as calc, script, TempFile, COUNT, LABEL, NAME, OCCURRENCE, SETTING, TABLE,
    TOTAL,
};
// Labels, the summary REPORT reads, is among the fields `add_calculated`
// adds.
use om_hbam::slot::uint_segment;
use om_hbam::{serial, OwnedNode, RidList};
use om_layout::{encode, Rect};
use om_scripts::specs::{self, SpecField, StoredRequest};
use om_scripts::{flag, text_node, FieldSpec, Script, ScriptStep, StoredKey};
use om_session::Session;

/// Set Field [Tasks::field; text].
fn set_field(field: u64, text: &str) -> ScriptStep {
    let mut step = ScriptStep::new(76);
    step.set_calculation(0, Some(&calc(text)));
    step.set_field_spec(
        0,
        Some(FieldSpec::new(
            StoredKey::from_id(OCCURRENCE).unwrap(),
            StoredKey::from_id(field).unwrap(),
            0,
        )),
    );
    step
}

fn set_var(name: &str, text: &str) -> ScriptStep {
    let mut step = ScriptStep::new(141);
    step.set_text(0, Some(name));
    step.set_calculation(1, Some(&calc(text)));
    step
}

/// Perform Quick Find with the text of `text`, a calculation, then exit
/// with the error and the found count.
fn quick_find_then_count(text: &str) -> Vec<ScriptStep> {
    let mut step = ScriptStep::new(150);
    step.set_calculation(0, Some(&calc(text)));
    vec![
        fixture::on(86, true),
        step,
        exit_with("Get ( LastError ) & \":\" & Get ( FoundCount )"),
    ]
}

/// Find Matching Records on Tasks::`field` (none: the field with the
/// focus), `value` 1 replacing the found set, 2 constraining it, 3
/// extending it, then exit with the error and the found count.
fn find_matching_then_count(field: Option<u64>, value: u16) -> Vec<ScriptStep> {
    let mut step = ScriptStep::new(155);
    step.value = value;
    if let Some(field) = field {
        step.set_field_spec(
            0,
            Some(FieldSpec::new(
                StoredKey::from_id(OCCURRENCE).unwrap(),
                StoredKey::from_id(field).unwrap(),
                0,
            )),
        );
    }
    vec![
        fixture::on(86, true),
        step,
        exit_with("Get ( LastError ) & \":\" & Get ( FoundCount )"),
    ]
}

fn exit_with(text: &str) -> ScriptStep {
    let mut step = ScriptStep::new(103);
    step.set_calculation(0, Some(&calc(text)));
    step
}

/// Perform Find [Restore] with one request of these criteria.
fn stored_find(criteria: &[(u64, &str)]) -> ScriptStep {
    let request = StoredRequest {
        omit: false,
        criteria: criteria
            .iter()
            .map(|(field, text)| {
                (
                    SpecField {
                        occurrence: Some(OCCURRENCE),
                        field: *field,
                        repetition: 1,
                    },
                    text.to_string(),
                )
            })
            .collect(),
    };
    let mut step = ScriptStep::new(28);
    step.set_data_leaf(0x06, Some(specs::encode_requests(&[request])));
    step.set_flag(flag::RESTORE, true);
    step
}

/// The result every script ends with: the error of the step that set the
/// first criterion, the find's error, the found count and, when records
/// were found, their labels (summary Labels, a list of Label) and the
/// global Setting.
const REPORT: &str = "$set & \"/\" & Get ( LastError ) & \":\" & Get ( FoundCount ) & \
    If ( Get ( FoundCount ) ; \":\" & Substitute ( Tasks::Labels ; \"¶\" ; \",\" ) & \":\" & \
    Tasks::Setting )";

/// The errors alone, for a find that fails: the window stays in Find mode.
const ERRORS: &str = "$set & \"/\" & Get ( LastError )";

/// Enter Find Mode, the criteria by Set Field, Perform Find, `report`.
fn find_by_set_field(id: u64, name: &str, criteria: &[(u64, &str)], report: &str) -> Script {
    let mut steps = vec![fixture::on(86, true), ScriptStep::new(22)];
    for (i, (field, text)) in criteria.iter().enumerate() {
        steps.push(set_field(*field, text));
        if i == 0 {
            steps.push(set_var("$set", "Get ( LastError )"));
        }
    }
    steps.push(ScriptStep::new(28));
    steps.push(exit_with(report));
    script(id, name, steps)
}

fn scripts() -> Vec<Script> {
    vec![
        find_by_set_field(1, "Label ship", &[(LABEL, "\"ship\"")], REPORT),
        find_by_set_field(2, "Label phrase", &[(LABEL, "\"\"\"(7)\"\"\"")], REPORT),
        find_by_set_field(
            3,
            "Label and Count",
            &[(LABEL, "\"s*\""), (COUNT, "\">4\"")],
            REPORT,
        ),
        find_by_set_field(4, "Label none", &[(LABEL, "\"zeta\"")], ERRORS),
        find_by_set_field(5, "Total", &[(TOTAL, "\"16\"")], ERRORS),
        find_by_set_field(
            6,
            "Total and Name",
            &[(TOTAL, "\"16\""), (NAME, "\"ship\"")],
            REPORT,
        ),
        script(
            7,
            "Setting",
            vec![
                fixture::on(86, true),
                set_field(SETTING, "\"x\""),
                ScriptStep::new(22),
                set_field(SETTING, "\"y\""),
                set_var("$set", "Get ( LastError )"),
                set_field(NAME, "\"ship\""),
                ScriptStep::new(28),
                exit_with(REPORT),
            ],
        ),
        script(
            8,
            "Stored",
            vec![
                fixture::on(86, true),
                set_var("$set", "0"),
                stored_find(&[(TOTAL, "16"), (SETTING, "x"), (LABEL, "alpha")]),
                exit_with(REPORT),
            ],
        ),
        script(
            9,
            "Stored ignored only",
            vec![
                fixture::on(86, true),
                set_var("$set", "0"),
                stored_find(&[(TOTAL, "16"), (SETTING, "x")]),
                exit_with(ERRORS),
            ],
        ),
        find_by_set_field(10, "Label empty", &[(LABEL, "\"=\"")], FOUND),
        find_by_set_field(11, "Label any", &[(LABEL, "\"*\"")], FOUND),
        script(
            13,
            "No criteria",
            vec![
                fixture::on(86, true),
                fixture::go_to_record(2),
                ScriptStep::new(22),
                ScriptStep::new(28),
                set_var("$e", "Get ( LastError )"),
                exit_with(
                    "$e & \":\" & Get ( WindowMode ) & \":\" & Get ( FoundCount ) & \":\" & \
                     Get ( RecordNumber ) & \":\" & Get ( RequestCount )",
                ),
            ],
        ),
        script(
            14,
            "Reads without a record",
            vec![
                fixture::on(86, true),
                set_field(SETTING, "\"x\""),
                ScriptStep::new(22),
                set_var("$g", "Tasks::Setting & EvaluationError ( Tasks::Setting )"),
                set_var(
                    "$n",
                    "Tasks::Name & EvaluationError ( Tasks::Name ) & IsEmpty ( Tasks::Name )",
                ),
                set_field(NAME, "\"zeta\""),
                set_var("$c", "Tasks::Name & EvaluationError ( Tasks::Name )"),
                ScriptStep::new(28),
                set_var(
                    "$empty",
                    "Get ( FoundCount ) & \",\" & Tasks::Setting & EvaluationError ( \
                     Tasks::Setting ) & \",\" & Tasks::Name & EvaluationError ( Tasks::Name )",
                ),
                exit_with("$g & \"/\" & $n & \"/\" & $c & \"/\" & $empty"),
            ],
        ),
        script(
            12,
            "Label read",
            vec![
                fixture::go_to_record(2),
                exit_with("IsEmpty ( Tasks::Label ) & \"|\" & Length ( Tasks::Label )"),
            ],
        ),
        script(40, "Quick Find", quick_find_then_count("\"ship\"")),
        script(41, "Quick Find no word", quick_find_then_count("\"  \"")),
        script(
            42,
            "Quick Find no match",
            quick_find_then_count("\"write qqq\""),
        ),
        script(
            43,
            "Quick Find merge field",
            quick_find_then_count("\"open\""),
        ),
        script(44, "Matching Name", find_matching_then_count(Some(NAME), 1)),
        script(
            45,
            "Matching Status",
            find_matching_then_count(Some(fixture::STATUS), 1),
        ),
        script(46, "Matching no field", find_matching_then_count(None, 1)),
    ]
}

/// A find's error, the found count and the first found record's id.
const FOUND: &str =
    "$set & \"/\" & Get ( LastError ) & \":\" & Get ( FoundCount ) & \":\" & Get ( RecordID )";

fn tree() -> OwnedNode {
    let mut root = fixture::tree(&scripts(), true);
    fixture::add_calculated(&mut root);
    let tasks = uint_segment(TABLE);
    let rows: [(u64, &[(u64, &str)]); 3] = [
        (
            3,
            &[(NAME, "alpha"), (COUNT, "7"), (fixture::DOUBLED, "14")],
        ),
        (4, &[(COUNT, "1"), (fixture::DOUBLED, "2")]),
        (5, &[(fixture::STATUS, "Open")]),
    ];
    for (id, cells) in rows {
        let record = serial::encode(id);
        for (field, text) in cells {
            root.put(
                &[&tasks, &[5], &record, &uint_segment(*field)],
                text_node(&[], text),
            );
        }
    }
    let ids: RidList = (1..=5).collect();
    root.put(&[&tasks, &[0x0d]], ids.to_node(Vec::new()));
    root.put(&[&tasks, &[0]], OwnedNode::leaf(Vec::new(), vec![1, 5]));
    // Layout Tasks shows Name in an edit box and Status as a merge field
    // stored by its catalog keys (layout-model.md 4.1), both of which Quick
    // Find searches.
    let key = |id: u64| serial::id_of_raw(id).unwrap();
    let bounds = Rect {
        top: 20.0,
        left: 20.0,
        bottom: 40.0,
        right: 200.0,
        other: Vec::new(),
    };
    let mut store = Vec::new();
    encode::uint(1, 1, &mut store);
    encode::uint(1, 2, &mut store);
    encode::bytes(
        2,
        &encode::field_object(1, &bounds, key(OCCURRENCE), key(NAME)),
        &mut store,
    );
    encode::bytes(2, &encode::text_object(2, &bounds), &mut store);
    let layout = uint_segment(fixture::LAYOUT);
    root.put(
        &[&[4], &[5], &layout, &[7]],
        OwnedNode::leaf(Vec::new(), store),
    );
    // The merge token: kind 2, then the occurrence and field keys with
    // their lengths, as hex (`om_layout`'s `merge_field_spec`).
    let token = format!("0003{OCCURRENCE:06X}01{:02X}", fixture::STATUS);
    let text = format!(
        "Status: <<\u{1}\u{2}{}{token}>>",
        char::from(token.len() as u8)
    );
    let scsu = om_scsu::encode_scsu(&text);
    let mut literal = vec![0x13, scsu.len() as u8];
    literal.extend(scsu);
    literal.push(0);
    root.put(
        &[&[4], &[5], &layout, &[5], &serial::encode(2), &[2], &[6]],
        OwnedNode::leaf(Vec::new(), literal),
    );
    root
}

fn run(session: &mut Session, script: u64) -> (u16, String) {
    let reply = session.perform_script_on_server(script, "", true).unwrap();
    (reply.error, reply.result)
}

#[test]
fn server_finds_compute_unstored_calculations_and_skip_summaries_and_globals() {
    let file = TempFile::from_tree("psos-finds", &tree());
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "client");
    // (script, the run's error: the last step's, the result).
    let cases: [(u64, u16, &str); 9] = [
        // Labels: "Write tests (3)", "Ship (5)", "alpha (7)", " (1)".
        (1, 0, "0/0:1:Ship (5):"),
        (2, 0, "0/0:1:alpha (7):"),
        (3, 0, "0/0:1:Ship (5):"),
        (4, 401, "0/401"),
        // A summary field takes no criterion: nothing is left to find.
        (5, 400, "201/400"),
        (6, 0, "201/0:1:Ship (5):"),
        // Find mode sets a global field; it is not a criterion.
        (7, 0, "0/0:1:Ship (5):y"),
        // Stored requests leave summary and global criteria out.
        (8, 0, "0/0:1:alpha (7):"),
        (9, 400, "0/400"),
    ];
    for (script, error, result) in cases {
        assert_eq!(
            run(&mut client, script),
            (error, result.to_string()),
            "script {script}"
        );
    }
}

#[test]
fn an_unstored_calculation_over_empty_fields_is_empty() {
    let file = TempFile::from_tree("psos-finds-empty", &tree());
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "client");
    // Record 5 has neither Name nor Count: Label, set not to evaluate then,
    // is empty there (not " ()"), so `=` finds it and `*` does not
    // (script-runtime.md 13.19, calc-values.md 8.8).
    assert_eq!(run(&mut client, 10), (0, "0/0:1:5".into()));
    assert_eq!(run(&mut client, 11), (0, "0/0:4:1".into()));
    assert_eq!(run(&mut client, 12), (0, "1|0".into()));
}

#[test]
fn find_mode_reads_and_finds_without_criteria_are_filemakers() {
    let file = TempFile::from_tree("psos-finds-mode", &tree());
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "client");
    // A find without criteria is 400 and the window is back in Browse
    // mode, its found set and current record as they were; the request
    // count stays the find's (script-runtime.md 13.19, 13.21).
    assert_eq!(run(&mut client, 13), (0, "400:0:5:5:1".into()));
    // Without a current record (in Find mode, with an empty found set) a
    // global field reads its value and a field the request's criterion or
    // nothing, without error 1224.
    assert_eq!(run(&mut client, 14), (0, "x0/01/zeta0/0,x0,0".into()));
}

/// Perform Quick Find on the server (query-program.md 6.6) searches the
/// layout's Name field object and its Status merge field: a word finds the
/// records holding it, the words narrow one another, and no match (401) or
/// no word (400) keeps the found set. It ran into error 3 before.
#[test]
fn server_quick_find_searches_the_layouts_quick_find_fields() {
    let file = TempFile::from_tree("psos-quick-find", &tree());
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "client");
    assert_eq!(run(&mut client, 40), (0, "0:1".into()));
    assert_eq!(run(&mut client, 41), (400, "400:5".into()));
    assert_eq!(run(&mut client, 42), (401, "401:5".into()));
    // Record 5's Status "Open", through the merge field.
    assert_eq!(run(&mut client, 43), (0, "0:1".into()));
}

/// Find Matching Records on the server: the records holding the current
/// record's value of the field, record 1's "Write tests" in Name and its
/// empty Status (records 1 to 4); without a field or a focus it is 102,
/// the found set kept. It ran into error 3 before.
#[test]
fn server_find_matching_finds_the_current_records_value() {
    let file = TempFile::from_tree("psos-find-matching", &tree());
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "client");
    assert_eq!(run(&mut client, 44), (0, "0:1".into()));
    assert_eq!(run(&mut client, 45), (0, "0:4".into()));
    assert_eq!(run(&mut client, 46), (102, "102:5".into()));
}
