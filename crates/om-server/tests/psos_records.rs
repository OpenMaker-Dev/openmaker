//! End to end: server scripts over related records, sorts, stored finds,
//! repetitions and the file's locale (script-runtime.md "Implementation
//! notes (PSOS)"; script-steps.md 11.1, 11.2, 11.8; file-metadata.md 2, 4).
//!
//! The Tasks file of `fixtures/tasks.rs`, extended here: Tasks gets records
//! 3 "alpha" (Count 7) and 4 (no name, Count 1) and a value-indexed text
//! field Tags with three repetitions; Notes gets a text field Task and
//! records 2 "b-note" and 3 "a-note" (Task "Ship") and 4 "w" (Task "Write
//! tests"); relationship 1 joins Tasks::Name = Notes::Task and sorts the
//! notes seen from Tasks by Body; value list 1 holds "Ship", a divider and
//! "Write tests".

#[path = "fixtures/tasks.rs"]
mod fixture;
#[path = "fixtures/server.rs"]
mod server;
#[path = "fixtures/transfer.rs"]
mod transfer;

use fixture::{
    script, TempFile, COUNT, NAME, NOTES, NOTES_LAYOUT, NOTES_OCCURRENCE, OCCURRENCE, TABLE,
};
use om_formula::NameTable;
use om_hbam::slot::uint_segment;
use om_hbam::{serial, OwnedNode, RidList};
use om_scripts::specs::{self, SortEntry, SortSpec, SpecField, StoredRequest};
use om_scripts::{flag, text_node, Calculation, FieldSpec, Script, ScriptStep, StoredKey};
use om_session::Session;

/// Tasks::Tags: text, three repetitions, value index.
const TAGS: u64 = 8;
/// Notes::Task: the name of the task a note belongs to.
const TASK: u64 = 2;
const BODY: u64 = fixture::BODY;
/// Value list 1.
const PRIORITY_LIST: u64 = 1;
/// Tasks::Note, in `set_match_fields_perform_lookups`' tree only: looked
/// up from Notes::Body through relationship 1, made unsorted there.
const NOTE: u64 = 9;
/// Tasks::Stamp, auto-entered `Name & Setting` where a test adds it.
const STAMP: u64 = 40;

fn names() -> NameTable {
    let mut names = fixture::names();
    names.add_field(TABLE, TAGS, "Tags");
    names.add_field(NOTES, TASK, "Task");
    names.add_field(TABLE, NOTE, "Note");
    names.add_field(TABLE, STAMP, "Stamp");
    names
}

/// `text` compiled in the Tasks context with this file's names.
fn calc(text: &str) -> Calculation {
    let names = names();
    let mut ctx = om_formula::Context::new(&names);
    ctx.occurrence = Some(OCCURRENCE);
    let compiled = om_formula::compile(text, &ctx).unwrap_or_else(|e| panic!("{text}: {e:?}"));
    Calculation::new(
        Some(om_formula::formatted::encode(&compiled.tokens)),
        om_formula::polish::encode(&compiled.expr),
    )
}

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

/// Set Field [Tasks::field[repetition]; text].
fn set_field(field: u64, repetition: i16, text: &str) -> ScriptStep {
    let mut step = ScriptStep::new(76);
    step.set_calculation(0, Some(&calc(text)));
    step.set_field_spec(
        0,
        Some(FieldSpec::new(
            StoredKey::from_id(OCCURRENCE).unwrap(),
            StoredKey::from_id(field).unwrap(),
            repetition,
        )),
    );
    step
}

/// Sort Records [Restore; No dialog] with `spec` (script-steps.md 11.1).
fn sort(spec: &SortSpec) -> ScriptStep {
    let mut step = ScriptStep::new(39);
    step.set_data_leaf(0x0a, Some(OwnedNode::leaf([0x0a], spec.encode())));
    step.set_flag(flag::RESTORE, true);
    step.flags |= flag::MENU_SELECTION;
    step
}

fn entry(occurrence: u64, field: u64, descending: bool, value_list: Option<u64>) -> SortEntry {
    SortEntry {
        field: SpecField {
            occurrence: Some(occurrence),
            field,
            repetition: 1,
        },
        summary: None,
        value_list,
        descending,
        locale: 0,
    }
}

fn spec(blanks_last: bool, fields: Vec<SortEntry>) -> SortSpec {
    SortSpec {
        keep_sorted: true,
        blanks_last,
        fields,
    }
}

/// A stored criterion: occurrence, field, text.
type Criterion<'a> = (u64, u64, &'a str);

/// A find step (Enter Find Mode 22, Perform Find 28, Constrain 126,
/// Extend 127) with stored requests, each an omit flag and its criteria
/// (script-steps.md 11.2).
fn find(id: u16, requests: &[(bool, &[Criterion])]) -> ScriptStep {
    let requests: Vec<StoredRequest> = requests
        .iter()
        .map(|(omit, criteria)| StoredRequest {
            omit: *omit,
            criteria: criteria
                .iter()
                .map(|(occurrence, field, text)| {
                    (
                        SpecField {
                            occurrence: Some(*occurrence),
                            field: *field,
                            repetition: 1,
                        },
                        text.to_string(),
                    )
                })
                .collect(),
        })
        .collect();
    let mut step = ScriptStep::new(id);
    step.set_data_leaf(0x06, Some(specs::encode_requests(&requests)));
    step.set_flag(flag::RESTORE, true);
    step
}

