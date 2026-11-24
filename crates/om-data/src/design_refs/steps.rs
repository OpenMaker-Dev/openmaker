//! Scripts `[17, 5, S]` and the references in each step, read through the
//! step's parameter schema (`om_scripts::schema`, script-steps.md 4 and
//! 5): field, layout, script, table occurrence and data source operands,
//! calculations, find requests, sort orders, and import and export orders
//! (`om_transfer::spec`, script-steps.md 11.3, 11.4). Each stored location
//! of each parameter is handled or reported unread, and so is stored data
//! no parameter explains. A layout object's single step (a button) is read
//! the same way.
//!
//! Steps that name one data key share its subtree (`om_scripts::StepData`);
//! what a shared value holds is read once per script and recorded at each
//! step that holds it.

use std::collections::HashMap;
use std::rc::Rc;

use om_hbam::slot::be_uint;
use om_hbam::{serial, KeyTree, Node, OwnedNode};
use om_scripts::display::{step_text_fidelity, MAX_PASTE_STEPS};
use om_scripts::specs::{self, SortSpec, SpecField};
use om_scripts::{
    flag, Item, Location, ParamKind, Script, ScriptKind, ScriptStep, SlotKind, StepData,
    Unclassified,
};
use om_transfer::spec::{ExportOptions, FieldFlags, FieldSpec, ImportOptions, Mapping};

use super::read::{
    members, polish_found, text_at, Builder, Found, GraphNames, MEMBER_STAMPS, NAME,
};
use super::{Kind, NameUse, ObjectId, Place, StepPlace, Target, UnreadReason};

const PERFORM_SCRIPT: u16 = 1;
const GO_TO_LAYOUT: u16 = 6;
const SEND_EVENT: u16 = 57;
const SEND_DDE_EXECUTE: u16 = 64;
const PERFORM_APPLESCRIPT: u16 = 67;
const GO_TO_RELATED_RECORD: u16 = 74;
const OPEN_URL: u16 = 111;
const SET_FIELD_BY_NAME: u16 = 147;
const INSERT_FROM_URL: u16 = 160;
const SERVER_SCRIPT: u16 = 164;
const EXECUTE_DATA_API: u16 = 203;
const SERVER_SCRIPT_WITH_CALLBACK: u16 = 210;
const SQL_BY_NATURAL_LANGUAGE: u16 = 214;
const GENERATE_RESPONSE: u16 = 220;
const FIND_BY_NATURAL_LANGUAGE: u16 = 221;
const EXECUTE_URL: u16 = 224;
const REPLACE_BY_NAME: u16 = 249;
/// Steps whose "Object name" calculation names a layout object (Go to
/// Object, Set Web Viewer, Refresh Object, Perform JavaScript in Web
/// Viewer, Refresh Portal; script-step-params.csv).
const OBJECT_NAME_STEPS: [u16; 5] = [145, 146, 167, 175, 180];
/// Step 210's callback script from another file: a file reference
/// calculation in slot 5 with this flag (script-steps.md 12.1).
const CALLBACK_IN_FILE: u32 = 0x400;
/// The "Web script" name of the AI steps: a calculation in slot 20
/// (script-step-params.csv, `LLMWebScript`).
const WEB_SCRIPT_SLOT: u16 = 20;
/// The keys of an import or export step's specs (script-steps.md 11.3 to
/// 11.5): the import order `84`, the export order `85`, the file type `86`,
/// the import profile extension `87` and the per-field import options
/// `f0`, read together by [`transfer`].
const IMPORT_ORDER: u8 = 0x84;
const EXPORT_ORDER: u8 = 0x85;
const IMPORT_FLAGS: u8 = 0xf0;
const TRANSFER_KEYS: [u8; 5] = [IMPORT_ORDER, EXPORT_ORDER, 0x86, 0x87, IMPORT_FLAGS];
/// A script definition's step list `04` and step data `05`, and its flags
/// `f0` (scripts.md).
const SCRIPT_STEPS: u64 = 0x04;
const SCRIPT_STEP_DATA: u64 = 0x05;
const SCRIPT_FLAGS: u64 = 0xf0;
/// `13`, 0 on every stored script (scripts.md).
const SCRIPT_ZERO: u64 = 0x13;
/// What text another application or a URL can name.
const NAMED_BY_TEXT: [Kind; 5] = [
    Kind::Script,
    Kind::Layout,
    Kind::Table,
    Kind::Occurrence,
    Kind::Field,
];

