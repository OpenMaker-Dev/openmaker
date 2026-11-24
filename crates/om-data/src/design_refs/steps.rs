//! The references in one script step, read through the step's parameter
//! schema (`om_scripts::schema`, script-steps.md 4 and 5): field, layout,
//! script, table occurrence and data source operands, calculations, find
//! requests and sort orders. Parameters the schema keeps as stored bytes
//! (import and export orders) and stored data no parameter explains are
//! reported unread. A layout object's single step (a button) is read the
//! same way.

use om_hbam::slot::be_uint;
use om_scripts::specs::{self, SortSpec, SpecField};
use om_scripts::{flag, Item, Location, ParamKind, ScriptStep, StepData, Unclassified};

use super::read::Builder;
use super::{Kind, NameUse, ObjectId, Place, StepPlace, Target, UnreadReason};

const PERFORM_SCRIPT: u16 = 1;
const GO_TO_LAYOUT: u16 = 6;
const GO_TO_RELATED_RECORD: u16 = 74;
const SET_FIELD_BY_NAME: u16 = 147;
const SERVER_SCRIPT: u16 = 164;
const EXECUTE_DATA_API: u16 = 203;
const SERVER_SCRIPT_WITH_CALLBACK: u16 = 210;
const SQL_BY_NATURAL_LANGUAGE: u16 = 214;
const FIND_BY_NATURAL_LANGUAGE: u16 = 221;
const REPLACE_BY_NAME: u16 = 249;
/// Steps whose "Object name" calculation names a layout object (Go to
/// Object, Set Web Viewer, Refresh Object, Perform JavaScript in Web
/// Viewer, Refresh Portal; script-step-params.csv).
const OBJECT_NAME_STEPS: [u16; 5] = [145, 146, 167, 175, 180];
/// Step 210's callback script from another file: a file reference
/// calculation in slot 5 with this flag (script-steps.md 12.1).
const CALLBACK_IN_FILE: u32 = 0x400;

/// What an import or export order could name.
const ORDER_KINDS: [Kind; 4] = [Kind::Field, Kind::Occurrence, Kind::Table, Kind::DataSource];

