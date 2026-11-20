//! Portal rows and a script's write rules through the GUI end to end:
//! scripts run on the provider worker against the native provider and an
//! in-process `om-server` hosting the in-memory Tasks file of
//! `om-server/tests/fixtures/tasks.rs`, given a layout "Portals" whose
//! portal shows Notes through relationship 1 (Tasks::Name = Notes::Task,
//! creation allowed), Notes records "first" and "second" of "Write
//! tests", and Status validated Always with at most 3 characters in 3
//! repetitions.
//!
//! - Go to Portal Row moves the active row; `Get ( ActivePortalRowNumber )`,
//!   `Get ( ActiveFieldName )`, `Get ( ActiveFieldTableName )` and a
//!   related field read that row; Next past the last row is 101.
//! - Set Field into a related field writes the active row's record, or
//!   creates one through the relationship (its match field set from the
//!   current record), committed with the current record; a related
//!   record's validation fails the commit (507), not the Set Field.
//! - Delete Portal Row deletes the active row's record with the commit.
//! - OnWindowTransaction (the file's trigger, script 9) runs after the
//!   script, once per commit, with FileMaker's JSON of the records.
//! - A script's writes follow a script's rules (script-runtime.md 13.20,
//!   13.21): a field whose auto-entered value prohibits modification takes
//!   a script's value, maximum characters is Set Field's own 511 (the field
//!   keeping its value), and repetitions past the first are written.
//! - Perform Quick Find searches the layout's own and related fields, each
//!   word in any of them, but not a global field; no match is 401 and
//!   keeps the found set, and a match keeps the current record when the
//!   new found set holds it (query-program.md 6.6).
mod common;
#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
mod hosted;
#[path = "../../om-server/tests/fixtures/transfer.rs"]
mod transfer;

use std::path::Path;

use egui_kittest::kittest::{By, NodeT as _, Queryable};
use fixture::{
    calculated_calc as calc, ACCOUNT, BODY, CODE, CREATOR, NAME, NOTES, NOTES_OCCURRENCE,
    OCCURRENCE, PASSWORD, SETTING, STATUS, TABLE, TASK,
};
use om_data::host::CalcNeed;
use om_data::runtime::TriggerEvent;
use om_data::DataProvider;
use om_hbam::slot::uint_segment;
use om_hbam::{serial, OwnedNode, RidList};
use om_scripts::specs::{SortEntry, SortSpec, SpecField};
use om_scripts::{flag, text_node, FieldSpec, Script, ScriptStep, StoredKey};

/// The layout with the portal.
const PORTALS: u64 = 3;
/// Its objects: the portal, the Notes::Body field in it, Tasks::Name,
/// Tasks::Code and the global Tasks::Setting.
const PORTAL: u64 = 1;
const PORTAL_BODY: u64 = 2;
const TASK_NAME: u64 = 3;
const TASK_CODE: u64 = 4;
const TASK_SETTING: u64 = 5;
/// Notes::Extra in the portal: a field FileMaker numbers 200 (its catalog
/// key reads [`EXTRA_RAW`]), two repetitions.
const PORTAL_EXTRA: u64 = 6;
const EXTRA: u64 = 200;
/// A Tasks occurrence related to Tasks by `Name ≠ Name` (relationship 4).
const OTHER_TASKS: u64 = 0xd0_0005;
/// The script OnObjectExit of the portal's Body field runs.
const KEEP: u64 = 18;
/// A button in the portal's rows, labeled [`ROW_ACTION`]: Perform Script
/// [`ROW_BUTTON_SCRIPT`; Parameter: [`ROW_PARAMETER`]].
const ROW_BUTTON: u64 = 7;
const ROW_ACTION: &str = "RowAction";
const ROW_BUTTON_SCRIPT: u64 = 52;
/// The parameter reads the row's record, the active row and the parent's
/// name: Tasks record 2 ("Ship") has the id of the Notes record "first".
const ROW_PARAMETER: &str =
    "Notes::Body & \":\" & Get ( ActivePortalRowNumber ) & \":\" & Tasks::Name";
/// OnObjectExit of Tasks::Code in [`add_code_exit_trigger`]: by `$$mode`
/// it shows a dialog and refuses ("veto") or rewrites the first related
/// note, which then sorts first, and commits ("reorder").
const CODE_EXIT: u64 = 53;
/// The portal's option word: it scrolls (`0x1`), as the templates' 257.
const PORTAL_FLAGS: u64 = 0x101;

/// A minimal protobuf writer for the layout's object store
/// (layout-model.md 5; `docs/re/protobuf/LayoutObject.proto`).
fn varint(mut v: u64, out: &mut Vec<u8>) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn uint(number: u32, v: u64, out: &mut Vec<u8>) {
    varint(u64::from(number) << 3, out);
    varint(v, out);
}

fn double(number: u32, v: f64, out: &mut Vec<u8>) {
    varint(u64::from(number) << 3 | 1, out);
    out.extend_from_slice(&v.to_le_bytes());
}

fn bytes(number: u32, b: &[u8], out: &mut Vec<u8>) {
    varint(u64::from(number) << 3 | 2, out);
    varint(b.len() as u64, out);
    out.extend_from_slice(b);
}

fn rect(top: f64, left: f64, bottom: f64, right: f64) -> Vec<u8> {
    let mut out = Vec::new();
    double(1, top, &mut out);
    double(2, left, &mut out);
    double(3, bottom, &mut out);
    double(4, right, &mut out);
    out
}

/// An occurrence as layouts store it: the value of its serial key.
fn alias(occurrence: u64) -> u64 {
    serial::decode(&uint_segment(occurrence)).expect("a serial key")
}

/// A button running `step` (layout-model.md 5.3, `ButtonLayoutObject`).
fn button_object(id: u64, bounds: Vec<u8>, step: &ScriptStep) -> Vec<u8> {
    let mut native = Vec::new();
    uint(1, step.opcode as u64, &mut native);
    uint(2, u64::from(step.value), &mut native);
    uint(3, u64::from(step.flags), &mut native);
    bytes(4, &step.reserved, &mut native);
    let mut button = Vec::new();
    bytes(3, &native, &mut button);
    let mut obj = Vec::new();
    uint(1, 10, &mut obj);
    uint(2, id, &mut obj);
    bytes(4, &bounds, &mut obj);
    bytes(16, &button, &mut obj);
    obj
}

fn field_object(id: u64, occurrence: u64, field: u64, bounds: Vec<u8>) -> Vec<u8> {
    shown_field_object(id, occurrence, field, bounds, 0)
}

/// A field object showing `shown` repetitions (`numOfReps`, stacked
/// vertically) of `field` from `repetition` on.
fn repetition_object(
    id: u64,
    occurrence: u64,
    field: u64,
    (repetition, shown): (u64, u64),
    bounds: Vec<u8>,
) -> Vec<u8> {
    let mut spec = Vec::new();
    uint(1, alias(occurrence), &mut spec);
    uint(2, field, &mut spec);
    uint(3, repetition, &mut spec);
    let mut field_obj = Vec::new();
    bytes(2, &spec, &mut field_obj);
    uint(3, shown, &mut field_obj);
    let mut obj = Vec::new();
    uint(1, 1, &mut obj);
    uint(2, id, &mut obj);
    bytes(4, &bounds, &mut obj);
    bytes(8, &field_obj, &mut obj);
    obj
}

/// [`field_object`] shown as `display` (`displayType`, layout-model.md
/// 5.4): 0 an edit box, 2 a pop-up menu (of no value list here).
fn shown_field_object(
    id: u64,
    occurrence: u64,
    field: u64,
    bounds: Vec<u8>,
    display: u64,
) -> Vec<u8> {
    let mut spec = Vec::new();
    uint(1, alias(occurrence), &mut spec);
    uint(2, field, &mut spec);
    let mut field_obj = Vec::new();
    bytes(2, &spec, &mut field_obj);
    if display != 0 {
        uint(5, display, &mut field_obj);
    }
    let mut obj = Vec::new();
    uint(1, 1, &mut obj);
    uint(2, id, &mut obj);
    bytes(4, &bounds, &mut obj);
    bytes(8, &field_obj, &mut obj);
    obj
}

/// Layout "Portals" on Tasks: Tasks::Name, Tasks::Code, Tasks::Setting,
/// and a portal of Notes (three rows) showing Notes::Body and a button.
fn add_portal_layout(root: &mut OwnedNode) {
    let layout = uint_segment(PORTALS);
    root.put(&[&[4], &[5], &layout, &[16]], text_node(&[], "Portals"));
    let mut def = Vec::new();
    uint(2, alias(OCCURRENCE), &mut def);
    // 480 points wide (`baseWidth`) with a 200-point Body part: Browse
    // draws only the objects on the layout's design surface.
    double(8, 480.0, &mut def);
    root.put(
        &[&[4], &[5], &layout, &[2]],
        OwnedNode::leaf(Vec::new(), def),
    );
    let mut body = Vec::new();
    uint(1, 4, &mut body);
    double(4, 0.0, &mut body);
    double(5, 200.0, &mut body);
    root.put(
        &[&[4], &[5], &layout, &[3], &[1], &[2]],
        OwnedNode::leaf(Vec::new(), body),
    );
    root.put(
        &[&[4], &[5], &layout, &[7]],
        OwnedNode::leaf(Vec::new(), portal_store(PORTAL_FLAGS)),
    );
    for (object, name) in [
        (PORTAL, "Notes portal"),
        (PORTAL_BODY, "Note body"),
        (PORTAL_EXTRA, "Note extra"),
        (TASK_NAME, "Task name"),
        (TASK_CODE, "Task code"),
        (TASK_SETTING, "Task setting"),
    ] {
        root.put(
            &[&[4], &[5], &layout, &[5], &serial::encode(object), &[16]],
            text_node(&[], name),
        );
    }
    // The row button's text (key 2) and parameter calculation (its
    // `ITEMS` 17, item 0).
    root.put(
        &[
            &[4],
            &[5],
            &layout,
            &[5],
            &serial::encode(ROW_BUTTON),
            &[2],
            &[6],
        ],
        OwnedNode::leaf(
            Vec::new(),
            calc(&format!("\"{ROW_ACTION}\"")).polish.unwrap(),
        ),
    );
    root.put(
        &[
            &[4],
            &[5],
            &layout,
            &[5],
            &serial::encode(ROW_BUTTON),
            &[17],
            &[0],
            &[5],
            &[6],
        ],
        OwnedNode::leaf(Vec::new(), calc(ROW_PARAMETER).polish.unwrap()),
    );
}

/// The Notes portal (three rows of Notes::Body, Notes::Extra and the row
/// button), its option word `flags`, at `bounds` in its container.
fn portal_object(flags: u64, bounds: Vec<u8>) -> Vec<u8> {
    let mut portal_obj = Vec::new();
    uint(2, alias(NOTES_OCCURRENCE), &mut portal_obj);
    uint(3, flags, &mut portal_obj);
    double(6, 3.0, &mut portal_obj);
    let mut portal = Vec::new();
    uint(1, 9, &mut portal);
    uint(2, PORTAL, &mut portal);
    bytes(4, &bounds, &mut portal);
    uint(6, PORTAL_BODY, &mut portal);
    uint(6, PORTAL_EXTRA, &mut portal);
    uint(6, ROW_BUTTON, &mut portal);
    bytes(15, &portal_obj, &mut portal);
    portal
}

/// The Portals layout's object store, the portal's option word `flags`.
fn portal_store(flags: u64) -> Vec<u8> {
    portal_store_shown(flags, 0)
}

/// [`portal_store`], the portal's Notes::Body shown as `body_display`
/// ([`shown_field_object`]).
fn portal_store_shown(flags: u64, body_display: u64) -> Vec<u8> {
    portal_store_with(
        flags,
        body_display,
        field_object(
            PORTAL_EXTRA,
            NOTES_OCCURRENCE,
            EXTRA,
            rect(0.0, 310.0, 20.0, 390.0),
        ),
        field_object(
            TASK_SETTING,
            OCCURRENCE,
            SETTING,
            rect(140.0, 120.0, 160.0, 320.0),
        ),
    )
}

/// [`portal_store_shown`] with the portal's [`PORTAL_EXTRA`] object and
/// the layout's [`TASK_SETTING`] object given.
fn portal_store_with(flags: u64, body_display: u64, extra: Vec<u8>, setting: Vec<u8>) -> Vec<u8> {
    let mut store = Vec::new();
    uint(1, PORTAL, &mut store);
    uint(1, TASK_NAME, &mut store);
    uint(1, TASK_CODE, &mut store);
    uint(1, TASK_SETTING, &mut store);
    bytes(
        2,
        &portal_object(flags, rect(60.0, 10.0, 130.0, 410.0)),
        &mut store,
    );
    bytes(
        2,
        &shown_field_object(
            PORTAL_BODY,
            NOTES_OCCURRENCE,
            BODY,
            rect(0.0, 0.0, 20.0, 300.0),
            body_display,
        ),
        &mut store,
    );
    bytes(2, &extra, &mut store);
    bytes(
        2,
        &button_object(
            ROW_BUTTON,
            // Relative to the portal, between Notes::Body and Notes::Extra
            // (its scroll bar is at the right).
            rect(2.0, 301.0, 18.0, 309.0),
            &fixture::perform_script(ROW_BUTTON_SCRIPT),
        ),
        &mut store,
    );
    bytes(
        2,
        &field_object(TASK_NAME, OCCURRENCE, NAME, rect(20.0, 10.0, 40.0, 210.0)),
        &mut store,
    );
    bytes(
        2,
        &field_object(TASK_CODE, OCCURRENCE, CODE, rect(140.0, 10.0, 160.0, 110.0)),
        &mut store,
    );
    bytes(2, &setting, &mut store);
    store
}

/// The Portals layout's objects with the portal alone: none of the
/// layout's own fields.
fn portal_only_store() -> Vec<u8> {
    let mut store = Vec::new();
    uint(1, PORTAL, &mut store);
    bytes(
        2,
        &portal_object(PORTAL_FLAGS, rect(60.0, 10.0, 130.0, 410.0)),
        &mut store,
    );
    bytes(
        2,
        &shown_field_object(
            PORTAL_BODY,
            NOTES_OCCURRENCE,
            BODY,
            rect(0.0, 0.0, 20.0, 300.0),
            0,
        ),
        &mut store,
    );
    store
}

/// Notes 2 "first" and 3 "second" of "Write tests".
fn add_notes(root: &mut OwnedNode) {
    let table = uint_segment(NOTES);
    let mut list = root
        .at(&[&table, &[0x0d]])
        .and_then(|n| RidList::from_owned(n).ok())
        .unwrap_or_default();
    for (id, body) in [(2u64, "first"), (3, "second")] {
        let record = serial::encode(id);
        for (field, text) in [(BODY, body), (TASK, "Write tests")] {
            root.put(
                &[&table, &[5], &record, &uint_segment(field)],
                text_node(&[], text),
            );
        }
        list.insert(id);
    }
    root.put(&[&table, &[0x0d]], list.to_node(Vec::new()));
    let last = serial::encode(3);
    let mut key = vec![last.len() as u8];
    key.extend(last);
    root.put(&[&table, &[0]], OwnedNode::leaf(Vec::new(), key));
}

/// Relationship 1 allows creating Notes from Tasks (option `0x10`,
/// file-metadata.md 4.1).
fn allow_creation(root: &mut OwnedNode) {
    let path: [&[u8]; 5] = [&[3], &[0xfb], &[5], &[1], &[2]];
    let mut definition = root.at(&path).and_then(|n| n.value.clone()).unwrap();
    let at = definition.len() - 4;
    definition[at..].copy_from_slice(&0x10u32.to_be_bytes());
    root.put(&path, OwnedNode::leaf(Vec::new(), definition));
}

/// The key Notes::Extra has in the catalog: its id's key segment read
/// big-endian (file-metadata.md 1), not 200.
fn extra_raw() -> u64 {
    serial::raw_of_id(EXTRA).unwrap()
}

/// Notes::Extra, text, two repetitions.
fn add_extra(root: &mut OwnedNode) {
    fixture::add_field(root, NOTES, extra_raw(), "Extra", 1, 1, 0, 0, 0);
    fixture::set_field_repetitions(root, NOTES, extra_raw(), 2);
}

/// Occurrence "Other tasks" of Tasks, related by `Tasks::Name ≠ Other
/// tasks::Name` (relationship 4, no creation): record 1's is record 2.
fn add_other_tasks(root: &mut OwnedNode) {
    fixture::add_occurrence(root, OTHER_TASKS, "Other tasks", TABLE);
    fixture::add_relationship(root, 4, (OCCURRENCE, NAME), 1, (OTHER_TASKS, NAME));
}

/// The script the Portals layout's triggers of [`add_layout_trigger`]
/// run: it counts each event in `$$<event>` and returns `$$allow_<event>`
/// (empty: True).
const COUNT: u64 = 40;
/// OnWindowClose's script in the window-close test: it lets the first
/// window close and keeps the second.
const CLOSE_GATE: u64 = 48;

/// A trigger of the Portals layout itself (its `KScriptTriggers` cache,
/// file-metadata.md 7) running [`COUNT`] with the event's name as its
/// parameter.
fn add_layout_trigger(root: &mut OwnedNode, event: TriggerEvent, name: &str) {
    add_layout_trigger_script(root, event, COUNT, name);
}

/// [`add_layout_trigger`] running `script`.
fn add_layout_trigger_script(root: &mut OwnedNode, event: TriggerEvent, script: u64, name: &str) {
    let id = event.id();
    let mut trigger = om_layout::ScriptTrigger {
        event: id,
        script: Some(script),
        file_reference: None,
        external: false,
        modes: 7,
        option: 0,
        extra: Vec::new(),
        parameter: None,
    };
    let compiled = calc(&format!("\"{name}\""));
    trigger.parameter = Some(om_layout::triggers::StoredFormula {
        context: None,
        polish: compiled.polish.unwrap(),
    });
    let layout = uint_segment(PORTALS);
    let event = uint_segment(u64::from(id));
    let cache: [&[u8]; 4] = [&[4], &[5], &layout, &[0x30]];
    let count = root
        .at(&[cache[0], cache[1], cache[2], cache[3], &[4]])
        .and_then(|n| n.value.clone())
        .map_or(0, |v| u16::from_be_bytes([v[0], v[1]]));
    root.put(
        &[cache[0], cache[1], cache[2], cache[3], &[4]],
        OwnedNode::leaf(Vec::new(), (count + 1).to_be_bytes().to_vec()),
    );
    root.put(
        &[cache[0], cache[1], cache[2], cache[3], &[5], &event, &[1]],
        OwnedNode::leaf(Vec::new(), om_layout::triggers::encode_record(&trigger)),
    );
    if let Some(parameter) = &trigger.parameter {
        root.put(
            &[
                cache[0],
                cache[1],
                cache[2],
                cache[3],
                &[5],
                &event,
                &[0x31],
                &[6],
            ],
            OwnedNode::leaf(Vec::new(), parameter.polish.clone()),
        );
    }
}

/// OnObjectExit of the portal's Body field runs [`KEEP`], whose result
/// (`$$allow`) lets the focus leave or keeps it.
fn add_exit_trigger(root: &mut OwnedNode) {
    add_object_exit_trigger(root, PORTAL_BODY, KEEP);
}

/// OnObjectExit of Tasks::Code runs [`CODE_EXIT`].
fn add_code_exit_trigger(root: &mut OwnedNode) {
    add_object_exit_trigger(root, TASK_CODE, CODE_EXIT);
}

/// OnObjectExit of layout object `object` runs `script`.
fn add_object_exit_trigger(root: &mut OwnedNode, object: u64, script: u64) {
    add_object_triggers(root, object, &[(TriggerEvent::ObjectExit, script)]);
}

/// Layout object `object`'s triggers: each event runs its script.
fn add_object_triggers(root: &mut OwnedNode, object: u64, triggers: &[(TriggerEvent, u64)]) {
    let (layout, object) = (uint_segment(PORTALS), serial::encode(object));
    let cache: [&[u8]; 6] = [&[4], &[5], &layout, &[5], &object, &[0x30]];
    let mut count = cache.to_vec();
    count.push(&[4]);
    root.put(
        &count,
        OwnedNode::leaf(Vec::new(), (triggers.len() as u16).to_be_bytes().to_vec()),
    );
    for (event, script) in triggers {
        let event = event.id();
        let record = om_layout::triggers::encode_record(&om_layout::ScriptTrigger {
            event,
            script: Some(*script),
            file_reference: None,
            external: false,
            modes: 1,
            option: 0,
            extra: Vec::new(),
            parameter: None,
        });
        let event = uint_segment(u64::from(event));
        let mut entry = cache.to_vec();
        entry.extend([&[5][..], &event, &[1]]);
        root.put(&entry, OwnedNode::leaf(Vec::new(), record));
    }
}

fn field_spec(occurrence: u64, field: u64, repetition: i16) -> FieldSpec {
    FieldSpec::new(
        StoredKey::from_id(occurrence).unwrap(),
        StoredKey::from_id(field).unwrap(),
        repetition,
    )
}

/// Set Field [occurrence::field[repetition]; text].
fn set_field(occurrence: u64, field: u64, repetition: i16, text: &str) -> ScriptStep {
    let mut step = ScriptStep::new(76);
    step.set_calculation(0, Some(&calc(text)));
    step.set_field_spec(0, Some(field_spec(occurrence, field, repetition)));
    step
}

/// Go to Portal Row [First 1 / Last 2 / Previous 3 / Next 4].
fn go_to_portal_row(choice: u16) -> ScriptStep {
    let mut step = ScriptStep::new(99);
    step.value = choice;
    step.flags |= flag::MENU_SELECTION | flag::NO_INTERACT;
    step
}

/// Go to Portal Row [By calculation: text].
fn go_to_portal_row_number(text: &str) -> ScriptStep {
    let mut step = go_to_portal_row(5);
    step.set_calculation(0, Some(&calc(text)));
    step
}

/// Go to Related Record [From table: Notes; Using layout: Notes], Show
/// only related records on or off (script-steps.md 11.8).
fn go_to_related_notes(only_related: bool) -> ScriptStep {
    let mut step = ScriptStep::new(74);
    step.value = 5;
    step.set_member_key(StoredKey::from_id(fixture::NOTES_LAYOUT));
    step.set_slot_child(
        0,
        0x11,
        Some(OwnedNode::leaf(
            [0x11],
            StoredKey::from_id(NOTES_OCCURRENCE).unwrap().0.to_vec(),
        )),
    );
    step.set_flag(flag::RESTORE, only_related);
    step
}

/// Go to Object [Object Name: text].
fn go_to_object(text: &str) -> ScriptStep {
    let mut step = ScriptStep::new(145);
    step.set_calculation(0, Some(&calc(text)));
    step
}

/// Go to Field [occurrence::field].
fn go_to_field(occurrence: u64, field: u64) -> ScriptStep {
    let mut step = ScriptStep::new(17);
    step.set_field_spec(0, Some(field_spec(occurrence, field, 0)));
    step
}

/// `repetition:field:object:row:error` of the focus.
const FOCUS: &str = "Get ( ActiveRepetitionNumber ) & \":\" & Get ( ActiveFieldName ) & \":\" & \
                     Get ( ActiveLayoutObjectName ) & \":\" & Get ( ActivePortalRowNumber ) & \
                     \":\" & Get ( LastError )";

/// Go to Object [Object Name: text; Repetition: repetition].
fn go_to_object_repetition(text: &str, repetition: &str) -> ScriptStep {
    let mut step = go_to_object(text);
    step.set_calculation(1, Some(&calc(repetition)));
    step
}

/// New Window [ Name: name ].
fn new_window(name: &str) -> ScriptStep {
    let mut step = ScriptStep::new(122);
    step.set_calculation(0, Some(&calc(&format!("\"{name}\""))));
    step
}

/// Select Window [ Name: name ].
fn select_window(name: &str) -> ScriptStep {
    let mut step = ScriptStep::new(123);
    step.value = 1;
    step.flags |= flag::MENU_SELECTION;
    step.set_calculation(0, Some(&calc(&format!("\"{name}\""))));
    step
}

fn without_dialog(id: u16) -> ScriptStep {
    let mut step = ScriptStep::new(id);
    step.flags |= flag::NO_INTERACT;
    step
}

/// Set Variable [$name; text], the text in the Tasks context.
fn note(name: &str, text: &str) -> ScriptStep {
    let mut step = ScriptStep::new(141);
    step.set_text(0, Some(name));
    step.set_calculation(1, Some(&calc(text)));
    step
}

/// Show Custom Dialog [ "Portals" ; message ] with an OK button.
fn show(message: &str) -> ScriptStep {
    let mut step = ScriptStep::new(87);
    step.set_calculation(0, Some(&calc("\"Portals\"")));
    step.set_calculation(1, Some(&calc(message)));
    step.set_text(0, Some("OK"));
    step
}

/// `error:row:field:table:body` after the last step.
const STATE: &str = "Get ( LastError ) & \":\" & Get ( ActivePortalRowNumber ) & \":\" & \
                     Get ( ActiveFieldName ) & \":\" & Get ( ActiveFieldTableName ) & \":\" & \
                     Notes::Body";

/// The file's OnWindowTransaction script (script-runtime.md 13.26).
const WINDOW_TRANSACTION: u64 = 9;

/// The document preferences `[2, 18]` (file-metadata.md 3.1) with
/// OnWindowTransaction running script 9 and no field name.
fn add_window_transaction(root: &mut OwnedNode) {
    let empty = 0u8;
    let script = serial::encode(WINDOW_TRANSACTION);
    let mut key = vec![script.len() as u8 + 1, script.len() as u8];
    key.extend(script);
    let mut prefs = 0x403u32.to_be_bytes().to_vec();
    prefs.extend([0, 0, empty, empty, empty]);
    prefs.extend(1u16.to_be_bytes());
    prefs.extend(0u16.to_be_bytes());
    prefs.extend(1u16.to_be_bytes());
    prefs.extend(512u32.to_be_bytes());
    prefs.extend([empty, empty]);
    prefs.extend([0, 0, 0, 0]);
    prefs.push(empty);
    prefs.extend(100u16.to_be_bytes());
    prefs.extend(1u16.to_be_bytes());
    prefs.extend(key);
    prefs.push(0);
    root.put(&[&[2], &[0x18]], OwnedNode::leaf(Vec::new(), prefs));
}

/// `error:found:record:body` on the Notes layout.
const RELATED: &str = "Get ( LastError ) & \":\" & Get ( FoundCount ) & \":\" & \
                       Get ( RecordNumber ) & \":\" & Notes::Body";

