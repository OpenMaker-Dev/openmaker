//! A small `.fmp12` file built in memory for the Perform Script on Server
//! tests, so they run without the git-ignored copies of test project.
//!
//! Table "Tasks" (`80 01`, occurrence `d0 00 01`) has fields Name (text),
//! Count (number), Due (date), Setting (global text), Doubled (a stored
//! calculation, `Count * 2`), Creator (text, auto-entered creation account
//! name, modification prohibited) and Code (text, validated Always by the
//! calculation `Length ( Self ) <= 3`); records 1 "Write tests" (Count 3,
//! Due 10/1/2026) and 2 "Ship" (Count 5); layout 1 "Tasks" shows it. Table
//! "Notes" (`80 02`, occurrence `d0 00 02`, layout 2 "Notes") has Body
//! (text, validated Always by `Length ( Self ) <= 10`) and record 1 "Hi".
//! Accounts "Admin" ([`PASSWORD`], in the built-in [Full Access] set) and
//! "Clerk" ([`CLERK_PASSWORD`], in the built-in [Data Entry Only] set) are
//! records 2 and 3 of the security catalog, their passwords stored as
//! FileMaker stores them; the theme, file reference and library
//! catalogs hold only an index. Scripts are compiled from text against these names and stored as
//! `[17, 5, S]`. The file is written with `om_hbam::writer`, the writer
//! OpenMaker Server saves with.
//!
//! [`add_calculated`] extends the tree with an unstored calculation, a
//! relationship and summary fields.
//!
//! Shared by the tests of `om-server` (unit and integration) and
//! `om-provider`.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use om_formula::Key;
use om_formula::NameTable;
use om_hbam::slot::uint_segment;
use om_hbam::{serial, OwnedNode, RidList};
use om_scripts::{flag, text_node, Calculation, FieldSpec, Script, ScriptStep, StoredKey};

pub const TABLE: u64 = 0x8001;
pub const OCCURRENCE: u64 = 0xd0_0001;
pub const LAYOUT: u64 = 1;
pub const NAME: u64 = 1;
pub const COUNT: u64 = 2;
pub const DUE: u64 = 3;
pub const SETTING: u64 = 4;
pub const DOUBLED: u64 = 5;
pub const CREATOR: u64 = 6;
pub const CODE: u64 = 7;
pub const NOTES: u64 = 0x8002;
pub const NOTES_OCCURRENCE: u64 = 0xd0_0002;
pub const NOTES_LAYOUT: u64 = 2;
pub const BODY: u64 = 1;
pub const FILE: &str = "Tasks.fmp12";
pub const ACCOUNT: &str = "Admin";
pub const PASSWORD: &str = "correct horse";
/// An account without [Full Access] (security catalog record 3), in
/// [Data Entry Only].
pub const CLERK: &str = "Clerk";
pub const CLERK_PASSWORD: &str = "clerk's secret";
/// An inactive account in [Full Access] in a file with
/// [`add_local_accounts`] (security catalog record 4), and its password.
pub const FORMER: &str = "Former";
pub const FORMER_PASSWORD: &str = "no longer";
/// An account in [Full Access] whose password must be changed at its next
/// login in a file with [`add_local_accounts`] (record 5), and its
/// password.
pub const NEWCOMER: &str = "Newcomer";
pub const NEWCOMER_PASSWORD: &str = "temporary";
/// An account in [Read-Only Access] in a file with [`add_reader`]
/// (security catalog record 6), and its password.
pub const READER: &str = "Reader";
pub const READER_PASSWORD: &str = "reader's secret";
/// An account in a custom set in a file with [`add_custom_clerk`]
/// (security catalog record 7), and its password.
pub const TEMP: &str = "Temp";
pub const TEMP_PASSWORD: &str = "temp's secret";
pub const INTERNAL_CREATION: u32 = 0x0102_0304;

/// Fields [`add_calculated`] adds to Tasks: Status (text); Label, an
/// unstored calculation `Name & " (" & Count & ")"`; Notes, an unstored
/// calculation over related records, `Count ( Notes::Body )`; Key, an
/// unstored calculation `Name` that relationships 2 and 3 match on; and
/// summary fields: Total (total of Count), Running (running total of Count,
/// restarting at each Status when sorted by it), Average (of Count), Share
/// (fraction of total of Count, subtotaled by Status), Tally (count of
/// Name) and Labels (list of Label).
pub const STATUS: u64 = 8;
pub const LABEL: u64 = 9;
pub const NOTE_COUNT: u64 = 10;
pub const TOTAL: u64 = 11;
pub const RUNNING: u64 = 12;
pub const AVERAGE: u64 = 13;
pub const SHARE: u64 = 14;
pub const TALLY: u64 = 15;
pub const LABELS: u64 = 16;
pub const KEY: u64 = 17;
/// Notes::Task (text); relationship 1 joins Tasks::Name = Notes::Task.
pub const TASK: u64 = 2;
/// Occurrences of Notes: "Notes by key" (relationship 2, Tasks::Key =
/// Task) and "Other notes" (relationship 3, Tasks::Key ≠ Task).
pub const NOTES_BY_KEY: u64 = 0xd0_0003;
pub const OTHER_NOTES: u64 = 0xd0_0004;
const KEY_OCCURRENCES: [(u64, &str); 2] =
    [(NOTES_BY_KEY, "Notes by key"), (OTHER_NOTES, "Other notes")];

/// Option bit: the calculation's result is not stored (schema-path.md
/// 5.5).
const UNSTORED: u32 = 0x2_0000;

/// The fields [`add_calculated`] adds: table, id, name, kind (1 normal, 2
/// calculation, 3 summary), type (1 text, 2 number) and option word.
const CALCULATED_FIELDS: [(u64, u64, &str, u8, u8, u32); 11] = [
    (TABLE, STATUS, "Status", 1, 1, 0),
    (TABLE, LABEL, "Label", 2, 1, UNSTORED),
    (TABLE, NOTE_COUNT, "Notes", 2, 2, UNSTORED),
    (TABLE, TOTAL, "Total", 3, 2, 0),
    (TABLE, RUNNING, "Running", 3, 2, 0),
    (TABLE, AVERAGE, "Average", 3, 2, 0),
    (TABLE, SHARE, "Share", 3, 2, 0),
    (TABLE, TALLY, "Tally", 3, 2, 0),
    (TABLE, LABELS, "Labels", 3, 1, 0),
    (TABLE, KEY, "Key", 2, 1, UNSTORED),
    (NOTES, TASK, "Task", 1, 1, 0),
];

/// The summary definitions of [`add_calculated`]: field, operation (1
/// total, 2 average, 3 count, 7 fraction, 9 list), flags (`0x01` the
/// option, `0x04` running, `0x10` restart), summarized field and whether
/// Status is the additional field (file-metadata.md 5.1).
const SUMMARIES: [(u64, u8, u8, u64, bool); 6] = [
    (TOTAL, 1, 0, COUNT, false),
    (RUNNING, 1, 0x14, COUNT, true),
    (AVERAGE, 2, 0, COUNT, false),
    (SHARE, 7, 0x01, COUNT, true),
    (TALLY, 3, 0, NAME, false),
    (LABELS, 9, 0, LABEL, false),
];

/// The file's names with the fields of [`add_calculated`].
pub fn calculated_names() -> NameTable {
    let mut names = names();
    for (table, id, name, kind, ..) in CALCULATED_FIELDS {
        names.add_field(table, id, name);
        names.set_summary_field(table, id, kind == 3);
    }
    for (id, name) in KEY_OCCURRENCES {
        names.add_occurrence(id, name, NOTES);
    }
    names
}

