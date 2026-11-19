//! Typed parameters: a step's stored descriptor and data read through its
//! schema (script-steps.md 4-5).

use crate::flow;
use crate::key::{FieldSpec, StoredKey};
use crate::schema::{step_schema, Location, ParamKind, ParamSchema, RefKind, SlotKind, StepSchema};
use crate::step::{flag, Calculation, ScriptStep, StepData, VARIABLE_NAME_SLOT};
use crate::value::{node_bytes, node_text};
use crate::ScriptError;
use om_hbam::OwnedNode;
use std::fmt;

/// The stored value of one location.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item<'a> {
    Bool(bool),
    /// The step value.
    Value(u16),
    Field(FieldSpec),
    /// A layout, menu set or script key.
    Key(StoredKey),
    /// The whole inline reference (data sources, privilege sets).
    Reserved(&'a [u8; 14]),
    Calculation(Calculation),
    Text(String),
    Number(i64),
    /// A subtree kept as stored (table occurrences, file references, specs).
    Node(&'a OwnedNode),
}

/// A parameter's value, for the kinds that have a single value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParamValue {
    Bool(bool),
    Choice(u32),
    Calculation(Calculation),
    Text(String),
    Number(i64),
    Field(FieldSpec),
    /// A target that is a variable (`$name`, `$$name`).
    Variable(String),
    /// A layout, script or menu set key.
    Key(StoredKey),
}

/// One schema parameter of a step and what is stored for it.
#[derive(Clone, Debug)]
pub struct Parameter<'a> {
    pub schema: &'static ParamSchema,
    /// Each location of the parameter and its stored value, if any.
    pub parts: Vec<(Location, Option<Item<'a>>)>,
}

impl Parameter<'_> {
    /// Whether anything is stored for the parameter (a set flag, a nonzero
    /// value, a valid reference or a leaf).
    pub fn is_stored(&self) -> bool {
        self.parts.iter().any(|(_, item)| match item {
            Some(Item::Bool(b)) => *b,
            Some(Item::Value(v)) => *v != 0,
            Some(_) => true,
            None => false,
        })
    }

    /// The parameter's value, for kinds with one (a Boolean, a choice, a
    /// calculation, text, a number, a field, a target, a key). Compound,
    /// opaque and file parameters are read through `parts`.
    pub fn value(&self) -> Option<ParamValue> {
        let items = || {
            self.parts
                .iter()
                .filter_map(|(l, i)| Some((l, i.as_ref()?)))
        };
        match self.schema.kind {
            ParamKind::Flag => items().find_map(|(_, item)| match item {
                Item::Bool(b) => Some(ParamValue::Bool(*b)),
                Item::Number(n) => Some(ParamValue::Bool(*n != 0)),
                _ => None,
            }),
            ParamKind::Choice => items().find_map(|(l, item)| match (l, item) {
                (Location::Value, Item::Value(v)) => Some(ParamValue::Choice(u32::from(*v))),
                (
                    Location::Slot {
                        kind: SlotKind::List,
                        ..
                    },
                    Item::Number(n),
                ) => u32::try_from(*n).ok().map(ParamValue::Choice),
                _ => None,
            }),
            ParamKind::Calculation => items().find_map(|(_, item)| match item {
                Item::Calculation(c) => Some(ParamValue::Calculation(c.clone())),
                _ => None,
            }),
            ParamKind::Text => items().find_map(|(_, item)| match item {
                Item::Text(t) => Some(ParamValue::Text(t.clone())),
                _ => None,
            }),
            ParamKind::Number | ParamKind::Index => items().find_map(|(_, item)| match item {
                Item::Number(n) => Some(ParamValue::Number(*n)),
                Item::Value(v) => Some(ParamValue::Number(i64::from(*v))),
                _ => None,
            }),
            ParamKind::Field | ParamKind::Target => {
                let field = items().find_map(|(_, item)| match item {
                    Item::Field(f) => Some(*f),
                    _ => None,
                })?;
                if !field.is_variable() {
                    return Some(ParamValue::Field(field));
                }
                // A variable's name: the target's own text slot (corpus).
                items()
                    .find_map(|(l, item)| match (l, item) {
                        (
                            Location::Slot {
                                slot,
                                kind: SlotKind::Text,
                            },
                            Item::Text(t),
                        ) if *slot >= VARIABLE_NAME_SLOT => Some(ParamValue::Variable(t.clone())),
                        _ => None,
                    })
                    .or(Some(ParamValue::Field(field)))
            }
            ParamKind::Layout | ParamKind::Script | ParamKind::MenuSet => {
                items().find_map(|(_, item)| match item {
                    Item::Key(k) => Some(ParamValue::Key(*k)),
                    _ => None,
                })
            }
            _ => None,
        }
    }
}