fn scripts() -> Vec<Script> {
    vec![
        fixture::script(
            1,
            "Rows",
            vec![
                fixture::on(86, true),
                fixture::go_to_layout(PORTALS),
                fixture::go_to_record(1),
                note("$none", "Get ( ActivePortalRowNumber )"),
                go_to_portal_row(1),
                note("$first", STATE),
                note("$rep", "Get ( ActiveRepetitionNumber )"),
                note(
                    "$object",
                    "Get ( ActiveLayoutObjectName ) & \"=\" & Get ( ActiveFieldContents )",
                ),
                go_to_portal_row(4),
                note("$next", STATE),
                go_to_portal_row(4),
                note("$creation", STATE),
                go_to_portal_row(4),
                note("$past", STATE),
                go_to_portal_row_number("9"),
                note("$nine", STATE),
                go_to_portal_row_number("1"),
                note("$one", STATE),
                go_to_object("\"no such\""),
                note("$missing", STATE),
                go_to_object("\"Notes portal\""),
                note("$portal", STATE),
                note("$rep", "$rep & \"/\" & Get ( ActiveRepetitionNumber )"),
                note(
                    "$object",
                    "$object & \"/\" & Get ( ActiveLayoutObjectName ) & \"=\" & \
                     Get ( ActiveFieldContents )",
                ),
                go_to_portal_row_number("2"),
                set_field(NOTES_OCCURRENCE, BODY, 0, "\"changed\""),
                note("$set", "Get ( LastError ) & \":\" & Notes::Body"),
                without_dialog(75),
                note(
                    "$commit",
                    "Get ( LastError ) & \":\" & Get ( ActivePortalRowNumber ) & \":\" & \
                     Substitute ( List ( Notes::Body ) ; ¶ ; \"/\" )",
                ),
                show(
                    "\"none \" & $none & \", first \" & $first & \", next \" & $next & \
                     \", creation \" & $creation & \", past \" & $past & \", nine \" & $nine & \
                     \", one \" & $one & \", missing \" & $missing & \", portal \" & $portal & \
                     \", set \" & $set & \", commit \" & $commit & \", rep \" & $rep & \
                     \", object \" & $object",
                ),
            ],
        ),
        fixture::script(
            2,
            "Create",
            vec![
                fixture::on(86, true),
                fixture::go_to_layout(PORTALS),
                fixture::go_to_record(2),
                set_field(NOTES_OCCURRENCE, BODY, 0, "\"new note\""),
                note("$set", "Get ( LastError ) & \":\" & Count ( Notes::Body )"),
                without_dialog(75),
                note(
                    "$commit",
                    "Get ( LastError ) & \":\" & List ( Notes::Body ) & \":\" & \
                     List ( Notes::Task )",
                ),
                show("\"set \" & $set & \", commit \" & $commit"),
            ],
        ),
        fixture::script(
            3,
            "Delete",
            vec![
                fixture::on(86, true),
                fixture::go_to_layout(PORTALS),
                fixture::go_to_record(1),
                without_dialog(104),
                note(
                    "$nothing",
                    "Get ( LastError ) & \":\" & Count ( Notes::Body )",
                ),
                go_to_object("\"Note body\""),
                note("$object", STATE),
                without_dialog(104),
                note(
                    "$deleted",
                    "Get ( LastError ) & \":\" & Get ( ActivePortalRowNumber ) & \":\" & \
                     Substitute ( List ( Notes::Body ) ; ¶ ; \"/\" )",
                ),
                without_dialog(75),
                note(
                    "$commit",
                    "Get ( LastError ) & \":\" & Count ( Notes::Body )",
                ),
                show(
                    "\"nothing \" & $nothing & \", object \" & $object & \", deleted \" & \
                     $deleted & \", commit \" & $commit",
                ),
            ],
        ),
        fixture::script(
            4,
            "Invalid note",
            vec![
                fixture::on(86, true),
                fixture::go_to_layout(PORTALS),
                fixture::go_to_record(1),
                go_to_portal_row(1),
                set_field(NOTES_OCCURRENCE, BODY, 0, "\"far too long for it\""),
                note("$set", "Get ( LastError )"),
                without_dialog(75),
                note("$commit", "Get ( LastError )"),
                without_dialog(51),
                note("$after", "Substitute ( List ( Notes::Body ) ; ¶ ; \"/\" )"),
                show("\"set \" & $set & \", commit \" & $commit & \", after \" & $after"),
            ],
        ),
        fixture::script(
            6,
            "Commits",
            vec![
                fixture::on(86, true),
                fixture::go_to_layout(PORTALS),
                fixture::go_to_record(1),
                set_field(OCCURRENCE, NAME, 0, "\"Write tests\""),
                without_dialog(75),
                note("$$after", "\"seen \" & $$n"),
                go_to_portal_row(1),
                set_field(NOTES_OCCURRENCE, BODY, 0, "\"edited\""),
                without_dialog(75),
                go_to_portal_row_number("2"),
                without_dialog(104),
                note("$$deleted", "Get ( LastError )"),
                without_dialog(75),
                note("$$committed", "Get ( LastError )"),
                without_dialog(75),
            ],
        ),
        fixture::script(
            8,
            "Related",
            vec![
                fixture::on(86, true),
                // The window's Notes found set: "Hi" and "first".
                fixture::go_to_layout(fixture::NOTES_LAYOUT),
                fixture::go_to_record(2),
                without_dialog(25),
                fixture::go_to_layout(PORTALS),
                fixture::go_to_record(1),
                go_to_portal_row(1),
                go_to_related_notes(false),
                note("$off", RELATED),
                fixture::go_to_layout(PORTALS),
                go_to_portal_row_number("2"),
                go_to_related_notes(true),
                note("$on", RELATED),
                show("\"off \" & $off & \", on \" & $on"),
            ],
        ),
        fixture::script(
            WINDOW_TRANSACTION,
            "On transaction",
            vec![
                note("$$n", "$$n + 1"),
                note(
                    "$$tx",
                    "$$tx & Substitute ( Substitute ( Get ( ScriptParameter ) ; ¶ ; \"\" ) ; \
                     Char ( 9 ) ; \"\" ) & \"|\"",
                ),
            ],
        ),
        fixture::script(
            7,
            "Show transactions",
            vec![show(
                "$$after & \", \" & $$deleted & \":\" & $$committed & \", \" & $$n & \" \" & $$tx",
            )],
        ),
        fixture::script(
            10,
            "Leave",
            vec![
                fixture::on(86, true),
                fixture::go_to_layout(PORTALS),
                fixture::go_to_record(1),
                set_field(OCCURRENCE, CODE, 0, "\"a\""),
                set_field(OCCURRENCE, STATUS, 2, "\"ab\""),
                // The first related record (no active row).
                set_field(NOTES_OCCURRENCE, BODY, 0, "\"pending\""),
                show("\"left \" & Get ( LastError )"),
            ],
        ),
        fixture::script(
            11,
            "Status",
            vec![
                fixture::on(86, true),
                set_field(OCCURRENCE, STATUS, 3, "\"x\""),
                without_dialog(75),
                show(
                    "\"status \" & Get ( LastError ) & \":\" & Tasks::Code & \":\" & \
                     Tasks::Status[2] & \":\" & Substitute ( List ( Notes::Body ) ; ¶ ; \"/\" ) & \
                     \" tx \" & $$n",
                ),
            ],
        ),
        fixture::script(
            13,
            "Sorted",
            vec![
                fixture::on(86, true),
                fixture::go_to_layout(PORTALS),
                fixture::go_to_record(1),
                go_to_portal_row(1),
                note(
                    "$row",
                    "Get ( LastError ) & \":\" & Get ( ActivePortalRowNumber ) & \":\" & \
                     Notes::Body",
                ),
                set_field(NOTES_OCCURRENCE, BODY, 0, "\"changed\""),
                note("$set", "Get ( LastError )"),
                without_dialog(104),
                note("$delete", "Get ( LastError )"),
                go_to_related_notes(true),
                note("$related", "Get ( LastError ) & \":\" & Get ( LayoutName )"),
                without_dialog(75),
                note(
                    "$commit",
                    "Get ( LastError ) & \":\" & Substitute ( List ( Notes::Body ) ; ¶ ; \"/\" )",
                ),
                show(
                    "\"sorted row \" & $row & \", set \" & $set & \", delete \" & $delete & \
                     \", related \" & $related & \", commit \" & $commit",
                ),
            ],
        ),
        fixture::script(
            14,
            "Field off",
            vec![
                fixture::on(86, true),
                fixture::go_to_layout(PORTALS),
                fixture::go_to_record(1),
                go_to_portal_row(1),
                set_field(NOTES_OCCURRENCE, BODY, 0, "\"edited\""),
                // Go to Field [ ]: no field.
                ScriptStep::new(17),
                show(
                    "\"off \" & Get ( LastError ) & \":\" & Get ( ActivePortalRowNumber ) & \
                     \":\" & Get ( ActiveFieldName ) & \":\" & Get ( RecordOpenState )",
                ),
            ],
        ),
        fixture::script(
            15,
            "Extra",
            vec![
                fixture::on(86, true),
                fixture::go_to_layout(PORTALS),
                fixture::go_to_record(1),
                go_to_object_repetition("\"Note extra\"", "2"),
                show(
                    "\"extra \" & Get ( LastError ) & \":\" & Get ( ActiveLayoutObjectName ) & \
                     \":\" & Get ( ActivePortalRowNumber )",
                ),
            ],
        ),
        fixture::script(
            25,
            "Repetition only",
            vec![
                fixture::on(86, true),
                fixture::go_to_layout(PORTALS),
                fixture::go_to_record(1),
                set_field(OCCURRENCE, STATUS, 2, "\"ab\""),
                show(
                    "\"reps \" & Get ( RecordOpenState ) & \":\" & Get ( RecordOpenCount ) & \
                     \":\" & Get ( ModifiedFields ) & \":\" & Tasks::Status[2]",
                ),
            ],
        ),
        fixture::script(
            26,
            "Extra two",
            vec![
                fixture::on(86, true),
                fixture::go_to_layout(PORTALS),
                fixture::go_to_record(1),
                // The first related note's two repetitions, committed
                // (the step names the field by its catalog key).
                set_field(NOTES_OCCURRENCE, extra_raw(), 1, "\"one\""),
                set_field(NOTES_OCCURRENCE, extra_raw(), 2, "\"two\""),
                without_dialog(75),
                go_to_object_repetition("\"Note extra\"", "2"),
                show(
                    "\"extra2 \" & Get ( LastError ) & \":\" & Get ( ActivePortalRowNumber ) & \
                     \":\" & Get ( ActiveRepetitionNumber ) & \":\" & Get ( ActiveFieldContents )",
                ),
            ],
        ),
        fixture::script(
            27,
            "Leave extra",
            vec![
                fixture::on(86, true),
                fixture::go_to_layout(PORTALS),
                fixture::go_to_record(1),
                go_to_object_repetition("\"Note extra\"", "2"),
                note("$portal", FOCUS),
                // To Tasks::Name by Go to Object, then by Go to Field.
                go_to_object("\"Task name\""),
                note("$object", FOCUS),
                go_to_object_repetition("\"Note extra\"", "2"),
                go_to_field(OCCURRENCE, NAME),
                note("$field", FOCUS),
                show("\"leave \" & $portal & \" | \" & $object & \" | \" & $field"),
            ],
        ),
        fixture::script(
            16,
            "Other table",
            vec![
                fixture::on(86, true),
                fixture::go_to_layout(PORTALS),
                fixture::go_to_record(1),
                go_to_portal_row(1),
                without_dialog(104),
                set_field(OTHER_TASKS, CODE, 0, "\"zz\""),
                note("$set", "Get ( LastError )"),
                without_dialog(75),
                show("\"other set \" & $set & \", commit \" & Get ( LastError )"),
            ],
        ),
        fixture::script(
            17,
            "Veto",
            vec![
                fixture::on(86, true),
                fixture::go_to_layout(PORTALS),
                fixture::go_to_record(1),
                note("$$allow", "0"),
                go_to_portal_row(1),
                go_to_portal_row(4),
                note("$veto", "Get ( ActivePortalRowNumber )"),
                note("$$allow", "1"),
                go_to_portal_row(4),
                note("$allowed", "Get ( ActivePortalRowNumber )"),
                show("\"veto \" & $veto & \", allowed \" & $allowed"),
            ],
        ),
        fixture::script(KEEP, "Keep", vec![fixture::exit_with("$$allow")]),
        fixture::script(
            ROW_BUTTON_SCRIPT,
            "Row button",
            vec![
                note("$p", "Get ( ScriptParameter )"),
                note(
                    "$row",
                    "Get ( ActivePortalRowNumber ) & \":\" & Notes::Body & \":\" & Tasks::Name",
                ),
                // The clicked row's record, not the first related one
                // (Body takes at most 10 characters).
                set_field(NOTES_OCCURRENCE, BODY, 0, "Notes::Body & \"!\""),
                without_dialog(75),
                show(
                    "\"row \" & $p & \"|\" & $row & \"|\" & Get ( LastError ) & \":\" & \
                     Tasks::Code",
                ),
            ],
        ),
        fixture::script(
            CODE_EXIT,
            "Code exit",
            vec![
                fixture::with_calc(68, "$$mode = \"veto\""),
                // The clicked row is not active before the button runs.
                show("\"vetoed \" & Get ( ActivePortalRowNumber )"),
                fixture::exit_with("0"),
                ScriptStep::new(70),
                fixture::with_calc(68, "$$mode = \"reorder\""),
                // No active row: the first related note ("first").
                set_field(NOTES_OCCURRENCE, BODY, 0, "\"zzz\""),
                without_dialog(75),
                ScriptStep::new(70),
                fixture::exit_with("1"),
            ],
        ),
        fixture::script(54, "Mode veto", vec![note("$$mode", "\"veto\"")]),
        fixture::script(55, "Mode reorder", vec![note("$$mode", "\"reorder\"")]),
        fixture::script(
            COUNT,
            "Count",
            vec![
                note(
                    "$event",
                    "Get ( ScriptParameter )",
                ),
                note(
                    "$$events",
                    "$$events & $event & \";\"",
                ),
                fixture::exit_with(
                    "Case ( $event = \"commit\" ; $$allow_commit ; $event = \"exit\" ; \
                     $$allow_exit ; $event = \"mode\" ; $$allow_mode ; \"\" ) ≠ \"0\"",
                ),
            ],
        ),
        dirty_windows(45, "Dirty windows", "w2"),
        // Exit Application.
        fixture::script(51, "Exit app", vec![ScriptStep::new(44)]),
        fixture::script(
            49,
            "Pause left",
            vec![
                fixture::on(86, true),
                fixture::go_to_layout(PORTALS),
                fixture::go_to_record(1),
                set_field(OCCURRENCE, CODE, 0, "\"p\""),
                // Pause/Resume Script [Indefinitely].
                ScriptStep::new(62),
                show("\"resumed \" & Get ( RecordNumber ) & \":\" & Tasks::Code"),
            ],
        ),
        fixture::script(
            50,
            "Pause here",
            vec![
                fixture::on(86, true),
                fixture::go_to_layout(PORTALS),
                fixture::go_to_record(1),
                ScriptStep::new(62),
                show("\"resumed \" & Get ( RecordNumber ) & \":\" & Tasks::Code"),
            ],
        ),
        fixture::script(
            CLOSE_GATE,
            "Close gate",
            vec![
                note("$$closes", "$$closes + 1"),
                note("$$events", "$$events & Get ( ScriptParameter ) & \";\""),
                fixture::exit_with("$$closes < 2"),
            ],
        ),
        dirty_windows(46, "Dirty refusal", "toolong"),
        fixture::script(
            47,
            "Restyle",
            vec![
                {
                    let mut title = ScriptStep::new(124);
                    title.set_calculation(1, Some(&calc("\"Renamed\"")));
                    title
                },
                {
                    let mut hide = ScriptStep::new(29);
                    hide.value = 2;
                    hide.flags |= flag::MENU_SELECTION;
                    hide
                },
            ],
        ),
        fixture::script(
            43,
            "Reset events",
            vec![note("$$events", "\"\""), note("$$n", "\"\"")],
        ),
        fixture::script(
            44,
            "Show events",
            vec![show("\"events \" & $$events & \" tx \" & $$n")],
        ),
        fixture::script(
            41,
            "Two portal windows",
            vec![
                fixture::on(86, true),
                fixture::go_to_layout(PORTALS),
                fixture::go_to_record(1),
                new_window("W2"),
                without_dialog(25),
                select_window("Tasks"),
                set_field(OCCURRENCE, CODE, 0, "\"w1\""),
                select_window("W2"),
            ],
        ),
        fixture::script(
            42,
            "Where",
            vec![
                fixture::on(86, true),
                select_window("Tasks"),
                note(
                    "$w1",
                    "Get ( RecordNumber ) & \"/\" & Get ( FoundCount ) & \"/\" & Tasks::Name",
                ),
                select_window("W2"),
                note(
                    "$w2",
                    "Get ( RecordNumber ) & \"/\" & Get ( FoundCount ) & \"/\" & Tasks::Name",
                ),
                show("\"where \" & $w1 & \" \" & $w2 & \" events \" & $$events"),
            ],
        ),
        fixture::script(31, "Mode gate", vec![note("$$exits", "$$exits + 1"), fixture::exit_with("$$allowExit")]),
        fixture::script(32, "Allow mode", vec![note("$$allow", "1"), note("$$allowExit", "1")]),
        fixture::script(33, "Refuse mode", vec![note("$$allow", "1"), note("$$allowExit", "0")]),
        fixture::script(34, "Show gates", vec![show("\"gates \" & $$commits & \":\" & $$exits")]),
        fixture::script(35, "Leave new", vec![
            fixture::go_to_layout(PORTALS), without_dialog(7),
            set_field(OCCURRENCE, NAME, 0, "\"New task\""),
            set_field(OCCURRENCE, STATUS, 2, "\"new\""),
        ]),
        // A record held in Tasks' window while W2 opens a transaction, and
        // the transaction committed from Tasks (#892).
        fixture::script(96, "Apart from a transaction", vec![
            fixture::on(86, true),
            fixture::go_to_layout(PORTALS),
            new_window("W2"),
            select_window("Tasks"),
            without_dialog(7),
            set_field(OCCURRENCE, NAME, 0, "\"Apart\""),
            select_window("W2"),
            ScriptStep::new(205),
            select_window("Tasks"),
            ScriptStep::new(206),
            note("$e", "Get ( LastError )"),
            show("\"apart \" & $e & \":\" & Tasks::Name"),
        ]),
        // A stored calculation read before the commit, over the record's
        // unsaved change of its input (#1104).
        fixture::script(99, "Doubled before commit", vec![
            fixture::go_to_layout(PORTALS),
            without_dialog(7),
            set_field(OCCURRENCE, fixture::COUNT, 0, "4"),
            show("\"doubled \" & Tasks::Doubled"),
        ]),
        // A new record whose commit has only a related record to write
        // (#931).
        fixture::script(64, "New with a note", vec![
            fixture::go_to_layout(PORTALS), without_dialog(7),
            set_field(NOTES_OCCURRENCE, BODY, 0, "\"new note\""),
            without_dialog(75),
        ]),
        fixture::script(30, "Delete then edit first", vec![
            fixture::on(86, true),
            fixture::go_to_layout(PORTALS),
            fixture::go_to_record(1),
            go_to_portal_row(1),
            without_dialog(104),
            note("$first", "Notes::Body"),
            note("$list", "List ( Notes::Body )"),
            set_field(NOTES_OCCURRENCE, BODY, 0, "\"after\""),
            note("$written", "Get ( LastError ) & \":\" & Notes::Body"),
            without_dialog(75),
            show("\"first \" & $first & \", list \" & $list & \", written \" & $written & \", count \" & Count ( Notes::Body )"),
        ]),
        fixture::script(
            21,
            "Commit gate",
            vec![
                note("$$commits", "$$commits + 1"),
                fixture::exit_with("$$allow"),
            ],
        ),
        fixture::script(22, "Allow commit", vec![note("$$allow", "1")]),
        fixture::script(23, "Refuse commit", vec![note("$$allow", "0")]),
        fixture::script(24, "Show commits", vec![show("\"commits \" & $$commits")]),
        fixture::script(
            19,
            "Show portals",
            vec![
                fixture::go_to_layout(PORTALS),
                fixture::go_to_record(1),
                show("\"on \" & Get ( LayoutName )"),
            ],
        ),
        fixture::script(
            20,
            "Second window",
            vec![
                fixture::on(86, true),
                fixture::go_to_layout(PORTALS),
                fixture::go_to_record(1),
                new_window("W2"),
                set_field(OCCURRENCE, CODE, 0, "\"w2\""),
                without_dialog(75),
                select_window("Tasks"),
                show("\"back \" & Get ( LastError ) & \":\" & Get ( WindowName )"),
            ],
        ),
        fixture::script(
            60,
            "Quick find",
            vec![
                fixture::on(86, true),
                fixture::go_to_layout(PORTALS),
                fixture::with_calc(150, "\"tests second\""),
                note("$both", QUICK_FOUND),
                fixture::with_calc(150, "\"ship first\""),
                note("$none", QUICK_FOUND),
                set_field(OCCURRENCE, SETTING, 0, "\"zebra\""),
                fixture::with_calc(150, "\"zebra\""),
                note("$global", QUICK_FOUND),
                fixture::with_calc(150, "\"ship\""),
                note("$ship", QUICK_FOUND),
                fixture::with_calc(150, "\" \""),
                note("$empty", "Get ( LastError )"),
                // Both tasks hold "zz": on the second, a Quick Find for it
                // keeps the second current.
                ScriptStep::new(23),
                fixture::go_to_record(1),
                set_field(OCCURRENCE, CODE, 0, "\"zz\""),
                fixture::go_to_record(2),
                set_field(OCCURRENCE, CODE, 0, "\"zz\""),
                fixture::with_calc(150, "\"zz\""),
                note(
                    "$kept",
                    "Get ( FoundCount ) & \":\" & Get ( RecordNumber ) & \":\" & Tasks::Name",
                ),
                show(
                    "\"quick \" & $both & \", none \" & $none & \", global \" & $global & \
                     \", ship \" & $ship & \", empty \" & $empty & \", kept \" & $kept",
                ),
            ],
        ),
        fixture::script(
            12,
            "Read setting",
            vec![show("\"setting \" & Tasks::Setting & \":\" & Tasks::Code")],
        ),
        fixture::script(
            5,
            "Script rules",
            vec![
                fixture::on(86, true),
                fixture::go_to_layout(PORTALS),
                fixture::go_to_record(1),
                set_field(OCCURRENCE, CREATOR, 0, "\"Script\""),
                note("$creator", "Get ( LastError )"),
                set_field(OCCURRENCE, STATUS, 0, "\"toolong\""),
                note("$long", "Get ( LastError ) & \":\" & Tasks::Status"),
                set_field(OCCURRENCE, STATUS, 2, "\"ok\""),
                note("$second", "Get ( LastError ) & \":\" & Tasks::Status[2]"),
                note(
                    "$modified",
                    "Substitute ( Get ( ModifiedFields ) ; ¶ ; \"/\" )",
                ),
                without_dialog(75),
                note(
                    "$commit",
                    "Get ( LastError ) & \":\" & Tasks::Creator & \":\" & Tasks::Status[2]",
                ),
                show(
                    "\"creator \" & $creator & \", long \" & $long & \", second \" & $second & \
                     \", modified \" & $modified & \", commit \" & $commit",
                ),
            ],
        ),
        // The starter solutions' add-row buttons (Meetings' Add Topic).
        fixture::script(
            61,
            "Add note",
            vec![
                go_to_object("\"Notes portal\""),
                go_to_portal_row(2),
                go_to_field(NOTES_OCCURRENCE, BODY),
            ],
        ),
        // The portal Body field's object triggers (`add_object_triggers`),
        // run by the user's edit, not in the Scripts menu.
        hidden(fixture::script(
            BODY_ENTER,
            "Body entered",
            vec![note(
                "$$enter",
                &format!("$$enter & \"[\" & {OBJECT_STATE} & \"]\""),
            )],
        )),
        // A step that leaves a field: its triggers' scripts (#1030).
        hidden(fixture::script(
            BODY_COMMIT,
            "Commit the row",
            vec![without_dialog(75)],
        )),
        hidden(fixture::script(
            BODY_LOG,
            "Log the row",
            vec![set_field(OCCURRENCE, fixture::COUNT, 0, "77")],
        )),
        hidden(fixture::script(
            UPPER_STATUS,
            "Upper status",
            vec![set_field(OCCURRENCE, STATUS, 2, "Upper ( Tasks::Status[2] )")],
        )),
        hidden(fixture::script(
            NOTE_GLOBAL,
            "Note global",
            vec![set_field(NOTES_OCCURRENCE, STATUS, 2, "\"g\"")],
        )),
        hidden(fixture::script(
            LEAVE_AND_RETURN,
            "Leave and return",
            vec![
                go_to_field(OCCURRENCE, CODE),
                go_to_field(NOTES_OCCURRENCE, BODY),
                without_dialog(75),
            ],
        )),
        hidden(fixture::script(
            SAVE_NUMBER,
            "Body saved with its number",
            vec![note(
                "$$save",
                &format!(
                    "$$save & \"[\" & {OBJECT_STATE} & \":\" & GetField ( \"Notes::Number\" ) & \"]\""
                ),
            )],
        )),
        hidden(fixture::script(
            SAVE_NUMBER_REFUSED_ONCE,
            "Body saved with its number, refused once",
            vec![
                note(
                    "$$save",
                    &format!(
                        "$$save & \"[\" & {OBJECT_STATE} & \":\" & GetField ( \"Notes::Number\" ) & \"]\""
                    ),
                ),
                note("$$refused", "$$refused + 1"),
                fixture::exit_with("$$refused > 1"),
            ],
        )),
        hidden(fixture::script(
            SAVE_REFUSED_ONCE,
            "Body saved, refused once",
            vec![
                note(
                    "$$save",
                    &format!("$$save & \"[\" & {OBJECT_STATE} & \":\" & Notes::Body & \"]\""),
                ),
                note("$$refused", "$$refused + 1"),
                fixture::exit_with("$$refused > 1"),
            ],
        )),
        hidden(fixture::script(
            CODE_NORMALIZE,
            "Normalize the code",
            vec![set_field(OCCURRENCE, CODE, 0, "\"zz\"")],
        )),
        hidden(fixture::script(
            COMMIT_VETO,
            "Refuse the commit",
            vec![fixture::exit_with("False")],
        )),
        hidden(fixture::script(
            BODY_MODIFY,
            "Body modified",
            vec![note("$$modify", "$$modify + 1")],
        )),
        hidden(fixture::script(
            BODY_SAVE,
            "Body saved",
            // The row's related field, in its row's context: a stored
            // record's, and a begun one's (#1014).
            vec![note(
                "$$save",
                &format!("$$save & \"[\" & {OBJECT_STATE} & \":\" & Notes::Body & \"]\""),
            )],
        )),
        fixture::script(
            75,
            "Filtered",
            vec![
                fixture::on(86, true),
                fixture::go_to_layout(PORTALS),
                fixture::go_to_record(1),
                go_to_portal_row(1),
                note(
                    "$row",
                    "Get ( LastError ) & \":\" & Get ( ActivePortalRowNumber ) & \":\" & \
                     Notes::Body",
                ),
                go_to_related_notes(true),
                note(
                    "$related",
                    "Get ( LastError ) & \":\" & Get ( LayoutName ) & \":\" & Get ( FoundCount ) & \
                     \":\" & Notes::Body",
                ),
                show("\"filtered row \" & $row & \", related \" & $related"),
            ],
        ),
        fixture::script(
            74,
            "Object triggers",
            vec![show(
                "\"objects \" & $$enter & \"|\" & $$modify & \"|\" & $$save",
            )],
        ),
    ]
}