/// Go to Related Record [From table: occurrence; Using layout: layout;
/// Show only related records] (script-steps.md 11.8).
fn go_to_related(occurrence: u64, layout: u64) -> ScriptStep {
    let mut step = ScriptStep::new(74);
    step.value = 5;
    step.set_member_key(StoredKey::from_id(layout));
    step.set_slot_child(
        0,
        0x11,
        Some(OwnedNode::leaf(
            [0x11],
            StoredKey::from_id(occurrence).unwrap().0.to_vec(),
        )),
    );
    step.set_flag(flag::RESTORE, true);
    step
}

/// The counts of the found set, in order.
const COUNTS: &str = "GetNthRecord ( Tasks::Count ; 1 ) & \",\" & GetNthRecord ( Tasks::Count ; 2 ) \
                      & \",\" & GetNthRecord ( Tasks::Count ; 3 ) & \",\" & GetNthRecord ( Tasks::Count ; 4 )";

fn scripts() -> Vec<Script> {
    vec![
        script(
            1,
            "Related",
            vec![
                fixture::go_to_record(1),
                set_var(
                    "$r",
                    "Notes::Body & \"|\" & Count ( Notes::Body ) & \"|\" & List ( Notes::Body )",
                ),
                fixture::go_to_record(4),
                exit_with(
                    "$r & \"/\" & Notes::Body & \"|\" & Count ( Notes::Body ) & \"|\" & \
                     List ( Notes::Body )",
                ),
            ],
        ),
        script(
            2,
            "Related Records",
            vec![
                fixture::on(86, true),
                fixture::go_to_record(4),
                go_to_related(NOTES_OCCURRENCE, NOTES_LAYOUT),
                set_var(
                    "$r",
                    "Get ( LastError ) & \"|\" & Get ( FoundCount ) & \"|\" & \
                     Get ( LayoutTableName ) & \"|\" & GetNthRecord ( Notes::Body ; 1 ) & \
                     \",\" & GetNthRecord ( Notes::Body ; 2 )",
                ),
                fixture::go_to_layout(fixture::LAYOUT),
                ScriptStep::new(23),
                fixture::go_to_record(4),
                fixture::go_to_record(4),
                go_to_related(NOTES_OCCURRENCE, NOTES_LAYOUT),
                exit_with("$r & \"/\" & Get ( LastError ) & \"|\" & Get ( LayoutTableName )"),
            ],
        ),
        script(
            3,
            "Sorts",
            vec![
                sort(&spec(false, vec![entry(OCCURRENCE, NAME, false, None)])),
                set_var("$r", &format!("{COUNTS} & \"|\" & Get ( SortState )")),
                sort(&spec(true, vec![entry(OCCURRENCE, COUNT, true, None)])),
                set_var("$r", &format!("$r & \"/\" & {COUNTS}")),
                sort(&spec(
                    false,
                    vec![entry(OCCURRENCE, NAME, false, Some(PRIORITY_LIST))],
                )),
                set_var("$r", &format!("$r & \"/\" & {COUNTS}")),
                sort(&spec(
                    false,
                    vec![entry(NOTES_OCCURRENCE, BODY, false, None)],
                )),
                set_var("$r", &format!("$r & \"/\" & {COUNTS}")),
                ScriptStep::new(21),
                exit_with(&format!(
                    "$r & \"/\" & {COUNTS} & \"|\" & Get ( SortState )"
                )),
            ],
        ),
        script(
            4,
            "Finds",
            vec![
                fixture::on(86, true),
                find(28, &[(false, &[(OCCURRENCE, COUNT, ">4")])]),
                set_var("$r", "Get ( FoundCount )"),
                find(126, &[(true, &[(OCCURRENCE, NAME, "alpha")])]),
                set_var(
                    "$r",
                    "$r & \"|\" & Get ( FoundCount ) & \":\" & Tasks::Name",
                ),
                find(127, &[(false, &[(OCCURRENCE, NAME, "Write")])]),
                set_var("$r", "$r & \"|\" & Get ( FoundCount )"),
                find(22, &[(false, &[(OCCURRENCE, COUNT, "3")])]),
                ScriptStep::new(28),
                set_var(
                    "$r",
                    "$r & \"|\" & Get ( FoundCount ) & \":\" & Tasks::Name",
                ),
                find(
                    28,
                    &[
                        (true, &[(OCCURRENCE, NAME, "Ship")]),
                        (false, &[(OCCURRENCE, NAME, "Ship")]),
                    ],
                ),
                set_var("$r", "$r & \"|\" & Get ( FoundCount )"),
                find(28, &[(false, &[(NOTES_OCCURRENCE, BODY, "w")])]),
                exit_with("$r & \"|\" & Get ( LastError )"),
            ],
        ),
        script(
            5,
            "Repetitions",
            vec![
                fixture::on(86, true),
                fixture::go_to_record(1),
                set_field(TAGS, 2, "\"blue\""),
                set_field(TAGS, 1, "\"red\""),
                set_field(TAGS, 4, "\"none\""),
                set_var("$e", "Get ( LastError )"),
                ScriptStep::new(75),
                find(28, &[(false, &[(OCCURRENCE, TAGS, "blue")])]),
                exit_with(
                    "Tasks::Tags[2] & \"|\" & Tasks::Tags[1] & \"|\" & Tasks::Tags[3] & \"|\" & \
                     $e & \"|\" & Get ( FoundCount ) & \":\" & Tasks::Name",
                ),
            ],
        ),
        script(
            6,
            "Locale",
            vec![exit_with(
                "GetAsText ( 1.5 ) & \"|\" & DayName ( Date ( 2 ; 17 ; 2014 ) ) & \"|\" & \
                 JSONGetElement ( Get ( FileLocaleElements ) ; \"LocaleID.IDNum\" ) & \
                 JSONGetElement ( Get ( FileLocaleElements ) ; \"Num.Decimal\" )",
            )],
        ),
        script(7, "Operators", operator_finds()),
        // For `set_match_fields_perform_lookups`, whose tree has the
        // lookup field Note.
        script(
            8,
            "Relookup",
            vec![
                fixture::go_to_record(1),
                set_field(NOTE, 1, "\"typed\""),
                ScriptStep::new(75),
                set_var("$typed", "Tasks::Note"),
                set_field(fixture::NAME, 1, "Tasks::Name"),
                ScriptStep::new(75),
                exit_with("$typed & \"/\" & Get ( LastError ) & \"/\" & Tasks::Note"),
            ],
        ),
        // Open Transaction [Skip auto-enter options], Name set to its own
        // value, Commit Transaction.
        script(
            9,
            "Skip auto-enter",
            vec![
                fixture::go_to_record(1),
                {
                    let mut open = ScriptStep::new(205);
                    open.set_flag(om_scripts::flag::SELECT_ALL, true);
                    open
                },
                set_field(fixture::NAME, 1, "Tasks::Name"),
                ScriptStep::new(206),
                exit_with("Get ( LastError )"),
            ],
        ),
    ]
}