/// `text` compiled in the Tasks context with [`calculated_names`].
pub fn calculated_calc(text: &str) -> Calculation {
    let names = calculated_names();
    let mut ctx = om_formula::Context::new(&names);
    ctx.occurrence = Some(OCCURRENCE);
    let compiled = om_formula::compile(text, &ctx).unwrap_or_else(|e| panic!("{text}: {e:?}"));
    Calculation::new(
        Some(om_formula::formatted::encode(&compiled.tokens)),
        om_formula::polish::encode(&compiled.expr),
    )
}

/// An `HBKey<5>` of a summary definition's field spec: total length, the
/// key string, zero-padded to 6 bytes.
fn short_key(id: u64) -> [u8; 6] {
    let segment = uint_segment(id);
    let mut out = [0u8; 6];
    out[0] = segment.len() as u8 + 1;
    out[1] = segment.len() as u8;
    out[2..2 + segment.len()].copy_from_slice(&segment);
    out
}

/// A 30-byte summary definition (file-metadata.md 5.1).
pub fn summary_definition(
    operation: u8,
    flags: u8,
    field: u64,
    additional: Option<(u64, u64)>,
) -> Vec<u8> {
    let mut out = vec![operation, flags];
    out.extend(short_key(0));
    out.extend(short_key(field));
    out.extend(1u16.to_be_bytes());
    let (occurrence, additional) = additional.unwrap_or((0, 0));
    out.extend(short_key(occurrence));
    out.extend(short_key(additional));
    out.extend(1u16.to_be_bytes());
    out
}

/// A key string: `[u8 total][u8 len][segment]`.
fn key(id: u64) -> Vec<u8> {
    let segment = uint_segment(id);
    let mut out = vec![segment.len() as u8 + 1, segment.len() as u8];
    out.extend(segment);
    out
}

/// Add table occurrence `id` of `table` (`[3, 17, 5, id]`).
pub fn add_occurrence(root: &mut OwnedNode, id: u64, name: &str, table: u64) {
    let (occurrence, table) = (uint_segment(id), uint_segment(table));
    root.put(
        &[&[3], &[17], &[5], &occurrence, &[16]],
        text_node(&[], name),
    );
    let mut definition = vec![2, 1, 0, table.len() as u8 + 1, table.len() as u8];
    definition.extend(&table);
    root.put(
        &[&[3], &[17], &[5], &occurrence, &[2]],
        OwnedNode::leaf(Vec::new(), definition),
    );
}

/// Add relationship `id`, `left::left_field op right::right_field` (`op`
/// 0 `=`, 1 `≠`), unsorted (file-metadata.md 4.1, 4.2).
pub fn add_relationship(
    root: &mut OwnedNode,
    id: u8,
    (left, left_field): (u64, u64),
    op: u8,
    (right, right_field): (u64, u64),
) {
    let mut definition = key(left);
    definition.extend(key(right));
    let unsorted = om_scripts::specs::SortSpec {
        keep_sorted: false,
        blanks_last: false,
        fields: Vec::new(),
    };
    definition.extend(unsorted.encode());
    definition.extend(unsorted.encode());
    definition.extend(0u32.to_be_bytes());
    root.put(
        &[&[3], &[0xfb], &[5], &[id], &[2]],
        OwnedNode::leaf(Vec::new(), definition),
    );
    let mut predicate = vec![op];
    predicate.extend(key(left_field));
    predicate.extend(key(right_field));
    root.put(
        &[&[3], &[0xfb], &[5], &[id], &[3], &[1]],
        OwnedNode::leaf(Vec::new(), predicate),
    );
}

/// Add to a [`tree`] the fields of [`STATUS`] and following, their
/// formulas and summary definitions, Notes::Task, the occurrences of
/// [`NOTES_BY_KEY`] and [`OTHER_NOTES`] and relationships 1 to 3. Records
/// keep their values.
pub fn add_calculated(root: &mut OwnedNode) {
    for (table, id, name, kind, data_type, options) in CALCULATED_FIELDS {
        let (t, f) = (uint_segment(table), uint_segment(id));
        root.put(&[&t, &[3], &[5], &f, &[16]], text_node(&[], name));
        root.put(
            &[&t, &[3], &[5], &f, &[2]],
            OwnedNode::leaf(Vec::new(), definition(kind, data_type, 0, options, 0)),
        );
    }
    for (field, text) in [
        (LABEL, "Name & \" (\" & Count & \")\""),
        (NOTE_COUNT, "Count ( Notes::Body )"),
        (KEY, "Name"),
    ] {
        let compiled = compiled(OCCURRENCE, text);
        let (t, f) = (uint_segment(TABLE), uint_segment(field));
        for (child, bytes) in [
            (4u8, Key::from_id(OCCURRENCE).0),
            (5, om_formula::formatted::encode(&compiled.tokens)),
            (6, om_formula::polish::encode(&compiled.expr)),
        ] {
            root.put(
                &[&t, &[3], &[5], &f, &[5], &[child]],
                OwnedNode::leaf(Vec::new(), bytes),
            );
        }
    }
    for (field, operation, flags, summarized, by_status) in SUMMARIES {
        let (t, f) = (uint_segment(TABLE), uint_segment(field));
        let additional = by_status.then_some((OCCURRENCE, STATUS));
        root.put(
            &[&t, &[3], &[5], &f, &[9]],
            OwnedNode::leaf(
                Vec::new(),
                summary_definition(operation, flags, summarized, additional),
            ),
        );
    }
    for (id, name) in KEY_OCCURRENCES {
        add_occurrence(root, id, name, NOTES);
    }
    add_relationship(root, 1, (OCCURRENCE, NAME), 0, (NOTES_OCCURRENCE, TASK));
    add_relationship(root, 2, (OCCURRENCE, KEY), 0, (NOTES_BY_KEY, TASK));
    add_relationship(root, 3, (OCCURRENCE, KEY), 1, (OTHER_NOTES, TASK));
}