/// The scripts the portal's Body field object's triggers run.
const BODY_ENTER: u64 = 71;
const BODY_MODIFY: u64 = 72;
const BODY_SAVE: u64 = 73;
/// Commit Records [With dialog: Off], and Set Field [Tasks::Count; 77]:
/// a portal field's triggers when a step leaves it (#1030).
const BODY_COMMIT: u64 = 76;
const BODY_LOG: u64 = 77;
/// Set Field [Tasks::Code; "zz"], and Exit Script [False] (#1030).
const CODE_NORMALIZE: u64 = 78;
/// Go to Field [Tasks::Code], Go to Field [Notes::Body], Commit Records
/// [With dialog: Off] (#1040).
const LEAVE_AND_RETURN: u64 = 80;
/// [`BODY_SAVE`]'s record, then a refusal the first time it runs.
const SAVE_REFUSED_ONCE: u64 = 81;
/// [`BODY_SAVE`]'s record with the row's Notes::Number, read by name (the
/// field exists only in the files that add it).
const SAVE_NUMBER: u64 = 82;
/// [`SAVE_NUMBER`]'s record, then a refusal the first time it runs.
const SAVE_NUMBER_REFUSED_ONCE: u64 = 83;
const COMMIT_VETO: u64 = 79;
/// Set Field [Tasks::Status[2]; Upper ( Tasks::Status[2] )] (#185).
const UPPER_STATUS: u64 = 84;
/// Set Field [Notes::gStatus[2]; "g"]: a repeating global of Notes whose
/// field id is Tasks::Status's (#185).
const NOTE_GLOBAL: u64 = 85;
/// `field:row:contents` of the active field.
const OBJECT_STATE: &str =
    "Get ( ActiveFieldName ) & \":\" & Get ( ActivePortalRowNumber ) & \":\" & \
                            Get ( ActiveFieldContents )";

/// `script`, left out of the Scripts menu.
fn hidden(mut script: Script) -> Script {
    script.set_in_scripts_menu(false);
    script
}

/// What a Quick Find left: its error, the found count and the current
/// task.
const QUICK_FOUND: &str = "Get ( LastError ) & \":\" & Get ( FoundCount ) & \":\" & Tasks::Name";

/// The portal sorts its own rows by Notes::Body, descending (its
/// `KSortOrder`, key 10 of its attribute catalog, calc-encoding.md 8).
fn sort_portal(root: &mut OwnedNode) {
    let spec = SortSpec {
        keep_sorted: true,
        blanks_last: false,
        fields: vec![SortEntry {
            field: SpecField {
                occurrence: Some(NOTES_OCCURRENCE),
                field: BODY,
                repetition: 1,
            },
            summary: None,
            value_list: None,
            descending: true,
            locale: 0,
        }],
    };
    root.put(
        &[
            &[4],
            &[5],
            &uint_segment(PORTALS),
            &[5],
            &serial::encode(PORTAL),
            &[10],
        ],
        OwnedNode::leaf(Vec::new(), spec.encode()),
    );
}

/// The portal's filter `Notes::Body ≠ "first"` (object calculation 37,
/// layout-model.md 5.3): it shows note 3 ("second") only.
fn filter_portal(root: &mut OwnedNode) {
    portal_filter(root, "Notes::Body ≠ \"first\"");
}

/// A filter by row number, `Mod ( Get ( RecordNumber ) ; 2 ) = 1`: the
/// portal shows its odd rows, note 2 ("first") only of the two.
fn filter_portal_by_row(root: &mut OwnedNode) {
    portal_filter(root, "Mod ( Get ( RecordNumber ) ; 2 ) = 1");
}

/// The portal's filter calculation `formula`.
fn portal_filter(root: &mut OwnedNode, formula: &str) {
    root.put(
        &[
            &[4],
            &[5],
            &uint_segment(PORTALS),
            &[5],
            &serial::encode(PORTAL),
            &[37],
            &[6],
        ],
        OwnedNode::leaf(Vec::new(), calc(formula).polish.unwrap()),
    );
}

/// [`sort_portal`], the sort turned on for the window too (option `0x8`):
/// Browse shows "second" before "first".
fn display_sorted(root: &mut OwnedNode) {
    sort_portal(root);
    root.put(
        &[&[4], &[5], &uint_segment(PORTALS), &[7]],
        OwnedNode::leaf(Vec::new(), portal_store(PORTAL_FLAGS | 0x8)),
    );
}

/// The fixture's file with the portal layout, the notes and Status's
/// rule, hosted.
fn host(tag: &str) -> hosted::Hosted {
    host_with(tag, |_| {})
}

/// [`host`], the file changed by `change` first.
fn host_with(tag: &str, change: impl FnOnce(&mut OwnedNode)) -> hosted::Hosted {
    host_configured(tag, change, |server| server)
}

/// [`host_with`], the server changed by `configure` before it serves.
fn host_configured(
    tag: &str,
    change: impl FnOnce(&mut OwnedNode),
    configure: impl FnOnce(om_server::Server) -> om_server::Server,
) -> hosted::Hosted {
    let mut root = fixture::tree(&scripts(), true);
    fixture::add_calculated(&mut root);
    add_portal_layout(&mut root);
    add_notes(&mut root);
    add_extra(&mut root);
    allow_creation(&mut root);
    add_window_transaction(&mut root);
    // Status: at most 3 characters (0x200), validated Always (0x400), three
    // repetitions.
    fixture::set_field_validation(&mut root, TABLE, STATUS, 0x600);
    fixture::set_field_byte_limit(&mut root, TABLE, STATUS, 3);
    fixture::set_field_repetitions(&mut root, TABLE, STATUS, 3);
    change(&mut root);
    let file = fixture::TempFile::from_tree(&format!("gui-portals-{tag}"), &root);
    hosted::host_file_with(
        &format!("portals-{tag}"),
        Path::new(&file.0),
        fixture::FILE,
        (ACCOUNT, PASSWORD),
        configure,
    )
}

/// Open the hosted file in the window.
fn open(hosted: &hosted::Hosted) -> common::App {
    // Tall enough for the whole Scripts menu of this file's scripts.
    let mut app = common::harness_sized(Box::new(hosted.provider()), egui::vec2(1060.0, 1300.0));
    hosted.fill_form(&mut app);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "hosted file", |app| {
        assert!(app.state().error().is_none(), "native connection failed");
        app.state().is_settled()
    });
    app
}

/// Run `script`, wait for its dialog, check that it shows `expected`, and
/// close it.
fn expect(app: &mut common::App, script: &str, expected: &str) {
    // The root window's Scripts menu (the first, with several windows).
    app.query_all(
        By::new()
            .role(egui::accesskit::Role::Button)
            .label("Scripts"),
    )
    .next()
    .unwrap()
    .click();
    app.run_steps(2);
    common::click(app, script);
    expect_shown(app, expected);
}