/// Finds with operators the finders select candidates for (a wildcard
/// character, any characters, a phrase, a date with a wildcard part) and
/// ones they express exactly (not empty, a prefix that finds nothing).
fn operator_finds() -> Vec<ScriptStep> {
    let found = "Get ( FoundCount ) & \":\" & Tasks::Name";
    let criteria: [(u64, &str); 5] = [
        (NAME, "Sh@p"),
        (NAME, "*ph*"),
        (NAME, "\"tests\""),
        (NAME, "*"),
        (fixture::DUE, "10/*/2026"),
    ];
    let mut steps = vec![fixture::on(86, true), set_var("$r", "\"\"")];
    for (field, text) in criteria {
        steps.push(find(28, &[(false, &[(OCCURRENCE, field, text)])]));
        steps.push(set_var("$r", &format!("$r & {found} & \"|\"")));
    }
    steps.push(find(28, &[(false, &[(OCCURRENCE, NAME, "Z*")])]));
    steps.push(exit_with("$r & Get ( LastError )"));
    steps
}

fn definition(kind: u8, data_type: u8, options: u32, repetitions: u16) -> Vec<u8> {
    let mut out = vec![0; 26];
    out[0] = kind;
    out[1] = data_type;
    out[4..8].copy_from_slice(&21u32.to_be_bytes());
    out[8..12].copy_from_slice(&options.to_be_bytes());
    out[24..26].copy_from_slice(&repetitions.to_be_bytes());
    out
}

/// A key string: `[u8 total][u8 len][segment]`.
fn key(id: u64) -> Vec<u8> {
    let segment = uint_segment(id);
    let mut out = vec![segment.len() as u8 + 1, segment.len() as u8];
    out.extend(segment);
    out
}

fn tree() -> OwnedNode {
    let mut root = fixture::tree(&scripts(), true);
    let (tasks, notes) = (uint_segment(TABLE), uint_segment(NOTES));
    let field = |root: &mut OwnedNode, table: &[u8], id: u64, name: &str, definition: Vec<u8>| {
        let f = uint_segment(id);
        root.put(&[table, &[3], &[5], &f, &[16]], text_node(&[], name));
        root.put(
            &[table, &[3], &[5], &f, &[2]],
            OwnedNode::leaf(Vec::new(), definition),
        );
    };
    field(
        &mut root,
        &tasks,
        TAGS,
        "Tags",
        definition(1, 1, 0x8000_0000, 3),
    );
    field(&mut root, &notes, TASK, "Task", definition(1, 1, 0, 1));
    type Row<'a> = (&'a [u8], u64, &'a [(u64, &'a str)]);
    let records: [Row; 5] = [
        (
            &tasks,
            3,
            &[(NAME, "alpha"), (COUNT, "7"), (fixture::DOUBLED, "14")],
        ),
        (&tasks, 4, &[(COUNT, "1"), (fixture::DOUBLED, "2")]),
        (&notes, 2, &[(BODY, "b-note"), (TASK, "Ship")]),
        (&notes, 3, &[(BODY, "a-note"), (TASK, "Ship")]),
        (&notes, 4, &[(BODY, "w"), (TASK, "Write tests")]),
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
    // Relationship 1: Tasks::Name = Notes::Task, the notes seen from Tasks
    // sorted by Body (file-metadata.md 4.1, 4.2).
    let mut definition = key(OCCURRENCE);
    definition.extend(key(NOTES_OCCURRENCE));
    definition.extend(spec(false, vec![entry(NOTES_OCCURRENCE, BODY, false, None)]).encode());
    definition.extend(spec(false, Vec::new()).encode());
    definition.extend(0x40u32.to_be_bytes());
    let relationship: [&[u8]; 4] = [&[3], &[0xfb], &[5], &[1]];
    let at = |key: u8| {
        let mut path = relationship.to_vec();
        path.push(Box::leak(Box::new([key])));
        path
    };
    root.put(&at(2), OwnedNode::leaf(Vec::new(), definition));
    let mut predicate = vec![0];
    predicate.extend(key(NAME));
    predicate.extend(key(TASK));
    let mut path = at(3);
    path.push(&[1]);
    root.put(&path, OwnedNode::leaf(Vec::new(), predicate));
    // Value list 1: custom values with a divider (file-metadata.md 6.1).
    let list = uint_segment(PRIORITY_LIST);
    root.put(&[&[0x21], &[5], &list, &[0x10]], text_node(&[], "Priority"));
    root.put(
        &[&[0x21], &[5], &list, &[3]],
        text_node(&[], "Ship\r-\rWrite tests"),
    );
    root
}

