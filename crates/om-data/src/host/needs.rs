//! The occurrences a layout reads when it is drawn and its records shown,
//! so a client opening a hosted file can open the related files that layout
//! needs first and leave the others until something reads them (#983;
//! FileMaker Pro 26 opened 19 of a solution's 63 files over a whole session,
//! the rest never).
//!
//! What a layout reads, from its definition and the schema:
//! - its own occurrence, and those of its field objects (portal rows
//!   included), merge fields and portals;
//! - the occurrences the field-based value lists of its field objects read;
//! - every occurrence a formula of an object names that decides more than
//!   its look: hide, script parameter, web address and portal filter;
//! - for each field object of a calculation field, the occurrences its
//!   formula names, and so on through the fields those name (bounded);
//! - every occurrence on the relationship path from the layout's own to
//!   each of those, which a related read walks.
//!
//! The formulas that only change how an object looks (conditional formats,
//! tooltip, calculated label, placeholder, active segment) are not: the
//! window evaluates them when it draws the object, and a read of a file not
//! open asks for it then. FileMaker Pro 26 opened none of the files only
//! such formulas of objects outside its window read (#1120: a first
//! layout's conditional formats right of its view read a file it opened
//! only later, after user actions; inferred from which files it opened).
//!
//! An occurrence named only in text a formula builds (`Evaluate`,
//! `GetField`, `ExecuteSQL`), or in a custom function's body, a script or
//! another layout is not listed either: reads of it ask for its file when
//! they happen ([`crate::host::CalcNeed::Occurrence`]).

use std::collections::BTreeSet;

use om_formula::Formula;
use om_layout::Layout;

use super::schema::CalcSchema;
use crate::meta::{ValueList, ValueSource};

/// How deep the fields a calculation names are followed for their own
/// formulas: a bound on a malformed file's cycles, past any real chain.
const MAX_DEPTH: usize = 16;

/// The occurrences `layout` reads (module docs), in id order. The file's
/// own and related ones alike; the caller opens those whose files are not
/// open ([`CalcSchema::is_pending`]).
pub fn layout_occurrences(schema: &CalcSchema, layout: &Layout) -> BTreeSet<u64> {
    let own = layout
        .def
        .as_ref()
        .and_then(|def| om_layout::occurrence_id(def.table_alias_key));
    let mut found = Occurrences::default();
    if let Some(own) = own {
        found.occurrences.insert(own);
    }
    for object in &layout.objects.objects {
        if let Some(spec) = object.field_spec() {
            if let (Some(occurrence), Some(field)) =
                (om_layout::occurrence_id(spec.table_key), spec.field_id())
            {
                found.field(schema, occurrence, field, 0);
            }
        }
        // Merge fields name their fields by occurrence and field id.
        for spec in object.merge_field_specs() {
            found.field(schema, spec.table_key, spec.field_key, 0);
        }
        if let Some(portal) = &object.portal {
            if let Some(occurrence) = om_layout::occurrence_id(portal.table_alias_key) {
                found.occurrences.insert(occurrence);
            }
        }
        if let Ok(list) = crate::choice::value_list(schema, object) {
            found.value_list(list);
        }
        let calcs = &object.calcs;
        let formulas = [
            &calcs.hide,
            &calcs.script_parameter,
            &calcs.web_address,
            &calcs.portal_filter,
        ]
        .into_iter()
        .flatten();
        for formula in formulas {
            found.formula(schema, formula, 0);
        }
    }
    // The paths a related read walks from the layout's occurrence.
    if let Some(own) = own {
        let reached: Vec<u64> = found.occurrences.iter().copied().collect();
        for to in reached {
            for hop in schema.path(own, to).unwrap_or_default() {
                found.occurrences.insert(hop.to);
            }
        }
    }
    found.occurrences
}

/// The occurrences a write reads through its rules (#1139), so the
/// related files they need can be opened before the write is planned, as a
/// layout's are before it is drawn: what `formulas` name (the stored,
/// auto-entered and validation calculations the write computes), followed
/// through the calculations of the fields they name, each with the
/// relationship path from the formula's own context; the occurrences of its
/// looked-up values (`lookups`: from, to) and of its validation value lists
/// (`lists`), with the path between them.
pub fn rule_occurrences(
    schema: &CalcSchema,
    formulas: &[Formula],
    lookups: &[(u64, u64)],
    lists: &[&ValueList],
) -> BTreeSet<u64> {
    let mut found = Occurrences::default();
    let mut paths: Vec<(u64, u64)> = Vec::new();
    for formula in formulas {
        let before: BTreeSet<u64> = found.occurrences.clone();
        found.formula(schema, formula, 0);
        if let Some(context) = formula.occurrence() {
            found.occurrences.insert(context);
            paths.extend(
                found
                    .occurrences
                    .difference(&before)
                    .map(|to| (context, *to)),
            );
        }
    }
    for &(from, to) in lookups {
        found.occurrences.extend([from, to]);
        paths.push((from, to));
    }
    for list in lists {
        found.value_list(list);
    }
    for (from, to) in paths {
        for hop in schema.path(from, to).unwrap_or_default() {
            found.occurrences.insert(hop.to);
        }
    }
    found.occurrences
}

#[derive(Default)]
struct Occurrences {
    occurrences: BTreeSet<u64>,
    /// The calculation fields followed, by occurrence and field.
    followed: BTreeSet<(u64, u64)>,
}

impl Occurrences {
    /// Field `field` of `occurrence` is read: its occurrence, and the
    /// occurrences its formula names when it is a calculation.
    fn field(&mut self, schema: &CalcSchema, occurrence: u64, field: u64, depth: usize) {
        self.occurrences.insert(occurrence);
        if depth >= MAX_DEPTH || !self.followed.insert((occurrence, field)) {
            return;
        }
        let Some(formula) = schema
            .field(occurrence, field)
            .and_then(|def| def.formula.clone())
        else {
            return;
        };
        self.formula(schema, &formula, depth + 1);
    }

    /// The occurrences `formula` names, and what their calculations name.
    fn formula(&mut self, schema: &CalcSchema, formula: &Formula, depth: usize) {
        for reference in formula.polish.field_references() {
            if let (Some(occurrence), Some(field)) =
                (reference.occurrence.id(), reference.field.id())
            {
                self.field(schema, occurrence, field, depth);
            }
        }
    }

    /// Value list `list` is shown: the occurrences its values come from.
    fn value_list(&mut self, list: &ValueList) {
        if let ValueSource::Field {
            primary,
            secondary,
            start,
        } = &list.source
        {
            for spec in std::iter::once(primary).chain(secondary.as_ref()) {
                if let Some(occurrence) = spec.occurrence {
                    self.occurrences.insert(occurrence);
                }
            }
            if let Some(start) = start {
                self.occurrences.insert(*start);
            }
        }
    }
}