/// A field definition: id, name, kind (1 normal, 2 calculation), type (1
/// text, 2 number, 3 date), auto-enter variable (byte 3), option word
/// (global storage `0x1_0000`, auto-enter variable `0x1`, prohibit
/// modification `0x100`) and validation word (by calculation `0x1`, strict
/// `0x4`, Always `0x400`), schema-path.md 5.5.
type FieldRow = (u64, &'static str, u8, u8, u8, u32, u32);

const FIELDS: [FieldRow; 7] = [
    (NAME, "Name", 1, 1, 0, 0, 0),
    (COUNT, "Count", 1, 2, 0, 0, 0),
    (DUE, "Due", 1, 3, 0, 0, 0),
    (SETTING, "Setting", 1, 1, 0, 0x1_0000, 0),
    (DOUBLED, "Doubled", 2, 2, 0, 0, 0),
    // Creation account name (variable 4).
    (CREATOR, "Creator", 1, 1, 4, 0x101, 0),
    (CODE, "Code", 1, 1, 0, 0, 0x405),
];

const NOTE_FIELDS: [FieldRow; 1] = [(BODY, "Body", 1, 1, 0, 0, 0x401)];

/// The formulas under field definitions: table, occurrence, field, key (5
/// the calculation, 0x19 the validation) and text.
const FORMULAS: [(u64, u64, u64, u8, &str); 3] = [
    (TABLE, OCCURRENCE, DOUBLED, 5, "Count * 2"),
    (TABLE, OCCURRENCE, CODE, 0x19, "Length ( Self ) <= 3"),
    (NOTES, NOTES_OCCURRENCE, BODY, 0x19, "Length ( Self ) <= 10"),
];

/// The file's names, for compiling calculations.
pub fn names() -> NameTable {
    let mut names = NameTable::default();
    names.add_occurrence(OCCURRENCE, "Tasks", TABLE);
    names.add_occurrence(NOTES_OCCURRENCE, "Notes", NOTES);
    for (id, name, ..) in FIELDS {
        names.add_field(TABLE, id, name);
    }
    for (id, name, ..) in NOTE_FIELDS {
        names.add_field(NOTES, id, name);
    }
    names
}

/// `text` compiled in `occurrence`'s context.
fn compiled(occurrence: u64, text: &str) -> om_formula::Compiled {
    let names = names();
    let mut ctx = om_formula::Context::new(&names);
    ctx.occurrence = Some(occurrence);
    ctx.support_self = true;
    om_formula::compile(text, &ctx).unwrap_or_else(|e| panic!("{text}: {e:?}"))
}

/// A calculation compiled in the Tasks context.
pub fn calc(text: &str) -> Calculation {
    calc_in(OCCURRENCE, text)
}

/// A calculation compiled in `occurrence`'s context.
pub fn calc_in(occurrence: u64, text: &str) -> Calculation {
    let compiled = compiled(occurrence, text);
    Calculation::new(
        Some(om_formula::formatted::encode(&compiled.tokens)),
        om_formula::polish::encode(&compiled.expr),
    )
}

/// A step with calculation `text` in slot 0.
pub fn with_calc(id: u16, text: &str) -> ScriptStep {
    let mut step = ScriptStep::new(id);
    step.set_calculation(0, Some(&calc(text)));
    step
}

/// Set Field [Tasks::field; text].
pub fn set_field(field: u64, text: &str) -> ScriptStep {
    set_field_in(OCCURRENCE, field, text)
}

/// Set Field [occurrence::field; text], the calculation in the
/// occurrence's context.
pub fn set_field_in(occurrence: u64, field: u64, text: &str) -> ScriptStep {
    let mut step = ScriptStep::new(76);
    step.set_calculation(0, Some(&calc_in(occurrence, text)));
    step.set_field_spec(
        0,
        Some(FieldSpec::new(
            StoredKey::from_id(occurrence).unwrap(),
            StoredKey::from_id(field).unwrap(),
            0,
        )),
    );
    step
}

/// Go to Layout [layout].
pub fn go_to_layout(layout: u64) -> ScriptStep {
    let mut step = ScriptStep::new(6);
    step.value = 2;
    step.set_member_key(StoredKey::from_id(layout));
    step
}

/// Perform Script [script] in this file.
pub fn perform_script(script: u64) -> ScriptStep {
    let mut step = ScriptStep::new(1);
    step.set_script_key(StoredKey::from_id(script));
    step
}

/// Set Variable [name; text].
pub fn set_variable(name: &str, text: &str) -> ScriptStep {
    let mut step = ScriptStep::new(141);
    step.set_text(0, Some(name));
    step.set_calculation(1, Some(&calc(text)));
    step
}

/// A step that turns an option on or off (Set Error Capture, ...).
pub fn on(id: u16, on: bool) -> ScriptStep {
    let mut step = ScriptStep::new(id);
    step.set_flag(flag::TURN_ON, on);
    step
}

/// Go to Record/Request/Page [First (1) / Last (2) / Previous (3) / Next (4)].
pub fn go_to_record(choice: u16) -> ScriptStep {
    let mut step = ScriptStep::new(16);
    step.value = choice;
    step.flags |= flag::MENU_SELECTION;
    step
}

/// Exit Script [Result: text].
pub fn exit_with(text: &str) -> ScriptStep {
    with_calc(103, text)
}

/// Perform Script on Server [script; Parameter: text], waiting or not.
pub fn perform_on_server(script: u64, parameter: &str, wait: bool) -> ScriptStep {
    let mut step = with_calc(164, parameter);
    step.set_script_key(StoredKey::from_id(script));
    step.set_flag(flag::OPTION, !wait);
    step
}

/// A script with its block indexes computed as the Script Workspace does.
pub fn script(id: u64, name: &str, steps: Vec<ScriptStep>) -> Script {
    let mut script = Script {
        id,
        name: name.into(),
        ..Default::default()
    };
    script.insert_steps(0, steps).expect("nests");
    script
}

/// A script set to "Run script with full access privileges".
pub fn full_access(id: u64, name: &str, steps: Vec<ScriptStep>) -> Script {
    let mut script = script(id, name, steps);
    script.set_runs_with_full_access(true);
    script
}

/// A script that never ends on its own: it changes record 1 inside a
/// transaction, then loops on a failing step (Set Field into a
/// calculation: 201). Set Revert Transaction on Error is Off, so the
/// errors do not skip to Commit Transaction (script-runtime.md 8.4).
pub fn runaway(id: u64) -> Script {
    script(
        id,
        "Runaway",
        vec![
            on(86, true),
            on(223, false),
            ScriptStep::new(205),
            go_to_record(1),
            set_field(NAME, "\"Runaway\""),
            ScriptStep::new(71),
            set_field(DOUBLED, "1"),
            ScriptStep::new(73),
            ScriptStep::new(206),
        ],
    )
}

/// The script every Perform Script on Server test runs: change record 1,
/// create a record named after the parameter, commit, and exit with what
/// the server saw.
pub fn server_work() -> Script {
    script(
        1,
        "Server Work",
        vec![
            on(86, true),
            go_to_record(1),
            set_field(COUNT, "Tasks::Count + 10"),
            ScriptStep::new(7),
            set_field(NAME, "\"From \" & Get ( ScriptParameter )"),
            set_field(DUE, "Date ( 10 ; 28 ; 2026 )"),
            ScriptStep::new(75),
            exit_with(
                "Get ( FoundCount ) & \"|\" & Get ( RecordID ) & \"|\" & Tasks::Name & \"|\" & \
                 Get ( AccountName ) & \"|\" & Get ( ApplicationVersion )",
            ),
        ],
    )
}

/// A script whose last step fails (Set Field into a calculation: 201).
pub fn failing() -> Script {
    script(
        2,
        "Failing",
        vec![set_field(DOUBLED, "1"), exit_with("\"failed\"")],
    )
}

/// A script that takes a while (Pause/Resume Script for `seconds`).
pub fn slow(seconds: f64) -> Script {
    let mut pause = with_calc(62, &seconds.to_string());
    pause.set_flag(flag::CALCULATION, true);
    script(3, "Slow", vec![pause, exit_with("\"slow done\"")])
}

/// A key string value: `[u8 length][serial key]`.
fn key_string(id: u64) -> Vec<u8> {
    let key = serial::encode(id);
    let mut out = vec![key.len() as u8];
    out.extend(key);
    out
}

/// The 26-byte field definition (schema-path.md 5.5): kind, type,
/// auto-enter variable, index language 21 (English), option word at 8,
/// validation word at 12.
fn definition(kind: u8, data_type: u8, variable: u8, options: u32, validation: u32) -> Vec<u8> {
    let mut out = vec![0; 26];
    out[0] = kind;
    out[1] = data_type;
    out[3] = variable;
    out[4..8].copy_from_slice(&21u32.to_be_bytes());
    out[8..12].copy_from_slice(&options.to_be_bytes());
    out[12..16].copy_from_slice(&validation.to_be_bytes());
    // One repetition.
    out[24..26].copy_from_slice(&1u16.to_be_bytes());
    out
}

/// Add field `id` named `name` to `table` of a tree [`tree`] built: kind
/// (1 normal, 2 calculation), type (1 text, 2 number, 3 date), auto-enter
/// variable, option and validation words, as in [`definition`].
#[allow(clippy::too_many_arguments)]
pub fn add_field(
    root: &mut OwnedNode,
    table: u64,
    id: u64,
    name: &str,
    kind: u8,
    data_type: u8,
    variable: u8,
    options: u32,
    validation: u32,
) {
    let (t, f) = (uint_segment(table), uint_segment(id));
    root.put(&[&t, &[3], &[5], &f, &[16]], text_node(&[], name));
    root.put(
        &[&t, &[3], &[5], &f, &[2]],
        OwnedNode::leaf(
            Vec::new(),
            definition(kind, data_type, variable, options, validation),
        ),
    );
}

/// Put formula `text` under field `field` of `table` (key 5 the
/// calculation, 0x19 the validation), compiled in `occurrence`'s context
/// with `names` (the file's names and the fields a test added).
pub fn add_formula(
    root: &mut OwnedNode,
    names: &NameTable,
    (table, occurrence): (u64, u64),
    field: u64,
    key: u8,
    text: &str,
) {
    let mut ctx = om_formula::Context::new(names);
    ctx.occurrence = Some(occurrence);
    ctx.support_self = true;
    let compiled = om_formula::compile(text, &ctx).unwrap_or_else(|e| panic!("{text}: {e:?}"));
    let (t, f) = (uint_segment(table), uint_segment(field));
    for (child, bytes) in [
        (4u8, Key::from_id(occurrence).0),
        (5, om_formula::formatted::encode(&compiled.tokens)),
        (6, om_formula::polish::encode(&compiled.expr)),
    ] {
        root.put(
            &[&t, &[3], &[5], &f, &[key], &[child]],
            OwnedNode::leaf(Vec::new(), bytes),
        );
    }
}

/// Change a field's option word in a tree [`tree`] built.
pub fn set_field_options(root: &mut OwnedNode, table: u64, field: u64, options: u32) {
    change_definition(root, table, field, 8, options);
}

/// Change a field's index language (definition bytes 4..7,
/// query-program.md 7.2) in a tree [`tree`] built.
pub fn set_field_language(root: &mut OwnedNode, table: u64, field: u64, language: u32) {
    change_definition(root, table, field, 4, language);
}

/// Change a field's validation word in a tree [`tree`] built.
pub fn set_field_validation(root: &mut OwnedNode, table: u64, field: u64, validation: u32) {
    change_definition(root, table, field, 12, validation);
}

/// Tasks::Code must be a member of value list 5, "Names", the values of
/// Tasks::Name with no second field, validated always (script-runtime.md
/// 13.21), in a tree [`tree`] built.
pub fn code_in_names_list(root: &mut OwnedNode) {
    code_in_list_of(root, NAME);
}

/// [`code_in_names_list`] with the values of Tasks' field `field` (an id
/// below 128).
pub fn code_in_list_of(root: &mut OwnedNode, field: u64) {
    set_field_validation(root, TABLE, CODE, 0x500);
    let (t, code) = (uint_segment(TABLE), uint_segment(CODE));
    let reference = vec![2, 1, 5, 0x90, 0, 0, 2, 1, 0, 0, 0, 0, 0xff];
    root.put(
        &[&t, &[3], &[5], &code, &[0x07]],
        OwnedNode::leaf(Vec::new(), reference),
    );
    root.put(&[&[0x21], &[5], &[5], &[0x10]], text_node(&[], "Names"));
    // Kind 1: Tasks::`field` (occurrence d0 00 01), no second field. Then
    // the counted separator text " " (one unit, SCSU as ASCII), spelled
    // out: om-author includes this fixture without om-scsu.
    let list = vec![
        1,
        0,
        0,
        4,
        3,
        0xd0,
        0,
        1,
        2,
        1,
        field as u8,
        0,
        1,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        1,
        b' ',
    ];
    root.put(
        &[&[0x21], &[5], &[5], &[2]],
        OwnedNode::leaf(Vec::new(), list),
    );
}

/// Change a field's serial number increment (`mSerialIncrement`) in a
/// tree [`tree`] built.
pub fn set_field_serial_increment(root: &mut OwnedNode, table: u64, field: u64, by: u32) {
    change_definition(root, table, field, 16, by);
}

/// Change a field's maximum number of characters (`mByteLimit`) in a tree
/// [`tree`] built.
pub fn set_field_byte_limit(root: &mut OwnedNode, table: u64, field: u64, limit: u32) {
    change_definition(root, table, field, 20, limit);
}

/// Change a field's number of repetitions in a tree [`tree`] built.
pub fn set_field_repetitions(root: &mut OwnedNode, table: u64, field: u64, repetitions: u16) {
    change_bytes(root, table, field, 24, &repetitions.to_be_bytes());
}

fn change_definition(root: &mut OwnedNode, table: u64, field: u64, at: usize, word: u32) {
    change_bytes(root, table, field, at, &word.to_be_bytes());
}

fn change_bytes(root: &mut OwnedNode, table: u64, field: u64, at: usize, new: &[u8]) {
    let (t, f) = (uint_segment(table), uint_segment(field));
    let node = root.at(&[&t, &[3], &[5], &f, &[2]]).expect("a field");
    let mut bytes = node.value.clone().expect("a definition");
    bytes[at..at + new.len()].copy_from_slice(new);
    root.put(
        &[&t, &[3], &[5], &f, &[2]],
        OwnedNode::leaf(Vec::new(), bytes),
    );
}

/// Add records `ids` to Tasks, each with a name and `Count` (and `Doubled`
/// as stored), keeping the record list and the last issued id.
pub fn add_tasks(root: &mut OwnedNode, ids: std::ops::RangeInclusive<u64>) {
    let table = uint_segment(TABLE);
    let mut list = root
        .at(&[&table, &[0x0d]])
        .and_then(|n| RidList::from_owned(n).ok())
        .unwrap_or_default();
    let last = *ids.end();
    for id in ids {
        let record = serial::encode(id);
        for (field, text) in [
            (NAME, format!("Task {id}")),
            (COUNT, (id % 10).to_string()),
            (DOUBLED, (id % 10 * 2).to_string()),
        ] {
            root.put(
                &[&table, &[5], &record, &uint_segment(field)],
                text_node(&[], &text),
            );
        }
        list.insert(id);
    }
    root.put(&[&table, &[0x0d]], list.to_node(Vec::new()));
    root.put(
        &[&table, &[0]],
        OwnedNode::leaf(Vec::new(), key_string(last)),
    );
}

/// A `LayoutDef` protobuf naming the occurrence (field 2, its serial-key
/// value, layout-model.md 4.1).
pub fn layout_def(occurrence: u64) -> Vec<u8> {
    let mut alias = serial::decode(&uint_segment(occurrence)).expect("a serial key");
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

/// The whole key tree.
/// Tasks::Photo, a container field ([`add_photo`]).
pub const PHOTO: u64 = 20;
/// The library member record 1's Photo names.
pub const PHOTO_MEMBER: u64 = 1;
/// A 3 x 2 red PNG, as the member holds it.
pub const PNG: &str = "89504e470d0a1a0a0000000d49484452000000030000000208020000001216f14d000000104944415408d763fccf00054c30060300132901033bb1691e0000000049454e44ae426082";

pub fn png() -> Vec<u8> {
    (0..PNG.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&PNG[i..i + 2], 16).unwrap())
        .collect()
}

/// Add the container field Photo to Tasks, record 1's value naming member 1
/// of Tasks' library (containers.md 1.4): [`png`] named `photo2.png`, with
/// its size, count 1, and the library header listing it.
pub fn add_photo(root: &mut OwnedNode) {
    let (t, f) = (uint_segment(TABLE), uint_segment(PHOTO));
    let mut definition = vec![0; 26];
    definition[0] = 1;
    definition[1] = 6;
    definition[24..26].copy_from_slice(&1u16.to_be_bytes());
    root.put(
        &[&t, &[3], &[5], &f, &[2]],
        OwnedNode::leaf(Vec::new(), definition),
    );
    root.put(&[&t, &[3], &[5], &f, &[16]], text_node(&[], "Photo"));
    let value = om_data::calc::Container {
        text: om_data::calc::Text::plain("photo2.png"),
        streams: vec![
            (om_data::calc::StreamType(*b"PNGf"), png()),
            (om_data::calc::StreamType(*b"SIZE"), vec![0, 3, 0, 2]),
        ],
        ..Default::default()
    };
    let (mut member, md5) = om_data::containers::tree(&value).unwrap();
    member.push(OwnedNode::leaf(vec![0x12], 1u32.to_be_bytes().to_vec()));
    member.sort_by(|a, b| a.segment.cmp(&b.segment));
    let m = serial::encode(PHOTO_MEMBER);
    root.put(&[&t, &[0x1f], &[5]], OwnedNode::list(vec![5], vec![]));
    root.put(&[&t, &[0x1f], &[5], &m], OwnedNode::list(m.clone(), member));
    let mut last = vec![m.len() as u8];
    last.extend(&m);
    let header = vec![
        OwnedNode::leaf(vec![0x00], last),
        OwnedNode::leaf(vec![0x04], 1u32.to_be_bytes().to_vec()),
        OwnedNode::list(vec![0x06], Vec::new()),
        OwnedNode::list(vec![0x07], vec![OwnedNode::leaf(m.clone(), Vec::new())]),
        OwnedNode::list(
            vec![0x08],
            vec![OwnedNode::list(
                md5.to_vec(),
                vec![OwnedNode::leaf(m.clone(), Vec::new())],
            )],
        ),
    ];
    root.put(&[&t, &[0x1f], &[1]], OwnedNode::list(vec![1], header));
    root.put(
        &[&t, &[5], &serial::encode(1), &f],
        text_node(&[], &om_data::containers::reference_text(&m)),
    );
}

/// Photo together with a global and an unstored calculation returning a
/// container. Neither added field can hold a duplicated record value.
pub fn add_nonwritable_photo_fields(root: &mut OwnedNode) {
    add_photo(root);
    add_field(root, TABLE, 30, "GlobalPhoto", 1, 6, 0, 0x1_0000, 0);
    add_field(root, TABLE, 31, "ComputedPhoto", 2, 6, 0, 0x2_0000, 0);
    let mut names = photo_names();
    names.add_field(TABLE, 30, "GlobalPhoto");
    names.add_field(TABLE, 31, "ComputedPhoto");
    add_formula(root, &names, (TABLE, OCCURRENCE), 31, 5, "Photo");
}

/// Photo stored externally, with a reference deliberately unavailable to
/// the test host. Reference bytes must never be returned as file data.
pub fn add_unavailable_external_photo(root: &mut OwnedNode) {
    add_photo(root);
    let name = om_data::containers::fnam("image", "photo2.png");
    let reference = b"unavailable-external-data";
    let mut directory = 2u16.to_be_bytes().to_vec();
    for (kind, len) in [(b"FNAM", name.len()), (b"pngF", reference.len())] {
        directory.extend(kind);
        directory.extend((len as u32).to_be_bytes());
    }
    let md5 = om_data::containers::md5([(b"FNAM".as_slice(), name.as_slice())]);
    let member = serial::encode(PHOTO_MEMBER);
    root.put(
        &[&uint_segment(TABLE), &[0x1f], &[5], &member],
        OwnedNode::list(
            member.clone(),
            vec![
                OwnedNode::leaf(vec![2], directory),
                OwnedNode::leaf(vec![4], md5.to_vec()),
                OwnedNode::leaf(vec![0x12], 1u32.to_be_bytes().to_vec()),
                OwnedNode::leaf(b"FNAM".to_vec(), name),
                OwnedNode::leaf(b"pngF".to_vec(), reference.to_vec()),
            ],
        ),
    );
}

/// The fields [`add_photo_calculations`] adds: stored calculations
/// PhotoName = GetContainerAttribute ( Photo ; "filename" ), PhotoLength =
/// Length ( Photo ), PhotoB64 = Base64Encode ( Photo ), and PhotoAE, text
/// auto-entered with Length ( Photo ) & "|" & GetAsText ( Photo ),
/// replacing its value (script oracle s34-containers-calc's fields).
pub const PHOTO_CALCULATIONS: [(u64, &str, u8, u8, u32, &str); 4] = [
    (
        21,
        "PhotoName",
        2,
        1,
        0,
        "GetContainerAttribute ( Photo ; \"filename\" )",
    ),
    (22, "PhotoLength", 2, 2, 0, "Length ( Photo )"),
    (23, "PhotoB64", 2, 1, 0, "Base64Encode ( Photo )"),
    // An auto-entered calculation (8) that replaces the value (0x80).
    (
        24,
        "PhotoAE",
        1,
        1,
        0x88,
        "Length ( Photo ) & \"|\" & GetAsText ( Photo )",
    ),
];

/// The Tasks names with Photo and [`PHOTO_CALCULATIONS`].
pub fn photo_names() -> NameTable {
    let mut names = names();
    names.add_field(TABLE, PHOTO, "Photo");
    for (id, name, ..) in PHOTO_CALCULATIONS {
        names.add_field(TABLE, id, name);
    }
    names
}

/// Add [`PHOTO_CALCULATIONS`] to a tree with [`add_photo`].
pub fn add_photo_calculations(root: &mut OwnedNode) {
    let names = photo_names();
    for (id, name, kind, data_type, options, text) in PHOTO_CALCULATIONS {
        add_field(root, TABLE, id, name, kind, data_type, 0, options, 0);
        add_formula(root, &names, (TABLE, OCCURRENCE), id, 5, text);
    }
}

/// Give `root`'s theme catalog `[6]` the themes `keys`, each a copy of
/// the theme OpenMaker writes in a new file (`om_data::new_file`), with
/// the index `[6, 1]` listing them as FileMaker lists its themes: the last
/// issued id `00`, the count `04` and an empty entry `07, T` for each
/// (layout-model.md 7.5). Its layouts keep the theme they name.
pub fn add_themes(root: &mut OwnedNode, keys: &[u64]) {
    let made = om_data::new_file::tree(
        &om_data::new_file::NewFile {
            name: "Themes".into(),
            locale: om_data::calc::Locale::en_us(),
            user: "Tester".into(),
            // 11/13/2026 at midnight as a date-time key: the date's three
            // bytes (`80 00 00` | year << 10 | month << 6 | day), then the
            // seconds since midnight as a number key, `80 00` for 0.
            time: vec![0x9f, 0xaa, 0xcd, 0x80, 0x00],
            internal_creation: 0,
        },
        &mut |out: &mut [u8]| out.fill(7),
    )
    .expect("a new file");
    let theme = made
        .at(&[&[6], &[5], &[1]])
        .cloned()
        .expect("a new file's theme");
    for &key in keys {
        let segment = serial::encode(key);
        let mut member = theme.clone();
        member.segment = segment.clone();
        root.put(&[&[6], &[5], &segment], member);
        root.put(
            &[&[6], &[1], &[7], &segment],
            OwnedNode::leaf(segment.clone(), Vec::new()),
        );
    }
    let last = keys.iter().copied().max().unwrap_or(0);
    root.put(
        &[&[6], &[1], &[0]],
        OwnedNode::leaf(vec![0], key_string(last)),
    );
    root.put(
        &[&[6], &[1], &[4]],
        OwnedNode::leaf(vec![4], (keys.len() as u32).to_be_bytes().to_vec()),
    );
}

/// Give `root`'s layouts theme 1 and index the layout catalog `[4, 1]` as
/// FileMaker indexes it (layout-model.md 10.6: the last issued id, the name
/// list, the display order, the count and each layout's entry), so a layout
/// can be added to it.
/// Give each entry of the layout index `[4, 1, 07, L]` its layout's
/// definition `02`, as every file FileMaker makes has it (layout-model.md
/// 10.2: the table occurrence and menu set), copied from the member
/// `[4, 5, L, 02]`; after [`index_layouts`].
pub fn index_layout_definitions(root: &mut OwnedNode) {
    let members = root.at(&[&[4], &[5]]).cloned().unwrap_or_default();
    for member in &members.children {
        if let Some(definition) = member.child(&[2]).and_then(|n| n.value.clone()) {
            root.put(
                &[&[4], &[1], &[7], &member.segment, &[2]],
                OwnedNode::leaf(Vec::new(), definition),
            );
        }
    }
}

pub fn index_layouts(root: &mut OwnedNode) {
    let members = root.at(&[&[4], &[5]]).cloned().unwrap_or_default();
    let mut index = OwnedNode::list(vec![1], Vec::new());
    let mut last = 0;
    for (n, member) in members.children.iter().enumerate() {
        root.put(
            &[&[4], &[5], &member.segment, &[0x0b]],
            OwnedNode::leaf(Vec::new(), vec![1, 1]),
        );
        last = last.max(serial::decode(&member.segment).unwrap_or(0));
        let name = member
            .child(&[0x10])
            .and_then(om_scripts::node_text)
            .unwrap_or_default();
        let id = [&[member.segment.len() as u8][..], &member.segment].concat();
        let mut listed = om_data::field_catalog::name_key(&name);
        listed.extend([0, 0, 0]);
        index.put(&[&[1], &listed], OwnedNode::leaf(Vec::new(), id.clone()));
        index.put(
            &[&[3], &(n as u32 + 1).to_be_bytes()],
            OwnedNode::leaf(Vec::new(), id),
        );
        index.put(&[&[7], &member.segment, &[0x10]], text_node(&[0x10], &name));
    }
    let last = serial::encode(last);
    index.set_child(OwnedNode::leaf(
        vec![0],
        [&[last.len() as u8][..], &last].concat(),
    ));
    index.set_child(OwnedNode::leaf(
        vec![4],
        (members.children.len() as u32).to_be_bytes().to_vec(),
    ));
    root.put(&[&[4], &[1]], index);
}

/// Give a tree's script catalog the index `[17, 1]` every file FileMaker
/// makes has (scripts.md, "The catalog index"): the last issued id, a name
/// list entry, the display position and the member cache (name `10`, flags
/// `f0`) of each script in `[17, 5]`, and the count.
pub fn index_scripts(root: &mut OwnedNode) {
    let members = root.at(&[&[17], &[5]]).cloned().unwrap_or_default();
    let mut index = OwnedNode::list(vec![1], Vec::new());
    let mut last = 0;
    for (n, member) in members.children.iter().enumerate() {
        last = last.max(serial::decode(&member.segment).unwrap_or(0));
        let name = member
            .child(&[0x10])
            .and_then(om_scripts::node_text)
            .unwrap_or_default();
        let id = [&[member.segment.len() as u8][..], &member.segment].concat();
        let mut listed = om_data::field_catalog::name_key(&name);
        listed.extend([0, 0, 0]);
        index.put(&[&[1], &listed], OwnedNode::leaf(Vec::new(), id.clone()));
        index.put(
            &[&[3], &(n as u32 + 1).to_be_bytes()],
            OwnedNode::leaf(Vec::new(), id),
        );
        index.put(&[&[7], &member.segment, &[0x10]], text_node(&[0x10], &name));
        if let Some(flags) = member.child(&[0xf0]) {
            index.put(&[&[7], &member.segment, &[0xf0]], flags.clone());
        }
    }
    let last = serial::encode(last);
    index.set_child(OwnedNode::leaf(
        vec![0],
        [&[last.len() as u8][..], &last].concat(),
    ));
    index.set_child(OwnedNode::leaf(
        vec![4],
        (members.children.len() as u32).to_be_bytes().to_vec(),
    ));
    root.put(&[&[17], &[1]], index);
}

pub fn tree(scripts: &[Script], with_layout: bool) -> OwnedNode {
    let mut root = OwnedNode::default();
    let put = |root: &mut OwnedNode, path: &[&[u8]], node: OwnedNode| root.put(path, node);
    put(
        &mut root,
        &[&[2], &[254]],
        OwnedNode::leaf(Vec::new(), INTERNAL_CREATION.to_be_bytes().to_vec()),
    );
    for (table, occurrence, layout, name) in [
        (TABLE, OCCURRENCE, LAYOUT, "Tasks"),
        (NOTES, NOTES_OCCURRENCE, NOTES_LAYOUT, "Notes"),
    ] {
        let table = uint_segment(table);
        let occurrence_key = uint_segment(occurrence);
        put(
            &mut root,
            &[&[3], &[16], &[5], &table, &[16]],
            text_node(&[], name),
        );
        put(
            &mut root,
            &[&[3], &[17], &[5], &occurrence_key, &[16]],
            text_node(&[], name),
        );
        let mut to_definition = vec![2, 1, 0, table.len() as u8 + 1, table.len() as u8];
        to_definition.extend(&table);
        put(
            &mut root,
            &[&[3], &[17], &[5], &occurrence_key, &[2]],
            OwnedNode::leaf(Vec::new(), to_definition),
        );
        if with_layout {
            let layout = uint_segment(layout);
            put(
                &mut root,
                &[&[4], &[5], &layout, &[16]],
                text_node(&[], name),
            );
            put(
                &mut root,
                &[&[4], &[5], &layout, &[2]],
                OwnedNode::leaf(Vec::new(), layout_def(occurrence)),
            );
        }
    }
    for script in scripts {
        let node = script.encode();
        let id = node.segment.clone();
        put(&mut root, &[&[17], &[5], &id], node);
    }
    for (record, account, password, set) in
        [(2u8, ACCOUNT, PASSWORD, 1u8), (3, CLERK, CLERK_PASSWORD, 2)]
    {
        put(
            &mut root,
            &[&[23], &[1], &[5], &[record], &[16]],
            text_node(&[], account),
        );
        put_account(&mut root, record, password, None, set);
    }
    // [Full Access] by the members that verify it (its name and record
    // access), and [Data Entry Only] as the starter solutions store it
    // (session-identity.md 2.7), each with a readable password lifetime.
    root.put(
        &[&[23], &[2], &[5], &[1], &[16]],
        text_node(&[], "[Full Access]"),
    );
    root.put(
        &[&[23], &[2], &[5], &[1], &[2]],
        OwnedNode::leaf(Vec::new(), vec![0x55, 0x03]),
    );
    put_data_entry_only(&mut root);
    for set in [1u8, 2] {
        put(
            &mut root,
            &[&[23], &[2], &[5], &[set], &[0x0c]],
            OwnedNode::leaf(Vec::new(), vec![0, 0]),
        );
    }
    // Catalogs FileMaker clients download when they open a file (themes,
    // the file reference catalog, the library), here with only an index.
    for catalog in [6u8, 25, 31] {
        put(
            &mut root,
            &[&[catalog], &[1], &[0]],
            OwnedNode::leaf(Vec::new(), key_string(0)),
        );
    }
    for (table, fields) in [(TABLE, &FIELDS[..]), (NOTES, &NOTE_FIELDS[..])] {
        let table = uint_segment(table);
        for &(id, name, kind, data_type, variable, options, validation) in fields {
            let field = uint_segment(id);
            let at = [&table[..], &[3], &[5], &field];
            put(
                &mut root,
                &[at[0], at[1], at[2], at[3], &[16]],
                text_node(&[], name),
            );
            put(
                &mut root,
                &[at[0], at[1], at[2], at[3], &[2]],
                OwnedNode::leaf(
                    Vec::new(),
                    definition(kind, data_type, variable, options, validation),
                ),
            );
        }
    }
    for (table, occurrence, field, key, text) in FORMULAS {
        let compiled = compiled(occurrence, text);
        let (table, field) = (uint_segment(table), uint_segment(field));
        let at = [&table[..], &[3], &[5], &field, &[key]];
        for (child, bytes) in [
            (4u8, Key::from_id(occurrence).0),
            (5, om_formula::formatted::encode(&compiled.tokens)),
            (6, om_formula::polish::encode(&compiled.expr)),
        ] {
            put(
                &mut root,
                &[at[0], at[1], at[2], at[3], at[4], &[child]],
                OwnedNode::leaf(Vec::new(), bytes),
            );
        }
    }
    type Row = (u64, &'static [(u64, &'static str)]);
    let tables: [(u64, &[Row]); 2] = [
        (
            TABLE,
            &[
                (
                    1,
                    &[
                        (NAME, "Write tests"),
                        (COUNT, "3"),
                        (DUE, "10/1/2026"),
                        (DOUBLED, "6"),
                    ],
                ),
                (2, &[(NAME, "Ship"), (COUNT, "5"), (DOUBLED, "10")]),
            ],
        ),
        (NOTES, &[(1, &[(BODY, "Hi")])]),
    ];
    for (table, records) in tables {
        let table = uint_segment(table);
        let mut ids = RidList::new();
        for (id, cells) in records {
            let record = serial::encode(*id);
            for (field, text) in cells.iter() {
                let field = uint_segment(*field);
                put(
                    &mut root,
                    &[&table, &[5], &record, &field],
                    text_node(&[], text),
                );
            }
            ids.insert(*id);
        }
        put(&mut root, &[&table, &[0x0d]], ids.to_node(Vec::new()));
        put(
            &mut root,
            &[&table, &[0]],
            OwnedNode::leaf(Vec::new(), key_string(records.len() as u64)),
        );
    }
    root
}