fn run(session: &mut Session, script: u64) -> (u16, String) {
    let reply = session.perform_script_on_server(script, "", true).unwrap();
    (reply.error, reply.result)
}

#[test]
fn server_scripts_follow_relationships() {
    let file = TempFile::from_tree("psos-related", &tree());
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "client");
    // "Write tests" has note w; "Ship" has b-note and a-note, sorted by
    // the relationship: a-note first.
    assert_eq!(
        run(&mut client, 1),
        (0, "w|1|w/a-note|2|a-note\rb-note".to_string())
    );
    // Go to Related Record shows the related notes in the relationship's
    // order; from a task without notes it is 101 and the layout stays.
    assert_eq!(
        run(&mut client, 2),
        (101, "0|2|Notes|a-note,b-note/101|Tasks".to_string())
    );
    // The index the join read was built on demand and stored.
    let notes = uint_segment(NOTES);
    let index = client
        .download(&[&notes, &[1], &uint_segment(TASK), &[0]])
        .unwrap();
    assert_eq!(index.node_state, om_protocol::NodeState::NonTerminal);
}

#[test]
fn server_scripts_sort_with_stored_orders() {
    let file = TempFile::from_tree("psos-sort", &tree());
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "client");
    let (error, result) = run(&mut client, 3);
    assert_eq!(error, 0, "{result}");
    let parts: Vec<&str> = result.split('/').collect();
    assert_eq!(
        parts,
        [
            // Name ascending: the empty name first.
            "1,7,5,3|1",
            // Count descending, blanks last.
            "7,5,3,1",
            // Name by the value list: Ship, Write tests, then alpha, then
            // the empty name.
            "5,3,7,1",
            // By the first related note's Body: tasks without notes first.
            "7,1,5,3",
            // Unsorted: creation order.
            "3,5,7,1|0",
        ]
    );
}

#[test]
fn server_scripts_find_with_stored_requests() {
    let file = TempFile::from_tree("psos-find", &tree());
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "client");
    // Count > 4: Ship and alpha; constrained by omitting alpha: Ship;
    // extended by Write: two; Enter Find Mode [Restore] and Perform Find:
    // Write tests; an omit request first omits from all records and the
    // next request adds Ship back (11.2.4): all four; a related criterion:
    // 3.
    assert_eq!(
        run(&mut client, 4),
        (3, "2|1:Ship|2|1:Write tests|4|3".to_string())
    );
}

#[test]
fn server_scripts_find_with_find_operators() {
    let file = TempFile::from_tree("psos-operators", &tree());
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "client");
    // Sh@p: Ship; *ph*: alpha; "tests": Write tests; *: the three named
    // tasks; 10/*/2026: Write tests (due 10/1/2026); Z*: nothing, 401.
    assert_eq!(
        run(&mut client, 7),
        (
            401,
            "1:Ship|1:alpha|1:Write tests|3:Write tests|1:Write tests|401".to_string()
        )
    );
}

#[test]
fn server_scripts_set_and_read_repetitions() {
    let file = TempFile::from_tree("psos-reps", &tree());
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "client");
    // Repetition 4 of a three-repetition field is 111; a find matches a
    // value in any repetition.
    assert_eq!(
        run(&mut client, 5),
        (0, "blue|red||111|1:Write tests".to_string())
    );
    // Repetition 2 is stored under F * 256 + 2.
    let records = server::records_of(&mut client, TABLE);
    assert_eq!(server::cell(&records, 1, TAGS * 256 + 2), Some("blue"));
    assert_eq!(server::cell(&records, 1, TAGS), Some("red"));
}

/// Set Field into a repetition, Commit Records, then `$r` gets both errors
/// (`set/commit,`) and Revert Record drops what the commit refused.
fn commit_repetition(field: u64, repetition: i16, text: &str) -> Vec<ScriptStep> {
    vec![
        set_field(field, repetition, text),
        set_var("$r", "$r & Get ( LastError )"),
        ScriptStep::new(75),
        set_var("$r", "$r & \"/\" & Get ( LastError ) & \",\""),
        ScriptStep::new(51),
    ]
}

