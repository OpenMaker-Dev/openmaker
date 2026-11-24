//! Scripts `[17, 5, S]` and the references in each step, read through the
//! step's parameter schema (`om_scripts::schema`, script-steps.md 4 and
//! 5): field, layout, script, table occurrence and data source operands,
//! calculations, find requests, sort orders, and import and export orders
//! (`om_transfer::spec`, script-steps.md 11.3, 11.4). Each stored location
//! of each parameter is handled or reported unread, and so is stored data
//! no parameter explains. A layout object's single step (a button) is read
//! the same way.
//!
//! Steps that name one data key share its subtree (`om_scripts::StepData`):
//! what a shared value holds is decoded once per script, its cost charged
//! before, and recorded at each step that holds it. Every script is listed
//! before any is decoded, and each is decoded only when it is read.

use std::collections::HashMap;
use std::rc::Rc;

use om_hbam::slot::be_uint;
use om_hbam::{serial, OwnedNode};
use om_scripts::display::{step_text_fidelity, MAX_PASTE_STEPS};
use om_scripts::specs::{self, SortSpec, SpecField};
use om_scripts::{
    flag, FieldSpec as StepFieldSpec, Item, Location, ParamKind, Script, ScriptStep, SlotKind,
    StepData, Unclassified,
};
use om_transfer::spec::{ExportOptions, FieldFlags, FieldSpec, ImportOptions, Mapping};

use super::access::{At, Declared, Layer, Read};
use super::read::{owned_size, polish_found, stamps, Builder, Found, GraphNames, Structure};
use super::{Kind, NameUse, ObjectId, Place, StepPlace, Target, UnreadReason};

const PERFORM_SCRIPT: u16 = 1;
const GO_TO_LAYOUT: u16 = 6;
const SEND_EVENT: u16 = 57;
const PERFORM_APPLESCRIPT: u16 = 67;
const GO_TO_RELATED_RECORD: u16 = 74;
const OPEN_URL: u16 = 111;
const SET_FIELD_BY_NAME: u16 = 147;
const SERVER_SCRIPT: u16 = 164;
const EXECUTE_DATA_API: u16 = 203;
const SERVER_SCRIPT_WITH_CALLBACK: u16 = 210;
const SQL_BY_NATURAL_LANGUAGE: u16 = 214;
const GENERATE_RESPONSE: u16 = 220;
const FIND_BY_NATURAL_LANGUAGE: u16 = 221;
const REPLACE_BY_NAME: u16 = 249;
/// Steps whose "Object name" calculation names a layout object (Go to
/// Object, Set Web Viewer, Refresh Object, Perform JavaScript in Web
/// Viewer, Refresh Portal; script-step-params.csv).
const OBJECT_NAME_STEPS: [u16; 5] = [145, 146, 167, 175, 180];
/// The unexplained stored data a step's detail names at most.
const UNEXPLAINED_SHOWN: usize = 8;
/// Step 210's callback script from another file: a file reference
/// calculation in slot 5 with this flag (script-steps.md 12.1).
const CALLBACK_IN_FILE: u32 = 0x400;
/// The AI steps' Perform JavaScript in Web Viewer option: the web
/// viewer's object name in slot 20 (script-step-params.csv `LLMWebScript`;
/// Claris help, Generate Response from Model).
const WEB_VIEWER_SLOT: u16 = 20;
/// Generate Response from Model's tool definitions, slot 13, which name
/// custom functions (Claris help; calc-functions.md, `GetToolDefinitions`).
const TOOL_DEFINITIONS_SLOT: u16 = 13;
/// Perform SQL Query by Natural Language's tables: `[00 8]` 0 from the
/// list, 1 by name, 2 by DDL (script-step-params.csv).
const SQL_TABLES_CHOICE_SLOT: u16 = 8;
/// Its options `[00 15]`: 0 from the list, 1 by JSON data, which sets flag
/// `0x800` and keeps the JSON in calculation slot 16, whose `data tables`
/// can name occurrences (script-step-params.csv; script-steps.md 12.2;
/// Claris help, Perform SQL Query by Natural Language).
const SQL_OPTIONS_CHOICE_SLOT: u16 = 15;
const SQL_OPTIONS_BY_JSON: u32 = 0x800;
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
/// `13`: zero on every stored script, meaning not known (scripts.md):
/// accepted only as zero.
const SCRIPT_ZERO: u64 = 0x13;
/// Slot children: a calculation `05`, a field spec `10`, occurrence keys
/// `11` (script-steps.md 4.2).
const SLOT_CALCULATION: u8 = 0x05;
const SLOT_FIELD: u8 = 0x10;
const SLOT_ALIASES: u8 = 0x11;
/// The count of a slot's several keys `[00 n, 04]` (`KCount`,
/// script-steps.md 4.2, 12.1), beside its keys `11`.
const SLOT_COUNT: u8 = 0x04;
/// What an AppleScript can name in FileMaker Pro, which has an AppleScript
/// dictionary (`fmpro.sdef`, docs/re/architecture.md): inferred, not
/// specified.
const NAMED_BY_APPLESCRIPT: [Kind; 5] = [
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
    /// Step 210's callback, whose reading depends on its file flag.
    Callback(bool),
}