/// Account statuses (`[23, 1, 5, A, 0a]`, `om_server::accounts`): active
/// with no password change due, as every account of a file FileMaker made
/// (calc-functions.md s33) has; inactive, as the starter solutions'
/// [Guest] is (microseconds bit 19); and due a password change (bit 18).
pub const ACTIVE_STATUS: [u8; 6] = [0x05, 0x90, 0x00, 0x41, 0x80, 0x00];
pub const INACTIVE_STATUS: [u8; 9] = [0x08, 0x90, 0x00, 0x41, 0x80, 0x00, 0x63, 0x53, 0x99];
pub const CHANGE_PASSWORD_STATUS: [u8; 9] = [0x08, 0x90, 0x00, 0x41, 0x80, 0x00, 0x37, 0x32, 0x55];

/// Set 2 as the starter solutions store [Data Entry Only] (session-identity.md
/// 2.7), every member that verifies it: its name, every record action
/// (`55 03`), layouts view only with their records modifiable (`de`),
/// scripts executable and value lists view only (`d6`), other privileges
/// `03 04`.
pub fn put_data_entry_only(root: &mut OwnedNode) {
    let set: &[u8] = &[2];
    root.put(
        &[&[23], &[2], &[5], set, &[16]],
        text_node(&[], "[Data Entry Only]"),
    );
    for (member, value) in [(&[2u8][..], &[0x55u8, 0x03][..]), (&[0x0a], &[0x03, 0x04])] {
        root.put(
            &[&[23], &[2], &[5], set, member],
            OwnedNode::leaf(Vec::new(), value.to_vec()),
        );
    }
    for (catalog, quad) in [(0x04u8, 0xdeu8), (0x11, 0xd6), (0x21, 0xd6)] {
        root.put(
            &[&[23], &[2], &[5], set, &[9], &[catalog]],
            OwnedNode::leaf(Vec::new(), vec![quad]),
        );
    }
}