/// A server script's new record takes an auto-entered formula over a
/// global field from the script's session value (#187, schema-path.md
/// 5.17): the file stores Setting "s"; script 8 leaves it, so a record
/// named Ada gets Stamp "Adas"; script 9 sets it to "x" first, so a record
/// named Bea gets "Beax".
#[test]
fn server_scripts_auto_enter_over_a_global_reads_the_sessions_value() {
    let make = |setting: Option<&str>, name: &str| {
        let mut steps = Vec::new();
        if let Some(setting) = setting {
            steps.push(fixture::set_field(fixture::SETTING, setting));
        }
        steps.extend([
            ScriptStep::new(7),
            fixture::set_field(NAME, name),
            ScriptStep::new(75),
            exit_with("Get ( LastError ) & \"|\" & Tasks::Stamp"),
        ]);
        steps
    };
    let mut root = tree();
    fixture::add_field(&mut root, TABLE, STAMP, "Stamp", 1, 1, 0, 0x8, 0);
    fixture::add_formula(
        &mut root,
        &names(),
        (TABLE, OCCURRENCE),
        STAMP,
        5,
        "Name & Setting",
    );
    root.put(
        &[&uint_segment(TABLE), &[8], &uint_segment(fixture::SETTING)],
        text_node(&[], "s"),
    );
    root.put(
        &[&[17], &[5], &uint_segment(8)],
        script(8, "Stored", make(None, "\"Ada\"")).encode(),
    );
    root.put(
        &[&[17], &[5], &uint_segment(9)],
        script(9, "Set", make(Some("\"x\""), "\"Bea\"")).encode(),
    );
    let file = TempFile::from_tree("psos-auto-enter-global", &root);
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "client");
    assert_eq!(run(&mut client, 8), (0, "0|Adas".to_string()));
    assert_eq!(run(&mut client, 9), (0, "0|Beax".to_string()));
}

#[test]
fn server_scripts_validate_every_repetition() {
    // Code (validated Always by `Length ( Self ) <= 3`) gets three
    // repetitions; Name, two, at most 11 characters, validated Always;
    // Tags (three repetitions, value-indexed) unique values, Always.
    let mut steps = vec![
        fixture::on(86, true),
        fixture::go_to_record(1),
        set_var("$r", "\"\""),
    ];
    let probes: [(u64, i16, &str); 6] = [
        (fixture::CODE, 2, "\"toolong\""),
        (fixture::CODE, 3, "\"abcd\""),
        (fixture::CODE, 2, "\"abc\""),
        (NAME, 2, "\"twelve chars\""),
        (NAME, 2, "\"eleven char\""),
        (TAGS, 3, "\"red\""),
    ];
    for (field, repetition, text) in probes {
        steps.extend(commit_repetition(field, repetition, text));
    }
    // Record 2: "red" is in record 1's third repetition; one record may
    // hold a value twice.
    steps.push(fixture::go_to_record(4));
    for (field, repetition, text) in [
        (TAGS, 2, "\"red\""),
        (TAGS, 1, "\"green\""),
        (TAGS, 2, "\"green\""),
    ] {
        steps.extend(commit_repetition(field, repetition, text));
    }
    steps.push(exit_with(
        "$r & Tasks::Tags[2] & \"|\" & GetNthRecord ( Tasks::Code[2] ; 1 ) & \"|\" & \
         GetNthRecord ( Tasks::Name[2] ; 1 ) & \"|\" & GetNthRecord ( Tasks::Name ; 1 )",
    ));
    let mut root = tree();
    root.put(
        &[&[17], &[5], &uint_segment(8)],
        script(8, "Validation", steps).encode(),
    );
    fixture::set_field_repetitions(&mut root, TABLE, fixture::CODE, 3);
    fixture::set_field_repetitions(&mut root, TABLE, NAME, 2);
    fixture::set_field_validation(&mut root, TABLE, NAME, 0x600);
    fixture::set_field_byte_limit(&mut root, TABLE, NAME, 11);
    fixture::set_field_validation(&mut root, TABLE, TAGS, 0x410);
    let file = TempFile::from_tree("psos-validation", &root);
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "client");
    // As FileMaker Server's engine (script oracle): a validation
    // calculation checks each repetition on its own value (507); Set Field
    // refuses a value over the maximum number of characters itself (511,
    // the commit has nothing to refuse); a unique value may be in no
    // repetition of another record (504).
    assert_eq!(
        run(&mut client, 8),
        (
            0,
            "0/507,0/507,0/0,511/0,0/0,0/0,0/504,0/0,0/0,green|abc|eleven char|Write tests"
                .to_string()
        )
    );
}

/// A repeating field whose id is above `u64::MAX / 256` (FileMaker issues
/// small ids; a crafted file or catalog upload can hold one): only its
/// first repetition can have a cell, `F * 256 + n` would pass `u64::MAX`.
/// A server script's write to its table validates the field from the
/// cells it can have, never from another field's cell the key would wrap
/// around to: validated not empty and empty, the commit is refused with
/// 509 and record 1 keeps its values (#388).
#[test]
fn server_scripts_validate_a_repeating_field_past_the_largest_cell_key() {
    let big = u64::MAX / 256 + 1;
    let steps = vec![
        fixture::on(86, true),
        fixture::go_to_record(1),
        set_field(NAME, 1, "\"Changed\""),
        ScriptStep::new(75),
        exit_with("Get ( LastError )"),
    ];
    let mut root = tree();
    root.put(
        &[&[17], &[5], &uint_segment(8)],
        script(8, "Write", steps).encode(),
    );
    let (tasks, f) = (uint_segment(TABLE), uint_segment(big));
    root.put(&[&tasks, &[3], &[5], &f, &[16]], text_node(&[], "Big"));
    root.put(
        &[&tasks, &[3], &[5], &f, &[2]],
        OwnedNode::leaf(Vec::new(), definition(1, 1, 0, 3)),
    );
    // Not empty, Always.
    fixture::set_field_validation(&mut root, TABLE, big, 0x408);
    let file = TempFile::from_tree("psos-large-field-id", &root);
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "client");
    // Record 1 holds Count and Due, cells 2 and 3: the cells repetitions 2
    // and 3 of the field would wrap around to.
    let records = server::records_of(&mut client, TABLE);
    assert!(server::cell(&records, 1, COUNT).is_some());
    assert_eq!(run(&mut client, 8), (509, "509".to_string()));
    let records = server::records_of(&mut client, TABLE);
    assert_eq!(server::cell(&records, 1, NAME), Some("Write tests"));
}

