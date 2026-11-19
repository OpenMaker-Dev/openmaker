//! Per-step parameter schemas: `docs/re/catalogs/script-step-params.csv`
//! (script-steps.md 5), plus the locations the sample corpus settled
//! (script-steps.md, "Corpus answers (implementation)").

use crate::catalog::csv_rows;
use crate::step::{flag, VARIABLE_NAME_SLOT};
use std::collections::BTreeMap;
use std::sync::OnceLock;

const PARAMS: &str = include_str!("../../../docs/re/catalogs/script-step-params.csv");

/// What an inline reference in `mReserved` holds (script-steps.md 4.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RefKind {
    /// A `DBFieldSpec` (`kFieldSpec`), or a variable target.
    Field,
    /// A catalog member key (`kMemberKey`): a layout, a menu set, or a base
    /// table (Truncate Table, corpus).
    Member,
    /// A script key (`kScriptKey`).
    Script,
    /// Another catalog key: a data source (file reference) or privilege set.
    Key,
}

/// What a slot child holds (script-steps.md 4.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SlotKind {
    /// `[00 n, 05]`: a stored calculation.
    Calculation,
    /// `[00 n, 01]`: text.
    Text,
    /// `[00 n, 03]`: numeric data.
    Number,
    /// The value of the slot node `[00 n]` itself: a list choice.
    List,
    /// `[00 n, 10]`: a field spec.
    Field,
    /// `[00 n, 11]`: table occurrence keys.
    Aliases,
    /// `[00 n, 82]`: a path list.
    Path,
}

impl SlotKind {
    /// The child key under the slot node; `None` for [`SlotKind::List`],
    /// which is the slot node's own value.
    pub fn child(self) -> Option<u8> {
        match self {
            SlotKind::Calculation => Some(0x05),
            SlotKind::Text => Some(0x01),
            SlotKind::Number => Some(0x03),
            SlotKind::List => None,
            SlotKind::Field => Some(0x10),
            SlotKind::Aliases => Some(0x11),
            SlotKind::Path => Some(0x82),
        }
    }

    /// The kind stored under a slot child key.
    pub fn from_child(key: u8) -> Option<Self> {
        [
            SlotKind::Calculation,
            SlotKind::Text,
            SlotKind::Number,
            SlotKind::Field,
            SlotKind::Aliases,
            SlotKind::Path,
        ]
        .into_iter()
        .find(|k| k.child() == Some(key))
    }
}

/// Where one part of a parameter is stored (the CSV `storage` grammar).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Location {
    /// `flag:0xNN` (bits set = true) or `flag:!0xNN` (bits clear = true).
    Flag { bits: u32, inverted: bool },
    /// `value`: the step value `mStepValue`.
    Value,
    /// `reserved:<kind>`: the 14-byte inline reference `mReserved`.
    Reserved(RefKind),
    /// `slot:<n>:<kind>`: under the slot node `[00 n]`.
    Slot { slot: u16, kind: SlotKind },
    /// `key:<hex>`: a step-level key of the data subtree.
    Key(u8),
}

impl Location {
    /// Parse one `|`-separated part of the CSV `storage` column.
    pub fn parse(part: &str) -> Option<Self> {
        let mut it = part.trim().split(':');
        Some(match (it.next()?, it.next(), it.next()) {
            ("flag", Some(bits), None) => {
                let inverted = bits.starts_with('!');
                let hex = bits.trim_start_matches('!').trim_start_matches("0x");
                Location::Flag {
                    bits: u32::from_str_radix(hex, 16).ok()?,
                    inverted,
                }
            }
            ("value", None, None) => Location::Value,
            ("reserved", Some(kind), None) => Location::Reserved(match kind {
                "field" => RefKind::Field,
                "layout" | "member" => RefKind::Member,
                "script" => RefKind::Script,
                "key" => RefKind::Key,
                _ => return None,
            }),
            ("slot", Some(n), Some(kind)) => Location::Slot {
                slot: n.parse().ok()?,
                kind: match kind {
                    "calc" => SlotKind::Calculation,
                    "text" => SlotKind::Text,
                    "num" => SlotKind::Number,
                    "list" => SlotKind::List,
                    "field" => SlotKind::Field,
                    "aliases" => SlotKind::Aliases,
                    "path" => SlotKind::Path,
                    _ => return None,
                },
            },
            ("key", Some(hex), None) => Location::Key(u8::from_str_radix(hex, 16).ok()?),
            _ => return None,
        })
    }