/// Add [`READER`], in set 3 as the starter solutions store [Read-Only
/// Access] (session-identity.md 2.7): its name, records view only (`10
/// 03`), layouts view only with their records modifiable (`de`), scripts
/// executable and value lists view only (`d6`), other privileges `03 04`,
/// no password lifetime.
pub fn add_reader(root: &mut OwnedNode) {
    let set: &[u8] = &[3];
    root.put(
        &[&[23], &[2], &[5], set, &[16]],
        text_node(&[], "[Read-Only Access]"),
    );
    for (member, value) in [(2u8, [0x10u8, 0x03]), (0x0a, [0x03, 0x04]), (0x0c, [0, 0])] {
        root.put(
            &[&[23], &[2], &[5], set, &[member]],
            OwnedNode::leaf(Vec::new(), value.to_vec()),
        );
    }
    for (catalog, quad) in [(0x04u8, 0xdeu8), (0x11, 0xd6), (0x21, 0xd6)] {
        root.put(
            &[&[23], &[2], &[5], set, &[9], &[catalog]],
            OwnedNode::leaf(Vec::new(), vec![quad]),
        );
    }
    root.put(&[&[23], &[1], &[5], &[6], &[16]], text_node(&[], READER));
    put_account(root, 6, READER_PASSWORD, None, 3);
}