/// What a shared value found, each with the part it is recorded under:
/// empty for the parameter's own.
type Parted = Vec<(&'static str, Found)>;

/// What a script's steps share, each read once: by data key, what each
/// shared value found; by descriptor (which holds the data key), the
/// stored data the step's schema does not explain.
#[derive(Default)]
pub(super) struct StepCache {
    shared: HashMap<(Vec<u8>, Shared), Rc<Parted>>,
    unexplained: HashMap<[u8; 28], Option<(UnreadReason, Rc<str>)>>,
}

/// `found` under the parameter's own part.
fn unparted(found: Vec<Found>) -> Parted {
    found.into_iter().map(|f| ("", f)).collect()
}

/// What a step's data holds, in bytes and nodes: what reading it costs.
fn data_size(step: &ScriptStep) -> usize {
    let Some(data) = &step.data else {
        return 1;
    };
    let mut total = 1usize;
    for (_, slot) in data.slots() {
        total += 1 + slot.value().map_or(0, <[u8]>::len);
        for key in slot.child_keys() {
            total += slot.child(key).map_or(0, owned_size);
        }
        total += slot.other().iter().map(owned_size).sum::<usize>();
    }
    for key in data.keys() {
        total += data.key(key).map_or(0, owned_size);
    }
    total + data.other().iter().map(owned_size).sum::<usize>()
}

/// The child `key` of slot `slot` of the step's data, as stored.
fn slot_child(step: &ScriptStep, slot: u16, key: u8) -> Option<&OwnedNode> {
    step.data.as_ref()?.slot(slot)?.child(key)
}

/// Whether the step's schema stores several keys in slot `slot`.
fn has_aliases(step: &ScriptStep, slot: u16) -> bool {
    step.schema().is_some_and(|schema| {
        schema
            .locations()
            .any(|l| matches!(l, Location::Slot { slot: n, kind: SlotKind::Aliases } if *n == slot))
    })
}

/// Whether slot `slot` stores a calculation, without decoding it.
fn has_calculation(step: &ScriptStep, slot: u16) -> bool {
    slot_child(step, slot, SLOT_CALCULATION).is_some()
}

/// Every script of the catalog: all listed first, from their names and
/// flags (folders by flags `01` and `02`, scripts by their step list
/// `04`, as `meta::scripts` tells them), then each decoded and read in
/// turn.
pub(super) fn scripts(b: &mut Builder, t: &Layer<'_, '_>) {
    // A catalog the source gave without this node holds no scripts.
    let Some(catalog) = t.at(&[17]) else {
        return;
    };
    if let Some(index) = catalog.get(1) {
        index.declare(Declared::CatalogIndex);
    }
    stamps(catalog);
    let list = catalog.get(5);
    let mut listed = Vec::new();
    for (_, entry) in b.members(list) {
        if !b.spend(1, ObjectId::File, &Place::new("scripts")) {
            return;
        }
        let Some(id) = serial::decode(entry.segment().0).filter(|id| *id != 0) else {
            continue;
        };
        let flags = match entry.uint_at(SCRIPT_FLAGS) {
            Read::Ok(flags) => flags,
            // Failed on its node (an anomaly): a script or a folder, not
            // known.
            Read::Unreadable => continue,
            Read::Absent => 0,
        };
        let name = b.name(entry, ObjectId::Script(id));
        stamps(entry);
        // `13`: four zero bytes on every stored script (scripts.md).
        b.observed(entry.get(SCRIPT_ZERO), &[0, 0, 0, 0]);
        if flags & 0x3 == 0 && entry.get(SCRIPT_STEPS).is_some() {
            b.object(ObjectId::Script(id), name, "");
            listed.push((id, entry));
        } else {
            // Folders, their ends and separators hold a name and flags,
            // and an empty step list (scripts.md); anything else is left
            // to the sweep.
            b.visit(Structure::ScriptFolder);
            if let Some(steps) = entry.get(SCRIPT_STEPS) {
                steps.observed(&[]);
            }
        }
    }
    let names = GraphNames::new(&b.graph);
    for (id, entry) in listed {
        let holder = ObjectId::Script(id);
        if !b.charge(entry, holder, &Place::new("definition")) {
            return;
        }
        // The step list, read whole: one that does not assemble is failed
        // on its node (and reported), never read as a script without steps.
        if !matches!(entry.bytes_at(SCRIPT_STEPS), Read::Ok(_)) {
            continue;
        }
        b.visit(Structure::Script);
        match entry.decode_script() {
            Ok(script) => read_script(b, &script, entry, &names),
            Err(e) => b.unread_node(
                entry,
                holder,
                Place::new("definition"),
                UnreadReason::Malformed,
                format!("a script that does not decode: {e}"),
                &Kind::ANY,
            ),
        }
    }
}

/// One script's steps, up to the text form's limit. Each step's line is
/// the one `get_script`'s source text gives it: the lines of the steps
/// before it (om-author's `source::script_text`). Each step's data
/// `05, K` is accounted for once the step's schema has read it; data no
/// step names is left to the sweep, but the counter `05, 00`, the last
/// data key issued (script-steps.md 4.2).
fn read_script(b: &mut Builder, script: &Script, entry: At<'_, '_>, names: &GraphNames) {
    let holder = ObjectId::Script(script.id);
    b.count("scripts");
    // One map of the step data by key, not a scan per step.
    let data = match entry.get(SCRIPT_STEP_DATA).map(At::by_segment) {
        Some(Read::Ok(map)) => map,
        _ => HashMap::new(),
    };
    if let Some(counter) = data.get(&[0][..]) {
        counter.counter();
    }
    let data_of = |step: &ScriptStep| {
        step.data_key
            .segment()
            .and_then(|segment| data.get(segment).copied())
    };
    // Whether every step naming a data key read all of it: only then is
    // the data taken whole.
    let mut consumed: HashMap<&[u8], bool> = HashMap::new();
    let mut cache = StepCache::default();
    // A step's text, and so its line count, follows from its descriptor
    // and its data, which the descriptor's data key names.
    let mut lines: HashMap<[u8; 28], usize> = HashMap::new();
    let mut line = 1;
    for (i, step) in script.steps.iter().take(MAX_PASTE_STEPS).enumerate() {
        let place = Place::new(format!("step {}", i + 1));
        if !b.spend(1, holder, &place) {
            return;
        }
        let descriptor = step.descriptor();
        let count = match lines.get(&descriptor) {
            Some(count) => *count,
            None => {
                // Rendering the text reads the step's data: charged first.
                if !b.spend(data_size(step), holder, &place) {
                    return;
                }
                let count = step_text_fidelity(step, names).0.split('\n').count();
                lines.insert(descriptor, count);
                count
            }
        };
        let at = StepPlace {
            index: i + 1,
            line,
            id: step.id(),
            name: step.name(),
            enabled: step.is_enabled(),
        };
        let read_all = read(b, holder, Some(&at), "", step, false, Some(&mut cache));
        if b.stopped() {
            return;
        }
        if let Some(segment) = step.data_key.segment() {
            *consumed.entry(segment).or_insert(true) &= read_all;
        }
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
        for step in &script.steps[MAX_PASTE_STEPS..] {
            if let Some(node) = data_of(step) {
                node.declare(Declared::Reported);
            }
        }
    }
    // Taken whole only when its steps read all of it; otherwise they
    // reported what they did not, as anomalies.
    for step in script.steps.iter().take(MAX_PASTE_STEPS) {
        if let (Some(segment), Some(node)) = (step.data_key.segment(), data_of(step)) {
            node.declare(if consumed.get(segment).copied().unwrap_or(false) {
                Declared::StepData
            } else {
                Declared::Reported
            });
        }
    }
}

/// What `step` stores at a shared place, from `cache` when an earlier step
/// with the same data key read it. Its `cost` is charged before it is
/// read, so the cache holds no more than the budget allows.
#[allow(clippy::too_many_arguments)]
fn shared(
    b: &mut Builder,
    cache: &mut Option<&mut StepCache>,
    step: &ScriptStep,
    what: Shared,
    holder: ObjectId,
    place: &Place,
    cost: impl FnOnce() -> usize,
    compute: impl FnOnce() -> Parted,
) -> Rc<Parted> {
    let key = step
        .data_key
        .key()
        .filter(|k| !k.is_empty())
        .map(|k| (k.to_vec(), what));
    if let (Some(cache), Some(key)) = (cache.as_deref(), key.as_ref()) {
        if let Some(found) = cache.shared.get(key) {
            return Rc::clone(found);
        }
    }
    // Sized and charged only when not cached: a hit does no work.
    if !b.spend(cost(), holder, place) {
        return Rc::new(Vec::new());
    }
    let found = Rc::new(compute());
    if let (Some(cache), Some(key)) = (cache.as_deref_mut(), key) {
        cache.shared.insert(key, Rc::clone(&found));
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

/// A field or variable target a step stores.
fn field_spec(b: &mut Builder, holder: ObjectId, at: Place, spec: StepFieldSpec) {
    if spec.is_variable() || !spec.names_field() {
        return;
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

/// Read `step`, held by `holder` (a script, or a layout object for a
/// button), into `b`. Each place is `prefix` and the parameter's name.
/// True when the step read all its stored data: every stored place of
/// every parameter read, nothing stored that the schema does not explain,
/// and no anomaly met. Only then may its data be taken whole.
pub(super) fn read(
    b: &mut Builder,
    holder: ObjectId,
    at: Option<&StepPlace>,
    prefix: &str,
    step: &ScriptStep,
    in_button: bool,
    mut cache: Option<&mut StepCache>,
) -> bool {
    let anomalies = b.graph.coverage.anomaly_count;
    let place = |part: &str| Place {
        step: at.cloned(),
        part: format!("{prefix}{part}"),
    };
    b.visit(Structure::Step);
    let id = step.id();
    let Some(schema) = step.schema() else {
        b.unread(
            holder,
            place("step"),
            UnreadReason::NotDecoded,
            format!("step {id} has no parameter schema"),
            &Kind::ANY,
        );
        return false;
    };
    // Names looked up when the step runs (script-steps.md 11.8, 12.1, 12.4;
    // script-runtime.md 3.6; script-step-params.csv; the by-name table in
    // docs/authoring.md gives each one's source).
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
        // Layouts, table occurrences, fields and portals by name; scripts
        // are ignored (Claris help, Execute FileMaker Data API).
        EXECUTE_DATA_API => Some((
            "Request",
            NameUse::RequestText,
            &[
                Kind::Layout,
                Kind::Occurrence,
                Kind::Field,
                Kind::LayoutObject,
            ],
        )),
        // The SQL the model writes names fields; tables by name or by DDL
        // name occurrences (Claris help).
        SQL_BY_NATURAL_LANGUAGE => {
            let tables_named = matches!(step.list_value(SQL_TABLES_CHOICE_SLOT), Some(1 | 2))
                || step.list_value(SQL_OPTIONS_CHOICE_SLOT) == Some(1)
                || step.has_flag(SQL_OPTIONS_BY_JSON);
            if tables_named {
                Some((
                    "Tables",
                    NameUse::OccurrenceByName,
                    &[Kind::Occurrence, Kind::Field],
                ))
            } else {
                Some(("Query", NameUse::RequestText, &[Kind::Field]))
            }
        }
        // The find request the model returns names fields (Claris help).
        FIND_BY_NATURAL_LANGUAGE => Some((
            "Query",
            NameUse::RequestText,
            &[Kind::Field, Kind::Occurrence],
        )),
        PERFORM_APPLESCRIPT => Some((
            "AppleScript",
            NameUse::ExternalScriptText,
            &NAMED_BY_APPLESCRIPT,
        )),
        // An Apple event sent to FileMaker Pro itself (it receives them:
        // Claris help, Receiving Apple events) can name what an AppleScript
        // can (inferred, as for Perform AppleScript).
        SEND_EVENT => Some((
            "Send Event",
            NameUse::ExternalScriptText,
            &NAMED_BY_APPLESCRIPT,
        )),
        // `fmp://...?script=name` (Claris help, Open URL and Opening files
        // using a URL).
        OPEN_URL => Some(("URL", NameUse::Url, &[Kind::Script])),
        _ => None,
    };
    if let Some((part, how, kinds)) = named {
        b.by_name(holder, place(part), how, None, kinds.to_vec());
    }
    if matches!(id, SQL_BY_NATURAL_LANGUAGE | GENERATE_RESPONSE)
        && has_calculation(step, WEB_VIEWER_SLOT)
    {
        b.by_name(
            holder,
            place("Web viewer"),
            NameUse::ObjectByName,
            None,
            vec![Kind::LayoutObject],
        );
    }
    if id == GENERATE_RESPONSE && has_calculation(step, TOOL_DEFINITIONS_SLOT) {
        b.by_name(
            holder,
            place("Tool definitions"),
            NameUse::FunctionByName,
            None,
            vec![Kind::CustomFunction],
        );
    }
    if id == SERVER_SCRIPT_WITH_CALLBACK {
        let at = place("Callback script");
        let in_file = step.has_flag(CALLBACK_IN_FILE);
        let cost = || {
            [3, 5, 6, 7]
                .iter()
                .filter_map(|slot| step.data.as_ref()?.slot(*slot))
                .map(|slot| {
                    slot.child_keys()
                        .filter_map(|k| slot.child(k))
                        .map(owned_size)
                        .sum::<usize>()
                        + 1
                })
                .sum::<usize>()
        };
        let found = shared(
            b,
            &mut cache,
            step,
            Shared::Callback(in_file),
            holder,
            &at,
            cost,
            || callback(step),
        );
        apply(b, holder, &at, &place, &found);
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
                return false;
            }
            let at = place(name);
            match (*location, kind) {
                // Options, choices and cached indexes; texts and numbers: a
                // variable's name, a comment, text another application
                // reads (named above). Not decoded: they name no object.
                (Location::Flag { .. } | Location::Value, _)
                | (
                    Location::Slot {
                        kind: SlotKind::Text | SlotKind::Number | SlotKind::List,
                        ..
                    },
                    _,
                ) => {}
                // File paths name files, not objects of this one.
                (
                    Location::Slot {
                        kind: SlotKind::Path,
                        ..
                    },
                    ParamKind::FileReference,
                ) => {}
                (
                    Location::Slot {
                        slot,
                        kind: SlotKind::Calculation,
                    },
                    _,
                ) => {
                    let Some(node) = slot_child(step, slot, SLOT_CALCULATION) else {
                        continue;
                    };
                    b.visit(Structure::StepCalculation);
                    let found = shared(
                        b,
                        &mut cache,
                        step,
                        Shared::Calculation(slot),
                        holder,
                        &at,
                        || owned_size(node),
                        || calculation_found(step, slot),
                    );
                    b.count("formulas");
                    apply(b, holder, &at, &place, &found);
                }
                (
                    Location::Slot {
                        slot,
                        kind: SlotKind::Aliases,
                    },
                    _,
                ) => {
                    let Some(node) = slot_child(step, slot, SLOT_ALIASES) else {
                        continue;
                    };
                    let count = slot_child(step, slot, SLOT_COUNT);
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
                        || owned_size(node) + count.map_or(0, owned_size),
                        || unparted(aliases_found(node, count, list_of)),
                    );
                    apply(b, holder, &at, &place, &found);
                }
                (
                    Location::Slot {
                        slot,
                        kind: SlotKind::Field,
                    },
                    _,
                ) => match step.item(location) {
                    Some(Item::Field(spec)) => field_spec(b, holder, at, spec),
                    // Stored but not a field spec (slot 0's is `mReserved`,
                    // read with its flag).
                    _ if slot > 0 && slot_child(step, slot, SLOT_FIELD).is_some() => b.unread(
                        holder,
                        at,
                        UnreadReason::Malformed,
                        "a field reference that does not read",
                        &[Kind::Field, Kind::Occurrence],
                    ),
                    _ => {}
                },
                // Read together by `transfer`, below.
                (Location::Key(key), _) if TRANSFER_KEYS.contains(&key) => {}
                _ => {
                    let Some(item) = step.item(location) else {
                        continue;
                    };
                    self::item(
                        b,
                        holder,
                        at,
                        step,
                        &mut cache,
                        parameter,
                        *location,
                        item,
                        ItemContext {
                            layout_key_counts,
                            script_by_name,
                            external,
                        },
                    );
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
    if let Some(data) = &step.data {
        if TRANSFER_KEYS.iter().any(|k| data.key(*k).is_some()) {
            let at = place("Import or export order");
            b.visit(Structure::Transfer);
            let cost = || {
                TRANSFER_KEYS
                    .iter()
                    .filter_map(|k| data.key(*k))
                    .map(owned_size)
                    .sum()
            };
            let found = shared(
                b,
                &mut cache,
                step,
                Shared::Transfer,
                holder,
                &at,
                cost,
                || transfer(step),
            );
            apply(b, holder, &at, &place, &found);
        }
    }
    if let Some((reason, detail)) = check_data(b, &mut cache, step, holder, &place("stored data")) {
        b.unread(
            holder,
            place("stored data"),
            reason,
            detail.as_ref(),
            &Kind::ANY,
        );
    }
    b.graph.coverage.anomaly_count == anomalies
}

/// What a step stores that it does not read: data no parameter of its
/// schema explains (`ScriptStep::unclassified`, but flag bits and the
/// step value, which hold no reference, and the import and export specs
/// [`transfer`] reads), and each stored place of a parameter that does not
/// read (a list choice of the wrong length, text that does not decode). As
/// one detail, its length bounded. Read once per descriptor (which names
/// the data), its cost charged first; a cache hit does no work.
fn check_data(
    b: &mut Builder,
    cache: &mut Option<&mut StepCache>,
    step: &ScriptStep,
    holder: ObjectId,
    place: &Place,
) -> Option<(UnreadReason, Rc<str>)> {
    let descriptor = step.descriptor();
    if let Some(found) = cache
        .as_deref()
        .and_then(|c| c.unexplained.get(&descriptor))
    {
        return found.clone();
    }
    if !b.spend(data_size(step), holder, place) {
        return None;
    }
    let unexplained: Vec<String> = step
        .unclassified()
        .into_iter()
        .filter(|u| !matches!(u, Unclassified::Flag(_) | Unclassified::Value(_)))
        .filter(|u| !matches!(u, Unclassified::Key(key) if TRANSFER_KEYS.contains(key)))
        // The count beside a slot's several keys, read with them.
        .filter(|u| !matches!(u, Unclassified::SlotChild(n, SLOT_COUNT) if has_aliases(step, *n)))
        .take(UNEXPLAINED_SHOWN + 1)
        .map(|u| u.to_string())
        .collect();
    let mut unreadable: Vec<String> = Vec::new();
    if let (Some(schema), Some(data)) = (step.schema(), &step.data) {
        for parameter in &schema.params {
            for location in &parameter.storage {
                let Location::Slot { slot, kind } = *location else {
                    continue;
                };
                // Slot 0's field spec is `mReserved`'s, read with its flag.
                if kind == SlotKind::Field && slot == 0 {
                    continue;
                }
                let Some(stored) = data.slot(slot) else {
                    continue;
                };
                let present = match kind.child() {
                    None => stored.value().is_some(),
                    Some(key) => stored.child(key).is_some(),
                };
                if present && step.item(location).is_none() && unreadable.len() <= UNEXPLAINED_SHOWN
                {
                    unreadable.push(format!("{} (slot {slot})", parameter.name));
                }
            }
        }
    }
    let shown = |items: &[String]| {
        let more = if items.len() > UNEXPLAINED_SHOWN {
            ", ..."
        } else {
            ""
        };
        format!(
            "{}{more}",
            items[..items.len().min(UNEXPLAINED_SHOWN)].join(", ")
        )
    };
    let found = match (unexplained.is_empty(), unreadable.is_empty()) {
        (true, true) => None,
        (false, true) => Some((
            UnreadReason::NotDecoded,
            format!(
                "stored data the step's schema does not explain: {}",
                shown(&unexplained)
            ),
        )),
        (true, false) => Some((
            UnreadReason::Malformed,
            format!("stored parameters that do not read: {}", shown(&unreadable)),
        )),
        (false, false) => Some((
            UnreadReason::Malformed,
            format!(
                "stored parameters that do not read: {}; stored data the step's schema does \
                 not explain: {}",
                shown(&unreadable),
                shown(&unexplained)
            ),
        )),
    }
    .map(|(reason, detail)| (reason, Rc::from(detail)));
    if let Some(cache) = cache.as_deref_mut() {
        cache.unexplained.insert(descriptor, found.clone());
    }
    found
}

/// A step calculation's references (slot `slot`).
fn calculation_found(step: &ScriptStep, slot: u16) -> Parted {
    let found = match step.calculation(slot) {
        Some(calculation) => match calculation.polish.as_deref() {
            Some(polish) => polish_found(polish, calculation.context.as_deref()).0,
            None => vec![Found::unread(
                UnreadReason::NotDecoded,
                "a calculation stored without its compiled form",
                &Kind::ANY,
            )],
        },
        None => vec![Found::unread(
            UnreadReason::Malformed,
            "a calculation that does not read",
            &Kind::ANY,
        )],
    };
    unparted(found)
}

/// What reading one item needs from its step.
struct ItemContext {
    layout_key_counts: bool,
    script_by_name: bool,
    external: Option<Result<u64, ()>>,
}

/// One stored item of a parameter that is not a calculation, an
/// occurrence list or a slot's field spec: an inline field, layout,
/// script, table or occurrence key, a node, or a reserved key.
#[allow(clippy::too_many_arguments)]
fn item(
    b: &mut Builder,
    holder: ObjectId,
    at: Place,
    step: &ScriptStep,
    cache: &mut Option<&mut StepCache>,
    parameter: &om_scripts::ParamSchema,
    location: Location,
    item: Item<'_>,
    context: ItemContext,
) {
    let name = parameter.name.as_str();
    let kind = parameter.kind;
    match item {
        Item::Calculation(calculation) => {
            let found = match calculation.polish.as_deref() {
                Some(polish) => polish_found(polish, calculation.context.as_deref()).0,
                None => vec![Found::unread(
                    UnreadReason::NotDecoded,
                    "a calculation stored without its compiled form",
                    &Kind::ANY,
                )],
            };
            b.apply(holder, &at, &found);
        }
        Item::Field(spec) => field_spec(b, holder, at, spec),
        Item::Key(key) => match kind {
            ParamKind::Layout => {
                if !context.layout_key_counts {
                    return;
                }
                match key.id() {
                    Some(layout) => {
                        b.reference(holder, at, Target::Object(ObjectId::Layout(layout)))
                    }
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
                if !context.script_by_name {
                    script(b, holder, at, key.serial_id(), context.external);
                }
            }
            ParamKind::TableOccurrence => match key.id() {
                // A base table by its member key (Truncate Table).
                Some(table) if parameter.xml_type == "BaseTableReference" => {
                    b.reference(holder, at, Target::Object(ObjectId::Table(table)))
                }
                Some(occurrence) => {
                    b.reference(holder, at, Target::Object(ObjectId::Occurrence(occurrence)))
                }
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
        Item::Node(node) => match (kind, location) {
            // A Perform Script's file is read with its script.
            (ParamKind::Script, Location::Key(StepData::FILE_REFERENCES)) => {}
            (_, Location::Key(StepData::FILE_REFERENCES)) => match specs::data_source(node) {
                Some(source) => {
                    b.reference(holder, at, Target::Object(ObjectId::DataSource(source)))
                }
                None => b.unread(
                    holder,
                    at,
                    UnreadReason::Malformed,
                    "a file reference that does not read",
                    &[Kind::DataSource],
                ),
            },
            // File paths name files, and print settings nothing.
            (ParamKind::FileReference, _) | (ParamKind::PrintSettings, _) => {}
            (ParamKind::FindRequests, _) => {
                b.visit(Structure::Requests);
                let found = shared(
                    b,
                    cache,
                    step,
                    Shared::Requests,
                    holder,
                    &at,
                    || owned_size(node),
                    || unparted(requests_found(node)),
                );
                apply(b, holder, &at, &|_| at.clone(), &found);
            }
            (ParamKind::SortOrder, _) => {
                b.visit(Structure::SortOrder);
                let found = shared(
                    b,
                    cache,
                    step,
                    Shared::Sort,
                    holder,
                    &at,
                    || owned_size(node),
                    || {
                        unparted(
                            match om_scripts::node_bytes(node).and_then(|b| SortSpec::decode(&b)) {
                                Some(sort) => sort_found(&sort),
                                None => vec![Found::unread(
                                    UnreadReason::Malformed,
                                    "a sort order that does not read",
                                    &[Kind::Field, Kind::Occurrence, Kind::ValueList],
                                )],
                            },
                        )
                    },
                );
                apply(b, holder, &at, &|_| at.clone(), &found);
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
        // Texts, numbers and choices name no object (see `read`).
        Item::Bool(_) | Item::Value(_) | Item::Text(_) | Item::Number(_) => {}
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
fn callback(step: &ScriptStep) -> Parted {
    let found = match step.list_value(6) {
        Some(1) => Found::ByName {
            how: NameUse::ScriptByName,
            function: None,
            kinds: vec![Kind::Script, Kind::DataSource],
        },
        Some(0) | None if !has_calculation(step, 3) => return Vec::new(),
        Some(0) | None => match step.positioned_id(3) {
            None => Found::unread(
                UnreadReason::Malformed,
                "a callback script key that does not read",
                &[Kind::Script],
            ),
            Some(script) if step.has_flag(CALLBACK_IN_FILE) => match step.positioned_id(5) {
                Some(data_source) => Found::Reference(Target::ExternalScript {
                    data_source,
                    script: Some(script),
                }),
                None => Found::unread(
                    UnreadReason::Malformed,
                    "a callback file reference that does not read",
                    &[Kind::Script, Kind::DataSource],
                ),
            },
            Some(script) => Found::Reference(Target::Object(ObjectId::Script(script))),
        },
        Some(other) => Found::unread(
            UnreadReason::NotDecoded,
            format!("a callback choice {other} OpenMaker does not know"),
            &[Kind::Script, Kind::DataSource],
        ),
    };
    vec![("", found)]
}

/// The keys of a `[00 n, 11]` node (`KAliases`, script-steps.md 4.2,
/// 12.1): table occurrences, or scripts for Validate Scripts. One key is
/// stored as its length and segment (`03 d0 00 02`; `01 00` names none);
/// several as 6 bytes each, the length, the segment and filler, with their
/// count in `[00 n, 04]` as a little-endian `u32` (`02 00 00 00` for two).
/// Any other form is an anomaly, not an empty list.
fn aliases_found(node: &OwnedNode, count: Option<&OwnedNode>, list_of: Kind) -> Vec<Found> {
    let malformed = |what: &str| {
        vec![Found::unread(
            UnreadReason::Malformed,
            format!("a list of {what} that does not read"),
            &[list_of],
        )]
    };
    let what = if list_of == Kind::Script {
        "scripts"
    } else {
        "table occurrences"
    };
    let Some(bytes) = node
        .children
        .is_empty()
        .then(|| om_scripts::node_bytes(node))
        .flatten()
    else {
        return malformed(what);
    };
    // One `[len][segment]`, or 6-byte entries.
    let entries: Vec<&[u8]> = match bytes.split_first() {
        Some((&len, segment)) if usize::from(len) == segment.len() && len > 0 => {
            vec![segment]
        }
        _ if !bytes.is_empty() && bytes.len() % 6 == 0 => {
            let mut entries = Vec::new();
            for chunk in bytes.chunks_exact(6) {
                let len = usize::from(chunk[0]);
                match chunk.get(1..1 + len).filter(|_| len > 0) {
                    Some(segment) => entries.push(segment),
                    None => return malformed(what),
                }
            }
            entries
        }
        _ => return malformed(what),
    };
    if let Some(count) = count {
        let stated = count
            .children
            .is_empty()
            .then(|| om_scripts::node_bytes(count))
            .flatten()
            .and_then(|b| <[u8; 4]>::try_from(b.as_ref()).ok())
            .map(u32::from_le_bytes);
        if stated != u32::try_from(entries.len()).ok() {
            return malformed(what);
        }
    }
    let mut found = Vec::new();
    for segment in entries {
        let id = if list_of == Kind::Script {
            serial::decode(segment)
        } else {
            be_uint(segment)
        };
        match id {
            // `01 00`: no occurrence (script-steps.md 11.8).
            Some(0) => {}
            Some(id) => found.push(Found::Reference(Target::Object(
                if list_of == Kind::Script {
                    ObjectId::Script(id)
                } else {
                    ObjectId::Occurrence(id)
                },
            ))),
            None => return malformed(what),
        }
    }
    found
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

/// A find step's request fields (11.2): its order `05` and its rows `06`,
/// each row's cells keyed by field. A criterion whose field key
/// `specs::requests` does not read is reported, not dropped, and so is
/// every key of the node, the order or the rows that is not one of these.
fn requests_found(node: &OwnedNode) -> Vec<Found> {
    let mut odd = Vec::new();
    if node.value.is_some() {
        odd.push("a value on the find requests node".to_owned());
    }
    for child in &node.children {
        match child.segment.as_slice() {
            [0x05] => {
                if child.value.is_some()
                    || child.children.iter().any(|c| !c.children.is_empty())
                    || om_hbam::RidList::from_owned(child).is_err()
                {
                    odd.push("a request order that does not read".to_owned());
                }
            }
            [0x06] => {
                if child.value.is_some() {
                    odd.push("a value on the request rows".to_owned());
                }
                for row in &child.children {
                    let highest = row.segment.as_slice() == [0x00];
                    let stray = if highest {
                        !row.children.is_empty()
                    } else {
                        row.value.as_ref().is_some_and(|v| !v.is_empty())
                            || om_hbam::serial::decode(&row.segment).is_none()
                    };
                    if stray {
                        odd.push(format!("request row {:02x?}", row.segment));
                    }
                }
            }
            other => odd.push(format!("key {other:02x?} of the find requests")),
        }
    }
    let mut found: Vec<Found> = odd
        .into_iter()
        .map(|what| {
            Found::unread(
                UnreadReason::NotDecoded,
                format!("{what}, which OpenMaker does not read"),
                &[Kind::Field, Kind::Occurrence],
            )
        })
        .collect();
    found.extend(criteria_found(node));
    found
}

/// The fields of a find step's criteria, with a count check: a cell
/// `specs::requests` does not read is reported.
fn criteria_found(node: &OwnedNode) -> Vec<Found> {
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
            found
        }
        None => vec![Found::unread(
            UnreadReason::Malformed,
            "find requests that do not read",
            &[Kind::Field, Kind::Occurrence],
        )],
    }
}

/// An import or export step's orders (script-steps.md 11.3, 11.4): the
/// import's target occurrence and each column's target field, its
/// per-field options, and the export's fields, summary break fields and
/// group-by fields, each with its part. The file type `86` and profile
/// `87` name no object.
fn transfer(step: &ScriptStep) -> Parted {
    let mut out: Parted = Vec::new();
    // A spec that is stored but does not assemble is unread, not absent.
    let mut bytes = |key: u8, part: &'static str| match step.data_leaf(key) {
        Some(node) => match om_scripts::node_bytes(node) {
            Some(bytes) => Some(bytes.into_owned()),
            None => {
                out.push((
                    part,
                    Found::unread(
                        UnreadReason::Malformed,
                        format!("the {part}, stored but not assembling"),
                        &[Kind::Field, Kind::Occurrence],
                    ),
                ));
                None
            }
        },
        None => None,
    };
    let import = bytes(IMPORT_ORDER, "Import order");
    let flags = bytes(IMPORT_FLAGS, "Import field options");
    let export = bytes(EXPORT_ORDER, "Export order");

    let mut target = None;
    if let Some(order) = import {
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
    if let Some(flags) = flags {
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
    if let Some(order) = export {
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
