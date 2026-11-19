//! End to end: unstored calculations and summary fields in server scripts
//! (script-runtime.md "Implementation notes (PSOS)"; calc-values.md 8.8;
//! file-metadata.md 5).
//!
//! The Tasks file of `fixtures/tasks.rs` with `add_calculated`: records 1
//! "Write tests" (Count 3, Open), 2 "Ship" (5, Done), 3 "alpha" (7, Open)
//! and 4 without a name (1, Done); notes 2 "b-note" and 3 "a-note" of Ship
//! and 4 "w" of Write tests (note 1 "Hi" has no task).

#[path = "fixtures/tasks.rs"]
mod fixture;
#[path = "fixtures/server.rs"]
mod server;

use fixture::{
    calculated_calc as calc, script, TempFile, AVERAGE, COUNT, LABEL, LABELS, NAME, NOTES,
    OCCURRENCE, RUNNING, SHARE, STATUS, TABLE, TALLY, TASK, TOTAL,
};
use om_hbam::slot::uint_segment;
use om_hbam::{serial, OwnedNode, RidList};
use om_scripts::specs::{self, SortEntry, SortSpec, SpecField, StoredRequest};
use om_scripts::{flag, text_node, FieldSpec, Script, ScriptStep, StoredKey};
use om_session::Session;

fn exit_with(text: &str) -> ScriptStep {
    let mut step = ScriptStep::new(103);
    step.set_calculation(0, Some(&calc(text)));
    step
}

fn set_var(name: &str, text: &str) -> ScriptStep {
    let mut step = ScriptStep::new(141);
    step.set_text(0, Some(name));
    step.set_calculation(1, Some(&calc(text)));
    step
}

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

/// Sort Records [Restore; No dialog] by Tasks fields, ascending.
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
    step.flags |= flag::MENU_SELECTION;
    step
}

/// Perform Find [Restore] with one request on Tasks::field.
fn find(field: u64, criterion: &str) -> ScriptStep {
    let request = StoredRequest {
        omit: false,
        criteria: vec![(
            SpecField {
                occurrence: Some(OCCURRENCE),
                field,
                repetition: 1,
            },
            criterion.to_string(),
        )],
    };
    let mut step = ScriptStep::new(28);
    step.set_data_leaf(0x06, Some(specs::encode_requests(&[request])));
    step.set_flag(flag::RESTORE, true);
    step
}

/// `GetNthRecord ( Tasks::field ; n )` for n = 1 to 4, separated by spaces.
fn each(field: &str) -> String {
    (1..=4)
        .map(|n| format!("GetNthRecord ( Tasks::{field} ; {n} )"))
        .collect::<Vec<_>>()
        .join(" & \" \" & ")
}

fn scripts() -> Vec<Script> {
    vec![
        script(
            1,
            "Unstored",
            vec![
                fixture::on(86, true),
                fixture::go_to_record(1),
                set_var("$a", "Tasks::Label & \"/\" & Tasks::Notes"),
                set_field(COUNT, "4"),
                // The open record's edit.
                set_var("$b", "Tasks::Label"),
                ScriptStep::new(205),
                fixture::go_to_record(4),
                set_field(COUNT, "6"),
                fixture::go_to_record(1),
                // The transaction's edit of record 2.
                set_var("$c", "GetNthRecord ( Tasks::Label ; 2 )"),
                ScriptStep::new(207),
                ScriptStep::new(206),
                exit_with(
                    "$a & \"|\" & $b & \"|\" & $c & \"|\" & GetNthRecord ( Tasks::Label ; 2 ) \
                     & \"|\" & GetNthRecord ( Tasks::Notes ; 2 )",
                ),
            ],
        ),
        script(
            2,
            "Summaries",
            vec![
                fixture::on(86, true),
                set_var(
                    "$r",
                    &format!(
                        "Tasks::Total & \" \" & Tasks::Tally & \" \" & Tasks::Average & \" [\" & \
                         Substitute ( Tasks::Labels ; ¶ ; \",\" ) & \"] \" & {} & \" [\" & \
                         Tasks::Share & \"] [\" & GetSummary ( Tasks::Total ; Tasks::Status ) & \
                         \"]\"",
                        each("Running")
                    ),
                ),
                sort(&[STATUS, COUNT]),
                set_var(
                    "$r",
                    &format!(
                        "$r & \" / \" & {} & \" / \" & {} & \" / \" & \
                         GetSummary ( Tasks::Total ; Tasks::Status ) & \" \" & \
                         GetSummary ( Tasks::Running ; Tasks::Status )",
                        each("Running"),
                        each("Share")
                    ),
                ),
                fixture::go_to_record(2),
                set_var(
                    "$r",
                    "$r & \" \" & GetSummary ( Tasks::Total ; Tasks::Status ) & \" \" & \
                     GetSummary ( Tasks::Total ; Tasks::Total ) & \" [\" & \
                     GetSummary ( Tasks::Total ; Tasks::Name ) & \"]\"",
                ),
                find(COUNT, ">2"),
                exit_with(
                    "$r & \" / \" & Get ( FoundCount ) & \" \" & Tasks::Total & \" \" & \
                     Tasks::Tally & \" \" & Tasks::Average & \" [\" & \
                     Substitute ( Tasks::Labels ; ¶ ; \",\" ) & \"]\"",
                ),
            ],
        ),
        script(
            3,
            "Sort by Label",
            vec![
                sort(&[LABEL]),
                exit_with(&format!("{} & \" | \" & {}", each("Count"), each("Label"))),
            ],
        ),
        script(
            4,
            "Match on Key",
            vec![
                fixture::go_to_record(1),
                set_var(
                    "$r",
                    "Count ( Notes by key::Body ) & \"|\" & \
                     Substitute ( List ( Other notes::Body ) ; ¶ ; \",\" )",
                ),
                fixture::go_to_record(4),
                exit_with(
                    "$r & \"/\" & Count ( Notes by key::Body ) & \"|\" & \
                     List ( Other notes::Body )",
                ),
            ],
        ),
    ]
}