/// Add [`TEMP`], in set 4 "Clerks", a custom set OpenMaker enforces
/// (session-identity.md 2.1-2.3, the members `om_data`'s
/// `custom_sets_are_enforced_only_with_plain_record_and_field_limits`
/// decodes): records of Tasks viewed, created and edited but not deleted
/// (`54 03`), every other table's view only (`10 03`); every layout,
/// script and value list modifiable; every other privilege; no password
/// lifetime. A server admits it only with `Server::admitting_custom_sets`.
pub fn add_custom_clerk(root: &mut OwnedNode) {
    let set: &[u8] = &[4];
    root.put(&[&[23], &[2], &[5], set, &[16]], text_node(&[], "Clerks"));
    for (member, value) in [(2u8, [0x10u8, 0x03]), (0x0a, [0x9f, 0xff]), (0x0c, [0, 0])] {
        root.put(
            &[&[23], &[2], &[5], set, &[member]],
            OwnedNode::leaf(Vec::new(), value.to_vec()),
        );
    }
    root.put(
        &[&[23], &[2], &[5], set, &[5], &uint_segment(TABLE), &[2]],
        OwnedNode::leaf(Vec::new(), vec![0x54, 0x03]),
    );
    for catalog in [0x04u8, 0x11, 0x21] {
        root.put(
            &[&[23], &[2], &[5], set, &[9], &[catalog]],
            OwnedNode::leaf(Vec::new(), vec![if catalog == 4 { 0xdf } else { 0xd7 }]),
        );
    }
    root.put(&[&[23], &[1], &[5], &[7], &[16]], text_node(&[], TEMP));
    put_account(root, 7, TEMP_PASSWORD, None, 4);
}