    /// The step flags written together with this location (script-steps.md
    /// 2.2 and 4): explicit bits, or the flag that marks the data as present.
    pub fn flags(&self) -> u32 {
        match *self {
            Location::Flag { bits, .. } => bits,
            Location::Value => flag::MENU_SELECTION,
            Location::Reserved(RefKind::Field) => flag::FIELD_SPEC,
            Location::Reserved(RefKind::Member) => flag::MEMBER_KEY,
            Location::Reserved(RefKind::Script) => flag::SCRIPT_KEY,
            Location::Reserved(RefKind::Key) => flag::MEMBER_KEY | flag::FILE_REFERENCE,
            Location::Slot { kind, .. } => match kind {
                SlotKind::Calculation => flag::CALCULATION,
                SlotKind::Text => flag::TEXT_DATA,
                SlotKind::Path => flag::FILE_PATH,
                _ => 0,
            },
            Location::Key(key) => match key {
                0x20 => flag::FILE_REFERENCE,
                0x82 | 0x86 => flag::FILE_PATH,
                0x83 => flag::TURN_ON,
                _ => flag::EXTENDED_DATA,
            },
        }
    }
}

/// How a parameter is presented and edited.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ParamKind {
    /// An option: flag bits, or numeric data 0/1.
    Flag,
    /// A choice from an enumeration (`mStepValue` or a slot's list value).
    Choice,
    Calculation,
    Text,
    Number,
    Field,
    /// A field or a variable.
    Target,
    TableOccurrence,
    Layout,
    Script,
    /// A window chosen from a list or by a name calculation.
    Window,
    MenuSet,
    PrivilegeSet,
    /// File paths, file references and data sources.
    FileReference,
    /// A step index the editor caches (script-runtime.md 3.1).
    Index,
    /// Find requests (`06`), kept verbatim.
    FindRequests,
    /// A sort order (`0a`), kept verbatim.
    SortOrder,
    /// Import order and format (`84`, `86`), kept verbatim.
    ImportOrder,
    /// Export order and format (`85`, `86`), kept verbatim.
    ExportOrder,
    /// Page setup and print settings (`87`, `88`), kept verbatim.
    PrintSettings,
    /// An options element of several parts (the XML serializer names it,
    /// e.g. `SendMail`); its parts are its locations.
    Compound,
    /// Named by the XML writer, but where it is stored is not known
    /// (script-steps.md 9).
    Unmapped,
}

impl ParamKind {
    fn derive(xml_type: &str, name: &str, storage: &[Location]) -> Self {
        let only_num = !storage.is_empty()
            && storage.iter().all(|l| {
                matches!(
                    l,
                    Location::Slot {
                        kind: SlotKind::Number,
                        ..
                    }
                )
            });
        let has = |pred: fn(&Location) -> bool| storage.iter().any(pred);
        match xml_type {
            _ if storage.is_empty() => ParamKind::Unmapped,
            "Boolean" if storage.iter().all(|l| *l == Location::Value) => ParamKind::Choice,
            "Boolean" => ParamKind::Flag,
            "List" if storage.iter().all(|l| matches!(l, Location::Flag { .. })) => ParamKind::Flag,
            "Calculation" | "URL" => ParamKind::Calculation,
            "Text" => ParamKind::Text,
            "List" if has(|l| matches!(l, Location::Key(0x20 | 0x82))) => ParamKind::FileReference,
            "List" | "Index" if only_num || name.to_lowercase().contains("index") => {
                ParamKind::Index
            }
            "Index" => ParamKind::Index,
            "List" => ParamKind::Choice,
            "FieldReference" => ParamKind::Field,
            "Target" => ParamKind::Target,
            "ScriptReference" | "CallbackScriptReference" => ParamKind::Script,
            "LayoutReference" => ParamKind::Layout,
            "TableOccurrence" | "BaseTableReference" => ParamKind::TableOccurrence,
            "WindowReference" => ParamKind::Window,
            "DataSourceReference" | "DataSource" => ParamKind::FileReference,
            "MenuSet" => ParamKind::MenuSet,
            "PrivilegeSet" => ParamKind::PrivilegeSet,
            "FindRequests" => ParamKind::FindRequests,
            "SortSpecification" => ParamKind::SortOrder,
            "ImportOrder" => ParamKind::ImportOrder,
            "ExportOrder" => ParamKind::ExportOrder,
            "PageSetup" | "Print" => ParamKind::PrintSettings,
            _ => ParamKind::Compound,
        }
    }