/// A locale record (file-metadata.md 2.1) with German formats.
fn german_locale() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend(25u32.to_be_bytes());
    out.push(0);
    out.extend(0xf300_0000u32.to_be_bytes());
    for c in [';', '\u{201e}', '\u{201c}', '.', ',', '.', ':'] {
        out.extend((c as u16).to_be_bytes());
    }
    out.extend([2, 1, 3, 1, 6, 8]);
    let days = [
        "Sonntag",
        "Montag",
        "Dienstag",
        "Mittwoch",
        "Donnerstag",
        "Freitag",
        "Samstag",
    ];
    let months = [
        "Januar",
        "Februar",
        "März",
        "April",
        "Mai",
        "Juni",
        "Juli",
        "August",
        "September",
        "Oktober",
        "November",
        "Dezember",
    ];
    let mut strings: Vec<&str> = vec!["", "", "", "€", "+", "-"];
    strings.extend(days);
    strings.extend(days.map(|d| &d[..2]));
    strings.extend(months);
    strings.extend(months.map(|m| &m[..3]));
    strings.extend([
        "Q1", "Q2", "Q3", "Q4", "1", "2", "3", "4", "", ", ", ". ", " ", "",
    ]);
    for text in strings {
        out.push(text.encode_utf16().count() as u8);
        out.extend(om_scsu::encode_scsu(text));
    }
    out
}

#[test]
fn server_scripts_use_the_files_locale() {
    let mut root = tree();
    root.put(
        &[&[2], &[0x0d]],
        OwnedNode::leaf(Vec::new(), german_locale()),
    );
    let file = TempFile::from_tree("psos-locale", &root);
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "client");
    // `Get ( FileLocaleElements )` is the file's record: German (25).
    assert_eq!(run(&mut client, 6), (0, "1,5|Montag|25,".to_string()));
}

/// A server script's Set Field of a lookup's match field performs the
/// lookup also when the value stays, as FileMaker Server's engine does
/// (auto-entry.md, Lookups, battery s41): Tasks::Note, looked up from
/// Notes::Body through Tasks::Name = Notes::Task, typed over, comes back
/// as task 1's note "w" when the script sets Name to its own value.
#[test]
fn set_match_fields_perform_lookups() {
    for calculated in [0, 1, 2] {
        let mut tree = lookup_tree();
        if calculated != 0 {
            const KEY: u64 = 31;
            fixture::add_field(&mut tree, TABLE, KEY, "Match", 2, 1, 0, 0xa000_0020, 0);
            fixture::add_formula(
                &mut tree,
                &names(),
                (TABLE, OCCURRENCE),
                KEY,
                5,
                if calculated == 2 {
                    "GetRepetition ( Name ; 2 )"
                } else {
                    "Name"
                },
            );
            if calculated == 2 {
                tree.put(
                    &[&uint_segment(TABLE), &[3], &[5], &uint_segment(NAME), &[2]],
                    OwnedNode::leaf([], definition(1, 1, 0, 2)),
                );
                let cell = om_data::rules::cell_of(NAME, 2).unwrap();
                tree.put(
                    &[
                        &uint_segment(TABLE),
                        &[5],
                        &serial::encode(1),
                        &uint_segment(cell),
                    ],
                    text_node(&[], "Write tests"),
                );
                tree.put(
                    &[&[17], &[5], &[8]],
                    script(
                        8,
                        "Relookup",
                        vec![
                            fixture::go_to_record(1),
                            set_field(NOTE, 1, "\"typed\""),
                            ScriptStep::new(75),
                            set_var("$typed", "Tasks::Note"),
                            set_field(NAME, 2, "GetRepetition ( Tasks::Name ; 2 )"),
                            ScriptStep::new(75),
                            exit_with("$typed & \"/\" & Get ( LastError ) & \"/\" & Tasks::Note"),
                        ],
                    )
                    .encode(),
                );
            }
            fixture::add_relationship(&mut tree, 1, (OCCURRENCE, KEY), 0, (NOTES_OCCURRENCE, TASK));
            tree.put(
                &[
                    &uint_segment(TABLE),
                    &[5],
                    &serial::encode(1),
                    &uint_segment(KEY),
                ],
                text_node(&[], "Write tests"),
            );
        }
        let file = TempFile::from_tree("psos-relookup", &tree);
        let port = server::start(&file);
        let (mut client, _) = server::logged_in(port, "client");
        assert_eq!(
            run(&mut client, 8),
            (0, "typed/0/w".to_string()),
            "calculated={calculated}"
        );
    }
}