/// Store account `record`'s password as FileMaker does (`[23, 1, 5, A,
/// 06]`, `om_crypto::account`), its status when given and its set
/// reference (`0b`).
pub fn put_account(
    root: &mut OwnedNode,
    record: u8,
    password: &str,
    status: Option<&[u8]>,
    set: u8,
) {
    use om_crypto::account::StoredPassword;
    // With the check bytes FileMaker writes after the hash.
    let stored = StoredPassword::encode(password, [record; 10]);
    let account = [&[23u8][..], &[1], &[5], &[record]];
    let mut members = vec![(6u8, stored), (0x0b, vec![1, set])];
    if let Some(status) = status {
        members.push((0x0a, status.to_vec()));
    }
    for (key, value) in members {
        root.put(
            &[account[0], account[1], account[2], account[3], &[key]],
            OwnedNode::leaf(Vec::new(), value),
        );
    }
}

/// Accounts in states a login refuses, and one the host cannot hold to its
/// set (docs/re/local-files.md 2): Admin and Clerk stamped active
/// ([`ACTIVE_STATUS`]); Clerk's set 2 cut back to its name and record
/// access, not every member FileMaker gives [Data Entry Only], so that a
/// login refuses it (9; [`put_data_entry_only`] puts the rest back);
/// [`FORMER`] (inactive) and [`NEWCOMER`] (password due a change) in [Full
/// Access]; and Admin named in the account catalog's header
/// (`[23, 1, 1, 16]`), the account a client suggests.
pub fn add_local_accounts(root: &mut OwnedNode) {
    for (record, name, password, status, set) in [
        (2u8, None, PASSWORD, &ACTIVE_STATUS[..], 1u8),
        (3, None, CLERK_PASSWORD, &ACTIVE_STATUS[..], 2),
        (4, Some(FORMER), FORMER_PASSWORD, &INACTIVE_STATUS[..], 1),
        (
            5,
            Some(NEWCOMER),
            NEWCOMER_PASSWORD,
            &CHANGE_PASSWORD_STATUS[..],
            1,
        ),
    ] {
        put_account(root, record, password, Some(status), set);
        if let Some(name) = name {
            root.put(&[&[23], &[1], &[5], &[record], &[16]], text_node(&[], name));
        }
    }
    for member in [9u8, 0x0a] {
        root.remove(&[&[23], &[2], &[5], &[2], &[member]]);
    }
    root.put(&[&[23], &[1], &[1], &[16]], text_node(&[], ACCOUNT));
}