    /// Whether the parameter is a spec subtree kept byte for byte.
    pub fn is_opaque(self) -> bool {
        matches!(
            self,
            ParamKind::FindRequests
                | ParamKind::SortOrder
                | ParamKind::ImportOrder
                | ParamKind::ExportOrder
                | ParamKind::PrintSettings
        )
    }
}

/// Where a schema row comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// `script-step-params.csv`.
    Spec,
    /// The sample corpus (script-steps.md, "Corpus answers (implementation)").
    Corpus,
}

/// One parameter of a step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParamSchema {
    /// XML element order (1-based).
    pub order: u16,
    pub name: String,
    /// The XML `<Parameter type=...>` kind or serializer name.
    pub xml_type: String,
    pub kind: ParamKind,
    /// Every location the parameter's parts are stored at.
    pub storage: Vec<Location>,
    /// Enumeration text, e.g. `1 First; 2 Last`.
    pub values: String,
    pub default: String,
    pub notes: String,
    pub source: Source,
}

impl ParamSchema {
    /// The numbered entries of `values` (`1 First; 2 Last` -> `(1, "First")`).
    pub fn choices(&self) -> Vec<(u32, String)> {
        self.values
            .split(';')
            .filter_map(|entry| {
                let (n, label) = entry.trim().split_once(' ')?;
                Some((n.parse().ok()?, label.trim().to_owned()))
            })
            .collect()
    }

    /// The flag bits this parameter accounts for.
    pub fn flags(&self) -> u32 {
        self.storage.iter().fold(0, |acc, l| acc | l.flags())
    }
}

/// A step id's parameters, in XML order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepSchema {
    pub id: u16,
    pub name: String,
    pub params: Vec<ParamSchema>,
}

impl StepSchema {
    pub fn parameter(&self, name: &str) -> Option<&ParamSchema> {
        self.params.iter().find(|p| p.name == name)
    }

    /// Every storage location of every parameter.
    pub fn locations(&self) -> impl Iterator<Item = &Location> {
        self.params.iter().flat_map(|p| &p.storage)
    }

    /// The flag bits the schema accounts for.
    pub fn flags(&self) -> u32 {
        self.params.iter().fold(0, |acc, p| acc | p.flags())
    }

    /// Whether `location` is one of the step's locations.
    pub fn has(&self, location: &Location) -> bool {
        self.locations().any(|l| l == location)
    }
}

/// The storage column: `|`-separated locations; corpus answers also write
/// slot ranges (`slot:0-6:num`).
fn parse_storage(storage: &str) -> Vec<Location> {
    let mut out = Vec::new();
    for part in storage.split('|').map(str::trim).filter(|p| !p.is_empty()) {
        let range = part
            .strip_prefix("slot:")
            .and_then(|rest| rest.split_once(':'))
            .and_then(|(slots, kind)| Some((slots.split_once('-')?, kind)));
        match range {
            Some(((from, to), kind)) => {
                let (Ok(from), Ok(to)) = (from.parse::<u16>(), to.parse::<u16>()) else {
                    continue;
                };
                out.extend(
                    (from..=to).filter_map(|n| Location::parse(&format!("slot:{n}:{kind}"))),
                );
            }
            None => out.extend(Location::parse(part)),
        }
    }
    out
}