/// Read `step`, held by `holder` (a script, or a layout object for a
/// button), into `b`. Each place is `prefix` and the parameter's name.
pub(super) fn read(
    b: &mut Builder,
    holder: ObjectId,
    at: Option<&StepPlace>,
    prefix: &str,
    step: &ScriptStep,
    in_button: bool,
) {
    let place = |part: &str| Place {
        step: at.cloned(),
        part: format!("{prefix}{part}"),
    };
    let id = step.id();
    let Some(_) = step.schema() else {
        b.unread(
            holder,
            place("step"),
            UnreadReason::NotDecoded,
            format!("step {id} has no parameter schema"),
            Kind::REFERABLE.to_vec(),
        );
        return;
    };
    // Names looked up when the step runs (script-steps.md 11.8, 12.1, 12.4
    // and script-step-params.csv).
    let script_by_name = match id {
        PERFORM_SCRIPT => step.list_value(1) == Some(1),
        // The canonical slot-2 form, or the older name calculation in slot 1.
        SERVER_SCRIPT | SERVER_SCRIPT_WITH_CALLBACK => {
            step.list_value(1) == Some(1) || step.calculation(1).is_some()
        }
        _ => false,
    };
    if script_by_name {
        b.by_name(
            holder,
            place("Specify by name"),
            NameUse::ScriptByName,
            None,
            vec![Kind::Script],
        );
    }
    // Go to Layout: 1 the original layout, 3 and 4 a name or number
    // calculation, otherwise the stored key (display: `go_to_layout`).
    let layout_by_calculation = id == GO_TO_LAYOUT && matches!(step.value, 3 | 4);
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
        // 1 is <Current Layout>; with the option, the layout is the
        // related table's file's (script-steps.md 11.8), not this file's.
        GO_TO_RELATED_RECORD => step.value != 1 && !step.has_flag(flag::OPTION),
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
    match id {
        EXECUTE_DATA_API => b.by_name(
            holder,
            place("Request"),
            NameUse::RequestText,
            None,
            vec![Kind::Layout, Kind::Field, Kind::Occurrence, Kind::Script],
        ),
        SQL_BY_NATURAL_LANGUAGE => b.by_name(
            holder,
            place("Tables"),
            NameUse::OccurrenceByName,
            None,
            vec![Kind::Occurrence, Kind::Field],
        ),
        FIND_BY_NATURAL_LANGUAGE => b.by_name(
            holder,
            place("Query"),
            NameUse::RequestText,
            None,
            vec![Kind::Field, Kind::Occurrence],
        ),
        _ => {}
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
    for parameter in step.parameters() {
        let name = parameter.schema.name.as_str();
        let kind = parameter.schema.kind;
        for (location, item) in &parameter.parts {
            let Some(item) = item else {
                continue;
            };
            let at = place(name);
            match item {
                Item::Calculation(calculation) => b.step_calculation(holder, at, calculation),
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
                            "a field reference whose keys do not read".into(),
                            vec![Kind::Field, Kind::Occurrence],
                        ),
                    }
                }
                Item::Key(key) => match kind {
                    ParamKind::Layout => {
                        if layout_key_counts && !layout_by_calculation {
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
                                    "a layout key that does not read".into(),
                                    vec![Kind::Layout],
                                ),
                            }
                        }
                    }
                    ParamKind::Script => {
                        if script_by_name {
                            continue;
                        }
                        script(b, holder, at, key.serial_id(), external);
                    }
                    ParamKind::TableOccurrence => match key.id() {
                        // A base table by its member key (Truncate Table).
                        Some(table) if parameter.schema.xml_type == "BaseTableReference" => {
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
                            "a table key that does not read".into(),
                            vec![Kind::Table, Kind::Occurrence],
                        ),
                    },
                    ParamKind::MenuSet => {}
                    _ => b.unread(
                        holder,
                        at,
                        UnreadReason::NotDecoded,
                        format!("a {name} key"),
                        Kind::REFERABLE.to_vec(),
                    ),
                },
                Item::Node(node) => match (kind, location) {
                    (ParamKind::TableOccurrence, _) => match aliases(node) {
                        Some(occurrence) => b.reference(
                            holder,
                            at,
                            Target::Object(ObjectId::Occurrence(occurrence)),
                        ),
                        None => b.unread(
                            holder,
                            at,
                            UnreadReason::NotDecoded,
                            "a list of table occurrences".into(),
                            vec![Kind::Occurrence],
                        ),
                    },
                    // A Perform Script's file is read with its script.
                    (ParamKind::Script, Location::Key(StepData::FILE_REFERENCES)) => {}
                    // Validate Scripts' list of script keys (script-step-params.csv).
                    (ParamKind::Script, _) => b.unread(
                        holder,
                        at,
                        UnreadReason::NotDecoded,
                        "a list of scripts".into(),
                        vec![Kind::Script],
                    ),
                    (_, Location::Key(StepData::FILE_REFERENCES)) => match specs::data_source(node)
                    {
                        Some(source) => {
                            b.reference(holder, at, Target::Object(ObjectId::DataSource(source)))
                        }
                        None => b.unread(
                            holder,
                            at,
                            UnreadReason::Malformed,
                            "a file reference that does not read".into(),
                            vec![Kind::DataSource],
                        ),
                    },
                    // File paths name files, not objects of this one.
                    (ParamKind::FileReference, _) | (ParamKind::PrintSettings, _) => {}
                    (ParamKind::FindRequests, _) => requests(b, holder, at, node),
                    (ParamKind::SortOrder, _) => match SortSpec::from_node(node) {
                        Some(sort) => sort_order(b, holder, &at, &sort),
                        None => b.unread(
                            holder,
                            at,
                            UnreadReason::Malformed,
                            "a sort order that does not read".into(),
                            vec![Kind::Field, Kind::Occurrence, Kind::ValueList],
                        ),
                    },
                    (ParamKind::ImportOrder | ParamKind::ExportOrder, _) => b.unread(
                        holder,
                        at,
                        UnreadReason::NotDecoded,
                        format!("the {name}, kept as stored"),
                        ORDER_KINDS.to_vec(),
                    ),
                    _ => b.unread(
                        holder,
                        at,
                        UnreadReason::NotDecoded,
                        format!("{name}, kept as stored"),
                        Kind::REFERABLE.to_vec(),
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
                            vec![Kind::DataSource],
                        );
                    }
                }
                Item::Bool(_) | Item::Value(_) | Item::Text(_) | Item::Number(_) => {}
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
    let unexplained: Vec<String> = step
        .unclassified()
        .into_iter()
        .filter(|u| !matches!(u, Unclassified::Flag(_) | Unclassified::Value(_)))
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
            Kind::REFERABLE.to_vec(),
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
            "a file reference that does not read".into(),
            vec![Kind::DataSource],
        ),
        (None, Some(script)) => b.reference(holder, at, Target::Object(ObjectId::Script(script))),
        (None, None) => b.unread(
            holder,
            at,
            UnreadReason::Malformed,
            "a script key that does not read".into(),
            vec![Kind::Script],
        ),
    }
}