/// A fresh superblock: the `HBAM7` magic and the 4096-byte page size
/// (file-format.md 1.2); the writer stamps the rest.
fn superblock() -> Vec<u8> {
    let mut page = vec![0u8; 4096];
    page[0x0f..0x14].copy_from_slice(b"HBAM7");
    page[0x14..0x18].copy_from_slice(&4096u32.to_be_bytes());
    page
}

/// Write the file with `scripts` to `path`.
pub fn write(path: &Path, scripts: &[Script], with_layout: bool) {
    write_tree(path, &tree(scripts, with_layout));
}

/// Write a file holding `root` to `path`.
pub fn write_tree(path: &Path, root: &OwnedNode) {
    let bytes = om_hbam::writer::rebuild(root, &superblock()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// A fresh copy in the temporary directory, removed when dropped.
pub struct TempFile(pub PathBuf);

impl TempFile {
    pub fn new(tag: &str, scripts: &[Script], with_layout: bool) -> TempFile {
        Self::from_tree(tag, &tree(scripts, with_layout))
    }

    /// A file holding `root`, a [`tree`] the test changed.
    pub fn from_tree(tag: &str, root: &OwnedNode) -> TempFile {
        let path = std::env::temp_dir().join(format!(
            "om-psos-{tag}-{}-{:?}.fmp12",
            std::process::id(),
            std::thread::current().id()
        ));
        write_tree(&path, root);
        TempFile(path)
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let mut save = self.0.as_os_str().to_owned();
        save.push(".omsave");
        let _ = std::fs::remove_file(save);
    }
}

/// A loopback SMTP server for one session per entry of `sessions`, each
/// the replies to give: the greeting, then one per command (DATA's
/// message ends at its `.` line). What each session read comes back.
pub fn smtp_server(
    sessions: Vec<Vec<&'static str>>,
) -> (u16, std::thread::JoinHandle<Vec<Vec<String>>>) {
    use std::io::{BufRead, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let mut all = Vec::new();
        for replies in sessions {
            let (socket, _) = listener.accept().unwrap();
            let mut writer = socket.try_clone().unwrap();
            let mut reader = std::io::BufReader::new(socket);
            let mut replies = replies.into_iter();
            writer
                .write_all(replies.next().unwrap().as_bytes())
                .unwrap();
            let (mut read, mut in_data) = (Vec::new(), false);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                read.push(line.clone());
                if in_data && line != ".\r\n" {
                    continue;
                }
                in_data = line == "DATA\r\n";
                let Some(reply) = replies.next() else { break };
                writer.write_all(reply.as_bytes()).unwrap();
                if line == "QUIT\r\n" {
                    break;
                }
            }
            all.push(read);
        }
        all
    });
    (port, handle)
}

/// Send Mail [Send via SMTP Server] to `port` on loopback, From Pat,
/// To kim@example.org; with `login`, a plain password login.
pub fn smtp_mail(port: u16, login: bool) -> ScriptStep {
    let mut step = ScriptStep::new(63);
    step.set_flag(om_scripts::flag::NO_INTERACT, true);
    step.set_number_bool(4, Some(true));
    let port = port.to_string();
    let mut calculations = vec![
        (0, "\"kim@example.org\""),
        (3, "\"Weekly report\""),
        (4, "\"See you\" & ¶ & \"Pat\""),
        (5, "\"Pat\""),
        (6, "\"pat@example.org\""),
        (8, "\"127.0.0.1\""),
        (9, port.as_str()),
    ];
    if login {
        step.set_number_u16(6, Some(2));
        calculations.extend([(10, "\"pat\""), (11, "\"wrong\"")]);
    }
    for (slot, text) in calculations {
        step.set_calculation(slot, Some(&calc(text)));
    }
    step
}

/// [`smtp_server`]'s replies for two sessions: one that takes a message
/// without a login, and one that refuses the password (535).
pub fn smtp_sent_then_refused() -> Vec<Vec<&'static str>> {
    vec![
        vec![
            "220 mail.example.org ESMTP\r\n",
            "250 mail.example.org\r\n",
            "250 OK\r\n",
            "250 OK\r\n",
            "354 Go ahead\r\n",
            "250 OK queued\r\n",
            "221 Bye\r\n",
        ],
        vec![
            "220 mail.example.org ESMTP\r\n",
            "250-mail.example.org\r\n250 AUTH PLAIN\r\n",
            "535 5.7.8 Authentication credentials invalid\r\n",
        ],
    ]
}