fn tree() -> OwnedNode {
    let mut root = fixture::tree(&scripts(), true);
    fixture::add_calculated(&mut root);
    let (tasks, notes) = (uint_segment(TABLE), uint_segment(NOTES));
    type Row<'a> = (&'a [u8], u64, &'a [(u64, &'a str)]);
    let records: [Row; 7] = [
        (&tasks, 1, &[(STATUS, "Open")]),
        (&tasks, 2, &[(STATUS, "Done")]),
        (
            &tasks,
            3,
            &[
                (NAME, "alpha"),
                (COUNT, "7"),
                (fixture::DOUBLED, "14"),
                (STATUS, "Open"),
            ],
        ),
        (
            &tasks,
            4,
            &[(COUNT, "1"), (fixture::DOUBLED, "2"), (STATUS, "Done")],
        ),
        (&notes, 2, &[(fixture::BODY, "b-note"), (TASK, "Ship")]),
        (&notes, 3, &[(fixture::BODY, "a-note"), (TASK, "Ship")]),
        (&notes, 4, &[(fixture::BODY, "w"), (TASK, "Write tests")]),
    ];
    for (table, id, cells) in records {
        let record = serial::encode(id);
        for (field, text) in cells {
            root.put(
                &[table, &[5], &record, &uint_segment(*field)],
                text_node(&[], text),
            );
        }
    }
    for table in [&tasks, &notes] {
        let ids: RidList = (1..=4).collect();
        root.put(&[table, &[0x0d]], ids.to_node(Vec::new()));
        root.put(&[table, &[0]], OwnedNode::leaf(Vec::new(), vec![1, 4]));
    }
    root
}

fn run(session: &mut Session, script: u64) -> (u16, String) {
    let reply = session.perform_script_on_server(script, "", true).unwrap();
    (reply.error, reply.result)
}

fn client(tag: &str) -> (TempFile, Session) {
    let file = TempFile::from_tree(tag, &tree());
    let port = server::start(&file);
    let (client, _) = server::logged_in(port, "client");
    (file, client)
}

#[test]
fn server_scripts_evaluate_unstored_calculations() {
    let (_file, mut client) = client("psos-unstored");
    // Label on record 1, and Notes, which counts the related notes; the
    // open record's edit; the transaction's edit of record 2, then gone
    // with Revert Transaction; Ship's two notes.
    assert_eq!(
        run(&mut client, 1),
        (
            0,
            "Write tests (3)/1|Write tests (4)|Ship (6)|Ship (5)|2".to_string()
        )
    );
    // Nothing of the unstored calculations is written.
    let records = server::records_of(&mut client, TABLE);
    assert_eq!(server::cell(&records, 1, COUNT), Some("4"));
    assert_eq!(server::cell(&records, 1, LABEL), None);
}

#[test]
fn server_scripts_compute_summaries_over_the_found_set() {
    let (_file, mut client) = client("psos-summaries");
    let (error, result) = run(&mut client, 2);
    assert_eq!(error, 0, "{result}");
    let parts: Vec<&str> = result.split(" / ").collect();
    assert_eq!(
        parts,
        [
            // Unsorted, in creation order: total, count of names, average,
            // the list of the unstored labels, the running total (no
            // restart: not sorted by Status), the subtotaled fraction and
            // GetSummary by Status (empty: not sorted by it).
            "16 3 4 [Write tests (3),Ship (5),alpha (7), (1)] 3 8 15 16 [] []",
            // Sorted by Status, Count: 4, 2 (Done), 1, 3 (Open). The
            // running total restarts at Open; the fraction is over its
            // Status's total.
            "1 6 3 10",
            ".1666666666666667 .8333333333333333 .3 .7",
            // GetSummary on the first record (Done), and of the running
            // total: its value at the end of the group; on the second
            // record of Open: its group, the grand total, and by a field the
            // found set is not sorted by: empty.
            "6 6 10 16 []",
            // After a find (Count > 2): records 1, 2, 3.
            "3 15 3 5 [Write tests (3),Ship (5),alpha (7)]",
        ]
    );
    // Summaries are computed, not stored.
    let records = server::records_of(&mut client, TABLE);
    for field in [TOTAL, RUNNING, AVERAGE, SHARE, TALLY, LABELS] {
        assert_eq!(server::cell(&records, 1, field), None);
    }
}

#[test]
fn relationships_match_unstored_calculations_on_the_side_travelled_from() {
    let (_file, mut client) = client("psos-unstored-keys");
    // Tasks::Key = Notes by key::Task through the index, Tasks::Key ≠
    // Other notes::Task by scanning; the note without a task never
    // matches. From Write tests, then from Ship.
    assert_eq!(run(&mut client, 4), (0, "1|b-note,a-note/2|w".to_string()));
}

#[test]
fn server_scripts_sort_by_unstored_calculations() {
    let (_file, mut client) = client("psos-sort-unstored");
    // Labels " (1)", "alpha (7)", "Ship (5)", "Write tests (3)".
    assert_eq!(
        run(&mut client, 3),
        (
            0,
            "1 7 5 3 |  (1) alpha (7) Ship (5) Write tests (3)".to_string()
        )
    );
}