/// A stored bit, value or node that no location of the step's schema
/// explains. Such items are kept verbatim.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Unclassified {
    Flag(u32),
    /// A nonzero step value.
    Value(u16),
    /// The value of slot node `[00 n]`, or an empty slot.
    Slot(u16),
    /// A one-byte child of a slot.
    SlotChild(u16, u8),
    /// A calculation child other than `04`-`06`, or a malformed stream.
    CalculationChild(u16, Vec<u8>),
    /// A step-level key.
    Key(u8),
    /// Any other node (path of segments below the step's subtree).
    Node(Vec<Vec<u8>>),
}

impl fmt::Display for Unclassified {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        match self {
            Unclassified::Flag(bit) => write!(f, "flag {bit:#x}"),
            Unclassified::Value(v) => write!(f, "value {v}"),
            Unclassified::Slot(n) => write!(f, "slot {n:04x}"),
            Unclassified::SlotChild(n, k) => write!(f, "slot {n:04x}/{k:02x}"),
            Unclassified::CalculationChild(n, seg) => write!(f, "slot {n:04x}/05/{}", hex(seg)),
            Unclassified::Key(k) => write!(f, "key {k:02x}"),
            Unclassified::Node(path) => {
                let path: Vec<String> = path.iter().map(|s| hex(s)).collect();
                write!(f, "node {}", path.join("/"))
            }
        }
    }
}