/// A storage location the sample corpus settled (script-steps.md, "Corpus
/// answers (implementation)").
struct Answer {
    id: u16,
    /// The CSV parameter it extends, or a new parameter.
    param: &'static str,
    /// XML type of a new parameter; also tells apart CSV parameters of the
    /// same name.
    xml_type: &'static str,
    storage: &'static str,
    /// Replace the CSV storage instead of extending it.
    replace: bool,
    /// Enumeration text replacing the CSV's, when not empty.
    values: &'static str,
    note: &'static str,
}

const fn add(
    id: u16,
    param: &'static str,
    xml_type: &'static str,
    storage: &'static str,
    note: &'static str,
) -> Answer {
    Answer {
        id,
        param,
        xml_type,
        storage,
        replace: false,
        values: "",
        note,
    }
}

const fn replace(
    id: u16,
    param: &'static str,
    xml_type: &'static str,
    storage: &'static str,
    values: &'static str,
    note: &'static str,
) -> Answer {
    Answer {
        id,
        param,
        xml_type,
        storage,
        replace: true,
        values,
        note,
    }
}

const CHOICE: &str = "list choice with kMenuSelection; meaning not mapped (corpus)";
const OPTIONS: &str = "compound element parts seen in the corpus; meanings not mapped";

const CORPUS: &[Answer] = &[
    add(1, "Script", "", "key:20", "script in another file: file reference under 20 with kFileReference"),
    replace(1, "Specify by name", "", "slot:1:list|slot:2:calc|flag:0x800|flag:0x2000000", "", "[00 1] = 1 by name (0 from list), name calculation in slot 2; 0x800 and 0x2000000 written with it"),
    add(1, "List choice", "List", "value", CHOICE),
    replace(164, "Script", "", "reserved:script", "", "by-name selection uses the separate Specify by name parameter"),
    add(164, "Specify by name", "Calculation", "slot:1:list|slot:2:calc|flag:0x800|flag:0x2000000", "as Perform Script: [00 1] = 1, name in slot 2 (native XML)"),
    replace(6, "Layout", "List", "value", "1 original layout; 3 layout name by calculation; 4 layout number by calculation; 5 named layout (stored key)", "values seen in the corpus"),
    replace(6, "Layout", "LayoutReference", "reserved:layout|slot:5:calc", "", "name or number calculation in slot 5"),
    add(17, "Field", "", "slot:10:calc", "repetition calculation of the inline field (spec repetition 0)"),
    add(33, "Data source", "", "key:20", "file reference under 20 with kFileReference"),
    add(34, "Data source", "", "key:20", "file reference under 20 with kFileReference"),
    add(35, "Data source", "", "slot:0:calc|slot:1:calc", OPTIONS),
    add(35, "Import order", "", "key:87|key:f0", "KImportProfileEx (87) and KFlags (f0)"),
    add(35, "Import action", "List", "value", "1, 2 and 4 seen with kMenuSelection; probably the three import actions (Low)"),
    add(36, "Automatically open", "Boolean", "flag:0x20000000", "kOpenAutomatically"),
    add(36, "Create email", "Boolean", "flag:0x40000000", "kCreateEmail"),
    add(37, "Automatically open", "Boolean", "flag:0x20000000", "kOpenAutomatically"),
    add(37, "Create email", "Boolean", "flag:0x40000000", "kCreateEmail"),
    add(48, "List choice", "List", "value", CHOICE),
    add(57, "Send Event", "", "slot:0-6:num|slot:7:text", "event class, event id and target signature as four-character numbers"),
    add(63, "Send Mail", "", "slot:0-6:num|slot:6:calc|slot:8-11:calc", OPTIONS),
    add(65, "List choice", "List", "value", CHOICE),
    add(66, "Speak options", "", "slot:0-2:num|slot:0:text|flag:0x4", "voice as a four-character number and its name as text"),
    add(67, "AppleScript", "", "value|slot:0:num|key:83", "compiled script blob under 83 (with 0x20000)"),
    add(74, "Animation", "List", "slot:6:list", "as Go to Layout"),
    replace(76, "Repetition", "", "slot:10:calc", "", "repetition calculation in slot 10 (spec repetition 0); slot 1 never seen"),
    replace(87, "Input labels", "", "slot:2-4:calc", "", "labels of inputs 1-3"),
    add(87, "Button label calculations", "Calculation", "slot:5-7:calc", "calculated labels of buttons 1-3, written beside the text labels"),
    replace(87, "Button commit", "", "slot:3-5:num", "", "always stored; slot 3 is 1 in 1224 of 1391 steps (OK commits)"),
    replace(87, "Password", "", "slot:0-2:num", "", "stored only with input fields (831 of 831)"),
    add(87, "Input fields", "", "slot:32-34:text", "variable names of inputs 1-3"),
    add(91, "Replace with", "", "slot:0:aliases|slot:1-6:num|slot:3:text|flag:0x4|flag:0x2000000", "[00 1]: 1 current contents, 2 serial numbers, 3 calculated result; [00 2] increment, [00 3] initial value text"),
    add(117, "SQL query", "", "key:86", "KProfile"),
    add(122, "Window style and options", "", "slot:1-3:num", OPTIONS),
    add(128, "Find/Replace", "", "slot:0-5:num", OPTIONS),
    add(131, "Insert File options", "", "slot:0:calc|key:8c", "KFileFilterSpec (8c) with kExtendedData"),
    add(131, "Store only a reference", "Boolean", "flag:0x800", "kStoreReference"),
    add(134, "List choice", "List", "value", CHOICE),
    add(136, "List choice", "List", "value", CHOICE),
    add(137, "List choice", "List", "value", CHOICE),
    add(139, "Data source", "", "key:86|slot:1:calc", OPTIONS),
    add(139, "List choice", "List", "value", CHOICE),
    add(143, "Excel options", "", "slot:0:num|slot:1:num|key:86", OPTIONS),
    add(144, "Source", "", "value", "values 1-6 in mStepValue; slot 11 never seen"),
    add(144, "PDF options", "", "slot:0-10:num|slot:0-8:calc", OPTIONS),
    add(144, "Automatically open", "Boolean", "flag:0x20000000", "kOpenAutomatically"),
    add(144, "Create email", "Boolean", "flag:0x40000000", "kCreateEmail"),
    add(146, "Action", "", "slot:0:num", OPTIONS),
    add(152, "Create email", "Boolean", "flag:0x40000000", "kCreateEmail"),
    add(161, "Insert from device options", "", "slot:0-6:num|slot:1:field", OPTIONS),
    add(175, "Parameters", "", "slot:0:num|slot:14-26:calc", "[00 0] = parameter count; parameter i in slot 10 + i"),
    add(177, "AVPlayer Play", "", "reserved:field|slot:6:list|slot:7:list|slot:10:list|slot:14:list|flag:0x4|flag:0x8|flag:0x40|flag:0x200|flag:0x400|flag:0x800", OPTIONS),
    add(179, "AVPlayer options", "", "slot:6-14:list|slot:13:calc|flag:0x4|flag:0x8|flag:0x10|flag:0x20|flag:0x40|flag:0x100|flag:0x200|flag:0x400|flag:0x800|flag:0x40000|flag:0x8000000", OPTIONS),
    add(181, "Target variable", "", "slot:2:calc", "repetition calculation, as Set Variable"),
    add(182, "Table", "", "reserved:member", "base table key (two-byte id >= 0x8000) with kMemberKey"),
    add(185, "Region monitor", "", "key:20|slot:6:calc|slot:7:calc|slot:8:list|flag:0x4|flag:0x8|flag:0x20|flag:0x100|flag:0x200|flag:0x400|flag:0x800", OPTIONS),
    add(186, "Function", "List", "value", "the plug-in's function id (script-runtime.md 2.5)"),
    add(186, "Plug-in", "PlugIn", "key:83", "the plug-in's QuadChar id (e.g. MBSP) and bytes"),
    add(186, "Parameters", "Calculation", "slot:0-9:calc", "one calculation per plug-in parameter"),
    add(186, "Target", "Target", "reserved:field|slot:32:text", "field or variable result target"),
    add(186, "Options", "Boolean", "flag:0x100|flag:0x200|flag:0x1000", "set on every corpus step; meanings not mapped"),
    add(187, "Local notification", "", "slot:1:list", OPTIONS),
    add(193, "Amount", "", "flag:0x8000", "kExtendedData marks the amount calculation"),
    add(195, "New position", "", "flag:0x8000", "kExtendedData marks the position calculation"),
    add(201, "NFC options", "", "key:20|slot:1:list|slot:3:calc|slot:4:calc|flag:0x4|flag:0x20|flag:0x400", OPTIONS),
    add(202, "Core ML model", "", "reserved:field|slot:2:list", OPTIONS),
];