/// Step 210's callback script (script-steps.md 12.1): `[00 6]` 0 from the
/// list, the key as a calculation of its quoted hex in slot 3; 1 by name,
/// slot 7; from another file, a file reference calculation in slot 5.
fn callback(b: &mut Builder, holder: ObjectId, at: &Place, step: &ScriptStep) {
    if step.list_value(6) == Some(1) {
        b.by_name(
            holder,
            at.clone(),
            NameUse::ScriptByName,
            None,
            vec![Kind::Script],
        );
        return;
    }
    let Some(script) = step.positioned_id(3) else {
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
                "a callback file reference that does not read".into(),
                vec![Kind::DataSource],
            ),
        }
    } else {
        b.reference(holder, at.clone(), Target::Object(ObjectId::Script(script)));
    }
}

/// The one table occurrence of a `[00 n, 11]` node (`KAliases`,
/// script-steps.md 4.2): `[len][segment]`. Several occurrences carry a
/// count `04`, which is not decoded here.
fn aliases(node: &om_hbam::OwnedNode) -> Option<u64> {
    if !node.children.is_empty() {
        return None;
    }
    let bytes = om_scripts::node_bytes(node)?;
    let (&len, segment) = bytes.split_first()?;
    (usize::from(len) == segment.len())
        .then(|| be_uint(segment))
        .flatten()
        .filter(|id| *id != 0)
}

/// A field a stored spec names; one with no occurrence cannot be placed.
fn spec_field(b: &mut Builder, holder: ObjectId, at: &Place, field: SpecField) {
    match field.occurrence {
        Some(occurrence) => b.reference(
            holder,
            at.clone(),
            Target::Field {
                occurrence,
                field: field.field,
            },
        ),
        None => b.unread(
            holder,
            at.clone(),
            UnreadReason::Malformed,
            "a field named without its table occurrence".into(),
            vec![Kind::Field],
        ),
    }
}

/// A sort order's fields, summary fields and value lists (11.1.1).
pub(super) fn sort_order(b: &mut Builder, holder: ObjectId, at: &Place, sort: &SortSpec) {
    for entry in &sort.fields {
        spec_field(b, holder, at, entry.field);
        if let Some(summary) = entry.summary {
            spec_field(b, holder, at, summary);
        }
        if let Some(list) = entry.value_list.filter(|id| *id != 0) {
            b.reference(
                holder,
                at.clone(),
                Target::Object(ObjectId::ValueList(list)),
            );
        }
    }
}

/// A find step's request fields (11.2).
fn requests(b: &mut Builder, holder: ObjectId, at: Place, node: &om_hbam::OwnedNode) {
    match specs::requests(node) {
        Some(requests) => {
            for request in &requests {
                for (field, _) in &request.criteria {
                    spec_field(b, holder, &at, *field);
                }
            }
        }
        None => b.unread(
            holder,
            at,
            UnreadReason::Malformed,
            "find requests that do not read".into(),
            vec![Kind::Field, Kind::Occurrence],
        ),
    }
}