/// Relookup Field Contents [Tasks::Name] copies each found task's lookup
/// again, Name itself unchanged (#87). Inside a transaction that skips
/// auto-entry it copies and writes nothing; inside an ordinary one the copy
/// waits for Commit Transaction, which writes it only when its rules
/// change a cell. Task 1's Note, typed over, comes
/// back as its note "w", and task 3, whose Name `東京` is stored in chunks,
/// takes its note "jp". Run again with nothing new to copy, it writes no
/// record: task 1's modification count stays. Through Count, which no
/// lookup matches through, or through a later repetition of Name, the
/// step is 3 and changes nothing.
#[test]
fn relookup_field_contents_copies_the_lookups_again() {
    let relookup = |field: u64, repetition: i16| {
        let mut step = ScriptStep::new(40);
        step.set_flag(flag::NO_INTERACT, true);
        step.set_field_spec(
            0,
            Some(FieldSpec::new(
                StoredKey::from_id(OCCURRENCE).unwrap(),
                StoredKey::from_id(field).unwrap(),
                repetition,
            )),
        );
        step
    };
    let mut tree = lookup_tree();
    let (tasks, notes) = (uint_segment(TABLE), uint_segment(NOTES));
    tree.put(
        &[&tasks, &[5], &serial::encode(3), &uint_segment(NAME)],
        text_node(&[], "東京"),
    );
    for (field, text) in [(BODY, "jp"), (TASK, "東京")] {
        tree.put(
            &[&notes, &[5], &serial::encode(5), &uint_segment(field)],
            text_node(&[], text),
        );
    }
    let ids: RidList = (1..=5).collect();
    tree.put(&[&notes, &[0x0d]], ids.to_node(Vec::new()));
    tree.put(&[&notes, &[0]], OwnedNode::leaf(Vec::new(), vec![1, 5]));
    let open_transaction = |skip_auto_enter: bool| {
        let mut open = ScriptStep::new(205);
        open.set_flag(om_scripts::flag::SELECT_ALL, skip_auto_enter);
        open
    };
    // In place of scripts 5 to 9, so the catalog's index lists them.
    for (id, name, steps) in [
        (
            5u64,
            "Relookup in a transaction",
            vec![
                fixture::go_to_record(1),
                set_field(NOTE, 1, "\"typed\""),
                ScriptStep::new(75),
                open_transaction(false),
                relookup(fixture::NAME, 0),
                set_var("$relookup", "Get ( LastError )"),
                ScriptStep::new(206),
                exit_with("Tasks::Note & \"/\" & $relookup & \"/\" & Get ( LastError )"),
            ],
        ),
        (
            6,
            "Relookup skipped",
            vec![
                fixture::go_to_record(1),
                open_transaction(true),
                relookup(fixture::NAME, 0),
                set_var("$relookup", "Get ( LastError )"),
                ScriptStep::new(206),
                exit_with("Tasks::Note & \"/\" & $relookup & \"/\" & Get ( LastError )"),
            ],
        ),
        (
            8,
            "Relookup through Name",
            vec![
                fixture::go_to_record(1),
                set_field(NOTE, 1, "\"typed\""),
                ScriptStep::new(75),
                relookup(fixture::NAME, 0),
                exit_with("Tasks::Note & \"/\" & Get ( LastError ) & \"/\" & Tasks::Name"),
            ],
        ),
        (
            7,
            "Relookup again",
            vec![
                fixture::go_to_record(1),
                relookup(fixture::NAME, 0),
                open_transaction(false),
                relookup(fixture::NAME, 0),
                ScriptStep::new(206),
            ],
        ),
        (
            9,
            "Relookup refused",
            vec![
                fixture::on(86, true),
                fixture::go_to_record(1),
                relookup(fixture::COUNT, 0),
                set_var("$count", "Get ( LastError )"),
                relookup(fixture::NAME, 2),
                exit_with("$count & \"/\" & Get ( LastError ) & \"/\" & Tasks::Note"),
            ],
        ),
    ] {
        tree.put(
            &[&[17], &[5], &uint_segment(id)],
            script(id, name, steps).encode(),
        );
    }
    let file = TempFile::from_tree("psos-relookup-step", &tree);
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "client");
    let count = |client: &mut Session| {
        let records = server::records_of(client, TABLE);
        server::cell(&records, 1, 0xfc).map(str::to_owned)
    };
    // A transaction that skips auto-entry copies nothing and writes
    // nothing.
    let before = count(&mut client);
    assert_eq!(run(&mut client, 6), (0, "/0/0".to_string()));
    assert_eq!(count(&mut client), before);
    // Inside an ordinary transaction the copied value is staged, and
    // Commit Transaction writes it.
    assert_eq!(run(&mut client, 5), (0, "w/0/0".to_string()));
    assert_eq!(run(&mut client, 8), (0, "w/0/Write tests".to_string()));
    let records = server::records_of(&mut client, TABLE);
    assert_eq!(server::cell(&records, 3, NOTE), Some("jp"));
    assert_eq!(server::cell(&records, 3, NAME), Some("東京"));
    // Nothing new to copy, outside a transaction or inside one: the
    // record is not written again.
    let before = count(&mut client);
    assert_eq!(run(&mut client, 7).0, 0);
    assert_eq!(count(&mut client), before);
    // The script's last error is the reply's too.
    assert_eq!(run(&mut client, 9), (3, "3/3/w".to_string()));
}