fn schemas() -> &'static BTreeMap<u16, StepSchema> {
    static SCHEMAS: OnceLock<BTreeMap<u16, StepSchema>> = OnceLock::new();
    SCHEMAS.get_or_init(|| {
        let mut out: BTreeMap<u16, StepSchema> = BTreeMap::new();
        for row in csv_rows(PARAMS) {
            let get = |k: &str| row.get(k).cloned().unwrap_or_default();
            let Ok(id) = get("step_id").parse::<u16>() else {
                continue;
            };
            let schema = out.entry(id).or_insert_with(|| StepSchema {
                id,
                name: get("step_name"),
                params: Vec::new(),
            });
            let order: u16 = get("order").parse().unwrap_or(0);
            if order == 0 {
                continue;
            }
            let storage = parse_storage(&get("storage"));
            let (name, xml_type) = (get("parameter"), get("xml_type"));
            schema.params.push(ParamSchema {
                order,
                kind: ParamKind::derive(&xml_type, &name, &storage),
                name,
                xml_type,
                storage,
                values: get("values"),
                default: get("default"),
                notes: get("notes"),
                source: Source::Spec,
            });
        }
        for answer in CORPUS {
            let schema = out.entry(answer.id).or_insert_with(|| StepSchema {
                id: answer.id,
                name: crate::step_name(answer.id).unwrap_or_default().into(),
                params: Vec::new(),
            });
            let locations = parse_storage(answer.storage);
            let note = format!("corpus: {}", answer.note);
            let target = schema.params.iter_mut().find(|p| {
                p.name == answer.param
                    && (answer.xml_type.is_empty() || p.xml_type == answer.xml_type)
            });
            match target {
                Some(param) => {
                    if answer.replace {
                        param.storage = locations;
                    } else {
                        param.storage.extend(locations);
                    }
                    if !answer.values.is_empty() {
                        param.values = answer.values.into();
                    }
                    param.notes = if param.notes.is_empty() {
                        note
                    } else {
                        format!("{}; {note}", param.notes)
                    };
                }
                None => {
                    let order = schema.params.len() as u16 + 1;
                    schema.params.push(ParamSchema {
                        order,
                        kind: ParamKind::derive(answer.xml_type, answer.param, &locations),
                        name: answer.param.into(),
                        xml_type: answer.xml_type.into(),
                        storage: locations,
                        values: answer.values.into(),
                        default: String::new(),
                        notes: note,
                        source: Source::Corpus,
                    });
                }
            }
        }
        for schema in out.values_mut() {
            for param in &mut schema.params {
                // File paths live at [00 0, 82]; a step-level 82 never occurs.
                if param.storage.contains(&Location::Key(0x82)) {
                    param.storage.push(Location::Slot {
                        slot: 0,
                        kind: SlotKind::Path,
                    });
                }
                // A variable target's name is text in slot 0x20.
                if param.kind == ParamKind::Target
                    && param.storage.contains(&Location::Reserved(RefKind::Field))
                {
                    let name = Location::Slot {
                        slot: VARIABLE_NAME_SLOT,
                        kind: SlotKind::Text,
                    };
                    if !param.storage.contains(&name) {
                        param.storage.push(name);
                    }
                }
            }
        }
        out
    })
}