/// A value stored in a step's data subtree, read once per script for
/// every step that names the same data key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Shared {
    Calculation(u16),
    Aliases(u16),
    Requests,
    Sort,
    Transfer,
}

/// What a shared value found, each with the part it is recorded under:
/// empty for the parameter's own.
type Parted = Vec<(&'static str, Found)>;

/// What a script's shared step data found, by data key.
pub(super) type StepCache = HashMap<(Vec<u8>, Shared), Rc<Parted>>;

/// `found` under the parameter's own part.
fn unparted(found: Vec<Found>) -> Parted {
    found.into_iter().map(|f| ("", f)).collect()
}

/// Every script of the catalog, then each one's steps.
pub(super) fn scripts(b: &mut Builder, tree: &KeyTree<'_>) {
    let mut scripts = Vec::new();
    for (_, entry) in members(tree, &[17, 5]) {
        // A deleted script's member is emptied (scripts.md): no member.
        if entry.children.is_empty() && entry.value.is_none() {
            continue;
        }
        let Some(id) = serial::decode(entry.segment.0).filter(|id| *id != 0) else {
            b.unread(
                ObjectId::File,
                Place::new(format!("script {}", entry.segment)),
                UnreadReason::Malformed,
                "a script whose key does not read",
                &Kind::ANY,
            );
            continue;
        };
        match Script::decode(entry) {
            Ok(script) if script.kind == ScriptKind::Script => {
                b.object(ObjectId::Script(id), &script.name, "");
                scripts.push((script, entry));
            }
            // Folders, their ends and separators have a name and flags,
            // and no steps (scripts.md).
            Ok(_) => {
                let mut member = b.member(ObjectId::File, entry, format!("script folder {id}"));
                member.get(NAME);
                member.known(&[SCRIPT_FLAGS, SCRIPT_ZERO]);
                member.known(&MEMBER_STAMPS);
                b.close(member);
            }
            Err(e) => {
                b.object(
                    ObjectId::Script(id),
                    text_at(entry.get(NAME)).unwrap_or_default(),
                    "",
                );
                b.unread(
                    ObjectId::Script(id),
                    Place::new("definition"),
                    UnreadReason::Malformed,
                    format!("a script that does not decode: {e}"),
                    &Kind::ANY,
                );
            }
        }
    }
    let names = GraphNames::new(&b.graph);
    for (script, entry) in &scripts {
        if b.stopped() {
            return;
        }
        read_script(b, script, entry, &names);
    }
}

/// One script's steps, up to the text form's limit. Each step's line is
/// the one `get_script`'s source text gives it: the lines of the steps
/// before it (om-author's `source::script_text`).
fn read_script(b: &mut Builder, script: &Script, entry: &Node<'_>, names: &GraphNames) {
    let holder = ObjectId::Script(script.id);
    b.count("scripts");
    let mut member = b.member(holder, entry, "script");
    member.get(NAME);
    member.get(SCRIPT_STEPS);
    member.get(SCRIPT_STEP_DATA);
    member.known(&[SCRIPT_FLAGS, SCRIPT_ZERO]);
    member.known(&MEMBER_STAMPS);
    let mut cache = StepCache::new();
    // A step's text, and so its line count, follows from its descriptor
    // and its data, which the descriptor's data key names.
    let mut lines: HashMap<[u8; 28], usize> = HashMap::new();
    let mut line = 1;
    for (i, step) in script.steps.iter().take(MAX_PASTE_STEPS).enumerate() {
        if !b.spend(1, holder, &Place::new(format!("step {}", i + 1))) {
            return;
        }
        let count = *lines
            .entry(step.descriptor())
            .or_insert_with(|| step_text_fidelity(step, names).0.split('\n').count());
        let at = StepPlace {
            index: i + 1,
            line,
            id: step.id(),
            name: step.name(),
            enabled: step.is_enabled(),
        };
        read(b, holder, Some(&at), "", step, false, Some(&mut cache));
        b.count("steps");
        line += count;
    }
    if script.steps.len() > MAX_PASTE_STEPS {
        b.unread(
            holder,
            Place::new(format!(
                "steps {} to {}",
                MAX_PASTE_STEPS + 1,
                script.steps.len()
            )),
            UnreadReason::Limit,
            format!("only the first {MAX_PASTE_STEPS} steps of a script are read"),
            &Kind::ANY,
        );
    }
    b.close(member);
}

/// What `step` stores at a shared place, from `cache` when an earlier step
/// with the same data key read it.
fn shared(
    b: &mut Builder,
    cache: &mut Option<&mut StepCache>,
    step: &ScriptStep,
    what: Shared,
    holder: ObjectId,
    place: &Place,
    compute: impl FnOnce() -> (Parted, usize),
) -> Rc<Parted> {
    let key = step
        .data_key
        .key()
        .filter(|k| !k.is_empty())
        .map(|k| (k.to_vec(), what));
    if let (Some(cache), Some(key)) = (cache.as_deref(), key.as_ref()) {
        if let Some(found) = cache.get(key) {
            return Rc::clone(found);
        }
    }
    let (found, work) = compute();
    b.spend(work, holder, place);
    let found = Rc::new(found);
    if let (Some(cache), Some(key)) = (cache.as_deref_mut(), key) {
        cache.insert(key, Rc::clone(&found));
    }
    found
}

/// Record what a shared value found at `at`, or under its own part.
fn apply(
    b: &mut Builder,
    holder: ObjectId,
    at: &Place,
    place: &dyn Fn(&str) -> Place,
    found: &Parted,
) {
    for (part, found) in found {
        let at = if part.is_empty() {
            at.clone()
        } else {
            place(part)
        };
        b.apply(holder, &at, std::slice::from_ref(found));
    }
}

/// Whether slot `slot` stores a calculation, without decoding it.
fn has_calculation(step: &ScriptStep, slot: u16) -> bool {
    step.data
        .as_ref()
        .and_then(|d| d.slot(slot))
        .and_then(|s| s.child(0x05))
        .is_some()
}

/// Read `step`, held by `holder` (a script, or a layout object for a
/// button), into `b`. Each place is `prefix` and the parameter's name.
pub(super) fn read(
    b: &mut Builder,
    holder: ObjectId,
    at: Option<&StepPlace>,
    prefix: &str,
    step: &ScriptStep,
    in_button: bool,
    mut cache: Option<&mut StepCache>,
) {
    let place = |part: &str| Place {
        step: at.cloned(),
        part: format!("{prefix}{part}"),
    };
    let id = step.id();
    let Some(schema) = step.schema() else {
        b.unread(
            holder,
            place("step"),
            UnreadReason::NotDecoded,
            format!("step {id} has no parameter schema"),
            &Kind::ANY,
        );
        return;
    };
    // Names looked up when the step runs (script-steps.md 11.8, 12.1, 12.4;
    // script-runtime.md 3.6; script-step-params.csv).
    let choice = step.list_value(1);
    let script_by_name = match id {
        PERFORM_SCRIPT => choice == Some(1),
        // The canonical slot-2 form, or the older name calculation in slot 1.
        SERVER_SCRIPT | SERVER_SCRIPT_WITH_CALLBACK => {
            choice == Some(1) || has_calculation(step, 1)
        }
        _ => false,
    };
    if matches!(
        id,
        PERFORM_SCRIPT | SERVER_SCRIPT | SERVER_SCRIPT_WITH_CALLBACK
    ) && choice.is_some_and(|c| c > 1)
    {
        b.unread(
            holder,
            place("Specify by name"),
            UnreadReason::NotDecoded,
            format!(
                "a script choice {} OpenMaker does not know",
                choice.unwrap_or(0)
            ),
            &[Kind::Script, Kind::DataSource],
        );
    }
    if script_by_name {
        // `script` or `datasource::script` (script-runtime.md 3.6).
        b.by_name(
            holder,
            place("Specify by name"),
            NameUse::ScriptByName,
            None,
            vec![Kind::Script, Kind::DataSource],
        );
    }
    // Go to Layout and Go to Related Record: 1 the original layout, 3 and
    // 4 a name or number calculation, otherwise the stored key
    // (script-runtime.md; the runtime's layout targets).
    let layout_by_calculation =
        matches!(id, GO_TO_LAYOUT | GO_TO_RELATED_RECORD) && matches!(step.value, 3 | 4);
    if layout_by_calculation {
        b.by_name(
            holder,
            place("Layout"),
            NameUse::LayoutByCalculation,
            None,
            vec![Kind::Layout],
        );
    }
    let layout_key_counts = match id {
        GO_TO_LAYOUT => !matches!(step.value, 1 | 3 | 4),
        // With the option, the layout is the related table's file's
        // (script-steps.md 11.8), not this file's.
        GO_TO_RELATED_RECORD => !matches!(step.value, 1 | 3 | 4) && !step.has_flag(flag::OPTION),
        _ => true,
    };
    if id == GO_TO_RELATED_RECORD && step.has_flag(flag::OPTION3) {
        b.by_name(
            holder,
            place("Related table"),
            NameUse::OccurrenceByName,
            None,
            vec![Kind::Occurrence],
        );
    }
    if matches!(id, SET_FIELD_BY_NAME | REPLACE_BY_NAME) && step.has_flag(flag::FIELD_SPEC_BY_CALC)
    {
        b.by_name(
            holder,
            place("Target field by name"),
            NameUse::FieldByName,
            None,
            vec![Kind::Field, Kind::Occurrence],
        );
    }
    if OBJECT_NAME_STEPS.contains(&id) {
        b.by_name(
            holder,
            place("Object name"),
            NameUse::ObjectByName,
            None,
            vec![Kind::LayoutObject],
        );
    }
    let named: Option<(&str, NameUse, &[Kind])> = match id {
        EXECUTE_DATA_API => Some((
            "Request",
            NameUse::RequestText,
            &[Kind::Layout, Kind::Field, Kind::Occurrence, Kind::Script],
        )),
        SQL_BY_NATURAL_LANGUAGE => Some((
            "Tables",
            NameUse::OccurrenceByName,
            &[Kind::Occurrence, Kind::Field],
        )),
        FIND_BY_NATURAL_LANGUAGE => Some((
            "Query",
            NameUse::RequestText,
            &[Kind::Field, Kind::Occurrence],
        )),
        PERFORM_APPLESCRIPT | SEND_EVENT | SEND_DDE_EXECUTE => {
            Some(("Script text", NameUse::ExternalScriptText, &NAMED_BY_TEXT))
        }
        OPEN_URL | INSERT_FROM_URL | EXECUTE_URL => Some(("URL", NameUse::Url, &NAMED_BY_TEXT)),
        _ => None,
    };
    if let Some((part, how, kinds)) = named {
        b.by_name(holder, place(part), how, None, kinds.to_vec());
    }
    if matches!(id, SQL_BY_NATURAL_LANGUAGE | GENERATE_RESPONSE)
        && has_calculation(step, WEB_SCRIPT_SLOT)
    {
        b.by_name(
            holder,
            place("Web script"),
            NameUse::ScriptByName,
            None,
            vec![Kind::Script],
        );
    }
    if id == SERVER_SCRIPT_WITH_CALLBACK {
        callback(b, holder, &place("Callback script"), step);
    }
    // The file a Perform Script's script is in, when it is another file's.
    let external = step
        .has_flag(flag::FILE_REFERENCE)
        .then(|| step.data_leaf(StepData::FILE_REFERENCES))
        .flatten()
        .map(|node| specs::data_source(node).ok_or(()));
    for parameter in &schema.params {
        let name = parameter.name.as_str();
        let kind = parameter.kind;
        for location in &parameter.storage {
            if b.stopped() {
                return;
            }
            let at = place(name);
            match *location {
                // Read without decoding it once per step: steps naming one
                // data key share it.
                Location::Slot {
                    slot,
                    kind: SlotKind::Calculation,
                } => {
                    if !has_calculation(step, slot) {
                        continue;
                    }
                    let found = shared(
                        b,
                        &mut cache,
                        step,
                        Shared::Calculation(slot),
                        holder,
                        &at,
                        || {
                            let (found, work) = match step.calculation(slot) {
                                Some(calculation) => match calculation.polish.as_deref() {
                                    Some(polish) => {
                                        polish_found(polish, calculation.context.as_deref())
                                    }
                                    None => (
                                        vec![Found::unread(
                                            UnreadReason::NotDecoded,
                                            "a calculation stored without its compiled form",
                                            &Kind::ANY,
                                        )],
                                        1,
                                    ),
                                },
                                None => (
                                    vec![Found::unread(
                                        UnreadReason::Malformed,
                                        "a calculation that does not read",
                                        &Kind::ANY,
                                    )],
                                    1,
                                ),
                            };
                            (unparted(found), work)
                        },
                    );
                    b.count("formulas");
                    apply(b, holder, &at, &place, &found);
                }
                Location::Slot {
                    slot,
                    kind: SlotKind::Aliases,
                } => {
                    let Some(Item::Node(node)) = step.item(location) else {
                        continue;
                    };
                    let list_of = if kind == ParamKind::Script {
                        Kind::Script
                    } else {
                        Kind::Occurrence
                    };
                    let found = shared(
                        b,
                        &mut cache,
                        step,
                        Shared::Aliases(slot),
                        holder,
                        &at,
                        || (unparted(aliases_found(node, list_of)), 1),
                    );
                    apply(b, holder, &at, &place, &found);
                }
                // Read together by `transfer`, below.
                Location::Key(key) if TRANSFER_KEYS.contains(&key) => {}
                _ => {
                    let Some(item) = step.item(location) else {
                        continue;
                    };
                    match item {
                        Item::Calculation(calculation) => {
                            let found = match calculation.polish.as_deref() {
                                Some(polish) => {
                                    polish_found(polish, calculation.context.as_deref()).0
                                }
                                None => vec![Found::unread(
                                    UnreadReason::NotDecoded,
                                    "a calculation stored without its compiled form",
                                    &Kind::ANY,
                                )],
                            };
                            b.apply(holder, &at, &found);
                        }
                        Item::Field(spec) => {
                            if spec.is_variable() || !spec.names_field() {
                                continue;
                            }
                            match (spec.table().id(), spec.field().id()) {
                                (Some(occurrence), Some(field)) => {
                                    b.reference(holder, at, Target::Field { occurrence, field })
                                }
                                _ => b.unread(
                                    holder,
                                    at,
                                    UnreadReason::Malformed,
                                    "a field reference whose keys do not read",
                                    &[Kind::Field, Kind::Occurrence],
                                ),
                            }
                        }
                        Item::Key(key) => match kind {
                            ParamKind::Layout => {
                                if !layout_key_counts {
                                    continue;
                                }
                                match key.id() {
                                    Some(layout) => b.reference(
                                        holder,
                                        at,
                                        Target::Object(ObjectId::Layout(layout)),
                                    ),
                                    None => b.unread(
                                        holder,
                                        at,
                                        UnreadReason::Malformed,
                                        "a layout key that does not read",
                                        &[Kind::Layout],
                                    ),
                                }
                            }
                            ParamKind::Script => {
                                if !script_by_name {
                                    script(b, holder, at, key.serial_id(), external);
                                }
                            }
                            ParamKind::TableOccurrence => match key.id() {
                                // A base table by its member key (Truncate Table).
                                Some(table) if parameter.xml_type == "BaseTableReference" => {
                                    b.reference(holder, at, Target::Object(ObjectId::Table(table)))
                                }
                                Some(occurrence) => b.reference(
                                    holder,
                                    at,
                                    Target::Object(ObjectId::Occurrence(occurrence)),
                                ),
                                None => b.unread(
                                    holder,
                                    at,
                                    UnreadReason::Malformed,
                                    "a table key that does not read",
                                    &[Kind::Table, Kind::Occurrence],
                                ),
                            },
                            // A custom menu set: not an object the graph has.
                            ParamKind::MenuSet => {}
                            _ => b.unread(
                                holder,
                                at,
                                UnreadReason::NotDecoded,
                                format!("a {name} key"),
                                &Kind::ANY,
                            ),
                        },
                        Item::Node(node) => match (kind, *location) {
                            // A Perform Script's file is read with its script.
                            (ParamKind::Script, Location::Key(StepData::FILE_REFERENCES)) => {}
                            (_, Location::Key(StepData::FILE_REFERENCES)) => {
                                match specs::data_source(node) {
                                    Some(source) => b.reference(
                                        holder,
                                        at,
                                        Target::Object(ObjectId::DataSource(source)),
                                    ),
                                    None => b.unread(
                                        holder,
                                        at,
                                        UnreadReason::Malformed,
                                        "a file reference that does not read",
                                        &[Kind::DataSource],
                                    ),
                                }
                            }
                            // File paths name files, and print settings
                            // nothing.
                            (ParamKind::FileReference, _) | (ParamKind::PrintSettings, _) => {}
                            (ParamKind::FindRequests, _) => {
                                let found = shared(
                                    b,
                                    &mut cache,
                                    step,
                                    Shared::Requests,
                                    holder,
                                    &at,
                                    || {
                                        let (found, work) = requests_found(node);
                                        (unparted(found), work)
                                    },
                                );
                                apply(b, holder, &at, &place, &found);
                            }
                            (ParamKind::SortOrder, _) => {
                                let found =
                                    shared(b, &mut cache, step, Shared::Sort, holder, &at, || {
                                        match SortSpec::from_node(node) {
                                            Some(sort) => {
                                                let found = sort_found(&sort);
                                                let work = found.len() + 1;
                                                (unparted(found), work)
                                            }
                                            None => (
                                                unparted(vec![Found::unread(
                                                    UnreadReason::Malformed,
                                                    "a sort order that does not read",
                                                    &[
                                                        Kind::Field,
                                                        Kind::Occurrence,
                                                        Kind::ValueList,
                                                    ],
                                                )]),
                                                1,
                                            ),
                                        }
                                    });
                                apply(b, holder, &at, &place, &found);
                            }
                            _ => b.unread(
                                holder,
                                at,
                                UnreadReason::NotDecoded,
                                format!("{name}, kept as stored"),
                                &Kind::ANY,
                            ),
                        },
                        // A data source or privilege set key in `mReserved`.
                        Item::Reserved(_) => {
                            if kind != ParamKind::PrivilegeSet {
                                b.unread(
                                    holder,
                                    at,
                                    UnreadReason::NotDecoded,
                                    format!("the {name} key"),
                                    &[Kind::DataSource],
                                );
                            }
                        }
                        // Options, choices, numbers and texts: a text
                        // parameter holds a variable's name, a comment or
                        // text another application reads (above).
                        Item::Bool(_) | Item::Value(_) | Item::Text(_) | Item::Number(_) => {}
                    }
                }
            }
        }
    }
    // A layout button's Perform Script names its script whatever its flags
    // say, as the runtime reads it (`ScriptStep::target_script_id`).
    if in_button && id == PERFORM_SCRIPT && !script_by_name && step.script_id().is_none() {
        if let Some(script_id) = step.target_script_id() {
            script(b, holder, place("Script"), Some(script_id), external);
        }
    }
    if step
        .data
        .as_ref()
        .is_some_and(|d| TRANSFER_KEYS.iter().any(|k| d.key(*k).is_some()))
    {
        let at = place("Import or export order");
        let found = shared(b, &mut cache, step, Shared::Transfer, holder, &at, || {
            let found = transfer(step);
            let work = found.len() + 1;
            (found, work)
        });
        apply(b, holder, &at, &place, &found);
    }
    let unexplained: Vec<String> = step
        .unclassified()
        .into_iter()
        .filter(|u| !matches!(u, Unclassified::Flag(_) | Unclassified::Value(_)))
        .filter(|u| !matches!(u, Unclassified::Key(key) if TRANSFER_KEYS.contains(key)))
        .map(|u| u.to_string())
        .collect();
    if !unexplained.is_empty() {
        b.unread(
            holder,
            place("stored data"),
            UnreadReason::NotDecoded,
            format!(
                "stored data the step's schema does not explain: {}",
                unexplained.join(", ")
            ),
            &Kind::ANY,
        );
    }
}

/// A script by its id, in this file or, with `external`, another.
fn script(
    b: &mut Builder,
    holder: ObjectId,
    at: Place,
    id: Option<u64>,
    external: Option<Result<u64, ()>>,
) {
    match (external, id) {
        (Some(Ok(data_source)), script) => b.reference(
            holder,
            at,
            Target::ExternalScript {
                data_source,
                script,
            },
        ),
        (Some(Err(())), _) => b.unread(
            holder,
            at,
            UnreadReason::Malformed,
            "a file reference that does not read",
            &[Kind::Script, Kind::DataSource],
        ),
        (None, Some(script)) => b.reference(holder, at, Target::Object(ObjectId::Script(script))),
        (None, None) => b.unread(
            holder,
            at,
            UnreadReason::Malformed,
            "a script key that does not read",
            &[Kind::Script],
        ),
    }
}

/// Step 210's callback script (script-steps.md 12.1): `[00 6]` 0 from the
/// list, the key as a calculation of its quoted hex in slot 3; 1 by name,
/// slot 7; from another file, a file reference calculation in slot 5.
fn callback(b: &mut Builder, holder: ObjectId, at: &Place, step: &ScriptStep) {
    match step.list_value(6) {
        Some(1) => {
            b.by_name(
                holder,
                at.clone(),
                NameUse::ScriptByName,
                None,
                vec![Kind::Script, Kind::DataSource],
            );
            return;
        }
        Some(0) | None => {}
        Some(other) => {
            b.unread(
                holder,
                at.clone(),
                UnreadReason::NotDecoded,
                format!("a callback choice {other} OpenMaker does not know"),
                &[Kind::Script, Kind::DataSource],
            );
            return;
        }
    }
    if !has_calculation(step, 3) {
        return;
    }
    let Some(script) = step.positioned_id(3) else {
        b.unread(
            holder,
            at.clone(),
            UnreadReason::Malformed,
            "a callback script key that does not read",
            &[Kind::Script],
        );
        return;
    };
    if step.has_flag(CALLBACK_IN_FILE) {
        match step.positioned_id(5) {
            Some(data_source) => b.reference(
                holder,
                at.clone(),
                Target::ExternalScript {
                    data_source,
                    script: Some(script),
                },
            ),
            None => b.unread(
                holder,
                at.clone(),
                UnreadReason::Malformed,
                "a callback file reference that does not read",
                &[Kind::Script, Kind::DataSource],
            ),
        }
    } else {
        b.reference(holder, at.clone(), Target::Object(ObjectId::Script(script)));
    }
}

/// The one key of a `[00 n, 11]` node (`KAliases`, script-steps.md 4.2):
/// `[len][segment]`, a table occurrence, or a script for Validate
/// Scripts. Several keys carry a count `04`, which is not decoded here.
fn aliases_found(node: &OwnedNode, list_of: Kind) -> Vec<Found> {
    let one = (node.children.is_empty())
        .then(|| om_scripts::node_bytes(node))
        .flatten()
        .and_then(|bytes| {
            let (&len, segment) = bytes.split_first()?;
            (usize::from(len) == segment.len())
                .then(|| be_uint(segment))
                .flatten()
                .filter(|id| *id != 0)
        });
    match (one, list_of) {
        (Some(occurrence), Kind::Occurrence) => vec![Found::Reference(Target::Object(
            ObjectId::Occurrence(occurrence),
        ))],
        (_, Kind::Script) => vec![Found::unread(
            UnreadReason::NotDecoded,
            "a list of scripts",
            &[Kind::Script],
        )],
        _ => vec![Found::unread(
            UnreadReason::NotDecoded,
            "a list of table occurrences",
            &[Kind::Occurrence],
        )],
    }
}

/// A field a stored spec names; one with no occurrence cannot be placed.
pub(super) fn spec_found(field: SpecField) -> Vec<Found> {
    match field.occurrence {
        Some(occurrence) => vec![Found::Reference(Target::Field {
            occurrence,
            field: field.field,
        })],
        None => vec![Found::unread(
            UnreadReason::Malformed,
            "a field named without its table occurrence",
            &[Kind::Field],
        )],
    }
}

/// A sort order's fields, summary fields and value lists (11.1.1).
pub(super) fn sort_found(sort: &SortSpec) -> Vec<Found> {
    let mut found = Vec::new();
    for entry in &sort.fields {
        found.extend(spec_found(entry.field));
        if let Some(summary) = entry.summary {
            found.extend(spec_found(summary));
        }
        if let Some(list) = entry.value_list.filter(|id| *id != 0) {
            found.push(Found::Reference(Target::Object(ObjectId::ValueList(list))));
        }
    }
    found
}

/// A find step's request fields (11.2). A criterion whose field key
/// `specs::requests` does not read is reported, not dropped.
fn requests_found(node: &OwnedNode) -> (Vec<Found>, usize) {
    // Each row's cells but its omit marker `00` and its stamp `fc`; the
    // rows' own `00` is the highest id issued.
    let stored: usize = node
        .child(&[0x06])
        .map(|rows| {
            rows.children
                .iter()
                .filter(|row| row.segment.as_slice() != [0x00])
                .map(|row| {
                    row.children
                        .iter()
                        .filter(|cell| !matches!(cell.segment.as_slice(), [0x00] | [0xfc]))
                        .count()
                })
                .sum()
        })
        .unwrap_or(0);
    match specs::requests(node) {
        Some(requests) => {
            let mut found = Vec::new();
            let mut read = 0;
            for request in &requests {
                for (field, _) in &request.criteria {
                    read += 1;
                    found.extend(spec_found(*field));
                }
            }
            if stored > read {
                found.push(Found::unread(
                    UnreadReason::Malformed,
                    format!("{} find criteria that do not read", stored - read),
                    &[Kind::Field, Kind::Occurrence],
                ));
            }
            (found, stored + 1)
        }
        None => (
            vec![Found::unread(
                UnreadReason::Malformed,
                "find requests that do not read",
                &[Kind::Field, Kind::Occurrence],
            )],
            1,
        ),
    }
}

/// An import or export step's orders (script-steps.md 11.3, 11.4): the
/// import's target occurrence and each column's target field, its
/// per-field options, and the export's fields, summary break fields and
/// group-by fields, each with its part. The file type `86` and profile
/// `87` name no object.
fn transfer(step: &ScriptStep) -> Parted {
    let bytes = |key: u8| {
        step.data_leaf(key)
            .and_then(om_scripts::node_bytes)
            .map(|b| b.into_owned())
    };
    let mut out: Parted = Vec::new();
    let mut target = None;
    if let Some(order) = bytes(IMPORT_ORDER) {
        match ImportOptions::decode(&order).map(|o| (o.table.id(), o)) {
            Ok((Some(occurrence), options)) => {
                target = Some(occurrence);
                out.push((
                    "Import order: target table",
                    Found::Reference(Target::Object(ObjectId::Occurrence(occurrence))),
                ));
                for entry in &options.map {
                    let Some(field) = entry.field.id() else {
                        continue;
                    };
                    let part = if entry.mapping == Mapping::DontImport {
                        "Import order (not imported)"
                    } else {
                        "Import order"
                    };
                    out.push((part, Found::Reference(Target::Field { occurrence, field })));
                }
            }
            Ok((None, _)) | Err(_) => out.push((
                "Import order",
                Found::unread(
                    UnreadReason::Malformed,
                    "an import order that does not read",
                    &[Kind::Field, Kind::Occurrence],
                ),
            )),
        }
    }
    if let Some(flags) = bytes(IMPORT_FLAGS) {
        match (FieldFlags::decode(&flags), target) {
            (Ok(flags), Some(occurrence)) => {
                for (key, _) in &flags.fields {
                    if let Some(field) = key.id() {
                        out.push((
                            "Import field options",
                            Found::Reference(Target::Field { occurrence, field }),
                        ));
                    }
                }
            }
            (Ok(flags), None) if flags.fields.iter().all(|(key, _)| key.id().is_none()) => {}
            _ => out.push((
                "Import field options",
                Found::unread(
                    UnreadReason::Malformed,
                    "per-field import options that do not read",
                    &[Kind::Field],
                ),
            )),
        }
    }
    if let Some(order) = bytes(EXPORT_ORDER) {
        match ExportOptions::decode(&order) {
            Ok(options) => {
                for field in &options.fields {
                    for spec in [&field.field, &field.sum_by] {
                        if let Some(found) = transfer_field(spec) {
                            out.push(("Export order", found));
                        }
                    }
                }
                for (field, _) in &options.group_by {
                    if let Some(found) = transfer_field(field) {
                        out.push(("Export order: group by", found));
                    }
                }
            }
            Err(_) => out.push((
                "Export order",
                Found::unread(
                    UnreadReason::Malformed,
                    "an export order that does not read",
                    &[Kind::Field, Kind::Occurrence],
                ),
            )),
        }
    }
    out
}

/// A field of an import or export spec; an empty spec names none.
fn transfer_field(spec: &FieldSpec) -> Option<Found> {
    match (spec.table.id(), spec.field.id()) {
        (Some(occurrence), Some(field)) => {
            Some(Found::Reference(Target::Field { occurrence, field }))
        }
        (None, None) => None,
        _ => Some(Found::unread(
            UnreadReason::Malformed,
            "a field spec whose keys do not read",
            &[Kind::Field, Kind::Occurrence],
        )),
    }
}