impl ScriptStep {
    /// The step's schema (`None` for ids the schema does not name).
    pub fn schema(&self) -> Option<&'static StepSchema> {
        step_schema(self.id())
    }

    /// The stored value of a location, `None` when nothing is stored there
    /// (an inline reference counts only while its flag is set).
    pub fn item(&self, location: &Location) -> Option<Item<'_>> {
        let data = self.data.as_ref();
        let slot = |n: u16| data.and_then(|d| d.slot(n));
        Some(match *location {
            Location::Flag { bits, inverted } => Item::Bool(self.has_flag(bits) != inverted),
            Location::Value => Item::Value(self.value),
            Location::Reserved(RefKind::Field) => Item::Field(self.field_spec()?),
            Location::Reserved(RefKind::Member) => Item::Key(self.member_key()?),
            Location::Reserved(RefKind::Script) => Item::Key(self.script_key()?),
            Location::Reserved(RefKind::Key) => {
                if self.flags & (flag::MEMBER_KEY | flag::FILE_REFERENCE) == 0 {
                    return None;
                }
                Item::Reserved(&self.reserved)
            }
            Location::Slot { slot: n, kind } => match kind {
                SlotKind::Calculation => Item::Calculation(self.calculation(n)?),
                SlotKind::Text => Item::Text(self.text(n)?),
                SlotKind::Number => Item::Number(self.number(n)?),
                SlotKind::List => Item::Number(i64::from(self.list_value(n)?)),
                SlotKind::Field => Item::Field(self.slot_field_spec(n)?),
                SlotKind::Aliases => Item::Node(slot(n)?.child(0x11)?),
                SlotKind::Path => {
                    let node = slot(n)?.child(0x82)?;
                    node_text(node).map_or(Item::Node(node), Item::Text)
                }
            },
            Location::Key(key) => Item::Node(data?.key(key)?),
        })
    }

    /// Every schema parameter with what is stored for it.
    pub fn parameters(&self) -> Vec<Parameter<'_>> {
        self.schema()
            .map(|schema| {
                schema
                    .params
                    .iter()
                    .map(|p| Parameter {
                        schema: p,
                        parts: p.storage.iter().map(|l| (*l, self.item(l))).collect(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// One schema parameter by name.
    pub fn parameter(&self, name: &str) -> Option<Parameter<'_>> {
        self.parameters()
            .into_iter()
            .find(|p| p.schema.name == name)
    }

    /// Everything stored that the step's schema does not explain: flag bits
    /// outside every location (other than the editor's breakpoint and
    /// collapsed bits), a step value no location uses, and data nodes in no
    /// location. Empty when the step is fully typed.
    pub fn unclassified(&self) -> Vec<Unclassified> {
        let schema = self.schema();
        let has = |l: &Location| schema.is_some_and(|s| s.has(l));
        let mut out = Vec::new();
        let explained = schema.map_or(0, StepSchema::flags) | flag::EDITOR;
        for bit in (0..32).map(|b| 1u32 << b) {
            if self.flags & bit != 0 && explained & bit == 0 {
                out.push(Unclassified::Flag(bit));
            }
        }
        // Block steps cache an index; disabled ones keep a stale one.
        if self.value != 0 && !flow::caches_index(self.id()) && !has(&Location::Value) {
            out.push(Unclassified::Value(self.value));
        }
        let Some(data) = &self.data else {
            return out;
        };
        let slot_used = |n: u16| {
            schema.is_some_and(|s| {
                s.locations()
                    .any(|l| matches!(l, Location::Slot { slot, .. } if *slot == n))
            })
        };
        for (n, slot) in data.slots() {
            let empty = slot.value().is_none() && slot.child_keys().next().is_none();
            if (slot.value().is_some()
                && !has(&Location::Slot {
                    slot: n,
                    kind: SlotKind::List,
                }))
                || (empty && slot.other().is_empty() && !slot_used(n))
            {
                out.push(Unclassified::Slot(n));
            }
            for key in slot.child_keys() {
                let known = SlotKind::from_child(key)
                    .is_some_and(|kind| has(&Location::Slot { slot: n, kind }));
                if !known {
                    out.push(Unclassified::SlotChild(n, key));
                } else if key == 0x05 {
                    let calc = slot.child(key).expect("present");
                    for child in &calc.children {
                        let stream = matches!(child.segment.as_slice(), [0x04..=0x06]);
                        if !stream || node_bytes(child).is_none() {
                            out.push(Unclassified::CalculationChild(n, child.segment.clone()));
                        }
                    }
                }
            }
            for node in slot.other() {
                out.push(Unclassified::Node(vec![
                    n.to_be_bytes().to_vec(),
                    node.segment.clone(),
                ]));
            }
        }
        for key in data.keys() {
            if !StepData::ITEM_KEYS.contains(&key) && !has(&Location::Key(key)) {
                out.push(Unclassified::Key(key));
            }
        }
        for node in data.other() {
            out.push(Unclassified::Node(vec![node.segment.clone()]));
        }
        if data.node_value().is_some() {
            out.push(Unclassified::Node(Vec::new()));
        }
        out
    }

    /// Set a parameter by its schema name, writing the location that holds
    /// that kind of value and maintaining the flags (script-steps.md 6).
    /// Compound and opaque parameters are set through the location setters.
    pub fn set_parameter(&mut self, name: &str, value: ParamValue) -> Result<(), ScriptError> {
        let step = self.name();
        let schema = self
            .schema()
            .and_then(|s| s.parameter(name))
            .ok_or_else(|| ScriptError(format!("{step} has no parameter \"{name}\".")))?;
        let unsupported = ScriptError(format!("\"{name}\" of {step} cannot hold {value:?}."));
        let has = |location: Location| schema.storage.contains(&location);
        let slot = |kind: SlotKind| {
            schema.storage.iter().find_map(|l| match *l {
                Location::Slot { slot, kind: k } if k == kind => Some(slot),
                _ => None,
            })
        };
        let flag = schema.storage.iter().find_map(|l| match *l {
            Location::Flag { bits, inverted } => Some((bits, inverted)),
            _ => None,
        });
        // A field or variable target: `mReserved` (slot 0) or a slot's spec.
        let target = if has(Location::Reserved(RefKind::Field)) {
            Some(0)
        } else {
            slot(SlotKind::Field)
        };
        match value {
            ParamValue::Bool(on) => match (flag, slot(SlotKind::Number)) {
                (Some((bits, inverted)), _) => self.set_flag(bits, on != inverted),
                (None, Some(n)) => self.set_number_bool(n, Some(on)),
                (None, None) => return Err(unsupported),
            },
            ParamValue::Choice(choice) if has(Location::Value) => {
                self.value = u16::try_from(choice).map_err(|_| unsupported)?;
                self.flags |= flag::MENU_SELECTION;
            }
            ParamValue::Choice(choice) => {
                let n = slot(SlotKind::List).ok_or(unsupported)?;
                self.set_list_value(n, Some(choice));
            }
            ParamValue::Calculation(calc) => {
                let n = slot(SlotKind::Calculation).ok_or(unsupported)?;
                self.set_calculation(n, Some(&calc));
            }
            ParamValue::Text(text) => {
                let n = slot(SlotKind::Text).ok_or(unsupported)?;
                self.set_text(n, Some(&text));
            }
            ParamValue::Number(number) => {
                let n = slot(SlotKind::Number).ok_or_else(|| unsupported.clone())?;
                self.set_number(n, Some(i32::try_from(number).map_err(|_| unsupported)?));
            }
            ParamValue::Field(spec) => {
                self.set_field_spec(target.ok_or(unsupported)?, Some(spec));
            }
            ParamValue::Variable(var) => match target {
                Some(n) if schema.kind == ParamKind::Target => self.set_variable_target(n, &var),
                _ => return Err(unsupported),
            },
            ParamValue::Key(key) if has(Location::Reserved(RefKind::Member)) => {
                self.set_member_key(Some(key));
            }
            ParamValue::Key(key) if has(Location::Reserved(RefKind::Script)) => {
                self.set_script_key(Some(key));
            }
            ParamValue::Key(_) => return Err(unsupported),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameters_read_through_the_schema() {
        let mut step = ScriptStep::new(141);
        step.set_parameter("Name", ParamValue::Text("$count".into()))
            .unwrap();
        let calc = Calculation::new(None, vec![0x13, 1, b'1' ^ 0x5a, 0]);
        step.set_parameter("Value", ParamValue::Calculation(calc.clone()))
            .unwrap();
        assert_eq!(
            step.parameter("Name").unwrap().value(),
            Some(ParamValue::Text("$count".into()))
        );
        assert_eq!(
            step.parameter("Value").unwrap().value(),
            Some(ParamValue::Calculation(calc))
        );
        assert!(!step.parameter("Repetition").unwrap().is_stored());
        assert!(step.unclassified().is_empty());

        let mut dialog = ScriptStep::new(9);
        assert_eq!(
            dialog.parameter("With dialog").unwrap().value(),
            Some(ParamValue::Bool(true))
        );
        dialog
            .set_parameter("With dialog", ParamValue::Bool(false))
            .unwrap();
        assert!(dialog.has_flag(flag::NO_INTERACT));

        let mut insert = ScriptStep::new(61);
        insert
            .set_parameter("Target", ParamValue::Variable("$out".into()))
            .unwrap();
        assert!(insert
            .set_parameter("Target", ParamValue::Choice(1))
            .is_err());
        assert!(insert
            .set_parameter("Nope", ParamValue::Bool(true))
            .is_err());
    }

    #[test]
    fn unexplained_bits_values_and_leaves_are_reported() {
        let mut step = ScriptStep::new(7);
        step.flags = flag::OPTION | flag::BREAKPOINT;
        step.value = 3;
        step.set_data_leaf(StepData::UNIQUE_ID, Some(OwnedNode::leaf([0xd8], [0; 16])));
        step.set_number(4, Some(1));
        assert_eq!(
            step.unclassified(),
            [
                Unclassified::Flag(flag::OPTION),
                Unclassified::Value(3),
                Unclassified::SlotChild(4, 0x03)
            ]
        );
        // Block steps' cached indexes are explained, stale ones included.
        let mut end_loop = ScriptStep::new(73);
        end_loop.value = 4;
        end_loop.set_enabled(false);
        assert!(end_loop.unclassified().is_empty());
    }
}