/// Imports with auto-enter off into a record Relookup staged in the same
/// transaction set their own fields and leave the relookup the script asked
/// for to Commit Transaction, which copies it: after two imports of Count
/// too, and not over a Note an import maps itself.
#[test]
fn an_import_without_auto_entry_keeps_a_staged_relookup() {
    let relookup = || {
        let mut step = ScriptStep::new(40);
        step.set_flag(flag::NO_INTERACT, true);
        step.set_field_spec(
            0,
            Some(FieldSpec::new(
                StoredKey::from_id(OCCURRENCE).unwrap(),
                StoredKey::from_id(fixture::NAME).unwrap(),
                0,
            )),
        );
        step
    };
    let dir = std::env::temp_dir().join(format!(
        "om-psos-relookup-import-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let documents = dir.join("Documents");
    std::fs::create_dir_all(&documents).unwrap();
    for (name, text) in [
        ("9.tab", "9\r"),
        ("10.tab", "10\r"),
        ("mapped.tab", "mapped\t5\r"),
    ] {
        std::fs::write(documents.join(name), text).unwrap();
    }
    let import = |file: &str, fields: &[u64]| {
        vec![
            set_var("$p", &format!("Get ( DocumentsPath ) & \"{file}\"")),
            transfer::import_as(
                &["$p"],
                fields,
                false,
                om_transfer::spec::ImportAction::UpdateFoundSet,
            ),
            set_var("$import", "$import & Get ( LastError )"),
        ]
    };
    let relooked_then = |imports: Vec<Vec<ScriptStep>>| {
        let mut steps = vec![
            fixture::go_to_record(1),
            set_field(NOTE, 1, "\"typed\""),
            ScriptStep::new(75),
            ScriptStep::new(205),
            relookup(),
        ];
        steps.extend(imports.into_iter().flatten());
        steps.push(ScriptStep::new(206));
        steps.push(exit_with(
            "Tasks::Note & \"/\" & Tasks::Count & \"/\" & $import & \"/\" & Get ( LastError )",
        ));
        steps
    };
    let mut tree = lookup_tree();
    for (id, name, steps) in [
        (
            8u64,
            "Relookup, then two imports",
            relooked_then(vec![import("9.tab", &[COUNT]), import("10.tab", &[COUNT])]),
        ),
        (
            7,
            "Relookup, then an import of Note",
            relooked_then(vec![import("mapped.tab", &[NOTE, COUNT])]),
        ),
    ] {
        tree.put(
            &[&[17], &[5], &uint_segment(id)],
            script(id, name, steps).encode(),
        );
    }
    let file = TempFile(dir.join(fixture::FILE));
    fixture::write_tree(&file.0, &tree);
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "client");
    let results = (run(&mut client, 8), run(&mut client, 7));
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(results.0, (0, "w/10/00/0".to_string()));
    assert_eq!(results.1, (0, "mapped/5/0/0".to_string()));
}

/// Open Transaction's "Skip auto-enter options" performs no lookup: a
/// Set Field of Name to its own value then changes nothing, and the
/// commit leaves the record's modification count as it was.
#[test]
fn a_skipped_auto_entry_leaves_a_set_match_field_unwritten() {
    let file = TempFile::from_tree("psos-skip-auto-enter", &lookup_tree());
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "client");
    let count = |client: &mut Session| {
        let records = server::records_of(client, TABLE);
        server::cell(&records, 1, 0xfc).map(str::to_owned)
    };
    let before = count(&mut client);
    assert_eq!(run(&mut client, 9), (0, "0".to_string()));
    assert_eq!(count(&mut client), before);
}

/// [`tree`] with Tasks::Note, looked up from Notes::Body through
/// relationship 1, made unsorted.
fn lookup_tree() -> OwnedNode {
    use om_data::field_options::{Lookup, NoMatch};
    let mut root = tree();
    let tasks = uint_segment(TABLE);
    let f = uint_segment(NOTE);
    root.put(&[&tasks, &[3], &[5], &f, &[16]], text_node(&[], "Note"));
    root.put(
        &[&tasks, &[3], &[5], &f, &[2]],
        OwnedNode::leaf(Vec::new(), definition(1, 1, 0x400, 1)),
    );
    let lookup = Lookup {
        from: OCCURRENCE,
        to: NOTES_OCCURRENCE,
        copied: BODY,
        copy_empty: false,
        no_match: NoMatch::DoNotCopy,
    };
    root.put(
        &[&tasks, &[3], &[5], &f, &[8]],
        OwnedNode::leaf(Vec::new(), lookup.encode().unwrap()),
    );
    // Relationship 1 without its sort: the record rules follow unsorted
    // relationships only.
    let mut unsorted = key(OCCURRENCE);
    unsorted.extend(key(NOTES_OCCURRENCE));
    unsorted.extend(spec(false, Vec::new()).encode());
    unsorted.extend(spec(false, Vec::new()).encode());
    // No sort in either direction (`SORT_LEFT_TO_RIGHT` 0x40 off).
    unsorted.extend(0u32.to_be_bytes());
    root.put(
        &[&[3], &[0xfb], &[5], &[1], &[2]],
        OwnedNode::leaf(Vec::new(), unsorted),
    );
    root
}