/// The parameter schema of a step id (`None` for ids the CSV does not name).
pub fn step_schema(id: u16) -> Option<&'static StepSchema> {
    schemas().get(&id)
}

/// Every step schema, by id.
pub fn step_schemas() -> impl Iterator<Item = &'static StepSchema> {
    schemas().values()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_storage_part_parses() {
        for row in csv_rows(PARAMS) {
            for part in row["storage"].split('|').filter(|p| !p.is_empty()) {
                assert!(Location::parse(part).is_some(), "{part}");
            }
        }
        assert!(step_schemas().count() >= 221);
    }

    #[test]
    fn schemas_type_their_parameters() {
        let set_variable = step_schema(141).unwrap();
        let kinds: Vec<_> = set_variable.params.iter().map(|p| p.kind).collect();
        assert_eq!(
            kinds,
            [
                ParamKind::Text,
                ParamKind::Calculation,
                ParamKind::Calculation
            ]
        );
        let go_to_record = step_schema(16).unwrap();
        let target = go_to_record.parameter("Record/Request/Page").unwrap();
        assert_eq!(target.kind, ParamKind::Choice);
        assert_eq!(target.choices()[0], (1, "First".into()));
        assert_eq!(
            go_to_record
                .parameter("Exit-after-last target")
                .unwrap()
                .kind,
            ParamKind::Index
        );
        assert_eq!(
            go_to_record.parameter("With dialog").unwrap().storage,
            [Location::Flag {
                bits: 0x80,
                inverted: true
            }]
        );
        let sort = step_schema(39).unwrap().parameter("Sort order").unwrap();
        assert!(sort.kind.is_opaque());
        assert_eq!(
            step_schema(63)
                .unwrap()
                .parameter("Send Mail")
                .unwrap()
                .kind,
            ParamKind::Compound
        );
        assert!(step_schema(7).unwrap().params.is_empty());
    }

    #[test]
    fn corpus_answers_extend_and_correct_the_csv() {
        let go_to_layout = step_schema(6).unwrap();
        let layouts: Vec<_> = go_to_layout
            .params
            .iter()
            .filter(|p| p.name == "Layout")
            .collect();
        assert_eq!(
            layouts[0].choices()[1],
            (3, "layout name by calculation".into())
        );
        assert!(layouts[1].storage.contains(&Location::Slot {
            slot: 5,
            kind: SlotKind::Calculation
        }));
        assert!(!layouts[1].storage.contains(&Location::Slot {
            slot: 0,
            kind: SlotKind::Calculation
        }));
        // File paths at [00 0, 82]; variable names at slot 0x20 + n.
        let paths = step_schema(36).unwrap().parameter("File paths").unwrap();
        assert!(paths.storage.contains(&Location::Slot {
            slot: 0,
            kind: SlotKind::Path
        }));
        let target = step_schema(61).unwrap().parameter("Target").unwrap();
        assert!(target.storage.contains(&Location::Slot {
            slot: 0x20,
            kind: SlotKind::Text
        }));
        let dialog = step_schema(87).unwrap();
        assert_eq!(
            parse_storage("slot:3-5:num"),
            dialog.parameter("Button commit").unwrap().storage
        );
        assert_eq!(
            dialog
                .parameter("Button label calculations")
                .unwrap()
                .source,
            Source::Corpus
        );
        let plug_in = step_schema(186).unwrap();
        assert_eq!(plug_in.parameter("Target").unwrap().kind, ParamKind::Target);
        assert_eq!(plug_in.parameter("Parameters").unwrap().storage.len(), 10);
    }
}