/// Wait for the dialog a script shows, check that it shows `expected`,
/// and close it.
fn expect_shown(app: &mut common::App, expected: &str) {
    let start = expected.split(' ').next().unwrap_or_default().to_owned();
    let prefix = format!("{start} ");
    // As `common::wait_until`, saying on a timeout which dialog it was and
    // what the window shows instead: a script still running, or the
    // alert of one that failed (#1085).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        app.step();
        if app.query(By::new().label_contains(&prefix)).is_some() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for the dialog {expected:?}: a script running {}, the window's error {:?}",
            app.state().script_running(),
            app.state().error(),
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    app.run_steps(3);
    let shown = app
        .query(By::new().label_contains(&prefix))
        .map(|n| {
            let node = n.accesskit_node();
            node.label().or_else(|| node.value()).unwrap_or_default()
        })
        .unwrap_or_default();
    assert_eq!(shown, expected);
    common::click(app, "OK");
    common::wait_until(app, "the script to end", |app| {
        !app.state().script_running()
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

/// The records of `table` as another client reads them: id and the text
/// of each `(field, repetition)`.
fn stored(hosted: &hosted::Hosted, table: u64, cells: &[(u64, u32)]) -> Vec<(u64, Vec<String>)> {
    let mut other = hosted.opened();
    let ids = other
        .calc_data(&[CalcNeed::Records { table }])
        .unwrap()
        .tables[&table]
        .to_vec();
    let needs: Vec<CalcNeed> = ids
        .iter()
        .map(|&record| CalcNeed::Record { table, record })
        .collect();
    let data = other.calc_data(&needs).unwrap();
    let locale = om_data::calc::Locale::en_us();
    let cx = om_data::calc::Ctx::new(
        &locale,
        om_data::calc::Date::from_ymd(2026, 10, 29).unwrap(),
    );
    let rows = ids
        .iter()
        .map(|id| {
            let values = data.records[&(table, *id)].as_deref().unwrap();
            let texts = cells
                .iter()
                .map(|(field, repetition)| {
                    values
                        .get(*field, *repetition)
                        .map(|v| v.to_text(&cx).into_string())
                        .unwrap_or_default()
                })
                .collect();
            (*id, texts)
        })
        .collect();
    other.close();
    rows
}

#[test]
fn portal_rows_move_and_write_their_records() {
    let hosted = host("rows");
    let mut app = open(&hosted);
    expect(
        &mut app,
        "Rows",
        "none 0, first 0:1:Body:Notes:first, next 0:2:Body:Notes:second, \
         creation 0:3:Body:Notes:, past 101:3:Body:Notes:, nine 101:3:Body:Notes:, \
         one 0:1:Body:Notes:first, missing 116:0:::first, portal 0:0:::first, \
         set 0:changed, commit 0:0:first/changed, rep 1/0, \
         object Note body=first/Notes portal=",
    );
    assert_eq!(
        stored(&hosted, NOTES, &[(BODY, 1)]),
        [
            (1, vec!["Hi".to_owned()]),
            (2, vec!["first".to_owned()]),
            (3, vec!["changed".to_owned()])
        ]
    );
}

#[test]
fn quick_find_needs_every_word_in_a_searched_field() {
    let hosted = host("quick");
    let mut app = open(&hosted);
    expect(
        &mut app,
        "Quick find",
        "quick 0:1:Write tests, none 401:1:Write tests, global 401:1:Write tests, \
         ship 0:1:Ship, empty 400, kept 2:2:Ship",
    );
}

#[test]
fn set_field_creates_a_related_record_through_the_relationship() {
    let hosted = host("create");
    let mut app = open(&hosted);
    expect(&mut app, "Create", "set 0:0, commit 0:new note:Ship");
    let notes = stored(&hosted, NOTES, &[(BODY, 1), (TASK, 1)]);
    assert_eq!(
        notes.last().map(|(_, v)| v.clone()),
        Some(vec!["new note".to_owned(), "Ship".to_owned()])
    );
}

#[test]
fn delete_portal_row_deletes_the_active_rows_record() {
    let hosted = host("delete");
    let mut app = open(&hosted);
    expect(
        &mut app,
        "Delete",
        "nothing 3:2, object 0:1:Body:Notes:first, deleted 0:0:second, commit 0:1",
    );
    let ids: Vec<u64> = stored(&hosted, NOTES, &[])
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(ids, [1, 3]);
}

#[test]
fn a_related_records_validation_fails_the_commit() {
    let hosted = host("invalid");
    let mut app = open(&hosted);
    expect(
        &mut app,
        "Invalid note",
        "set 0, commit 507, after first/second",
    );
    assert_eq!(
        stored(&hosted, NOTES, &[(BODY, 1)])[1],
        (2, vec!["first".to_owned()])
    );
}

#[test]
fn script_writes_follow_a_scripts_rules() {
    let hosted = host("rules");
    let mut app = open(&hosted);
    expect(
        &mut app,
        "Script rules",
        "creator 0, long 511:, second 0:ok, modified Creator/Status, \
         commit 0:Script:ok",
    );
    assert_eq!(
        stored(&hosted, TABLE, &[(CREATOR, 1), (STATUS, 1), (STATUS, 2)])[0],
        (1, vec!["Script".to_owned(), String::new(), "ok".to_owned()])
    );
}

#[test]
fn window_transaction_runs_after_the_script_once_per_commit() {
    let hosted = host("transactions");
    let mut app = open(&hosted);
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Commits");
    common::wait_until(&mut app, "the scripts to end", |app| {
        !app.state().script_running()
    });
    // Commit Records with nothing changed (the last one) fires nothing;
    // the edit of the record, of its related record and the related
    // record's deletion fire once each, after the script ($$after: none
    // yet).
    expect(
        &mut app,
        "Show transactions",
        "seen , 0:0, 3 {\"Tasks\" : {\"Tasks\" : [[\"Modified\",1,\"\"]]}}|\
         {\"Tasks\" : {\"Notes\" : [[\"Modified\",2,\"\"]]}}|\
         {\"Tasks\" : {\"Notes\" : [[\"Deleted\",3,\"\"]]}}|",
    );
}

/// A new record whose commit writes only a related record is still in
/// the OnWindowTransaction it runs, as "New", with that record (#931).
#[test]
fn window_transaction_lists_a_new_record_that_wrote_only_a_related_record() {
    let hosted = host("transaction-new-related");
    let mut app = open(&hosted);
    run_without_dialog(&mut app, "New with a note");
    common::wait_until(&mut app, "the script to end", |app| {
        !app.state().script_running()
    });
    // One transaction: the note the commit created and task 3 itself.
    expect(
        &mut app,
        "Show transactions",
        ", :, 1 {\"Tasks\" : {\"Notes\" : [[\"New\",4,\"\"]],\"Tasks\" : [[\"New\",3,\"\"]]}}|",
    );
}

#[test]
fn go_to_related_record_starts_from_the_active_row() {
    let hosted = host("related");
    let mut app = open(&hosted);
    // Without "show only related" the window's Notes found set stays and
    // the row's record is current; with it the found set is the related
    // records, the row's record current (script-runtime.md 13.24).
    expect(&mut app, "Related", "off 0:2:2:first, on 0:2:2:second");
}

/// Wait until the window's record has nothing unsaved and no save runs.
fn wait_saved(app: &mut common::App) {
    common::wait_until(app, "the record saved", |app| {
        !common::unsaved(app) && !common::saving(app) && !app.state().script_running()
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

/// "Leave" sets Code, Status[2] and the first related note and ends
/// without committing: the window shows the record unsaved.
fn leave(app: &mut common::App) {
    expect(app, "Leave", "left 0");
    common::wait_until(app, "unsaved record changes", common::unsaved);
    common::wait_until(app, "the script's Code", |app| {
        common::find_text_field(app, "Code")
            .and_then(|f| f.value())
            .as_deref()
            == Some("a")
    });
}

/// Tasks 1's Code, Status[2] and Status[3], and the first note's body, as
/// another client reads them.
fn left_values(hosted: &hosted::Hosted) -> (Vec<String>, String) {
    let task = stored(hosted, TABLE, &[(CODE, 1), (STATUS, 2), (STATUS, 3)])
        .swap_remove(0)
        .1;
    let note = stored(hosted, NOTES, &[(BODY, 1)])
        .into_iter()
        .find(|(id, _)| *id == 2)
        .map(|(_, v)| v[0].clone())
        .unwrap_or_default();
    (task, note)
}

/// What scripts leave on a record belongs to the record, not to the
/// script (script-runtime.md, desktop notes "Writes"): the user's revert
/// drops it, and a later script's commit writes only its own change.
#[test]
fn the_users_revert_drops_what_scripts_left_on_the_record() {
    let hosted = host("left-revert");
    let mut app = open(&hosted);
    leave(&mut app);
    common::revert_record(&mut app);
    wait_saved(&mut app);
    expect(&mut app, "Status", "status 0:::first/second tx ");
    assert_eq!(
        left_values(&hosted),
        (
            vec![String::new(), String::new(), "x".to_owned()],
            "first".to_owned()
        )
    );
}

/// The user's save commits what scripts left with the user's own typing
/// (which wins over the script's value of the same field), and its
/// OnWindowTransaction runs; a later script's commit does not bring the
/// script's earlier value back.
#[test]
fn the_users_save_commits_what_scripts_left_on_the_record() {
    let hosted = host("left-save");
    let mut app = open(&hosted);
    leave(&mut app);
    common::text_field(&app, "Code").focus();
    app.step();
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    common::text_field(&app, "Code").type_text("b");
    app.run_steps(2);
    common::save_record(&mut app);
    wait_saved(&mut app);
    assert_eq!(
        left_values(&hosted),
        (
            vec!["b".to_owned(), "ab".to_owned(), String::new()],
            "pending".to_owned()
        )
    );
    expect(&mut app, "Status", "status 0:b:ab:pending/second tx 1");
    assert_eq!(
        left_values(&hosted),
        (
            vec!["b".to_owned(), "ab".to_owned(), "x".to_owned()],
            "pending".to_owned()
        )
    );
}

/// Type `text` over the first portal row's Notes::Body (#189).
fn retype_first_row(app: &mut common::App, text: &str) {
    let row = || {
        By::new()
            .role(egui::accesskit::Role::TextInput)
            .label("Notes::Body")
    };
    // The rows paint once their related records are read.
    common::wait_until(app, "the portal rows", |app| {
        app.query_all(row()).next().is_some()
    });
    app.query_all(row())
        .next()
        .expect("a portal row editor")
        .focus();
    app.run_steps(2);
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.query_all(row())
        .next()
        .expect("the row's editor")
        .type_text(text);
    app.run_steps(2);
}

/// The user edits a portal row's field and a field of the record in
/// Browse (#189): a related record's validation failing refuses the
/// save of both, nothing written and both edits kept; the next save
/// commits them together.
#[test]
fn the_users_portal_row_edit_is_committed_with_the_record() {
    let hosted = host("browse-row");
    let mut app = open(&hosted);
    expect(&mut app, "Show portals", "on Portals");
    // Notes::Body is validated Always: at most 10 characters.
    retype_first_row(&mut app, "far too long for it");
    retype(&mut app, "Code", "c1");
    common::save_record(&mut app);
    common::wait_until(&mut app, "the refusal", |app| {
        app.state().error().is_some() && !app.state().script_running()
    });
    common::click(&mut app, "OK");
    app.run_steps(2);
    let note = |hosted: &hosted::Hosted| {
        stored(hosted, NOTES, &[(BODY, 1)])
            .into_iter()
            .find(|(id, _)| *id == 2)
            .map(|(_, v)| v[0].clone())
    };
    assert_eq!(note(&hosted).as_deref(), Some("first"));
    assert_eq!(stored(&hosted, TABLE, &[(CODE, 1)])[0].1, [""]);
    assert!(common::unsaved(&app), "both edits are kept");
    retype_first_row(&mut app, "edited");
    common::save_record(&mut app);
    wait_saved(&mut app);
    assert_eq!(note(&hosted).as_deref(), Some("edited"));
    assert_eq!(stored(&hosted, TABLE, &[(CODE, 1)])[0].1, ["c1"]);
}

/// A save keeps the record's portal rows on screen while their data is
/// read again: no frame between the save and the fresh rows draws them
/// empty. Each row shows its last value, the saved text among them,
/// read-only while its record is read again, then in its editor.
#[test]
fn a_save_keeps_the_portal_rows_while_they_are_read_again() {
    let hosted = host("save-keeps-rows");
    let mut app = open(&hosted);
    expect(&mut app, "Show portals", "on Portals");
    let editors = |app: &common::App| -> Vec<String> {
        app.query_all(
            By::new()
                .role(egui::accesskit::Role::TextInput)
                .label("Notes::Body"),
        )
        .filter_map(|row| row.value())
        .filter(|value| !value.is_empty())
        .collect()
    };
    // In an editor or as text.
    let shown = |app: &common::App, text: &str| {
        app.query_all(By::new())
            .any(|node| node.value().as_deref() == Some(text))
    };
    retype_first_row(&mut app, "edited");
    common::wait_until(&mut app, "unsaved record changes", common::unsaved);
    common::save_record(&mut app);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        app.step();
        assert!(
            shown(&app, "edited") && shown(&app, "second"),
            "a frame after the save drew the rows empty"
        );
        if !common::unsaved(&app) && !common::saving(&app) && app.state().is_settled() {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "timed out saving");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    common::wait_until(&mut app, "the rows' editors", |app| {
        editors(app) == ["edited", "second"]
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

/// A menu command other than the field's own (View > Find Mode) ends the
/// field's editing first: its OnObjectValidate refusing keeps the field
/// entered, the window in Browse and the record unsaved, as a click on a
/// button does. The menus' own commands act on the field without leaving it.
#[test]
fn a_menu_command_waits_for_the_fields_validation_trigger() {
    let hosted = host_with("menu-validate", |root| {
        add_object_triggers(
            root,
            TASK_NAME,
            &[(TriggerEvent::ObjectValidate, COMMIT_VETO)],
        );
    });
    let before = stored(&hosted, TABLE, &[(NAME, 1)]);
    let mut app = open(&hosted);
    expect(&mut app, "Show portals", "on Portals");
    common::text_field(&app, "Name").focus();
    app.step();
    app.key_press(egui::Key::End);
    common::text_field(&app, "Name").type_text(" changed");
    app.run_steps(2);
    common::open_menu(&mut app, "View");
    common::click(&mut app, "Find Mode");
    let idle = |app: &common::App| !app.state().script_running() && app.state().is_settled();
    common::wait_until(&mut app, "the validation trigger", idle);
    app.run_steps(4);
    assert_eq!(app.state().mode(), om_gui::Mode::Browse);
    assert!(common::text_field(&app, "Name").is_focused());
    assert!(common::unsaved(&app));
    assert_eq!(stored(&hosted, TABLE, &[(NAME, 1)]), before);
    // Revert keeps the field active for the discard triggers and can
    // remove the draft that validation would reject on a saving exit.
    common::revert_record(&mut app);
    common::wait_until(&mut app, "the draft reverted", |app| {
        idle(app) && !common::unsaved(app)
    });
    assert!(!common::text_field(&app, "Name").is_focused());
    assert_eq!(stored(&hosted, TABLE, &[(NAME, 1)]), before);
}

/// The user's focus and typing in a portal row's field run the field
/// object's triggers (#1005), with its row and field active: OnObjectEnter
/// as the field is entered, OnObjectModify as it is typed into, and
/// OnObjectSave as the user leaves it changed. Moving to the same field in
/// the next row leaves one row's and enters the other's; a record begun in
/// the creation row answers what was typed into it.
#[test]
fn a_portal_fields_object_triggers_run_for_the_users_edit() {
    let hosted = host_with("object-triggers", |root| {
        add_object_triggers(
            root,
            PORTAL_BODY,
            &[
                (TriggerEvent::ObjectEnter, BODY_ENTER),
                (TriggerEvent::ObjectModify, BODY_MODIFY),
                (TriggerEvent::ObjectSave, BODY_SAVE),
            ],
        );
    });
    let mut app = open(&hosted);
    expect(&mut app, "Show portals", "on Portals");
    let row = || {
        By::new()
            .role(egui::accesskit::Role::TextInput)
            .label("Notes::Body")
    };
    common::wait_until(&mut app, "the portal rows", |app| {
        app.query_all(row()).next().is_some()
    });
    let idle = |app: &common::App| !app.state().script_running() && app.state().is_settled();
    app.query_all(row()).next().unwrap().focus();
    common::wait_until(&mut app, "OnObjectEnter", idle);
    // The trigger's script read the portal's rows again (it set a
    // variable): the row's editor keeps the focus through it.
    common::wait_until(&mut app, "the editor's focus back", |app| {
        app.query_all(row())
            .next()
            .is_some_and(|editor| editor.accesskit_node().is_focused())
    });
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.query_all(row()).next().unwrap().type_text("ab");
    common::wait_until(&mut app, "OnObjectModify", idle);
    let nth = |app: &common::App, n: usize| app.query_all(row()).nth(n).unwrap().focus();
    nth(&app, 1);
    common::wait_until(
        &mut app,
        "row 1's OnObjectSave, row 2's OnObjectEnter",
        idle,
    );
    // The creation row's field.
    nth(&app, 2);
    common::wait_until(&mut app, "the creation row's OnObjectEnter", idle);
    app.query_all(row()).nth(2).unwrap().type_text("cd");
    common::wait_until(&mut app, "the begun row's OnObjectModify", idle);
    common::text_field(&app, "Code").focus();
    common::wait_until(&mut app, "the begun row's OnObjectSave", idle);
    expect(
        &mut app,
        "Object triggers",
        "objects [Body:1:first][Body:2:second][Body:3:]|2|[Body:1:ab:ab][Body:3:cd:cd]",
    );
}

/// In a portal that sorts its rows (Browse shows "second" first), the row
/// the user is in is still its record for its field's triggers (#1027): a
/// stored row's and a begun row's contents and related field. Typed "ab"
/// sorts the edited record below "first" at once (#1308; a draft's
/// related read no longer waits on a scan), and scripts number the rows
/// as Browse shows them (script-runtime.md 13.24): row 2.
#[test]
fn a_sorted_portals_rows_answer_their_records_in_triggers() {
    let hosted = host_with("sorted-object-triggers", |root| {
        display_sorted(root);
        add_object_triggers(
            root,
            PORTAL_BODY,
            &[
                (TriggerEvent::ObjectEnter, BODY_ENTER),
                (TriggerEvent::ObjectModify, BODY_MODIFY),
                (TriggerEvent::ObjectSave, BODY_SAVE),
            ],
        );
    });
    let mut app = open(&hosted);
    expect(&mut app, "Show portals", "on Portals");
    let row = || {
        By::new()
            .role(egui::accesskit::Role::TextInput)
            .label("Notes::Body")
    };
    common::wait_until(&mut app, "the portal rows", |app| {
        app.query_all(row()).next().is_some()
    });
    let idle = |app: &common::App| !app.state().script_running() && app.state().is_settled();
    let focused = |app: &common::App, n: usize| {
        app.query_all(row())
            .nth(n)
            .is_some_and(|editor| editor.accesskit_node().is_focused())
    };
    app.query_all(row()).next().unwrap().focus();
    common::wait_until(&mut app, "row 1's OnObjectEnter", idle);
    common::wait_until(&mut app, "row 1's editor", |app| focused(app, 0));
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.query_all(row()).next().unwrap().type_text("ab");
    common::wait_until(&mut app, "row 1's OnObjectModify", idle);
    common::text_field(&app, "Code").focus();
    common::wait_until(&mut app, "row 1's OnObjectSave", idle);
    // The creation row's field.
    app.query_all(row()).nth(2).unwrap().focus();
    common::wait_until(&mut app, "the creation row's OnObjectEnter", idle);
    common::wait_until(&mut app, "the creation row's editor", |app| focused(app, 2));
    app.query_all(row()).nth(2).unwrap().type_text("cd");
    common::wait_until(&mut app, "the begun row's OnObjectModify", idle);
    common::text_field(&app, "Code").focus();
    common::wait_until(&mut app, "the begun row's OnObjectSave", idle);
    expect(
        &mut app,
        "Object triggers",
        "objects [Body:1:second][Body:3:]|2|[Body:2:ab:ab][Body:3:cd:cd]",
    );
}

/// A step that leaves the field the user changed saves it first, as the
/// user's own exit does (#1030; Claris, "OnObjectSave"): Commit Records,
/// run by the portal field's OnObjectModify while the field is active,
/// runs its OnObjectSave, whose Set Field of the record joins the one
/// commit. A field the user did not change is not saved, even with
/// another field of the record changed.
#[test]
fn a_step_leaving_a_changed_field_runs_its_save_trigger() {
    use fixture::COUNT;
    let count_and_code = |hosted: &hosted::Hosted| {
        stored(hosted, TABLE, &[(COUNT, 1), (CODE, 1)])
            .into_iter()
            .find(|(id, _)| *id == 1)
            .map(|(_, values)| values)
    };
    let note = |hosted: &hosted::Hosted, id: u64| {
        stored(hosted, NOTES, &[(BODY, 1)])
            .into_iter()
            .find(|(record, _)| *record == id)
            .map(|(_, values)| values[0].clone())
    };
    let row = || {
        By::new()
            .role(egui::accesskit::Role::TextInput)
            .label("Notes::Body")
    };
    let idle = |app: &common::App| !app.state().script_running() && app.state().is_settled();

    // The row's field, changed: OnObjectModify commits, and OnObjectSave
    // logs into the task in the same commit.
    let hosted = host_with("leaving-changed", |root| {
        add_object_triggers(
            root,
            PORTAL_BODY,
            &[
                (TriggerEvent::ObjectModify, BODY_COMMIT),
                (TriggerEvent::ObjectSave, BODY_LOG),
            ],
        );
    });
    let mut app = open(&hosted);
    expect(&mut app, "Show portals", "on Portals");
    common::wait_until(&mut app, "the portal rows", |app| {
        app.query_all(row()).next().is_some()
    });
    app.query_all(row()).next().unwrap().focus();
    common::wait_until(&mut app, "row 1's editor", idle);
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.query_all(row()).next().unwrap().type_text("x");
    common::wait_until(&mut app, "the commit", |app| {
        idle(app) && !common::unsaved(app)
    });
    assert_eq!(note(&hosted, 2).as_deref(), Some("x"));
    assert_eq!(
        count_and_code(&hosted),
        Some(vec!["77".to_owned(), String::new()])
    );

    // The row's field entered unchanged, Code changed: OnObjectEnter
    // commits, and OnObjectSave does not run.
    let hosted = host_with("leaving-unchanged", |root| {
        add_object_triggers(
            root,
            PORTAL_BODY,
            &[
                (TriggerEvent::ObjectEnter, BODY_COMMIT),
                (TriggerEvent::ObjectSave, BODY_LOG),
            ],
        );
    });
    let mut app = open(&hosted);
    expect(&mut app, "Show portals", "on Portals");
    common::wait_until(&mut app, "the portal rows", |app| {
        app.query_all(row()).next().is_some()
    });
    retype(&mut app, "Code", "ab");
    app.query_all(row()).next().unwrap().focus();
    common::wait_until(&mut app, "the commit", |app| {
        idle(app) && !common::unsaved(app)
    });
    assert_eq!(
        count_and_code(&hosted),
        Some(vec!["3".to_owned(), "ab".to_owned()])
    );
    assert_eq!(note(&hosted, 2).as_deref(), Some("first"));

    // Code changed: its OnObjectModify commits, and its OnObjectSave sets
    // Code itself, which ends its typing. The layout's OnRecordCommit, the
    // stage after them, still runs and refuses: nothing is saved.
    let hosted = host_with("leaving-vetoed", |root| {
        add_object_triggers(
            root,
            TASK_CODE,
            &[
                (TriggerEvent::ObjectModify, BODY_COMMIT),
                (TriggerEvent::ObjectSave, CODE_NORMALIZE),
            ],
        );
        add_layout_trigger_script(root, TriggerEvent::RecordCommit, COMMIT_VETO, "veto");
    });
    let mut app = open(&hosted);
    expect(&mut app, "Show portals", "on Portals");
    common::text_field(&app, "Code").focus();
    app.step();
    common::text_field(&app, "Code").type_text("a");
    common::wait_until(&mut app, "the refused commit", idle);
    app.run_steps(5);
    assert_eq!(
        count_and_code(&hosted),
        Some(vec!["3".to_owned(), String::new()]),
        "OnRecordCommit refused the commit"
    );
}

/// A trigger that refuses the user's exit from a portal row's field gives
/// that row's field the focus back, as one of the record's own fields gets
/// it back (#1050): the click into Code is cancelled, and the row keeps
/// what was typed. A keyboard commit refused by OnObjectExit does the
/// same, keeping the row uncommitted (#1070).
#[test]
fn a_refused_exit_gives_a_portal_rows_field_the_focus_back() {
    for (keyboard, event) in [
        (false, TriggerEvent::ObjectSave),
        (true, TriggerEvent::ObjectExit),
    ] {
        let hosted = host_with("refused-exit-focus", |root| {
            add_object_triggers(root, PORTAL_BODY, &[(event, COMMIT_VETO)]);
        });
        let mut app = open(&hosted);
        expect(&mut app, "Show portals", "on Portals");
        let row = || {
            By::new()
                .role(egui::accesskit::Role::TextInput)
                .label("Notes::Body")
        };
        let idle = |app: &common::App| !app.state().script_running() && app.state().is_settled();
        common::wait_until(&mut app, "the portal rows", |app| {
            app.query_all(row()).next().is_some()
        });
        app.query_all(row()).next().unwrap().focus();
        common::wait_until(&mut app, "row 1's editor", idle);
        app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        app.query_all(row()).next().unwrap().type_text("x");
        if keyboard {
            common::save_record(&mut app);
        } else {
            common::text_field(&app, "Code").focus();
        }
        common::wait_until(&mut app, "row 1's editor focused again", |app| {
            idle(app)
                && app
                    .query_all(row())
                    .next()
                    .is_some_and(|editor| editor.accesskit_node().is_focused())
        });
        assert!(!common::text_field(&app, "Code").is_focused());
        let value = app
            .query_all(row())
            .next()
            .unwrap()
            .accesskit_node()
            .value();
        assert_eq!(value.as_deref(), Some("x"));
        assert!(common::unsaved(&app));
        assert_eq!(
            stored(&hosted, NOTES, &[(BODY, 1)])
                .into_iter()
                .find(|(id, _)| *id == 2)
                .unwrap()
                .1,
            ["first"],
            "a refused exit must not commit the typed row"
        );
    }
}

/// A portal field's edit is saved once (#1040): after a step or the user
/// left the field and its OnObjectSave ran, returning to it and leaving it
/// again while it holds the saved value does not run OnObjectSave again; a
/// new value does. A save its OnObjectSave refused saved nothing: leaving
/// the field again runs it again.
#[test]
fn a_saved_portal_field_left_again_is_not_saved_again() {
    let row = || {
        By::new()
            .role(egui::accesskit::Role::TextInput)
            .label("Notes::Body")
    };
    let idle = |app: &common::App| !app.state().script_running() && app.state().is_settled();
    let type_into_row_1 = |app: &mut common::App| {
        common::wait_until(app, "the portal rows", |app| {
            app.query_all(row()).next().is_some()
        });
        app.query_all(row()).next().unwrap().focus();
        common::wait_until(app, "row 1's editor", idle);
        app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        app.query_all(row()).next().unwrap().type_text("x");
    };

    // A step leaves the field, then returns to it and commits.
    let hosted = host_with("saved-once-by-steps", |root| {
        add_object_triggers(
            root,
            PORTAL_BODY,
            &[
                (TriggerEvent::ObjectModify, LEAVE_AND_RETURN),
                (TriggerEvent::ObjectSave, BODY_SAVE),
            ],
        );
    });
    let mut app = open(&hosted);
    expect(&mut app, "Show portals", "on Portals");
    type_into_row_1(&mut app);
    common::wait_until(&mut app, "the commit", |app| {
        idle(app) && !common::unsaved(app)
    });
    expect(&mut app, "Object triggers", "objects ||[Body:1:x:x]");

    // The user leaves the field, comes back to it unchanged and leaves it
    // again.
    let hosted = host_with("saved-once-by-the-user", |root| {
        add_object_triggers(root, PORTAL_BODY, &[(TriggerEvent::ObjectSave, BODY_SAVE)]);
    });
    let mut app = open(&hosted);
    expect(&mut app, "Show portals", "on Portals");
    type_into_row_1(&mut app);
    common::text_field(&app, "Code").focus();
    common::wait_until(&mut app, "the first save", idle);
    app.query_all(row()).next().unwrap().focus();
    common::wait_until(&mut app, "row 1's editor again", idle);
    common::text_field(&app, "Code").focus();
    common::wait_until(&mut app, "leaving it again", idle);
    // A new value, though, is saved again.
    app.query_all(row()).next().unwrap().focus();
    common::wait_until(&mut app, "row 1's editor a third time", idle);
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.query_all(row()).next().unwrap().type_text("y");
    common::text_field(&app, "Code").focus();
    common::wait_until(&mut app, "saving the new value", idle);
    expect(
        &mut app,
        "Object triggers",
        "objects ||[Body:1:x:x][Body:1:y:y]",
    );

    // OnObjectSave refuses the step's first save: the commit saves again.
    let hosted = host_with("save-refused-by-steps", |root| {
        add_object_triggers(
            root,
            PORTAL_BODY,
            &[
                (TriggerEvent::ObjectModify, LEAVE_AND_RETURN),
                (TriggerEvent::ObjectSave, SAVE_REFUSED_ONCE),
            ],
        );
    });
    let mut app = open(&hosted);
    expect(&mut app, "Show portals", "on Portals");
    type_into_row_1(&mut app);
    common::wait_until(&mut app, "the commit", |app| {
        idle(app) && !common::unsaved(app)
    });
    expect(
        &mut app,
        "Object triggers",
        "objects ||[Body:1:x:x][Body:1:x:x]",
    );

    // OnObjectSave refuses the user's first exit: leaving again saves.
    let hosted = host_with("save-refused-for-the-user", |root| {
        add_object_triggers(
            root,
            PORTAL_BODY,
            &[(TriggerEvent::ObjectSave, SAVE_REFUSED_ONCE)],
        );
    });
    let mut app = open(&hosted);
    expect(&mut app, "Show portals", "on Portals");
    type_into_row_1(&mut app);
    common::text_field(&app, "Code").focus();
    common::wait_until(&mut app, "the refused save", idle);
    // Back to the row's field, unchanged, and out of it again.
    app.query_all(row()).next().unwrap().focus();
    common::wait_until(&mut app, "row 1's editor again", idle);
    common::text_field(&app, "Code").focus();
    common::wait_until(&mut app, "the second save", idle);
    expect(
        &mut app,
        "Object triggers",
        "objects ||[Body:1:x:x][Body:1:x:x]",
    );
}

/// Delete Record of a record a script made and left uncommitted takes what
/// the window held for it with it (#871): the record shown next keeps its
/// own portal edit and saves it, rather than leaving it to a save that
/// writes only the record's own fields.
#[test]
fn deleting_a_scripts_new_record_leaves_the_next_records_portal_edit_to_save() {
    let hosted = host("worker-new-delete");
    let mut app = open(&hosted);
    run_without_dialog(&mut app, "Leave new");
    common::wait_until(&mut app, "unsaved record changes", common::unsaved);
    let new = common::record_number(&app);
    common::click(&mut app, "Delete Record");
    common::wait_for_label(&mut app, "Permanently delete this ENTIRE record?");
    common::click(&mut app, "Delete");
    common::wait_until(&mut app, "the new record deleted", |app| {
        !common::editing(app) && !app.state().script_running() && common::record_number(app) != new
    });
    // "Leave new" went to Portals. No script may run from here on: one
    // would hand the window its state afresh. Back to the first record,
    // whose first portal row is note 2.
    while common::record_number(&app) != "1" {
        common::click(&mut app, "Previous");
        common::wait_until(&mut app, "the previous record", |app| {
            app.state().is_settled()
        });
    }
    retype_first_row(&mut app, "edited");
    common::wait_until(&mut app, "unsaved record changes", common::unsaved);
    common::save_record(&mut app);
    wait_saved(&mut app);
    let note = stored(&hosted, NOTES, &[(BODY, 1)])
        .into_iter()
        .find(|(id, _)| *id == 2)
        .map(|(_, v)| v[0].clone());
    assert_eq!(note.as_deref(), Some("edited"));
}

/// Typing into the portal's creation row begins a related record, kept
/// with the record; the save creates it with its match field from the
/// record (#189). Its `Get ( RecordNumber )` auto-entered is its place
/// after the two notes related before (#1038).
#[test]
fn typing_into_the_creation_row_creates_a_related_record() {
    const NUMBER: u64 = 3;
    let hosted = host_with("browse-create", |root| {
        fixture::add_field(root, NOTES, NUMBER, "Number", 1, 2, 0, 0x8, 0);
        let mut names = fixture::names();
        names.add_field(NOTES, NUMBER, "Number");
        let number = "Get ( RecordNumber )";
        fixture::add_formula(root, &names, (NOTES, NOTES_OCCURRENCE), NUMBER, 5, number);
    });
    let mut app = open(&hosted);
    expect(&mut app, "Show portals", "on Portals");
    let body = || {
        By::new()
            .role(egui::accesskit::Role::TextInput)
            .label("Notes::Body")
    };
    // "first", "second", then the creation row.
    assert_eq!(app.query_all(body()).count(), 3);
    app.query_all(body()).nth(2).unwrap().focus();
    app.run_steps(2);
    // One key a frame, as a person types: the row the first key begins
    // appears between them, and keeps every key (#536).
    for key in ["t", "h", "i", "r", "d"] {
        app.input_mut().events.push(egui::Event::Text(key.into()));
        app.run_steps(2);
    }
    common::wait_until(&mut app, "unsaved record changes", common::unsaved);
    let typed: Vec<String> = app
        .query_all(body())
        .filter_map(|n| n.accesskit_node().value())
        .collect();
    assert!(typed.iter().any(|v| v == "third"), "{typed:?}");
    common::save_record(&mut app);
    wait_saved(&mut app);
    let notes = stored(&hosted, NOTES, &[(BODY, 1), (TASK, 1), (NUMBER, 1)]);
    assert_eq!(
        notes.last().map(|(_, v)| v.clone()),
        Some(vec![
            "third".to_owned(),
            "Write tests".to_owned(),
            "3".to_owned()
        ])
    );
    common::wait_until(&mut app, "the created row", |app| {
        app.query_all(body())
            .filter_map(|n| n.accesskit_node().value())
            .any(|v| v == "third")
    });
}

/// A click on the Portals layout's background, right of the Name field and
/// above the portal.
fn click_background(app: &mut common::App) {
    let name = common::text_field(app, "Name").rect();
    let background = egui::pos2(name.max.x + 200.0, name.center().y);
    for pressed in [true, false] {
        app.event(egui::Event::PointerButton {
            pos: background,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        });
    }
    app.run_steps(2);
}

/// The OnObjectSave script of an ordinary commit (a click on the layout's
/// background) reads a row begun in the portal's creation row with what
/// its rules enter, as FileMaker Pro 26.0.3's did (#1047): Notes::Number
/// auto-enters `"n-" & Notes::Body`, which the provider previews before the
/// script runs, and the commit stores the value the script read. A save
/// the script refused leaves the preview for the next commit, computed
/// again over the row's new text.
#[test]
fn an_ordinary_commits_save_trigger_reads_a_new_rows_auto_entry() {
    const NUMBER: u64 = 3;
    let numbered = |tag: &str, save: u64| {
        host_with(tag, |root| {
            fixture::add_field(root, NOTES, NUMBER, "Number", 1, 1, 0, 0x8, 0);
            let mut names = fixture::names();
            names.add_field(NOTES, NUMBER, "Number");
            let entered = "\"n-\" & Notes::Body";
            fixture::add_formula(root, &names, (NOTES, NOTES_OCCURRENCE), NUMBER, 5, entered);
            add_object_triggers(root, PORTAL_BODY, &[(TriggerEvent::ObjectSave, save)]);
        })
    };
    let hosted = numbered("begun-autoentry", SAVE_NUMBER);
    let mut app = open(&hosted);
    expect(&mut app, "Show portals", "on Portals");
    let body = || {
        By::new()
            .role(egui::accesskit::Role::TextInput)
            .label("Notes::Body")
    };
    let idle = |app: &common::App| !app.state().script_running() && app.state().is_settled();
    // "first", "second", then the creation row.
    common::wait_until(&mut app, "the portal rows", |app| {
        app.query_all(body()).count() == 3
    });
    app.query_all(body()).nth(2).unwrap().focus();
    common::wait_until(&mut app, "the creation row's editor", idle);
    app.query_all(body()).nth(2).unwrap().type_text("third");
    common::wait_until(&mut app, "unsaved record changes", common::unsaved);
    click_background(&mut app);
    common::wait_until(&mut app, "the commit", |app| {
        idle(app) && !common::unsaved(app)
    });
    let notes = stored(&hosted, NOTES, &[(BODY, 1), (TASK, 1), (NUMBER, 1)]);
    assert_eq!(
        notes.last().map(|(_, v)| v.clone()),
        Some(vec![
            "third".to_owned(),
            "Write tests".to_owned(),
            "n-third".to_owned()
        ])
    );
    expect(
        &mut app,
        "Object triggers",
        "objects ||[Body:3:third:n-third]",
    );

    // Refused, edited, committed again.
    let hosted = numbered("begun-autoentry-refused", SAVE_NUMBER_REFUSED_ONCE);
    let mut app = open(&hosted);
    expect(&mut app, "Show portals", "on Portals");
    common::wait_until(&mut app, "the portal rows", |app| {
        app.query_all(body()).count() == 3
    });
    app.query_all(body()).nth(2).unwrap().focus();
    common::wait_until(&mut app, "the creation row's editor", idle);
    app.query_all(body()).nth(2).unwrap().type_text("third");
    common::wait_until(&mut app, "unsaved record changes", common::unsaved);
    click_background(&mut app);
    common::wait_until(&mut app, "the refused save", |app| {
        idle(app)
            && app
                .query_all(body())
                .nth(2)
                .is_some_and(|editor| editor.accesskit_node().is_focused())
    });
    assert!(common::unsaved(&app));
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.query_all(body()).nth(2).unwrap().type_text("fourth");
    click_background(&mut app);
    common::wait_until(&mut app, "the commit", |app| {
        idle(app) && !common::unsaved(app)
    });
    let notes = stored(&hosted, NOTES, &[(BODY, 1), (NUMBER, 1)]);
    assert_eq!(
        notes.last().map(|(_, v)| v.clone()),
        Some(vec!["fourth".to_owned(), "n-fourth".to_owned()])
    );
    expect(
        &mut app,
        "Object triggers",
        "objects ||[Body:3:third:n-third][Body:3:fourth:n-fourth]",
    );
}

/// A new row's preview reads the window's records as the window holds them
/// (#1063): Notes::Number auto-enters `"c-" & Tasks::Code`, read through the
/// relationship from the row to its parent, whose Code the user typed and
/// has not saved. The commit's OnObjectSave script reads it, and the commit
/// stores it.
#[test]
fn a_new_rows_preview_reads_its_parents_unsaved_value() {
    const NUMBER: u64 = 3;
    let hosted = host_with("begun-pending-parent", |root| {
        fixture::add_field(root, NOTES, NUMBER, "Number", 1, 1, 0, 0x8, 0);
        let mut names = fixture::names();
        names.add_field(NOTES, NUMBER, "Number");
        let entered = "\"c-\" & Tasks::Code";
        fixture::add_formula(root, &names, (NOTES, NOTES_OCCURRENCE), NUMBER, 5, entered);
        add_object_triggers(
            root,
            PORTAL_BODY,
            &[(TriggerEvent::ObjectSave, SAVE_NUMBER)],
        );
    });
    let mut app = open(&hosted);
    expect(&mut app, "Show portals", "on Portals");
    let body = || {
        By::new()
            .role(egui::accesskit::Role::TextInput)
            .label("Notes::Body")
    };
    let idle = |app: &common::App| !app.state().script_running() && app.state().is_settled();
    common::wait_until(&mut app, "the portal rows", |app| {
        app.query_all(body()).count() == 3
    });
    common::text_field(&app, "Code").focus();
    common::wait_until(&mut app, "Code's editor", idle);
    common::text_field(&app, "Code").type_text("ab");
    common::wait_until(&mut app, "unsaved record changes", common::unsaved);
    // "first", "second", then the creation row.
    app.query_all(body()).nth(2).unwrap().focus();
    common::wait_until(&mut app, "the creation row's editor", idle);
    app.query_all(body()).nth(2).unwrap().type_text("third");
    click_background(&mut app);
    common::wait_until(&mut app, "the commit", |app| {
        idle(app) && !common::unsaved(app)
    });
    let notes = stored(&hosted, NOTES, &[(BODY, 1), (NUMBER, 1)]);
    assert_eq!(
        notes.last().map(|(_, v)| v.clone()),
        Some(vec!["third".to_owned(), "c-ab".to_owned()])
    );
    expect(&mut app, "Object triggers", "objects ||[Body:3:third:c-ab]");
}

/// A click on the layout's background after typing into a field commits
/// the record once the field's exit triggers let the click go on (#1066):
/// the portal row's OnObjectSave runs, then the new related record is
/// stored. Keyboard Commit does the same (#1070): Save/Exit/Commit is
/// visible before another interaction, with no field editor focused.
/// Global Setting is the trace; the marker scripts explicitly return True.
#[test]
fn a_background_click_commits_after_the_fields_exit_triggers() {
    let body = || {
        By::new()
            .role(egui::accesskit::Role::TextInput)
            .label("Notes::Body")
    };
    let idle = |app: &common::App| !app.state().script_running() && app.state().is_settled();
    for keys in [false, true] {
        let tag = if keys {
            "keyboard-commit"
        } else {
            "background-commit"
        };
        let hosted = host_with(tag, |root| {
            // Keep the existing row-context checks, and append a visible
            // trace without clicking a diagnostic script after committing.
            for (id, marker) in [(BODY_SAVE, "S"), (BODY_ENTER, "E")] {
                let mut script = scripts().into_iter().find(|s| s.id == id).unwrap();
                script.steps.push(set_field(
                    OCCURRENCE,
                    SETTING,
                    0,
                    &format!("Setting & \"|{marker}\""),
                ));
                script.steps.push(fixture::exit_with("True"));
                add_script(
                    root,
                    &hidden(fixture::script(id, &script.name, script.steps)),
                );
            }
            add_script(
                root,
                &fixture::script(
                    COUNT,
                    "Trace commit",
                    vec![
                        set_field(OCCURRENCE, SETTING, 0, "Setting & \"|C\""),
                        fixture::exit_with("True"),
                    ],
                ),
            );
            add_layout_trigger_script(root, TriggerEvent::RecordCommit, COUNT, "commit");
            add_object_triggers(
                root,
                PORTAL_BODY,
                &[
                    (TriggerEvent::ObjectSave, BODY_SAVE),
                    (TriggerEvent::ObjectExit, BODY_ENTER),
                ],
            );
        });
        let mut app = open(&hosted);
        expect(&mut app, "Show portals", "on Portals");
        // "first", "second", then the creation row.
        common::wait_until(&mut app, "the portal rows", |app| {
            app.query_all(body()).count() == 3
        });
        app.query_all(body()).nth(2).unwrap().focus();
        common::wait_until(&mut app, "the creation row's editor", idle);
        app.query_all(body()).nth(2).unwrap().type_text("third");
        common::wait_until(&mut app, "unsaved record changes", common::unsaved);
        let artifact = |app: &mut common::App, stage: &str| {
            if let Some(dir) = std::env::var_os("OM_COMMIT_ARTIFACT_DIR") {
                let dir = std::path::PathBuf::from(dir);
                std::fs::create_dir_all(&dir).unwrap();
                app.render()
                    .unwrap()
                    .save(dir.join(format!("{tag}-{stage}.png")))
                    .unwrap();
            }
        };
        artifact(&mut app, "before");
        if keys {
            common::save_record(&mut app);
        } else {
            click_background(&mut app);
        }
        common::wait_until(&mut app, &format!("{tag} commit"), |app| {
            assert!(
                app.state().error().is_none(),
                "{tag}: {:?}",
                app.state().error()
            );
            idle(app) && !common::unsaved(app)
        });
        // Inspect the first settled result, before any subsequent click,
        // menu, navigation or dialog can supply a missing exit trigger.
        assert_eq!(
            common::text_field(&app, "Setting")
                .accesskit_node()
                .value()
                .as_deref(),
            Some("|S|E|C"),
            "{tag}: first settled trace"
        );
        assert!(
            app.query_all(By::new().role(egui::accesskit::Role::TextInput))
                .all(|editor| !editor.accesskit_node().is_focused()),
            "{tag}: commit leaves the field"
        );
        artifact(&mut app, "after");
        let notes = stored(&hosted, NOTES, &[(BODY, 1), (TASK, 1)]);
        assert_eq!(
            notes.last().map(|(_, v)| v.clone()),
            Some(vec!["third".to_owned(), "Write tests".to_owned()]),
            "{tag}"
        );
        expect(
            &mut app,
            "Object triggers",
            "objects [Body:3:third]||[Body:3:third:third]",
        );
    }
}

/// The save of a row begun in the portal, lost on its way (#152). The
/// host drops the connection at the request named.
/// - Lost at op71, before any record data went, the row is kept as typed
///   and the reconnect opens the file again with the row on screen, to
///   save, and no word of a save on its way. The host has no such record.
/// - Lost at op70, after the upload, whether the host has the row cannot be
///   told, and the reconnect is refused.
/// - A new parent the user made, with a row begun, lost at the save's op71:
///   the reconnect holds the parent again under a new id, its serial
///   number as New Record gave it, and one save stores both once (#152).
///   When the host refuses that op71, the window keeps the record as it
///   was, frozen, until Try Again makes it.
#[cfg(debug_assertions)]
#[test]
fn a_row_begun_and_lost_before_its_data_went_is_kept_for_the_reconnect() {
    use om_protocol::Opcode;
    let body = || {
        By::new()
            .role(egui::accesskit::Role::TextInput)
            .label("Notes::Body")
    };
    let shown = |app: &common::App| {
        app.query_all(body())
            .filter_map(|n| n.accesskit_node().value())
            .any(|v| v == "third")
    };
    for (lost_at, kept) in [
        (Opcode::HostCreateRecord, true),
        (Opcode::HostCommit, false),
    ] {
        let tag = format!("lost-begun-{}", lost_at as u8);
        let hosted = host_configured(
            &tag,
            |_| {},
            |server| server.panicking_on(lost_at as u8, false),
        );
        let mut app = open(&hosted);
        expect(&mut app, "Show portals", "on Portals");
        // The Portals layout's portal has read its records: its rows and
        // their editors are the ones the records show (#1113).
        common::wait_until(&mut app, "the portal's records", |app| {
            app.state().is_settled()
        });
        // "first", "second", then the creation row.
        app.query_all(body()).nth(2).unwrap().focus();
        app.run_steps(2);
        app.input_mut()
            .events
            .push(egui::Event::Text("third".into()));
        app.run_steps(2);
        common::wait_until(&mut app, "unsaved record changes", common::unsaved);
        common::save_record(&mut app);
        common::wait_until(&mut app, "the lost connection", |app| {
            app.state().connection_lost().is_some() && !app.state().script_running()
        });
        app.get_by_role_and_label(egui::accesskit::Role::Button, "Reconnect…")
            .click();
        common::wait_until(&mut app, "the reconnect dialog", |app| {
            common::find_text_field(app, "Password").is_some()
        });
        common::fill(&mut app, "Password", PASSWORD);
        app.get_by_role_and_label(egui::accesskit::Role::Button, "Reconnect")
            .click();
        if kept {
            common::wait_until(&mut app, "the file open again", |app| {
                app.state().connection_lost().is_none() && shown(app)
            });
            assert!(
                app.query_by_label_contains("on its way").is_none(),
                "nothing says the save may have reached the host"
            );
            assert!(common::unsaved(&app), "the begun row is still to save");
            let notes = stored(&hosted, NOTES, &[(BODY, 1)]);
            assert!(
                notes.iter().all(|(_, v)| v[0] != "third"),
                "the host has no such record: {notes:?}"
            );
        } else {
            common::wait_until(&mut app, "the refusal", |app| {
                app.query_by_label_contains("related records").is_some()
            });
            assert!(app.state().connection_lost().is_some());
            assert!(shown(&app), "the row stays on screen");
        }
    }

    // A new parent the user made and typed, with a row begun, lost at the
    // save's op71 (New Record's own went through): nothing reached the
    // host, so the reconnect holds the parent again under a new id, with
    // the serial number New Record gave it (Count, auto-entered), and one
    // save stores it and the row once. With `refused`, the host refuses
    // the reconnect's op71 once: the window keeps the record as it was,
    // frozen, the host has none of it, and Try Again makes it.
    /// Notes::Serial, auto-entered (the new-parent cases' hosts).
    const ROW_SERIAL: u64 = 9;
    for refused in [false, true] {
        let hosted = host_configured(
            if refused {
                "lost-new-parent-refused"
            } else {
                "lost-new-parent"
            },
            |root| {
                fixture::set_field_options(root, TABLE, fixture::COUNT, 0x2);
                fixture::set_field_serial_increment(root, TABLE, fixture::COUNT, 5);
                // The portal's records take a serial number too, which the
                // begun row's preview issues before the loss: the save's
                // OnObjectSave reads the row as previewed (#1047).
                fixture::add_field(root, NOTES, ROW_SERIAL, "Serial", 1, 2, 0, 0x2, 0);
                fixture::set_field_serial_increment(root, NOTES, ROW_SERIAL, 5);
                add_object_triggers(root, PORTAL_BODY, &[(TriggerEvent::ObjectSave, BODY_SAVE)]);
            },
            |server| {
                let server = server.panicking_on_after(Opcode::HostCreateRecord as u8, 1);
                if refused {
                    server.refusing_create_after(1)
                } else {
                    server
                }
            },
        );
        let mut app = open(&hosted);
        expect(&mut app, "Show portals", "on Portals");
        let idle = |app: &common::App| !app.state().script_running() && app.state().is_settled();
        app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
        common::wait_until(&mut app, "the new record", idle);
        common::wait_until(&mut app, "the new record's fields", |app| {
            common::find_text_field(app, "Name").is_some()
        });
        common::fill(&mut app, "Name", "Fresh");
        app.query_all(body()).last().unwrap().focus();
        app.run_steps(2);
        app.input_mut()
            .events
            .push(egui::Event::Text("third".into()));
        app.run_steps(2);
        common::wait_until(&mut app, "unsaved record changes", common::unsaved);
        // Saved by a click on the layout: the row's OnObjectSave previews
        // it, which issues its serial number, before the commit is lost.
        click_background(&mut app);
        common::wait_until(&mut app, "the lost connection", |app| {
            app.state().connection_lost().is_some() && !app.state().script_running()
        });
        // Another client makes a record meanwhile: the parent's new id
        // comes after it, and the window finds the parent there.
        if !refused {
            let mut other = hosted.opened();
            let name = [("Name".to_owned(), Some("Elsewhere".to_owned()))]
                .into_iter()
                .collect();
            other.create_record(fixture::LAYOUT as u32, name).unwrap();
            other.close();
        }
        // A calculation's read on the lost connection is told in an alert.
        if app.state().error().is_some() {
            common::click(&mut app, "OK");
        }
        app.get_by_role_and_label(egui::accesskit::Role::Button, "Reconnect…")
            .click();
        common::wait_until(&mut app, "the reconnect dialog", |app| {
            common::find_text_field(app, "Password").is_some()
        });
        common::fill(&mut app, "Password", PASSWORD);
        app.get_by_role_and_label(egui::accesskit::Role::Button, "Reconnect")
            .click();
        let fresh_editable = |app: &common::App| {
            common::find_text_field(app, "Name")
                .and_then(|f| f.value())
                .as_deref()
                == Some("Fresh")
        };
        let host_has_fresh = || {
            stored(&hosted, TABLE, &[(NAME, 1)])
                .iter()
                .any(|(_, v)| v[0] == "Fresh")
        };
        if refused {
            common::wait_until(&mut app, "the failure kept", |app| {
                app.state().connection_lost().is_none()
                    && app
                        .query_by_label_contains("could not be made again on the new connection")
                        .is_some()
            });
            common::wait_until(&mut app, "the kept values", |app| {
                app.query_by_label("Fresh").is_some()
            });
            assert!(!fresh_editable(&app), "frozen");
            assert!(!host_has_fresh(), "the host has none of it");
            common::click(&mut app, "Try Again");
            common::wait_until(&mut app, "the record made again", |app| {
                idle(app)
                    && app
                        .query_by_label_contains("could not be made again")
                        .is_none()
                    && fresh_editable(app)
            });
        } else {
            // The reconnected bar says so; dismissed, the record is still
            // to save.
            common::wait_until(&mut app, "the file open again", |app| {
                app.state().connection_lost().is_none() && shown(app)
            });
            assert!(
                app.query_by_label_contains("made again on the new connection")
                    .is_some(),
                "the window says the record was made again"
            );
            common::wait_until(&mut app, "the typed parent", fresh_editable);
            app.get_by_role_and_label(egui::accesskit::Role::Button, "OK")
                .click();
            app.run_steps(2);
        }
        assert!(common::unsaved(&app), "the new record is still to save");
        assert!(!host_has_fresh(), "the host has none of it yet");
        common::save_record(&mut app);
        common::wait_until(&mut app, "the save", |app| {
            idle(app) && !common::unsaved(app)
        });
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        let tasks = stored(&hosted, TABLE, &[(NAME, 1), (fixture::COUNT, 1)]);
        let fresh: Vec<_> = tasks.iter().filter(|(_, v)| v[0] == "Fresh").collect();
        assert_eq!(fresh.len(), 1, "the parent once: {tasks:?}");
        assert_eq!(fresh[0].1[1], "1", "the serial number New Record gave it");
        if !refused {
            let elsewhere: Vec<_> = tasks.iter().filter(|(_, v)| v[0] == "Elsewhere").collect();
            assert_eq!(elsewhere.len(), 1, "the other client's record: {tasks:?}");
            assert_ne!(elsewhere[0].0, fresh[0].0, "a record of its own");
        }
        let notes = stored(&hosted, NOTES, &[(BODY, 1), (TASK, 1), (ROW_SERIAL, 1)]);
        let rows: Vec<_> = notes.iter().filter(|(_, v)| v[0] == "third").collect();
        assert_eq!(rows.len(), 1, "the row once: {notes:?}");
        assert_eq!(rows[0].1[1], "Fresh", "the row is the parent's");
        // The serial number its preview issued before the loss, not a
        // second one (6).
        assert_eq!(rows[0].1[2], "1", "the row's serial number: {notes:?}");
    }
}

/// The Portals layout, its objects given places in the tab order: Task
/// name 1, the portal's Body 2, Code 3, Setting 4.
fn portals_with_tab_order(tag: &str) -> common::App {
    let hosted = host_with(tag, |root| {
        for (object, place) in [
            (TASK_NAME, 1u16),
            (PORTAL_BODY, 2),
            (TASK_CODE, 3),
            (TASK_SETTING, 4),
        ] {
            root.put(
                &[
                    &[4],
                    &[5],
                    &uint_segment(PORTALS),
                    &[5],
                    &serial::encode(object),
                    &[7],
                ],
                OwnedNode::leaf(Vec::new(), place.to_be_bytes().to_vec()),
            );
        }
    });
    let mut app = open(&hosted);
    expect(&mut app, "Show portals", "on Portals");
    app
}

/// A Tab pressed in the very frame after a portal row's field took the
/// focus starts from that field: with a key typed after it, from the first
/// row the second Tab goes on to the creation row, which the key begins,
/// and no row in between gets it (#859).
#[test]
fn a_tab_right_after_entering_a_row_starts_from_that_row() {
    let mut app = portals_with_tab_order("tab-twice");
    let body = || {
        By::new()
            .role(egui::accesskit::Role::TextInput)
            .label("Notes::Body")
    };
    let focused_top = |app: &common::App| {
        app.query_all(body())
            .find(|node| node.is_focused())
            .map(|node| node.rect().min.y)
    };
    let tops = |app: &common::App| -> Vec<f32> {
        app.query_all(body())
            .map(|node| node.rect().min.y)
            .collect()
    };
    app.query_all(body()).next().unwrap().focus();
    app.run_steps(2);
    let tab = egui::Event::Key {
        key: egui::Key::Tab,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    app.event(tab.clone());
    // One frame at a time, until the second row has the focus.
    let second = tops(&app)[1];
    for _ in 0..20 {
        app.step();
        if focused_top(&app) == Some(second) {
            break;
        }
    }
    assert_eq!(focused_top(&app), Some(second));
    app.event(tab);
    app.event(egui::Event::Text("X".into()));
    app.run_steps(10);
    common::wait_until(&mut app, "the row the key begins", |app| {
        app.state().is_settled()
    });
    let typed: Vec<String> = app
        .query_all(body())
        .filter_map(|node| node.accesskit_node().value())
        .collect();
    assert_eq!(typed[..2], ["first", "second"], "{typed:?}");
    assert!(typed[2..].iter().any(|v| v == "X"), "{typed:?}");
}

/// Tab goes through a portal row by row, at the portal's place in the
/// layout's tab order, the creation row included, then on to the next
/// field (#859), as FileMaker Pro 26.0.3 goes through the Tasks starter's
/// attachment portal (Pro also stops on the row's buttons, #856). The key
/// typed with a Tab goes to the field entered, after its text.
#[test]
fn tab_goes_through_a_portal_row_by_row() {
    let mut app = portals_with_tab_order("tab-rows");
    let field = |label: &'static str| {
        By::new()
            .role(egui::accesskit::Role::TextInput)
            .label(label)
    };
    // The focused field: its label, and which of the fields of that label
    // it is from the top (the portal's rows: "first", "second", then the
    // creation row).
    let focused = |app: &common::App| {
        app.query_all(By::new().role(egui::accesskit::Role::TextInput))
            .find(|node| node.is_focused())
            .and_then(|node| {
                let label = node.accesskit_node().label()?;
                let top = node.rect().min.y;
                if label != "Notes::Body" {
                    return Some((label, 0));
                }
                let row = app
                    .query_all(field("Notes::Body"))
                    .filter(|other| other.rect().min.y < top)
                    .count();
                Some((label, row))
            })
    };
    let tab = |app: &mut common::App, typed: &str| {
        app.event(egui::Event::Key {
            key: egui::Key::Tab,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        if !typed.is_empty() {
            app.event(egui::Event::Text(typed.to_owned()));
        }
        app.run_steps(4);
        focused(app)
    };
    app.query_all(field("Notes::Body")).next().unwrap().focus();
    app.run_steps(2);
    let at = |label: &str, row: usize| Some((label.to_owned(), row));
    assert_eq!(focused(&app), at("Notes::Body", 0));
    assert_eq!(tab(&mut app, "Q"), at("Notes::Body", 1));
    let bodies = |app: &common::App| -> Vec<String> {
        app.query_all(field("Notes::Body"))
            .filter_map(|node| node.accesskit_node().value())
            .collect()
    };
    assert_eq!(bodies(&app)[1], "secondQ", "after the row's text");
    assert_eq!(tab(&mut app, ""), at("Notes::Body", 2), "the creation row");
    assert_eq!(tab(&mut app, ""), at("Code", 0));
    // Shift-Tab goes back the same way, as FileMaker Pro 26.0.3 does (#848).
    app.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::Tab);
    app.run_steps(4);
    assert_eq!(focused(&app), at("Notes::Body", 2));
    // Typing into the creation row begins a record and the portal is read
    // again; the Tab and key typed at once wait for the field they go to,
    // and are not lost.
    app.event(egui::Event::Text("N".into()));
    app.run_steps(2);
    let entered = tab(&mut app, "Z");
    common::wait_until(&mut app, "the portal read again", |app| {
        app.state().is_settled()
    });
    let code = common::text_field(&app, "Code").value().unwrap_or_default();
    let typed = bodies(&app);
    assert!(typed.iter().any(|v| v == "N"), "{typed:?}");
    assert!(
        typed.iter().any(|v| v == "Z") || code == "Z",
        "Z went to {entered:?}: bodies {typed:?}, Code {code:?}"
    );
}

/// A script that goes into the field of a portal's last row, as the
/// starter solutions' add buttons do (Go to Object, Go to Portal Row
/// [Last], Go to Field), leaves the keyboard in the creation row's editor:
/// typing begins a related record, which the save creates (#536).
#[test]
fn a_script_into_the_creation_row_gives_its_field_the_keyboard() {
    let hosted = host("add-note");
    let mut app = open(&hosted);
    expect(&mut app, "Show portals", "on Portals");
    run_without_dialog(&mut app, "Add note");
    app.run_steps(3);
    let body = || {
        By::new()
            .role(egui::accesskit::Role::TextInput)
            .label("Notes::Body")
    };
    // "first", "second", then the creation row.
    assert!(app.query_all(body()).nth(2).unwrap().is_focused());
    // Body takes at most 10 characters.
    app.event(egui::Event::Text("via button".into()));
    app.run_steps(3);
    common::wait_until(&mut app, "unsaved record changes", common::unsaved);
    common::save_record(&mut app);
    wait_saved(&mut app);
    let notes = stored(&hosted, NOTES, &[(BODY, 1), (TASK, 1)]);
    assert_eq!(
        notes.last().map(|(_, v)| v.clone()),
        Some(vec!["via button".to_owned(), "Write tests".to_owned()])
    );
}

/// Add `script` to the file.
fn add_script(root: &mut OwnedNode, script: &Script) {
    let node = script.encode();
    let id = node.segment.clone();
    root.put(&[&[17], &[5], &id], node);
}

/// A step into a pop-up menu in a portal row gives the menu the keyboard
/// with its list closed, as FileMaker Pro 26 leaves Assets' Used By menu
/// (#866): Go to Object on the field's own name, then Select All (the
/// starter's Go to Entry Field), in the first row when no row is active;
/// the add button's steps (Go to Object, Go to Portal Row [Last], Go to
/// Field) in the creation row.
#[test]
fn a_script_into_a_portal_rows_pop_up_menu_gives_it_the_keyboard() {
    let hosted = host_with("pop-up-focus", |root| {
        root.put(
            &[&[4], &[5], &uint_segment(PORTALS), &[7]],
            OwnedNode::leaf(Vec::new(), portal_store_shown(PORTAL_FLAGS, 2)),
        );
        let steps = vec![go_to_object("\"Note body\""), ScriptStep::new(50)];
        add_script(root, &fixture::script(63, "Enter note body", steps));
    });
    let mut app = open(&hosted);
    expect(&mut app, "Show portals", "on Portals");
    let focused = |app: &common::App| {
        app.query_all(
            By::new()
                .role(egui::accesskit::Role::ComboBox)
                .label("Notes::Body"),
        )
        .position(|menu| menu.is_focused())
    };
    assert_eq!(focused(&app), None);
    // No row is active: Go to Object enters the first row ("first").
    run_without_dialog(&mut app, "Enter note body");
    app.run_steps(3);
    assert_eq!(focused(&app), Some(0));
    assert!(!egui::Popup::is_any_open(&app.ctx));
    run_without_dialog(&mut app, "Add note");
    app.run_steps(3);
    // "first", "second", then the creation row.
    assert_eq!(focused(&app), Some(2));
    assert!(!egui::Popup::is_any_open(&app.ctx));
}

/// The tabbed Portals layout's tab control, its first panel (in front at
/// first) and its second, which holds the Notes portal.
const TABS: u64 = 20;
const FIRST_TAB: u64 = 21;
const NOTES_TAB: u64 = 22;

/// The Notes portal on the back panel of a tab control, not at the
/// layout's top level. Bounds are relative to the object's container.
fn tab_the_portal(root: &mut OwnedNode) {
    let layout = uint_segment(PORTALS);
    let store = om_layout::encode::without_objects(&portal_store(PORTAL_FLAGS), &[PORTAL])
        .expect("the portal at the top level");
    let mut tab = Vec::new();
    uint(3, 18, &mut tab);
    uint(5, FIRST_TAB, &mut tab);
    let mut control = Vec::new();
    uint(1, 11, &mut control);
    uint(2, TABS, &mut control);
    bytes(4, &rect(42.0, 5.0, 140.0, 420.0), &mut control);
    uint(6, FIRST_TAB, &mut control);
    uint(6, NOTES_TAB, &mut control);
    bytes(13, &tab, &mut control);
    let panel = |id: u64, left: u64, children: &[u64]| {
        let mut tab_panel = Vec::new();
        uint(2, left, &mut tab_panel);
        uint(3, 100, &mut tab_panel);
        let mut panel = Vec::new();
        uint(1, 12, &mut panel);
        uint(2, id, &mut panel);
        bytes(4, &rect(18.0, 0.0, 98.0, 415.0), &mut panel);
        for child in children {
            uint(6, *child, &mut panel);
        }
        bytes(14, &tab_panel, &mut panel);
        panel
    };
    let objects = [
        control,
        panel(FIRST_TAB, 0, &[]),
        panel(NOTES_TAB, 100, &[PORTAL]),
        portal_object(PORTAL_FLAGS, rect(1.0, 5.0, 71.0, 405.0)),
    ];
    let store = om_layout::encode::append_to_store(&store, &[TABS], &objects);
    root.put(
        &[&[4], &[5], &layout, &[7]],
        OwnedNode::leaf(Vec::new(), store),
    );
    for (object, name) in [(FIRST_TAB, "First tab"), (NOTES_TAB, "Notes tab")] {
        root.put(
            &[&[4], &[5], &layout, &[5], &serial::encode(object), &[16]],
            text_node(&[], name),
        );
    }
}

/// Go to Object brings the tab panel holding its object to the front, and
/// a tab panel it names, as FileMaker Pro 26 does for Meetings' Topics
/// portal and Inventory's Units on Hand panel (#866).
#[test]
fn go_to_object_brings_its_tab_panel_to_the_front() {
    let hosted = host_with("tab-panels", |root| {
        tab_the_portal(root);
        let notes = vec![go_to_object("\"Notes portal\"")];
        add_script(root, &fixture::script(64, "Front notes", notes));
        let first = vec![go_to_object("\"First tab\"")];
        add_script(root, &fixture::script(65, "Front first", first));
    });
    let mut app = open(&hosted);
    expect(&mut app, "Show portals", "on Portals");
    let bodies = |app: &common::App| {
        app.query_all(
            By::new()
                .role(egui::accesskit::Role::TextInput)
                .label("Notes::Body"),
        )
        .count()
    };
    assert_eq!(bodies(&app), 0);
    run_without_dialog(&mut app, "Front notes");
    // "first", "second" and the creation row.
    common::wait_until(&mut app, "the notes panel", |app| bodies(app) == 3);
    run_without_dialog(&mut app, "Front first");
    common::wait_until(&mut app, "the first panel", |app| bodies(app) == 0);
}

/// A click in a portal row outside its fields selects the row; Delete
/// Record then deletes its record, when the portal allows deleting its
/// records (`0x4`), with the record's next save (#189).
#[test]
fn delete_record_deletes_the_selected_portal_rows_record() {
    let hosted = host_with("browse-delete", |root| {
        let layout = uint_segment(PORTALS);
        root.put(
            &[&[4], &[5], &layout, &[7]],
            OwnedNode::leaf(Vec::new(), portal_store(PORTAL_FLAGS | 0x4)),
        );
    });
    let mut app = open(&hosted);
    expect(&mut app, "Show portals", "on Portals");
    let bodies = |app: &common::App| -> Vec<String> {
        app.query_all(
            By::new()
                .role(egui::accesskit::Role::TextInput)
                .label("Notes::Body"),
        )
        .filter_map(|n| n.accesskit_node().value())
        .collect()
    };
    // The rows paint once their related records are read.
    common::wait_until(&mut app, "the portal rows", |app| {
        bodies(app) == ["first", "second", ""]
    });
    // The row's own click (its center is on the Body field, which a
    // pointer click enters).
    app.get_by_label("Portal Row 2").click_accesskit();
    app.run_steps(2);
    common::click(&mut app, "Delete Record");
    common::wait_for_label(&mut app, "Delete this related record?");
    common::click(&mut app, "Delete");
    app.run_steps(3);
    assert_eq!(bodies(&app), ["first", ""], "the row leaves the portal");
    common::save_record(&mut app);
    wait_saved(&mut app);
    let ids: Vec<u64> = stored(&hosted, NOTES, &[])
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(ids, [1, 2]);
}

/// In a portal that sorts its rows (Browse shows "second" first) and
/// allows deleting them, the user's edit of a row, a record begun in its
/// creation row and a deleted row are each saved to the record that row
/// shows, not to the one the relationship's order puts at that row number
/// (#1027): the relationship's order is notes 2, 3 where Browse shows 3, 2.
/// A draft whose row moves before the save is not covered here.
#[test]
fn a_sorted_portals_rows_are_edited_created_and_deleted_by_record() {
    let hosted = host_with("sorted-writes", |root| {
        sort_portal(root);
        root.put(
            &[&[4], &[5], &uint_segment(PORTALS), &[7]],
            OwnedNode::leaf(Vec::new(), portal_store(PORTAL_FLAGS | 0x8 | 0x4)),
        );
    });
    let mut app = open(&hosted);
    expect(&mut app, "Show portals", "on Portals");
    let body = || {
        By::new()
            .role(egui::accesskit::Role::TextInput)
            .label("Notes::Body")
    };
    let bodies = |app: &common::App| -> Vec<String> {
        app.query_all(body())
            .filter_map(|n| n.accesskit_node().value())
            .collect()
    };
    let note = |hosted: &hosted::Hosted, id: u64| {
        stored(hosted, NOTES, &[(BODY, 1)])
            .into_iter()
            .find(|(record, _)| *record == id)
            .map(|(_, values)| values[0].clone())
    };
    common::wait_until(&mut app, "the sorted rows", |app| {
        bodies(app) == ["second", "first", ""]
    });

    // Row 1 is note 3 ("second").
    retype_first_row(&mut app, "zz edited");
    common::save_record(&mut app);
    wait_saved(&mut app);
    assert_eq!(note(&hosted, 3).as_deref(), Some("zz edited"));
    assert_eq!(note(&hosted, 2).as_deref(), Some("first"));

    // The creation row, one key a frame.
    common::wait_until(&mut app, "the rows after the save", |app| {
        bodies(app) == ["zz edited", "first", ""]
    });
    app.query_all(body()).nth(2).unwrap().focus();
    app.run_steps(2);
    for key in ["t", "h", "i", "r", "d"] {
        app.input_mut().events.push(egui::Event::Text(key.into()));
        app.run_steps(2);
    }
    common::wait_until(&mut app, "unsaved record changes", common::unsaved);
    common::save_record(&mut app);
    wait_saved(&mut app);
    let notes = stored(&hosted, NOTES, &[(BODY, 1), (TASK, 1)]);
    assert_eq!(
        notes.last().map(|(_, v)| v.clone()),
        Some(vec!["third".to_owned(), "Write tests".to_owned()])
    );

    // Sorted again: "zz edited", "third", "first". Delete "first"'s row.
    common::wait_until(&mut app, "the created row in its place", |app| {
        bodies(app) == ["zz edited", "third", "first", ""]
    });
    app.get_by_label("Portal Row 3").click_accesskit();
    app.run_steps(2);
    common::click(&mut app, "Delete Record");
    common::wait_for_label(&mut app, "Delete this related record?");
    common::click(&mut app, "Delete");
    app.run_steps(3);
    assert_eq!(
        bodies(&app),
        ["zz edited", "third", ""],
        "the row leaves the portal"
    );
    common::save_record(&mut app);
    wait_saved(&mut app);
    assert_eq!(note(&hosted, 2), None, "note 2 (\"first\") is deleted");
    assert_eq!(note(&hosted, 3).as_deref(), Some("zz edited"));
}

/// The creation row shows the row's buttons and other objects, over no
/// record (#536: Tasks' attachment row): its button runs in that row, where
/// the related fields read empty and Set Field creates a related record
/// (script-runtime.md 13.24).
#[test]
fn a_button_in_the_creation_row_runs_in_that_row() {
    let hosted = host("creation-button");
    let mut app = open(&hosted);
    expect(&mut app, "Show portals", "on Portals");
    // "first", "second", then the creation row's.
    click_row_button(&mut app, 3);
    expect_shown(&mut app, "row :3:Write tests|3::Write tests|0:");
    let notes = stored(&hosted, NOTES, &[(BODY, 1), (TASK, 1)]);
    assert_eq!(
        notes.last().map(|(_, v)| v.clone()),
        Some(vec!["!".to_owned(), "Write tests".to_owned()])
    );
}

/// Two Set Field steps of a creation-row button write one new related
/// record: the first creates it, and the row is then that record's.
#[test]
fn a_creation_row_buttons_writes_make_one_record() {
    let hosted = host_with("creation-two-writes", |root| {
        let script = fixture::script(
            ROW_BUTTON_SCRIPT,
            "Row button",
            vec![
                set_field(NOTES_OCCURRENCE, BODY, 0, "\"one\""),
                set_field(NOTES_OCCURRENCE, BODY, 0, "\"two\""),
                without_dialog(75),
                show("\"made \" & Get ( LastError )"),
            ],
        );
        add_script(root, &script);
    });
    let mut app = open(&hosted);
    expect(&mut app, "Show portals", "on Portals");
    click_row_button(&mut app, 3);
    expect_shown(&mut app, "made 0");
    let notes = stored(&hosted, NOTES, &[(BODY, 1), (TASK, 1)]);
    assert_eq!(notes.len(), 4, "one note made: {notes:?}");
    assert_eq!(
        notes.last().map(|(_, v)| v.clone()),
        Some(vec!["two".to_owned(), "Write tests".to_owned()])
    );
}

/// A record a script created through the creation row keeps the match
/// field the script then set: the window shows the record as a row of its
/// own until the user's save, which does not set its match fields again.
#[test]
fn a_scripts_match_field_on_a_record_it_created_stays_at_the_save() {
    let hosted = host_with("creation-own-match", |root| {
        let script = fixture::script(
            62,
            "Move note",
            vec![
                go_to_object("\"Notes portal\""),
                go_to_portal_row(2),
                set_field(NOTES_OCCURRENCE, BODY, 0, "\"moved\""),
                set_field(NOTES_OCCURRENCE, TASK, 0, "\"Elsewhere\""),
            ],
        );
        add_script(root, &script);
    });
    let mut app = open(&hosted);
    expect(&mut app, "Show portals", "on Portals");
    run_without_dialog(&mut app, "Move note");
    common::wait_until(&mut app, "unsaved record changes", common::unsaved);
    common::save_record(&mut app);
    wait_saved(&mut app);
    let notes = stored(&hosted, NOTES, &[(BODY, 1), (TASK, 1)]);
    assert_eq!(
        notes.last().map(|(_, v)| v.clone()),
        Some(vec!["moved".to_owned(), "Elsewhere".to_owned()]),
        "{notes:?}"
    );
}

/// Notes::Picture, a container field (#568).
const PICTURE: u64 = 5;

/// Insert File into a portal row's related container (#568): the file goes
/// into the related table's library and the row's record holds it, with
/// the window's record's commit; another session reads it back.
#[test]
fn insert_file_fills_a_portal_rows_container() {
    let temporary = std::env::temp_dir().join(format!("OpenMaker-S{}", std::process::id()));
    std::fs::create_dir_all(&temporary).unwrap();
    std::fs::write(temporary.join("row.png"), fixture::png()).unwrap();
    let hosted = host_with("row-container", |root| {
        let (t, f) = (uint_segment(NOTES), uint_segment(PICTURE));
        let mut definition = vec![0; 26];
        definition[0] = 1;
        definition[1] = 6;
        definition[24..26].copy_from_slice(&1u16.to_be_bytes());
        root.put(
            &[&t, &[3], &[5], &f, &[2]],
            OwnedNode::leaf(Vec::new(), definition),
        );
        root.put(&[&t, &[3], &[5], &f, &[16]], text_node(&[], "Picture"));
        let mut insert = ScriptStep::new(131);
        insert.set_field_spec(0, Some(field_spec(NOTES_OCCURRENCE, PICTURE, 0)));
        insert.flags |= flag::NO_INTERACT;
        let script = fixture::script(
            ROW_BUTTON_SCRIPT,
            "Row button",
            vec![
                note("$p", "Get ( TemporaryPath ) & \"row.png\""),
                transfer::with_paths(insert, &["$p"]),
                note("$insert", "Get ( LastError )"),
                without_dialog(75),
                show("\"inserted \" & $insert & \":\" & Get ( LastError )"),
            ],
        );
        add_script(root, &script);
    });
    let mut app = open(&hosted);
    expect(&mut app, "Show portals", "on Portals");
    // Row 1, Notes record 2 ("first").
    click_row_button(&mut app, 1);
    expect_shown(&mut app, "inserted 0:0");
    let mut other = hosted.opened();
    let data = other
        .calc_data(&[CalcNeed::Record {
            table: NOTES,
            record: 2,
        }])
        .unwrap();
    let values = data.records[&(NOTES, 2)].as_deref().unwrap();
    // The record holds a reference to a member of Notes' library
    // (containers.md 1.4).
    let text = values
        .get(PICTURE, 1)
        .map(|v| {
            v.to_text(&om_data::calc::Ctx::new(
                &om_data::calc::Locale::en_us(),
                om_data::calc::Date::from_ymd(2026, 11, 13).unwrap(),
            ))
            .into_string()
        })
        .unwrap_or_default();
    assert!(
        om_data::containers::reference(&text).is_some(),
        "a library reference: {text:?}"
    );
    other.close();
    let _ = std::fs::remove_file(temporary.join("row.png"));
}

/// Normal portal Insert carries the accepted UI row version into preparation;
/// the worker's later record reads cannot supply that pre-upload baseline.
#[test]
fn root_and_portal_attachment_inserts_reconnect_before_the_first_save() {
    #[path = "../../om-provider/tests/common/related.rs"]
    mod related;
    use om_protocol::Opcode;
    const REMOTE_NOTES: u64 = 0xd0_0060;
    let temporary = std::env::temp_dir().join(format!("OpenMaker-S{}", std::process::id()));
    std::fs::create_dir_all(&temporary).unwrap();
    std::fs::write(temporary.join("two-owner-root.txt"), b"Root attachment").unwrap();
    std::fs::write(temporary.join("two-owner-row.txt"), b"Related attachment").unwrap();
    let insert = |occurrence, field, filename: &str| {
        let mut step = ScriptStep::new(131);
        step.set_field_spec(0, Some(field_spec(occurrence, field, 0)));
        step.flags |= flag::NO_INTERACT;
        vec![
            note("$p", &format!("Get ( TemporaryPath ) & \"{filename}\"")),
            transfer::with_paths(step, &["$p"]),
            show("\"attachment held \" & Get ( LastError )"),
        ]
    };
    let mut root = fixture::tree(
        &[fixture::script(
            19,
            "Show portals",
            vec![
                fixture::go_to_layout(PORTALS),
                fixture::go_to_record(1),
                show("\"on \" & Get ( LayoutName )"),
            ],
        )],
        true,
    );
    fixture::add_calculated(&mut root);
    add_portal_layout(&mut root);
    add_extra(&mut root);
    add_notes(&mut root);
    fixture::add_field(&mut root, TABLE, fixture::PHOTO, "Photo", 1, 6, 0, 0, 0);
    add_script(
        &mut root,
        &fixture::script(
            60,
            "Hold Root attachment",
            insert(OCCURRENCE, fixture::PHOTO, "two-owner-root.txt"),
        ),
    );
    let mut portal_insert = vec![go_to_object("\"Notes portal\""), go_to_portal_row(1)];
    portal_insert.extend(insert(REMOTE_NOTES, PICTURE, "two-owner-row.txt"));
    add_script(
        &mut root,
        &fixture::script(ROW_BUTTON_SCRIPT, "Hold portal attachment", portal_insert),
    );
    let mut owner = fixture::tree(&[], true);
    fixture::add_calculated(&mut owner);
    add_notes(&mut owner);
    add_extra(&mut owner);
    fixture::add_field(&mut owner, NOTES, PICTURE, "Picture", 1, 6, 0, 0, 0);
    related::reference(
        &mut root,
        (REMOTE_NOTES, "Remote Notes"),
        1,
        "Related Notes",
    );
    // The ordinary data-source definition names Notes in its owning file.
    let key = |id| {
        let s = uint_segment(id);
        [vec![s.len() as u8 + 1, s.len() as u8], s].concat()
    };
    root.put(
        &[&[3], &[17], &[5], &uint_segment(REMOTE_NOTES), &[2]],
        OwnedNode::leaf(Vec::new(), [key(1), key(NOTES)].concat()),
    );
    // A separate foreign occurrence keeps the local Notes validation's own
    // context valid. Use the product layout writer for the external portal.
    fixture::add_relationship(&mut root, 7, (OCCURRENCE, NAME), 0, (REMOTE_NOTES, TASK));
    let bounds = |top, left, bottom, right| om_layout::Rect {
        top,
        left,
        bottom,
        right,
        other: Vec::new(),
    };
    let objects = vec![
        om_layout::encode::portal_object(
            PORTAL,
            &bounds(60.0, 10.0, 130.0, 410.0),
            &[PORTAL_BODY],
            alias(REMOTE_NOTES),
            3,
        ),
        om_layout::encode::portal_field_object(
            PORTAL_BODY,
            &bounds(0.0, 0.0, 20.0, 300.0),
            alias(REMOTE_NOTES),
            BODY,
        ),
        om_layout::encode::field_object(
            TASK_NAME,
            &bounds(10.0, 10.0, 30.0, 200.0),
            alias(OCCURRENCE),
            NAME,
        ),
    ];
    root.put(
        &[&[4], &[5], &uint_segment(PORTALS), &[7]],
        OwnedNode::leaf(
            Vec::new(),
            om_layout::encode::append_to_store(&[], &[PORTAL, TASK_NAME], &objects),
        ),
    );
    fixture::index_scripts(&mut root);
    let host = related::Host::new(
        "portal-attachment-before-save",
        vec![(fixture::FILE, root), ("Related Notes.fmp12", owner)],
    );
    let mut app = common::harness_sized(Box::new(host.provider()), egui::vec2(1060.0, 1300.0));
    common::click(&mut app, "Use saved license");
    common::fill(&mut app, "Host", "127.0.0.1");
    hosted::replace(&mut app, "Port", &host.port.to_string());
    common::fill(&mut app, "File", fixture::FILE);
    common::fill(&mut app, "Account", ACCOUNT);
    common::fill(&mut app, "Password", PASSWORD);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "the two-file portal", |a| {
        assert!(a.state().error().is_none(), "{:?}", a.state().error());
        a.state().is_open() && a.state().is_settled()
    });
    let setup = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        expect(&mut app, "Show portals", "on Portals");
        common::wait_until(&mut app, "the accepted related rows", |a| {
            a.query_all(
                By::new()
                    .role(egui::Role::TextInput)
                    .label("Remote Notes::Body"),
            )
            .any(|row| row.value().as_deref() == Some("first"))
        });
    }));
    if let Err(panic) = setup {
        let labels: Vec<_> = app
            .query_all(By::new())
            .map(|n| {
                let node = n.accesskit_node();
                (node.role(), node.label(), node.value())
            })
            .collect();
        eprintln!(
            "setup error {:?}, script {}, labels {labels:?}",
            app.state().error(),
            app.state().script_running()
        );
        std::panic::resume_unwind(panic);
    }
    // A portal button intentionally saves before its script. Use the normal
    // Scripts commands, starting with Related before Root has any acquisition.
    expect(&mut app, "Hold portal attachment", "attachment held 0");
    expect(&mut app, "Hold Root attachment", "attachment held 0");
    // The calculation/display refresh can read the host clock. These are
    // record/container mutation counts; the provider vector also proves its
    // recovery operation itself makes no timestamp request.
    let wire = || {
        [
            Opcode::HostCreateRecord,
            Opcode::UploadListToHostTempLocation,
            Opcode::HostCommit,
            Opcode::HostGetSerial,
            Opcode::UploadBinaryData,
            Opcode::AdjustReferenceCount,
        ]
        .map(|op| host.captures.with_opcode(op).len())
    };
    assert_eq!(wire(), [0, 0, 0, 0, 2, 0]);
    let read = || {
        let mut root = host.open(fixture::FILE);
        let page = root.record_by_id(PORTALS as u32, 1).unwrap();
        let root_data = root
            .calc_data(&[CalcNeed::Record {
                table: TABLE,
                record: 1,
            }])
            .unwrap();
        assert_eq!(page.records[0].id, 1);
        let related_table = root
            .calc_schema()
            .unwrap()
            .unwrap()
            .table_of(REMOTE_NOTES)
            .unwrap()
            .id;
        let row_data = root
            .calc_data(&[CalcNeed::Record {
                table: related_table,
                record: 2,
            }])
            .unwrap();
        let values = (
            root_data.records[&(TABLE, 1)]
                .as_ref()
                .unwrap()
                .get(fixture::PHOTO, 1)
                .is_some(),
            row_data.records[&(related_table, 2)]
                .as_ref()
                .unwrap()
                .get(PICTURE, 1)
                .is_some(),
        );
        root.close();
        values
    };
    assert_eq!(read(), (false, false));
    let before = wire();
    host.handle.close_connections();
    common::open_menu(&mut app, "Records");
    common::click(&mut app, "Refresh Window");
    common::wait_until(&mut app, "the lost connection", |a| {
        a.query_by_label("Reconnect…").is_some()
    });
    common::click(&mut app, "Reconnect…");
    common::wait_until(&mut app, "reconnect password", |a| {
        common::find_text_field(a, "Password").is_some()
    });
    common::fill(&mut app, "Password", PASSWORD);
    app.get_by_role_and_label(egui::Role::Button, "Reconnect")
        .click();
    common::wait_until(&mut app, "both attachment drafts reconnected", |a| {
        assert!(a.state().error().is_none(), "{:?}", a.state().error());
        a.state().connection_lost().is_none() && a.state().is_settled()
    });
    assert_eq!(
        wire(),
        before,
        "recovery makes no record or container mutation"
    );
    assert_eq!(read(), (false, false));
    common::save_record(&mut app);
    common::wait_until(&mut app, "explicit two-file Save", |a| {
        assert!(a.state().error().is_none(), "{:?}", a.state().error());
        a.state().is_settled() && !common::editing(a)
    });
    assert_eq!(read(), (true, true));
    assert_eq!(wire()[4], 2, "each attachment uploads only once");
    let _ = std::fs::remove_file(temporary.join("two-owner-root.txt"));
    let _ = std::fs::remove_file(temporary.join("two-owner-row.txt"));
}

/// Going to another record commits what scripts left on the one the user
/// leaves, its related record too.
#[test]
fn leaving_the_record_commits_what_scripts_left_on_it() {
    let hosted = host("left-next");
    let mut app = open(&hosted);
    leave(&mut app);
    common::click(&mut app, "Next");
    wait_saved(&mut app);
    common::wait_until(&mut app, "the next record", |app| {
        common::record_number(app) == "2"
    });
    assert_eq!(
        left_values(&hosted),
        (
            vec!["a".to_owned(), "ab".to_owned(), String::new()],
            "pending".to_owned()
        )
    );
    expect(&mut app, "Status", "status 0::: tx 1");
}

/// The user types into a global on a record scripts left changes on: the
/// save commits the record with them and the typed value is the
/// session's global, as when the user saves a record alone (no 201); the
/// host never sees it.
#[test]
fn a_typed_global_stays_the_sessions_when_the_save_commits_what_scripts_left() {
    let hosted = host("left-global");
    let mut app = open(&hosted);
    leave(&mut app);
    common::text_field(&app, "Setting").focus();
    app.step();
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    common::text_field(&app, "Setting").type_text("typed");
    app.run_steps(2);
    common::save_record(&mut app);
    wait_saved(&mut app);
    assert_eq!(
        left_values(&hosted),
        (
            vec!["a".to_owned(), "ab".to_owned(), String::new()],
            "pending".to_owned()
        )
    );
    expect(&mut app, "Read setting", "setting typed:a");
    let mut other = hosted.opened();
    let globals = other
        .calc_data(&[CalcNeed::Globals { table: TABLE }])
        .unwrap();
    assert!(globals.globals[&TABLE].get(SETTING, 1).is_none());
    other.close();
}

/// A portal that sorts its own rows (Browse shows "second" first) numbers
/// them for scripts as Browse shows them (#192; Claris, Go to Portal Row):
/// Go to Portal Row 1 is note 3 ("second"), which Set Field changes and
/// Delete Portal Row deletes; Go to Related Record then finds the related
/// note left, and the commit keeps note 2.
#[test]
fn a_sorted_portals_rows_are_numbered_as_browse_shows_them() {
    let hosted = host_with("sorted", display_sorted);
    let mut app = open(&hosted);
    expect(
        &mut app,
        "Sorted",
        "sorted row 0:1:second, set 0, delete 0, related 0:Notes, commit 0:first",
    );
    assert_eq!(
        stored(&hosted, NOTES, &[(BODY, 1)]),
        [(1, vec!["Hi".to_owned()]), (2, vec!["first".to_owned()])]
    );
}

/// A portal that filters its rows numbers them for scripts as Browse
/// shows them (#192): Go to Portal Row 1 is the first row the filter
/// admits, and Go to Related Record with "Show only related records" from
/// that row finds the records the portal shows, not every related record.
/// A filter by row number counts the portal's rows, as Browse's does.
#[test]
fn a_filtered_portals_rows_and_related_records_are_the_ones_it_shows() {
    for (tag, filter, expected) in [
        (
            "filtered",
            filter_portal as fn(&mut OwnedNode),
            "filtered row 0:1:second, related 0:Notes:1:second",
        ),
        (
            "filtered-rows",
            filter_portal_by_row,
            "filtered row 0:1:first, related 0:Notes:1:first",
        ),
    ] {
        let hosted = host_with(tag, filter);
        let mut app = open(&hosted);
        expect(&mut app, "Filtered", expected);
    }
}

/// Click the button of portal row `row` (from 1, as the window shows the
/// rows, top to bottom).
fn click_row_button(app: &mut common::App, row: usize) {
    let buttons = By::new()
        .role(egui::accesskit::Role::Button)
        .label(ROW_ACTION);
    common::wait_until(app, "the portal rows' buttons", |app| {
        app.query_all(buttons.clone()).count() >= row
    });
    let mut buttons: Vec<_> = app
        .query_all(buttons)
        .map(|n| (n.accesskit_node().bounding_box().map_or(0.0, |b| b.y0), n))
        .collect();
    buttons.sort_by(|a, b| a.0.total_cmp(&b.0));
    buttons.into_iter().nth(row - 1).unwrap().1.click();
    app.run_steps(2);
}

/// A button in a portal row runs its Perform Script in that row: the
/// parameter and `Get ( ActivePortalRowNumber )` see the clicked row, a
/// related Set Field writes its record, and the window's record stays the
/// script's. The user's typing is saved first. In a portal that sorts its
/// rows the row is the record Browse shows there, not the relationship's
/// row of that number; Tasks record 2 ("Ship"), whose id is the Notes
/// record "first"'s, is never read or written.
#[test]
fn a_portal_row_button_runs_in_its_row_after_the_users_save() {
    for (tag, sorted, expected, notes) in [
        (
            "row-button",
            false,
            "row second:2:Write tests|2:second:Write tests|0:ok",
            ["first", "second!"],
        ),
        (
            "row-button-sorted",
            true,
            "row first:2:Write tests|2:first:Write tests|0:ok",
            ["first!", "second"],
        ),
    ] {
        let hosted = host_with(tag, |root| {
            if sorted {
                display_sorted(root);
            }
        });
        let tasks = stored(&hosted, TABLE, &[(NAME, 1), (CODE, 1)]);
        let mut app = open(&hosted);
        expect(&mut app, "Show portals", "on Portals");
        common::text_field(&app, "Code").focus();
        app.step();
        // Code takes at most 3 characters (its validation).
        common::text_field(&app, "Code").type_text("ok");
        app.run_steps(2);
        click_row_button(&mut app, 2);
        expect_shown(&mut app, expected);
        let after = stored(&hosted, NOTES, &[(BODY, 1)]);
        assert_eq!(
            after,
            [
                (1, vec!["Hi".to_owned()]),
                (2, vec![notes[0].to_owned()]),
                (3, vec![notes[1].to_owned()])
            ],
            "{tag}"
        );
        let after = stored(&hosted, TABLE, &[(NAME, 1), (CODE, 1)]);
        assert_eq!(after[0].1, ["Write tests", "ok"], "{tag}");
        assert_eq!(after[1..], tasks[1..], "{tag}");
    }
}

/// A row button's click that the focused field's OnObjectExit holds goes
/// on only when the trigger lets it, in the row of the record it was
/// clicked on even when the trigger moved that record to another row,
/// and never when the trigger refuses.
#[test]
fn a_held_portal_row_click_follows_its_record_or_goes() {
    let hosted = host_with("row-button-held", |root| {
        display_sorted(root);
        add_code_exit_trigger(root);
    });
    let mut app = open(&hosted);
    expect(&mut app, "Show portals", "on Portals");
    // The trigger renames "first" to "zzz", which then sorts before the
    // clicked "second": that record is row 2 when the button runs.
    run_without_dialog(&mut app, "Mode reorder");
    common::text_field(&app, "Code").focus();
    app.step();
    click_row_button(&mut app, 1);
    expect_shown(&mut app, "row second:2:Write tests|2:second:Write tests|0:");
    let reordered = [
        (1, vec!["Hi".to_owned()]),
        (2, vec!["zzz".to_owned()]),
        (3, vec!["second!".to_owned()]),
    ];
    assert_eq!(stored(&hosted, NOTES, &[(BODY, 1)]), reordered);
    // The trigger refuses: the field keeps the focus and the click goes.
    run_without_dialog(&mut app, "Mode veto");
    common::text_field(&app, "Code").focus();
    app.step();
    click_row_button(&mut app, 1);
    expect_shown(&mut app, "vetoed 0");
    common::wait_until(&mut app, "the refusing trigger", |app| {
        !app.state().script_running() && common::text_field(app, "Code").is_focused()
    });
    app.run_steps(10);
    assert!(app.query(By::new().label_contains("row ")).is_none());
    assert!(!app.state().script_running());
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    assert_eq!(stored(&hosted, NOTES, &[(BODY, 1)]), reordered);
}

/// Go to Field [ ] leaves the field the focus is in, a portal row's too:
/// the record is committed (s33) and no row is active (13.24).
#[test]
fn go_to_field_without_a_field_leaves_a_portal_field_and_commits() {
    let hosted = host("field-off");
    let mut app = open(&hosted);
    expect(&mut app, "Field off", "off 0:0::0");
    assert_eq!(
        stored(&hosted, NOTES, &[(BODY, 1)])[1],
        (2, vec!["edited".to_owned()])
    );
}

/// Go to Object checks a repetition against the field's own definition,
/// found by its catalog key (a field numbered 128 or more has a key that
/// reads otherwise).
#[test]
fn go_to_object_finds_a_portal_fields_repetitions_by_its_catalog_key() {
    let hosted = host("extra");
    let mut app = open(&hosted);
    expect(&mut app, "Extra", "extra 0:Note extra:1");
}

/// Go to Object's repetition is the repetition of the portal field that
/// has the focus: `Get ( ActiveRepetitionNumber )` and `Get (
/// ActiveFieldContents )` answer it in the row.
#[test]
fn go_to_object_focuses_the_portal_fields_repetition_it_names() {
    let hosted = host("extra-two");
    let mut app = open(&hosted);
    expect(&mut app, "Extra two", "extra2 0:1:2:two");
    // The first related note is record 2; its data has the field's
    // catalog key.
    assert_eq!(
        stored(&hosted, NOTES, &[(extra_raw(), 1), (extra_raw(), 2)])[1],
        (2, vec!["one".to_owned(), "two".to_owned()])
    );
}

/// A field of the layout's table that takes the focus from a portal
/// field's repetition, by Go to Object or Go to Field, answers the focus
/// selectors itself: repetition 1, its own name and object, no portal row
/// (no portal is active).
#[test]
fn a_field_outside_the_portal_takes_the_focus_from_a_portal_repetition() {
    let hosted = host("leave-extra");
    let mut app = open(&hosted);
    expect(
        &mut app,
        "Leave extra",
        "leave 2:Extra:Note extra:1:0 | 1:Name:Task name:0:0 | 1:Name:Task name:0:0",
    );
}

/// A script's change to a later repetition only is an uncommitted
/// modification of the record (`Get ( RecordOpenState )` 2), as its
/// `Get ( ModifiedFields )` says.
#[test]
fn a_change_to_a_later_repetition_only_leaves_the_record_modified() {
    let hosted = host("repetition-only");
    let mut app = open(&hosted);
    expect(&mut app, "Repetition only", "reps 2:1:Status:ab");
}

/// A record Delete Portal Row deleted is one of its table: another
/// table's related record with the same id is still written.
#[test]
fn a_deleted_portal_row_is_its_tables_record_only() {
    let hosted = host_with("other-table", add_other_tasks);
    let mut app = open(&hosted);
    expect(&mut app, "Other table", "other set 0, commit 0");
    assert_eq!(
        stored(&hosted, TABLE, &[(CODE, 1)]),
        [(1, vec![String::new()]), (2, vec!["zz".to_owned()])]
    );
    let notes: Vec<u64> = stored(&hosted, NOTES, &[])
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(notes, [1, 3]);
}

/// Go to Portal Row leaves the field the focus is in first: its
/// OnObjectExit returning False keeps the row.
#[test]
fn an_object_exit_trigger_keeps_the_portal_row() {
    let hosted = host_with("veto", add_exit_trigger);
    let mut app = open(&hosted);
    expect(&mut app, "Veto", "veto 1, allowed 2");
}

/// A single save can clear the first repetition and fill another.
#[test]
fn a_browse_save_validates_the_proposed_repetitions_of_a_required_global() {
    let hosted = host_with("required-global-repetitions", |root| {
        fixture::add_field(root, TABLE, SETTING, "Setting", 1, 2, 0, 0x1_0000, 0x8);
        fixture::set_field_repetitions(root, TABLE, SETTING, 2);
        let second = repetition_object(
            602,
            OCCURRENCE,
            SETTING,
            (2, 1),
            rect(164.0, 120.0, 184.0, 320.0),
        );
        let store =
            om_layout::encode::append_to_store(&portal_store(PORTAL_FLAGS), &[602], &[second]);
        root.put(
            &[&[4], &[5], &uint_segment(PORTALS), &[7]],
            OwnedNode::leaf(Vec::new(), store),
        );
        add_script(
            root,
            &fixture::script(
                12,
                "Read setting",
                vec![show(
                    r#""setting " & Tasks::Setting & ":" & Tasks::Setting[2]"#,
                )],
            ),
        );
    });
    let mut app = open(&hosted);
    expect(&mut app, "Show portals", "on Portals");
    common::text_field(&app, "Setting").focus();
    app.step();
    app.event(egui::Event::Text("7".into()));
    app.run_steps(2);
    common::save_record(&mut app);
    wait_saved(&mut app);

    common::text_field(&app, "Setting").focus();
    app.step();
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.key_press(egui::Key::Backspace);
    app.run_steps(2);
    common::text_field(&app, "Tasks::Setting").focus();
    app.step();
    app.event(egui::Event::Text("3".into()));
    app.run_steps(2);
    common::save_record(&mut app);
    wait_saved(&mut app);
    expect(&mut app, "Read setting", "setting :3");
}

/// Typing only into a text, number or date global and saving writes no record
/// (the value is the session's): OnWindowTransaction does not run for it.
#[test]
fn saving_only_a_typed_global_runs_no_window_transaction() {
    let shot = |app: &mut common::App, name: &str| {
        if let Some(dir) = std::env::var_os("OM_SCREENSHOTS") {
            app.set_render_cursor(false);
            app.render()
                .expect("offscreen render")
                .save(Path::new(&dir).join(format!("{name}.png")))
                .expect("write png");
        }
    };
    for repetition in [1, 2] {
        for (data_type, typed, displayed) in [
            (1, "typed", "typed"),
            (2, "1234.5", "1234.5"),
            (3, "01/02/2003", "01/02/2003"),
            (4, "13:30", "13:30:00"),
            (5, "1/2/2003 13:30", "1/2/2003 1:30 PM"),
        ] {
            let hosted = host_with(&format!("global-only-{data_type}"), |root| {
                fixture::add_field(
                    root, TABLE, SETTING, "Setting", 1, data_type, 0, 0x1_0000, 0,
                );
                fixture::set_field_repetitions(root, TABLE, SETTING, 2);
                if repetition == 2 {
                    let store = om_layout::encode::without_objects(
                        &portal_store(PORTAL_FLAGS),
                        &[TASK_SETTING],
                    )
                    .unwrap();
                    let object = repetition_object(
                        TASK_SETTING,
                        OCCURRENCE,
                        SETTING,
                        (2, 1),
                        rect(140.0, 120.0, 160.0, 320.0),
                    );
                    let store =
                        om_layout::encode::append_to_store(&store, &[TASK_SETTING], &[object]);
                    root.put(
                        &[&[4], &[5], &uint_segment(PORTALS), &[7]],
                        OwnedNode::leaf(Vec::new(), store),
                    );
                    add_script(
                        root,
                        &fixture::script(
                            12,
                            "Read setting",
                            vec![show(
                                "\"setting \" & Tasks::Setting[2] & \":\" & Tasks::Setting",
                            )],
                        ),
                    );
                }
            });
            let before = stored(&hosted, TABLE, &[(NAME, 1), (CODE, 1)]);
            let mut app = open(&hosted);
            expect(&mut app, "Show portals", "on Portals");
            let kind = match data_type {
                3 => "date",
                4 => "time",
                5 => "timestamp",
                _ => "number",
            };
            if data_type != 1 {
                shot(&mut app, &format!("{kind}-global-before-entry"));
            }
            let label = if repetition == 1 {
                "Setting"
            } else {
                "Tasks::Setting"
            };
            common::text_field(&app, label).focus();
            app.step();
            app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
            common::text_field(&app, label).type_text(typed);
            app.run_steps(2);
            common::save_record(&mut app);
            wait_saved(&mut app);
            if data_type != 1 {
                shot(&mut app, &format!("{kind}-global-after-save"));
            }
            expect(&mut app, "Status", "status 0:::first/second tx ");
            expect(&mut app, "Read setting", &format!("setting {displayed}:"));
            assert_eq!(stored(&hosted, TABLE, &[(NAME, 1), (CODE, 1)]), before);
            let mut other = hosted.opened();
            let data = other
                .calc_data(&[CalcNeed::Globals { table: TABLE }])
                .unwrap();
            assert!(data.globals[&TABLE]
                .get(SETTING, 1)
                .is_none_or(|value| value.is_empty()));
            assert!(data.globals[&TABLE]
                .get(SETTING, 2)
                .is_none_or(|value| value.is_empty()));
            other.close();
            if data_type >= 3 {
                common::text_field(&app, label).focus();
                app.step();
                app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
                common::text_field(&app, label).type_text("not a date");
                app.run_steps(2);
                common::save_record(&mut app);
                common::wait_until(&mut app, "invalid global date refused", |app| {
                    app.state().error().is_some() && !app.state().script_running()
                });
                assert_eq!(
                    app.state().error(),
                    Some(match data_type {
                        4 => "Field Setting requires a valid time.",
                        5 => "Field Setting requires a valid date and time.",
                        _ => "Field Setting requires a valid date.",
                    })
                );
                assert_eq!(
                    common::text_field(&app, label).accesskit_node().value(),
                    Some("not a date".into())
                );
                shot(&mut app, "date-global-refused");
                assert_eq!(stored(&hosted, TABLE, &[(NAME, 1), (CODE, 1)]), before);
            }
        }
    }
    // A number or date global validated by its entry rules is offered, its
    // typing checked as it is applied; one validated by calculation, or
    // prohibiting user modification, is not.
    for (tag, data_type, options, validation, offered) in [
        ("validated", 2, 0x1_0000, 0x1000, true),
        ("calculated", 2, 0x1_0000, 0x1, false),
        ("prohibited", 2, 0x1_0100, 0, false),
        ("date-validated", 3, 0x1_0000, 0x2000, true),
        ("date-calculated", 3, 0x1_0000, 0x1, false),
        ("date-prohibited", 3, 0x1_0100, 0, false),
    ] {
        let hosted = host_with(tag, |root| {
            fixture::add_field(
                root, TABLE, SETTING, "Setting", 1, data_type, 0, options, validation,
            );
            // Its formula, so the table's other fields are planned and only
            // the global is left out.
            if validation & 0x1 != 0 {
                let mut names = fixture::names();
                names.add_field(TABLE, SETTING, "Setting");
                let formula = "not IsEmpty ( Setting )";
                fixture::add_formula(root, &names, (TABLE, OCCURRENCE), SETTING, 0x19, formula);
            }
        });
        let mut app = open(&hosted);
        expect(&mut app, "Show portals", "on Portals");
        assert_eq!(
            common::find_text_field(&app, "Setting").is_some(),
            offered,
            "{tag}"
        );
        assert!(common::find_text_field(&app, "Name").is_some(), "{tag}");
        // Numeric only: the ordinary save refuses other typing, and the
        // field keeps it.
        if tag == "validated" {
            common::text_field(&app, "Setting").focus();
            app.step();
            common::text_field(&app, "Setting").type_text("12abc");
            app.run_steps(2);
            common::save_record(&mut app);
            common::wait_until(&mut app, "non-numeric global refused", |app| {
                app.state().error().is_some() && !app.state().script_running()
            });
            assert!(
                app.state().error().is_some_and(|e| e.contains("Setting")),
                "{:?}",
                app.state().error()
            );
            assert_eq!(
                common::text_field(&app, "Setting").accesskit_node().value(),
                Some("12abc".into())
            );
        }
    }
}

/// Browse types into later repetitions, each into its own cell: a field
/// object showing Tasks::Status's repetitions 2 and 3 gives each its own
/// editor, the first takes "ab", and a portal row's Notes::Extra[2]
/// "later". The record and its related record are saved in one commit,
/// with the other repetitions unchanged, and a later repetition keeps its
/// field's validation (#185).
#[test]
fn browse_repetitions_keep_their_cell_and_share_one_record_commit() {
    // Tasks::Status, repetitions 2 and 3, in Tasks::Setting's place, and
    // the portal's Notes::Extra showing its second repetition.
    let repetitions = |root: &mut OwnedNode| {
        let store = portal_store_with(
            PORTAL_FLAGS,
            0,
            repetition_object(
                PORTAL_EXTRA,
                NOTES_OCCURRENCE,
                EXTRA,
                (2, 1),
                rect(0.0, 310.0, 20.0, 390.0),
            ),
            repetition_object(
                TASK_SETTING,
                OCCURRENCE,
                STATUS,
                (2, 2),
                rect(140.0, 120.0, 180.0, 220.0),
            ),
        );
        root.put(
            &[&[4], &[5], &uint_segment(PORTALS), &[7]],
            OwnedNode::leaf(Vec::new(), store),
        );
    };
    let hosted = host_with("browse-repetitions", repetitions);
    let extra = || {
        By::new()
            .role(egui::accesskit::Role::TextInput)
            .label("Notes::Extra")
    };
    // Tasks::Status[2] and [3].
    let later = || {
        By::new()
            .role(egui::accesskit::Role::TextInput)
            .label("Tasks::Status")
    };
    let mut app = open(&hosted);
    expect(&mut app, "Show portals", "on Portals");
    common::wait_until(&mut app, "later repetition editors", |app| {
        app.query_all(later()).count() == 2 && app.query_all(extra()).count() == 3
    });
    // Each half of the object, stacked.
    let rects: Vec<egui::Rect> = app.query_all(later()).map(|n| n.rect()).collect();
    assert!(
        rects[1].top() - rects[0].top() > 15.0 && rects[0].height() < 25.0,
        "{rects:?}"
    );
    retype(&mut app, "Code", "b");
    app.query_all(later()).next().unwrap().focus();
    app.step();
    app.query_all(later()).next().unwrap().type_text("ab");
    app.run_steps(2);
    // The first portal row's Notes::Extra[2]: note 2, "first".
    app.query_all(extra()).next().unwrap().focus();
    app.step();
    app.query_all(extra()).next().unwrap().type_text("later");
    app.run_steps(2);
    common::save_record(&mut app);
    wait_saved(&mut app);
    let own = stored(
        &hosted,
        TABLE,
        &[(CODE, 1), (STATUS, 1), (STATUS, 2), (STATUS, 3)],
    );
    assert_eq!(own[0].1, ["b", "", "ab", ""]);
    let notes = stored(&hosted, NOTES, &[(extra_raw(), 1), (extra_raw(), 2)]);
    assert_eq!(
        notes
            .iter()
            .find(|(id, _)| *id == 2)
            .map(|(_, v)| v.clone()),
        Some(vec![String::new(), "later".to_owned()])
    );
    assert_eq!(
        notes
            .iter()
            .find(|(id, _)| *id == 3)
            .map(|(_, v)| v.clone()),
        Some(vec![String::new(), String::new()])
    );
    expect(&mut app, "Status", "status 0:b:ab:first/second tx 1");
    // A later repetition keeps its field's validation (3 characters).
    app.query_all(later()).next().unwrap().focus();
    app.step();
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    app.query_all(later()).next().unwrap().type_text("long");
    app.run_steps(2);
    common::save_record(&mut app);
    common::wait_until(&mut app, "later repetition validation", |app| {
        app.state().error().is_some()
    });
    assert!(app.state().error().unwrap().contains("3"));
    assert_eq!(stored(&hosted, TABLE, &[(STATUS, 2)])[0].1, ["ab"]);
    // Its editor keeps the refused typing once the record is drawn again.
    common::wait_until(&mut app, "the refused repetition's typing", |app| {
        app.query_all(later())
            .next()
            .and_then(|n| n.accesskit_node().value())
            .as_deref()
            == Some("long")
    });

    // Only Status[2] typed, and its OnObjectSave setting it again: the
    // commit stores the script's value, not the typing it replaced, and
    // the editor shows it.
    let hosted = host_with("browse-repetitions-script", |root| {
        repetitions(root);
        add_object_triggers(
            root,
            TASK_SETTING,
            &[(TriggerEvent::ObjectSave, UPPER_STATUS)],
        );
    });
    let mut app = open(&hosted);
    expect(&mut app, "Show portals", "on Portals");
    common::wait_until(&mut app, "later repetition editors", |app| {
        app.query_all(later()).count() == 2
    });
    app.query_all(later()).next().unwrap().focus();
    app.step();
    app.query_all(later()).next().unwrap().type_text("ab");
    app.run_steps(2);
    // Leaving the box runs its OnObjectSave: the script's value shows at
    // once, before the record is committed.
    app.key_press(egui::Key::Tab);
    let shown = |app: &common::App| {
        app.query_all(later())
            .next()
            .and_then(|n| n.accesskit_node().value())
    };
    common::wait_until(&mut app, "the script's value", |app| {
        app.state().is_settled() && shown(app).as_deref() == Some("AB")
    });
    assert_eq!(stored(&hosted, TABLE, &[(STATUS, 2)])[0].1, [""]);
    common::save_record(&mut app);
    wait_saved(&mut app);
    assert_eq!(
        stored(&hosted, TABLE, &[(STATUS, 1), (STATUS, 2), (STATUS, 3)])[0].1,
        ["", "AB", ""]
    );
    assert_eq!(
        app.query_all(later())
            .next()
            .and_then(|n| n.accesskit_node().value()),
        Some("AB".into())
    );
    // Typing after it and leaving: the script sets "ABC", uncommitted.
    // Entered again, the box opens with the script's value, not the stored
    // one, and typing goes on from it.
    app.query_all(later()).next().unwrap().focus();
    app.step();
    app.key_press(egui::Key::End);
    app.query_all(later()).next().unwrap().type_text("c");
    app.run_steps(2);
    assert_eq!(shown(&app).as_deref(), Some("ABc"));
    app.key_press(egui::Key::Tab);
    common::wait_until(&mut app, "the script's ABC", |app| {
        app.state().is_settled() && shown(app).as_deref() == Some("ABC")
    });
    app.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::Tab);
    app.event(egui::Event::Text("d".into()));
    app.run_steps(2);
    assert_eq!(shown(&app).as_deref(), Some("ABCd"));
    // Erased back to the stored value, the box still replaces the
    // script's value.
    app.key_press(egui::Key::Backspace);
    app.key_press(egui::Key::Backspace);
    app.run_steps(2);
    assert_eq!(shown(&app).as_deref(), Some("AB"));
    common::save_record(&mut app);
    wait_saved(&mut app);
    assert_eq!(stored(&hosted, TABLE, &[(STATUS, 2)])[0].1, ["AB"]);
    assert_eq!(shown(&app).as_deref(), Some("AB"));

    // Its OnObjectSave setting a global of another table whose field has
    // Status's id: the typing in Status[2] stays and is saved.
    let hosted = host_with("browse-repetitions-other-global", |root| {
        repetitions(root);
        fixture::add_field(root, NOTES, STATUS, "gStatus", 1, 1, 0, 0x1_0000, 0);
        fixture::set_field_repetitions(root, NOTES, STATUS, 2);
        add_object_triggers(
            root,
            TASK_SETTING,
            &[(TriggerEvent::ObjectSave, NOTE_GLOBAL)],
        );
    });
    let mut app = open(&hosted);
    expect(&mut app, "Show portals", "on Portals");
    common::wait_until(&mut app, "later repetition editors", |app| {
        app.query_all(later()).count() == 2
    });
    app.query_all(later()).next().unwrap().focus();
    app.step();
    app.query_all(later()).next().unwrap().type_text("ab");
    app.run_steps(2);
    common::save_record(&mut app);
    wait_saved(&mut app);
    assert_eq!(
        stored(&hosted, TABLE, &[(STATUS, 1), (STATUS, 2), (STATUS, 3)])[0].1,
        ["", "ab", ""]
    );
}

/// A commit in one window shows in the file's other windows: the first
/// window reads the record W2 changed again.
#[test]
fn a_commit_in_one_window_refreshes_the_others() {
    let hosted = host("windows");
    let mut app = open(&hosted);
    expect(&mut app, "Second window", "back 0:Tasks");
    common::wait_until(&mut app, "both windows' Code", |app| {
        let codes: Vec<String> = app
            .query_all(
                By::new()
                    .role(egui::accesskit::Role::TextInput)
                    .label("Code"),
            )
            .map(|n| n.accesskit_node().value().unwrap_or_default())
            .collect();
        codes == ["w2", "w2"]
    });
}

/// OnRecordCommit is a UI pre-event trigger. Its Action is delivered
/// before its Finished event, so the save it releases queues behind it.
fn add_commit_gate(root: &mut OwnedNode) {
    let event = TriggerEvent::RecordCommit.id();
    let record = om_layout::triggers::encode_record(&om_layout::ScriptTrigger {
        event,
        script: Some(21),
        file_reference: None,
        external: false,
        modes: 1,
        option: 0,
        extra: Vec::new(),
        parameter: None,
    });
    let layout = uint_segment(PORTALS);
    let event_key = uint_segment(u64::from(event));
    root.put(
        &[&[4], &[5], &layout, &[0x30], &[4]],
        OwnedNode::leaf(Vec::new(), 1u16.to_be_bytes().to_vec()),
    );
    root.put(
        &[&[4], &[5], &layout, &[0x30], &[5], &event_key, &[1]],
        OwnedNode::leaf(Vec::new(), record),
    );
}

fn run_without_dialog(app: &mut common::App, name: &str) {
    common::open_menu(app, "Scripts");
    common::click(app, name);
    common::wait_until(app, "the script", |app| !app.state().script_running());
}

/// Wait until the commit gate (OnRecordCommit, script 21) that an action
/// after the window's `started`th script start runs has run and ended. The
/// action starts it only once the record's field exit is through, a frame
/// or more later, so a wait for no running script alone can pass before it
/// starts; and the window discards the user's next menu command while it
/// runs (#544).
fn wait_commit_gate(app: &mut common::App, started: usize, what: &str) {
    common::wait_until(app, what, |app| {
        app.state().scripts_started()[started..]
            .iter()
            .any(|start| start == "RecordCommit 21")
            && !app.state().script_running()
    });
}

#[test]
fn a_trigger_released_save_finishes_before_navigation_and_rearms_the_trigger() {
    let hosted = host_with("commit-navigation", add_commit_gate);
    let mut app = open(&hosted);
    run_without_dialog(&mut app, "Allow commit");
    leave(&mut app);
    common::click(&mut app, "Next");
    wait_saved(&mut app);
    common::wait_until(&mut app, "the next record after the worker commit", |app| {
        common::record_number(app) == "2"
    });
    assert_eq!(left_values(&hosted).0[..2], ["a", "ab"]);
    // One trigger when leaving record 1; the worker commit consumes the
    // prior approval instead of firing a second copy.
    expect(&mut app, "Show commits", "commits 1");
    run_without_dialog(&mut app, "Refuse commit");
    common::text_field(&app, "Code").focus();
    app.step();
    common::text_field(&app, "Code").type_text("vetoed");
    app.run_steps(2);
    let started = app.state().scripts_started().len();
    common::save_record(&mut app);
    wait_commit_gate(&mut app, started, "the veto");
    common::wait_until(&mut app, "unsaved record changes", common::unsaved);
    expect(&mut app, "Show commits", "commits 2");
    assert_eq!(stored(&hosted, TABLE, &[(CODE, 1)])[1].1, [""]);
}

#[test]
fn deleting_the_first_related_row_moves_scalar_reads_and_writes_to_the_next_one() {
    let hosted = host("delete-then-edit");
    let mut app = open(&hosted);
    expect(
        &mut app,
        "Delete then edit first",
        "first second, list second, written 0:after, count 1",
    );
    assert_eq!(
        stored(&hosted, NOTES, &[(BODY, 1)]),
        [(1, vec!["Hi".to_owned()]), (3, vec!["after".to_owned()]),]
    );
}

fn add_mode_gates(root: &mut OwnedNode) {
    add_commit_gate(root);
    let event = TriggerEvent::ModeExit.id();
    let record = om_layout::triggers::encode_record(&om_layout::ScriptTrigger {
        event,
        script: Some(31),
        file_reference: None,
        external: false,
        modes: 1,
        option: 0,
        extra: Vec::new(),
        parameter: None,
    });
    let layout = uint_segment(PORTALS);
    let key = uint_segment(u64::from(event));
    root.put(
        &[&[4], &[5], &layout, &[0x30], &[4]],
        OwnedNode::leaf(Vec::new(), 2u16.to_be_bytes().to_vec()),
    );
    root.put(
        &[&[4], &[5], &layout, &[0x30], &[5], &key, &[1]],
        OwnedNode::leaf(Vec::new(), record),
    );
}
fn enter_find(app: &mut common::App) {
    common::open_menu(app, "View");
    common::click(app, "Find Mode");
}
fn wait_find(app: &mut common::App) {
    common::wait_until(app, "Find mode after saving", |app| {
        app.state().mode() == om_gui::Mode::Find
            && !app.state().script_running()
            && common::find_text_field(app, "Name").is_some()
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

#[test]
fn entering_find_commits_repetitions_and_related_edits_once_before_changing_mode() {
    let hosted = host_with("mode-save", add_mode_gates);
    let mut app = open(&hosted);
    run_without_dialog(&mut app, "Allow mode");
    leave(&mut app);
    enter_find(&mut app);
    wait_find(&mut app);
    assert_eq!(
        left_values(&hosted),
        (
            vec!["a".into(), "ab".into(), String::new()],
            "pending".into()
        )
    );
    // Perform Find and a later commit cannot revive the old drafts.
    common::fill(&mut app, "Name", "Write tests");
    common::click(&mut app, "Perform Find");
    common::wait_until(&mut app, "the find", |app| {
        app.state().is_settled() && app.state().mode() == om_gui::Mode::Browse
    });
    expect(&mut app, "Show gates", "gates 1:1");
    expect(&mut app, "Status", "status 0:a:ab:pending/second tx 1");
}

#[test]
fn a_commit_veto_keeps_browse_mode_and_the_pending_record() {
    let hosted = host_with("mode-commit-veto", add_mode_gates);
    let mut app = open(&hosted);
    run_without_dialog(&mut app, "Allow mode");
    leave(&mut app);
    run_without_dialog(&mut app, "Refuse commit");
    let before = left_values(&hosted);
    let started = app.state().scripts_started().len();
    enter_find(&mut app);
    wait_commit_gate(&mut app, started, "the commit veto");
    assert_eq!(app.state().mode(), om_gui::Mode::Browse);
    assert_eq!(left_values(&hosted), before);
    common::wait_until(&mut app, "unsaved record changes", common::unsaved);
    expect(&mut app, "Show gates", "gates 1:");
    run_without_dialog(&mut app, "Allow mode");
    enter_find(&mut app);
    wait_find(&mut app);
}

#[test]
fn a_mode_exit_veto_leaves_every_pending_change_uncommitted() {
    let hosted = host_with("mode-exit-veto", add_mode_gates);
    let mut app = open(&hosted);
    run_without_dialog(&mut app, "Refuse mode");
    leave(&mut app);
    let before = left_values(&hosted);
    enter_find(&mut app);
    common::wait_until(&mut app, "the mode veto", |app| {
        !app.state().script_running()
    });
    assert_eq!(app.state().mode(), om_gui::Mode::Browse);
    assert_eq!(left_values(&hosted), before);
    common::wait_until(&mut app, "unsaved record changes", common::unsaved);
    expect(&mut app, "Show gates", "gates 1:1");
    run_without_dialog(&mut app, "Allow mode");
    enter_find(&mut app);
    wait_find(&mut app);
}

#[test]
fn a_user_save_keeps_a_scripts_new_record_and_its_repetitions() {
    let hosted = host("worker-new-save");
    let mut app = open(&hosted);
    run_without_dialog(&mut app, "Leave new");
    common::wait_until(&mut app, "unsaved record changes", common::unsaved);
    common::save_record(&mut app);
    wait_saved(&mut app);
    let rows = stored(&hosted, TABLE, &[(NAME, 1), (STATUS, 2)]);
    assert_eq!(rows.len(), 3);
    assert_eq!(rows.last().unwrap().1, ["New task", "new"]);
}

#[test]
fn a_user_revert_removes_a_scripts_new_record() {
    let hosted = host("worker-new-revert");
    let before = stored(&hosted, TABLE, &[(NAME, 1)]);
    let mut app = open(&hosted);
    run_without_dialog(&mut app, "Leave new");
    common::wait_until(&mut app, "unsaved record changes", common::unsaved);
    common::revert_record(&mut app);
    wait_saved(&mut app);
    assert_eq!(stored(&hosted, TABLE, &[(NAME, 1)]), before);
}

#[test]
fn performing_find_after_a_ui_mode_change_keeps_pending_related_and_repeating_edits() {
    let hosted = host("mode-save-without-triggers");
    let mut app = open(&hosted);
    leave(&mut app);
    enter_find(&mut app);
    wait_find(&mut app);
    common::fill(&mut app, "Name", "Write tests");
    common::click(&mut app, "Perform Find");
    common::wait_until(&mut app, "the completed find", |app| {
        app.state().is_settled() && app.state().mode() == om_gui::Mode::Browse
    });
    assert_eq!(
        left_values(&hosted),
        (
            vec!["a".into(), "ab".into(), String::new()],
            "pending".into()
        )
    );
}

/// The Portals windows of "Two portal windows": window 1 ("Tasks") on
/// task 1 with Code set by the script and not committed, W2 in front on
/// its own found set (task 1 omitted).
fn two_portal_windows(tag: &str) -> (hosted::Hosted, common::App) {
    let hosted = host_with(tag, |root| {
        add_layout_trigger(root, TriggerEvent::RecordCommit, "commit")
    });
    let mut app = open(&hosted);
    common::open_menu(&mut app, "Scripts");
    common::click(&mut app, "Two portal windows");
    common::wait_until(
        &mut app,
        "both rendered windows and the pending edit",
        |app| {
            !app.state().script_running()
                && app.state().is_settled()
                && app
                    .query_all(
                        By::new()
                            .role(egui::accesskit::Role::TextInput)
                            .label("Code"),
                    )
                    .next()
                    .and_then(|field| field.value())
                    .as_deref()
                    == Some("w1")
        },
    );
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    assert_eq!(app.state_mut().window_titles()[0], "W2");
    (hosted, app)
}

/// Click the control `label` of the window behind the front one (the
/// root window, drawn first). The test backend embeds the windows in one
/// screen, the front one over the others, so the click goes to the
/// control itself (AccessKit), as a click in that window's own native
/// window would.
fn click_behind(app: &mut common::App, label: &str) {
    common::wait_until(app, label, |app| {
        app.query_all(By::new().role(egui::accesskit::Role::Button).label(label))
            .next()
            .is_some_and(|button| !button.accesskit_node().is_disabled())
    });
    app.query_all(By::new().role(egui::accesskit::Role::Button).label(label))
        .next()
        .unwrap_or_else(|| panic!("no {label}"))
        .click_accesskit();
    app.run_steps(2);
}

fn wait_quiet(app: &mut common::App) {
    common::wait_until(app, "the window to settle", |app| {
        !app.state().script_running() && !common::saving(app)
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

/// A quiet frame can occur before the worker starts the queued save.
/// Wait for its observable completion: the root window's record position
/// and the value independently read from the synthetic server. The tests
/// still assert the full stored rows and both windows' script context.
fn wait_behind_commit(hosted: &hosted::Hosted, app: &mut common::App, number: &str) {
    common::wait_until(app, "the background commit and record position", |app| {
        app.query_all(
            By::new()
                .role(egui::accesskit::Role::TextInput)
                .label("Record Number"),
        )
        .next()
        .and_then(|field| field.value())
        .as_deref()
            == Some(number)
            && codes(hosted) == ["w1", ""]
    });
    wait_quiet(app);
}

/// Next in the window behind the front one: its OnRecordCommit runs, its
/// record (not the front window's) is committed and it goes to its next
/// record; the front window stays on its own found set and record.
#[test]
fn next_behind_the_front_window_commits_and_moves_that_window() {
    let (hosted, mut app) = two_portal_windows("held-next");
    click_behind(&mut app, "Next");
    wait_behind_commit(&hosted, &mut app, "2");
    assert_eq!(
        stored(&hosted, TABLE, &[(CODE, 1)]),
        [(1, vec!["w1".to_owned()]), (2, vec![String::new()])]
    );
    expect(&mut app, "Where", "where 2/2/Ship 1/1/Ship events commit;");
}

/// Replace the text of the field labelled `label` by typing.
fn retype(app: &mut common::App, label: &str, text: &str) {
    common::text_field(app, label).focus();
    app.step();
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    common::text_field(app, label).type_text(text);
    app.run_steps(2);
}

/// The Portals layout on task 1 with the given layout triggers (each
/// logged by [`COUNT`] in `$$events`), the events of getting there
/// cleared.
fn portals_with_triggers(
    tag: &str,
    triggers: &'static [(TriggerEvent, &'static str)],
) -> (hosted::Hosted, common::App) {
    let hosted = host_with(tag, |root| {
        for (event, name) in triggers {
            add_layout_trigger(root, *event, name);
        }
    });
    let mut app = open(&hosted);
    expect(&mut app, "Show portals", "on Portals");
    run_without_dialog(&mut app, "Reset events");
    (hosted, app)
}

/// The user's typed record saved by Next: OnRecordCommit runs, the next
/// record's OnRecordLoad runs after the save (OnWindowTransaction, which
/// starts after it, does not drop it), and the next record's own typed
/// save runs OnRecordCommit again.
#[test]
fn a_typed_save_on_next_runs_record_load_and_rearms_record_commit() {
    let (hosted, mut app) = portals_with_triggers(
        "typed-next",
        &[
            (TriggerEvent::RecordCommit, "commit"),
            (TriggerEvent::RecordLoad, "load"),
        ],
    );
    retype(&mut app, "Code", "x");
    common::click(&mut app, "Next");
    wait_saved(&mut app);
    common::wait_until(&mut app, "record 2", |app| {
        common::record_number(app) == "2"
    });
    retype(&mut app, "Code", "y");
    common::save_record(&mut app);
    wait_saved(&mut app);
    assert_eq!(
        stored(&hosted, TABLE, &[(CODE, 1)]),
        [(1, vec!["x".to_owned()]), (2, vec!["y".to_owned()])]
    );
    expect(&mut app, "Show events", "events commit;load;commit; tx 2");
}

/// A typed save refused on the way to another layout keeps the record and
/// the layout; trying again runs OnLayoutExit again.
#[test]
fn a_refused_typed_save_on_leaving_the_layout_runs_its_exit_again_on_retry() {
    let (hosted, mut app) =
        portals_with_triggers("typed-layout-retry", &[(TriggerEvent::LayoutExit, "exit")]);
    // Code is validated Always: at most 3 characters.
    retype(&mut app, "Code", "toolong");
    common::select_layout(&mut app, "Tasks");
    common::wait_until(&mut app, "the refusal", |app| {
        app.state().error().is_some() && !app.state().script_running()
    });
    common::click(&mut app, "OK");
    app.run_steps(2);
    assert!(
        common::find_text_field(&app, "Code").is_some(),
        "still on Portals"
    );
    retype(&mut app, "Code", "ok");
    common::select_layout(&mut app, "Tasks");
    common::wait_until(&mut app, "the Tasks layout", |app| {
        common::find_text_field(app, "Code").is_none() && !app.state().script_running()
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    assert_eq!(stored(&hosted, TABLE, &[(CODE, 1)])[0].1, ["ok"]);
    expect(&mut app, "Show events", "events exit;exit; tx 1");
}

/// Entering Find mode saves the user's typing after OnRecordCommit and
/// OnModeExit; back in Browse mode the next typed save runs OnRecordCommit
/// again.
#[test]
fn a_typed_save_before_find_mode_rearms_record_commit_back_in_browse() {
    let (hosted, mut app) = portals_with_triggers(
        "typed-find",
        &[
            (TriggerEvent::RecordCommit, "commit"),
            (TriggerEvent::ModeExit, "mode"),
        ],
    );
    retype(&mut app, "Code", "f1");
    enter_find(&mut app);
    common::wait_until(&mut app, "Find mode", |app| {
        app.state().mode() == om_gui::Mode::Find && !app.state().script_running()
    });
    assert_eq!(stored(&hosted, TABLE, &[(CODE, 1)])[0].1, ["f1"]);
    common::open_menu(&mut app, "View");
    common::click(&mut app, "Browse Mode");
    common::wait_until(&mut app, "Browse mode", |app| {
        app.state().mode() == om_gui::Mode::Browse
            && !app.state().script_running()
            && common::find_text_field(app, "Code").is_some()
    });
    retype(&mut app, "Code", "f2");
    common::save_record(&mut app);
    wait_saved(&mut app);
    assert_eq!(stored(&hosted, TABLE, &[(CODE, 1)])[0].1, ["f2"]);
    expect(
        &mut app,
        "Show events",
        "events commit;mode;mode;commit; tx 2",
    );
}

/// Window 1 ("Tasks", in front) on task 1 and W2 on task 2, each with
/// Code set by the script and not committed: "w1", and `w2` in W2.
fn dirty_windows(id: u64, name: &str, w2: &str) -> Script {
    fixture::script(
        id,
        name,
        vec![
            fixture::on(86, true),
            fixture::go_to_layout(PORTALS),
            fixture::go_to_record(1),
            new_window("W2"),
            fixture::go_to_record(2),
            set_field(OCCURRENCE, CODE, 0, &format!("\"{w2}\"")),
            select_window("Tasks"),
            set_field(OCCURRENCE, CODE, 0, "\"w1\""),
        ],
    )
}

fn codes(hosted: &hosted::Hosted) -> Vec<String> {
    stored(hosted, TABLE, &[(CODE, 1)])
        .into_iter()
        .map(|(_, v)| v[0].clone())
        .collect()
}

/// File > Close closes every window of the file, each committing what
/// was left on its record first (the window behind the front one too).
#[test]
fn closing_the_file_commits_every_windows_record_first() {
    let hosted = host("close-dirty");
    let mut app = open(&hosted);
    run_without_dialog(&mut app, "Dirty windows");
    assert_eq!(app.state_mut().window_titles().len(), 2);
    open_root_menu(&mut app, "File");
    app.get_by_role_and_label(egui::accesskit::Role::Button, "Close")
        .click();
    common::wait_until(&mut app, "the file to close", |app| {
        !app.state().is_open() && !app.state().script_running()
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    assert_eq!(codes(&hosted), ["w1", "w2"]);
}

/// A window whose record cannot be committed stops closing the file: it
/// stays open with its changes; the window closed before it kept its own.
#[test]
fn a_refused_commit_stops_closing_the_file_with_that_window_open() {
    let hosted = host("close-refused");
    let mut app = open(&hosted);
    run_without_dialog(&mut app, "Dirty refusal");
    open_root_menu(&mut app, "File");
    app.get_by_role_and_label(egui::accesskit::Role::Button, "Close")
        .click();
    common::wait_until(&mut app, "the refusal", |app| {
        app.state().error().is_some() && !app.state().script_running()
    });
    common::click(&mut app, "OK");
    app.run_steps(2);
    assert!(app.state().is_open());
    assert_eq!(app.state_mut().window_titles(), ["W2"]);
    common::wait_until(&mut app, "unsaved record changes", common::unsaved);
    assert_eq!(
        common::find_text_field(&app, "Code")
            .and_then(|f| f.value())
            .as_deref(),
        Some("toolong")
    );
    assert_eq!(codes(&hosted), ["w1", ""]);
}

/// The root window's close box quits as Quit does: every window commits
/// and closes first.
#[test]
fn the_root_close_box_commits_every_window_before_quitting() {
    let hosted = host("close-box");
    let mut app = open(&hosted);
    run_without_dialog(&mut app, "Dirty windows");
    app.input_mut()
        .viewports
        .entry(egui::ViewportId::ROOT)
        .or_default()
        .events
        .push(egui::ViewportEvent::Close);
    app.step();
    common::wait_until(&mut app, "every window to close", |app| {
        !app.state().is_open() && !app.state().script_running()
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    assert_eq!(codes(&hosted), ["w1", "w2"]);
}

/// A file opened after another one closed starts from a fresh window: not
/// the closed window's title or hidden toolbar.
#[test]
fn a_file_opened_again_starts_from_a_fresh_window() {
    let hosted = host("reopen");
    let mut app = open(&hosted);
    run_without_dialog(&mut app, "Restyle");
    assert_eq!(app.state_mut().window_titles(), ["Renamed"]);
    common::open_menu(&mut app, "File");
    common::click(&mut app, "Close");
    common::wait_until(&mut app, "the file to close", |app| !app.state().is_open());
    // The Hosts form keeps what was entered but the password.
    common::fill(&mut app, "Password", PASSWORD);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "the file again", |app| app.state().is_settled());
    assert_eq!(app.state_mut().window_titles(), ["Tasks (127.0.0.1)"]);
    assert!(
        app.query(By::new().label("Show All")).is_some(),
        "toolbar shown"
    );
}

/// Open the menu `menu` of the root window, drawn first.
fn open_root_menu(app: &mut common::App, menu: &str) {
    app.query_all(By::new().role(egui::accesskit::Role::Button).label(menu))
        .next()
        .unwrap()
        .click();
    app.run_steps(2);
}

/// Closing the file runs OnWindowClose for each window in turn (here from
/// the layout's trigger cache, which the window consults as the file's):
/// the first window closes after committing, the second one's veto stops
/// closing the file with that window open and its changes kept.
#[test]
fn each_window_runs_its_close_trigger_and_a_veto_stops_closing_the_file() {
    let hosted = host_with("close-triggers", |root| {
        add_layout_trigger_script(root, TriggerEvent::WindowClose, CLOSE_GATE, "close")
    });
    let mut app = open(&hosted);
    run_without_dialog(&mut app, "Dirty windows");
    open_root_menu(&mut app, "File");
    app.get_by_role_and_label(egui::accesskit::Role::Button, "Close")
        .click();
    common::wait_until(&mut app, "the veto", |app| {
        !app.state().script_running() && app.query(By::new().label("W2")).is_none()
    });
    app.run_steps(4);
    assert!(app.state().is_open());
    assert_eq!(app.state_mut().window_titles(), ["W2"]);
    common::wait_until(&mut app, "unsaved record changes", common::unsaved);
    assert_eq!(codes(&hosted), ["w1", ""]);
    expect(&mut app, "Show events", "events close;close; tx 1");
}

/// Run `script` and wait until it pauses.
fn run_to_pause(app: &mut common::App, script: &str) {
    app.query_all(
        By::new()
            .role(egui::accesskit::Role::Button)
            .label("Scripts"),
    )
    .next()
    .unwrap()
    .click();
    app.run_steps(2);
    common::click(app, script);
    common::wait_until(app, "the pause", |app| app.state().script_paused());
}

/// While a script is paused, run `script` on top of it and check its
/// dialog shows `expected`; the paused script stays paused.
fn expect_paused(app: &mut common::App, script: &str, expected: &str) {
    app.query_all(
        By::new()
            .role(egui::accesskit::Role::Button)
            .label("Scripts"),
    )
    .next()
    .unwrap()
    .click();
    app.run_steps(2);
    common::click(app, script);
    let start = expected.split(' ').next().unwrap_or_default().to_owned() + " ";
    common::wait_until(app, "the dialog", |app| {
        app.query(By::new().label_contains(&start)).is_some()
    });
    let shown = app
        .query(By::new().label_contains(&start))
        .map(|n| {
            let node = n.accesskit_node();
            node.label().or_else(|| node.value()).unwrap_or_default()
        })
        .unwrap_or_default();
    assert_eq!(shown, expected);
    common::click(app, "OK");
    common::wait_until(app, "the pause again", |app| app.state().script_paused());
}

/// Wait while the script stays paused until the window has gone to record
/// `number` with nothing left to save.
fn wait_paused_on(app: &mut common::App, number: &str) {
    common::wait_until(app, "the save and the move during the pause", |app| {
        app.state().script_paused()
            && common::record_number(app) == number
            && !common::saving(app)
            && !common::unsaved(app)
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

/// What a paused script left on the record is saved by the user's Next
/// while it stays paused (its OnRecordCommit first, the save on top of the
/// paused script, in the window it was made in); the paused script then
/// goes on from the record the user left it on.
#[test]
fn next_during_a_pause_saves_what_the_script_left_and_moves() {
    let (hosted, mut app) =
        portals_with_triggers("pause-left", &[(TriggerEvent::RecordCommit, "commit")]);
    run_to_pause(&mut app, "Pause left");
    common::wait_until(&mut app, "unsaved record changes", common::unsaved);
    common::click(&mut app, "Next");
    wait_paused_on(&mut app, "2");
    assert_eq!(codes(&hosted), ["p", ""]);
    common::click(&mut app, "Continue");
    common::wait_for_label(&mut app, "resumed 2:");
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "the end", |app| !app.state().script_running());
    expect(&mut app, "Show events", "events commit; tx 1");
}

/// The user's typed save during a pause, by Next: OnRecordCommit runs on
/// top of the paused script and the save and the move happen while it
/// stays paused. The next record's OnRecordLoad, a delayed trigger of the
/// user's event, waits for the running (paused) script (13.12), and so
/// does the save's OnWindowTransaction, as a script's commits' do; both
/// run when the paused script has ended, none is lost.
#[test]
fn a_typed_save_during_a_pause_keeps_every_trigger_for_after_the_pause() {
    let (hosted, mut app) = portals_with_triggers(
        "pause-typed",
        &[
            (TriggerEvent::RecordCommit, "commit"),
            (TriggerEvent::RecordLoad, "load"),
        ],
    );
    run_to_pause(&mut app, "Pause here");
    retype(&mut app, "Code", "q");
    common::click(&mut app, "Next");
    wait_paused_on(&mut app, "2");
    assert_eq!(codes(&hosted), ["q", ""]);
    // The script's own Go to Record loaded record 1 before the pause.
    expect_paused(&mut app, "Show events", "events load;commit; tx ");
    common::click(&mut app, "Continue");
    common::wait_for_label(&mut app, "resumed 2:");
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "the end", |app| !app.state().script_running());
    expect(&mut app, "Show events", "events load;commit;load; tx 1");
}

/// Exit Application commits its window, then, once its script ended,
/// every other window of the file commits and closes as Quit closes them.
#[test]
fn exit_application_commits_every_window_before_quitting() {
    let hosted = host("exit-app");
    let mut app = open(&hosted);
    run_without_dialog(&mut app, "Dirty windows");
    open_root_menu(&mut app, "Scripts");
    common::click(&mut app, "Exit app");
    common::wait_until(&mut app, "every window to close", |app| {
        !app.state().is_open() && !app.state().script_running()
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    assert_eq!(codes(&hosted), ["w1", "w2"]);
}

/// A clicked related record changing the clicked record's join key must
/// not make the following related write target the next displayed row.
#[test]
fn clicked_identity_never_redirects_after_membership_change() {
    let hosted = host_with("review-membership", |root| {
        let mut capture = ScriptStep::new(86);
        capture.flags |= flag::TURN_ON;
        let script = fixture::script(
            ROW_BUTTON_SCRIPT,
            "Row button",
            vec![
                capture,
                note("$before", "Notes::Body"),
                set_field(NOTES_OCCURRENCE, TASK, 0, "\"Ship\""),
                note("$key_error", "Get ( LastError )"),
                set_field(NOTES_OCCURRENCE, BODY, 0, "\"wrong\""),
                note("$write_error", "Get ( LastError )"),
                without_dialog(75),
                show("\"probe \" & $before & \":\" & $key_error & \":\" & $write_error"),
            ],
        );
        add_script(root, &script);
    });
    let mut app = open(&hosted);
    expect(&mut app, "Show portals", "on Portals");
    click_row_button(&mut app, 1);
    common::wait_until(&mut app, "the probe dialog", |app| {
        app.query(By::new().label_contains("probe ")).is_some()
    });
    common::click(&mut app, "OK");
    common::wait_until(&mut app, "the script to end", |app| {
        !app.state().script_running()
    });
    let rows = stored(&hosted, NOTES, &[(BODY, 1), (TASK, 1)]);
    eprintln!("PROBE_PERSISTED {rows:?}");
    assert_eq!(
        rows[1].1[1], "Ship",
        "the clicked record's join key was changed"
    );
    assert_eq!(
        rows[2].1[0], "second",
        "the unclicked record must remain unchanged"
    );
}

/// The Portals record with a script's pending repetition (Status[2]
/// "ab") and the user's typing ("b" in Code): the window holds the record
/// on the host, as FileMaker Pro holds a record a script or the user began
/// editing (write-path.md 4.1).
fn mixed_edit(tag: &str) -> (hosted::Hosted, common::App) {
    let hosted = host(tag);
    let mut app = open(&hosted);
    expect(&mut app, "Repetition only", "reps 2:1:Status:ab");
    common::wait_until(&mut app, "the accepted editable record", |app| {
        app.state().is_settled() && common::text_field(app, "Code").value().is_some()
    });
    retype(&mut app, "Code", "b");
    common::wait_until(&mut app, "the edit taken", common::unsaved);
    assert_eq!(
        common::text_field(&app, "Code").value().as_deref(),
        Some("b")
    );
    (hosted, app)
}

/// Another client's change of a record the window holds with a script's
/// and the user's edits is refused (301), whether a field, a repetition or
/// the record's deletion; the window's save stores both edits.
#[test]
fn a_record_with_script_and_user_edits_refuses_a_peers_change() {
    let (hosted, mut app) = mixed_edit("mixed-ui-held");
    let mut other = hosted.opened();
    other.records(PORTALS as u32, 0, 10).unwrap();
    let refusals = [
        other
            .update_record(
                PORTALS as u32,
                1,
                [("Code".to_owned(), Some("c".to_owned()))].into(),
            )
            .map(|_| ()),
        other
            .write_record(&om_data::ScriptWrite {
                layout: PORTALS as u32,
                record: Some(1),
                cells: [(STATUS * 256 + 2, Some("xy".into()))].into(),
                ..Default::default()
            })
            .map(|_| ()),
        other.delete_record(PORTALS as u32, 1),
    ];
    for refused in refusals {
        assert_eq!(refused.unwrap_err().code, Some(301));
    }
    other.close();
    common::save_record(&mut app);
    wait_saved(&mut app);
    assert_eq!(
        stored(&hosted, TABLE, &[(CODE, 1), (STATUS, 2)])[0].1,
        ["b", "ab"]
    );
}

/// Reverting the record drops the script's pending repetition with the
/// user's typing, and lets the record go: the next edit starts from the
/// host's values.
#[test]
fn reverting_mixed_edits_drops_pending_worker_repetitions() {
    let (hosted, mut app) = mixed_edit("mixed-ui-revert");
    common::revert_record(&mut app);
    common::wait_until(&mut app, "the mixed edits reverted", |app| {
        app.state().is_settled()
            && !app.state().script_running()
            && common::text_field(app, "Code").value().as_deref() == Some("")
    });
    let mut other = hosted.opened();
    other.records(PORTALS as u32, 0, 10).unwrap();
    other
        .update_record(
            PORTALS as u32,
            1,
            [("Code".to_owned(), Some("c".to_owned()))].into(),
        )
        .unwrap();
    other.close();
    assert_eq!(
        stored(&hosted, TABLE, &[(CODE, 1), (STATUS, 2)])[0].1,
        ["c", ""]
    );
}

/// A script's pending repetition alone holds the record on the host, as
/// the user's typing does: another client's change of that repetition is
/// refused (301), and the window's save stores the script's value.
#[test]
fn a_scripts_repetition_alone_holds_the_record_against_a_peer() {
    let hosted = host("repetition-held");
    let mut app = open(&hosted);
    expect(&mut app, "Repetition only", "reps 2:1:Status:ab");
    common::wait_until(&mut app, "the accepted record", |app| {
        app.state().is_settled() && common::text_field(app, "Code").value().is_some()
    });
    let mut other = hosted.opened();
    other.records(PORTALS as u32, 0, 10).unwrap();
    let refused = other
        .write_record(&om_data::ScriptWrite {
            layout: PORTALS as u32,
            record: Some(1),
            cells: [(STATUS * 256 + 2, Some("xy".into()))].into(),
            ..Default::default()
        })
        .unwrap_err();
    assert_eq!(refused.code, Some(301), "{refused:?}");
    other.close();
    common::save_record(&mut app);
    wait_saved(&mut app);
    assert_eq!(
        stored(&hosted, TABLE, &[(CODE, 1), (STATUS, 2)])[0].1,
        ["", "ab"]
    );
}

/// A record a script's New Record left uncommitted is held in the window's
/// record session, not on the host (#892): another client can neither see
/// nor change it, and Revert writes nothing, so it can never delete another
/// client's change. Before, the record was saved at once and its Revert
/// deleted it.
#[test]
fn a_scripts_new_record_stays_off_the_host_and_its_revert_writes_nothing() {
    let hosted = host("held-new-revert");
    let mut app = open(&hosted);
    let before = stored(&hosted, TABLE, &[(NAME, 1)]);
    run_without_dialog(&mut app, "Leave new");
    common::wait_until(&mut app, "new editable row", |app| {
        app.state().is_settled() && common::text_field(app, "Code").value().is_some()
    });
    assert_eq!(stored(&hosted, TABLE, &[(NAME, 1)]), before);
    // Its id, the next after the stored ones, names no record another
    // client can write.
    let held = i64::try_from(before.iter().map(|row| row.0).max().unwrap() + 1).unwrap();
    let mut other = hosted.opened();
    other.records(PORTALS as u32, 0, 10).unwrap();
    assert!(other
        .update_record(
            PORTALS as u32,
            held,
            [("Code".to_owned(), Some("c".to_owned()))].into(),
        )
        .is_err());
    other.close();
    retype(&mut app, "Code", "b");
    common::revert_record(&mut app);
    common::wait_until(&mut app, "the new record released", |app| {
        app.state().is_settled() && !app.state().script_running() && !common::editing(app)
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    assert_eq!(stored(&hosted, TABLE, &[(NAME, 1)]), before);
}

/// A script's transaction opened in another window leaves the record a
/// window holds outside it (script-runtime.md 8.5): Commit Transaction run
/// from that window commits the record apart, not into the transaction,
/// and neither is lost (#892).
#[test]
fn a_held_new_record_is_committed_apart_from_another_windows_transaction() {
    let hosted = host("held-new-apart");
    let mut app = open(&hosted);
    expect(&mut app, "Apart from a transaction", "apart 0:Apart");
    assert!(
        stored(&hosted, TABLE, &[(NAME, 1)])
            .iter()
            .any(|(_, values)| values[0] == "Apart"),
        "the record is stored"
    );
}

/// A stored calculation of the window's record reads its value over the
/// record's unsaved changes before the commit (#1104): `Doubled = Count * 2`
/// after Set Field `Count` to 4 reads 8, as the commit stores it [I:
/// FileMaker's answer is unmeasured].
#[test]
fn a_stored_calculation_reads_over_the_records_unsaved_input() {
    let hosted = host("doubled-before-commit");
    let mut app = open(&hosted);
    expect(&mut app, "Doubled before commit", "doubled 8");
}

/// A refused save of a new record the window holds (#892) leaves its
/// record session as it found it: the portal row typed into the record is
/// written once, by the save that succeeds, not held again by each try.
#[test]
fn a_held_new_records_refused_save_writes_its_portal_row_once_when_retried() {
    let hosted = host("held-new-retry");
    let mut app = open(&hosted);
    let before = stored(&hosted, NOTES, &[(BODY, 1)]);
    run_without_dialog(&mut app, "Leave new");
    common::wait_until(&mut app, "new editable row", |app| {
        app.state().is_settled() && common::text_field(app, "Code").value().is_some()
    });
    // Its creation row. Notes::Body is validated Always: at most 10
    // characters.
    retype_first_row(&mut app, "far too long for it");
    common::save_record(&mut app);
    common::wait_until(&mut app, "the refusal", |app| {
        app.state().error().is_some() && !app.state().script_running()
    });
    common::click(&mut app, "OK");
    app.run_steps(2);
    assert!(common::unsaved(&app), "the row is kept");
    assert_eq!(stored(&hosted, NOTES, &[(BODY, 1)]), before);
    retype_first_row(&mut app, "kept");
    common::save_record(&mut app);
    wait_saved(&mut app);
    let notes = stored(&hosted, NOTES, &[(BODY, 1), (TASK, 1)]);
    assert_eq!(notes.len(), before.len() + 1, "{notes:?}");
    assert_eq!(
        notes.last().map(|(_, values)| values.clone()),
        Some(vec!["kept".to_owned(), "New task".to_owned()])
    );
}

/// A new record the window holds relates records by the key it was
/// auto-entered with before anything is typed into it, as FileMaker's new
/// record does (Claris, "Creating related records"): with relationship 1
/// joining `Tasks::Creator = Notes::Task`, Creator auto-entered with the
/// account's name and both notes the account's, New Record's portal shows
/// them at once. The stored tasks have no creator and relate none. The
/// layout shows the portal alone, so nothing else reads the new record.
#[test]
fn a_held_new_records_portal_follows_its_auto_entered_key_before_typing() {
    let hosted = host_with("held-new-auto-key", |root| {
        root.put(
            &[&[4], &[5], &uint_segment(PORTALS), &[7]],
            OwnedNode::leaf(Vec::new(), portal_only_store()),
        );
        fixture::add_relationship(root, 1, (OCCURRENCE, CREATOR), 0, (NOTES_OCCURRENCE, TASK));
        for note in [2u64, 3] {
            root.put(
                &[
                    &uint_segment(NOTES),
                    &[5],
                    &serial::encode(note),
                    &uint_segment(TASK),
                ],
                text_node(&[], fixture::ACCOUNT),
            );
        }
    });
    let mut app = open(&hosted);
    common::select_layout(&mut app, "Portals");
    let rows = |app: &common::App| -> Vec<String> {
        app.query_all(
            By::new()
                .role(egui::accesskit::Role::TextInput)
                .label("Notes::Body"),
        )
        .filter_map(|row| row.value())
        .filter(|value| !value.is_empty())
        .collect()
    };
    common::wait_until(&mut app, "the Portals layout", |app| {
        app.state().is_settled()
    });
    assert!(common::find_text_field(&app, "Name").is_none());
    assert!(rows(&app).is_empty(), "{:?}", rows(&app));
    common::click(&mut app, "New Record");
    common::wait_until(&mut app, "the new record's related notes", |app| {
        app.state().is_settled() && rows(app) == ["first", "second"]
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

/// Find mode draws the layout itself: its fields, the portal's first row
/// among them, take the request's criteria; a global field takes none
/// (Claris "Making a find request"). A criterion in the portal's field
/// finds the records with a matching related record.
#[test]
fn find_mode_takes_criteria_in_the_layouts_own_and_portal_fields() {
    let hosted = host("find-layout");
    let mut app = open(&hosted);
    common::select_layout(&mut app, "Portals");
    common::wait_until(&mut app, "the layout", |app| app.state().is_settled());
    common::open_menu(&mut app, "View");
    common::click(&mut app, "Find Mode");
    common::wait_for_label(&mut app, "Perform Find");
    assert!(common::find_text_field(&app, "Name").is_some());
    assert!(common::find_text_field(&app, "Setting").is_none());
    common::fill(&mut app, "Notes::Body", "second");
    common::click(&mut app, "Perform Find");
    common::wait_until(&mut app, "the found set", |app| {
        app.state().is_settled()
            && app.state().mode() == om_gui::Mode::Browse
            && app.query_by_label("Found (Unsorted)").is_some()
    });
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    common::wait_for_value(&mut app, "Write tests");
    assert_eq!(common::record_number(&app), "1");
}
