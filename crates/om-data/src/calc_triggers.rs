//! A table's evaluation lists in the dependency index, `[3, 17, 1, 0e, T]`,
//! rebuilt from its fields' direct triggers in the field catalog's
//! `[T, 3, 1, 0e, fff0, F]` and their definitions, the way FileMaker's
//! engine writes them (docs/re/schema-path.md 5.10):
//!
//! - every field with triggers is evaluated; its triggers are its direct
//!   ones and, through each evaluated field among them, that field's
//!   (the closure);
//! - fields are ordered by the size of their closure, then by id, and
//!   `ff, F` holds the position from 1;
//! - `ff00`, `ff01` and `ff02` list, keyed by position + 1, the fields
//!   whose closure holds that event, and every field `G` lists the fields
//!   whose closure holds `G`;
//! - a listing is `<type> <kind> <F as a nested key string>`: the type is
//!   the direct trigger's (`01`, or `00` for a formula without field
//!   inputs), and the kind the definition's kind with `0x40` for an
//!   unstored calculation and `0x80` for global storage.
//!
//! A lookup or an auto-entered formula over related fields also has
//! triggers the caller derives from its field catalog's `fb` nodes: the
//! match fields of the first relationship towards each occurrence it reads
//! (5.13, [`Derived`]).
//!
//! The rest of the entry (key fields' reach and markers, `ff03`, `fffa`,
//! `fffc`) is not touched. A table whose stored lists this rebuild does not
//! reproduce is refused by the caller.

use std::collections::{BTreeMap, BTreeSet};

use om_hbam::OwnedNode;

/// `ff, F` = position.
const ORDER: &[u8] = &[0xff];
/// The events a field can be triggered by: on creation, "evaluate when the
/// referenced fields are empty", on modification.
pub(crate) const CREATION: &[u8] = &[0xff, 0x00];
pub(crate) const WHEN_EMPTY: &[u8] = &[0xff, 0x01];
pub(crate) const MODIFICATION: &[u8] = &[0xff, 0x02];
const EVENTS: [&[u8]; 3] = [CREATION, WHEN_EMPTY, MODIFICATION];
/// Positions are written as one byte keyed position + 1 (OpenMaker's
/// limit: longer keys are not established).
const LAST_KEY: usize = 0x7f;

/// A field's direct triggers: input field keys and events, each with its
/// dependency type byte.
pub(crate) type Triggers = BTreeMap<Vec<u8>, u8>;

const UNREADABLE: &str = "The table's evaluation lists in the dependency index cannot be read.";

fn segment_number(segment: &[u8]) -> u64 {
    segment.iter().fold(0, |n, b| n << 8 | u64::from(*b))
}

pub(crate) fn is_event(key: &[u8]) -> bool {
    key.len() == 2 && key[0] == 0xff
}

/// Every field's direct triggers, from a field catalog's `fff0`.
pub(crate) fn read(depends: &OwnedNode) -> Result<BTreeMap<Vec<u8>, Triggers>, String> {
    depends
        .children
        .iter()
        .map(|field| {
            let triggers = field
                .children
                .iter()
                .map(|t| match t.value.as_deref() {
                    Some([kind]) if t.children.is_empty() => Ok((t.segment.clone(), *kind)),
                    _ => Err(UNREADABLE.to_owned()),
                })
                .collect::<Result<Triggers, String>>()?;
            Ok((field.segment.clone(), triggers))
        })
        .collect()
}

/// Write `triggers` as field `field`'s entry in `fff0`; none removes it.
pub(crate) fn write(depends: &mut OwnedNode, field: &[u8], triggers: &Triggers) {
    depends.children.retain(|c| c.segment != field);
    if triggers.is_empty() {
        return;
    }
    let children = triggers
        .iter()
        .map(|(key, kind)| OwnedNode::leaf(key.clone(), vec![*kind]))
        .collect();
    depends
        .children
        .push(OwnedNode::list(field.to_vec(), children));
    depends.children.sort_by(|a, b| a.segment.cmp(&b.segment));
}

/// Whether a field is a calculation without option `0x80000`, or a field
/// with an auto-entered calculation (option `0x8`, never pure), whose
/// type-`00` listings the engine writes as `02` (schema-path.md 5.10,
/// 5.13).
fn raises_zero(fields: &OwnedNode, field: &[u8]) -> bool {
    fields
        .at(&[&[0x05], field, &[0x02]])
        .and_then(|d| d.value.as_deref())
        .filter(|d| d.len() == 26)
        .is_some_and(|d| {
            let options = u32::from_be_bytes(d[8..12].try_into().expect("4 bytes"));
            (d[0] == 2 && options & 0x8_0000 == 0) || (d[0] == 1 && options & 0x8 != 0)
        })
}

/// The definition of `field` in `fields`, when it reads.
fn definition_in<'a>(fields: &'a OwnedNode, field: &[u8]) -> Option<&'a [u8]> {
    fields
        .at(&[&[0x05], field, &[0x02]])
        .and_then(|d| d.value.as_deref())
        .filter(|d| d.len() == 26)
}

/// Whether a definition is a summary field's or one of global storage.
pub(crate) fn summary_or_global(definition: &[u8]) -> bool {
    let options = u32::from_be_bytes(definition[8..12].try_into().expect("4 bytes"));
    definition[0] == 3 || options & 0x1_0000 != 0
}

/// Whether calculation `field` is unstored only because it reads its own
/// table's global or summary fields (5.16): option `0x20000` with byte 2
/// left 0, such a field among its direct `triggers`, and no related
/// trigger, event or unstored calculation among them.
fn unstored_by_global_or_summary(fields: &OwnedNode, field: &[u8], triggers: &Triggers) -> bool {
    let Some(definition) = definition_in(fields, field) else {
        return false;
    };
    let options = u32::from_be_bytes(definition[8..12].try_into().expect("4 bytes"));
    if definition[0] != 2 || options & 0x2_0000 == 0 || definition[2] != 0 {
        return false;
    }
    let mut reads = false;
    for key in triggers.keys() {
        if is_event(key) || key.first() == Some(&0) {
            return false;
        }
        let Some(input) = definition_in(fields, key) else {
            return false;
        };
        let input_options = u32::from_be_bytes(input[8..12].try_into().expect("4 bytes"));
        if summary_or_global(input) {
            reads = true;
        } else if input[0] == 2 && input_options & 0x2_0000 != 0 {
            return false;
        }
    }
    reads
}

/// The flags a listing under `input` takes for a calculation unstored by
/// its global or summary inputs: `0x50` under a global, `0x40` under a
/// summary, none under another field (5.16).
fn input_flags(fields: &OwnedNode, input: &[u8]) -> u8 {
    match definition_in(fields, input) {
        Some(d) if u32::from_be_bytes(d[8..12].try_into().expect("4 bytes")) & 0x1_0000 != 0 => {
            0x50
        }
        Some(d) if d[0] == 3 => 0x40,
        _ => 0,
    }
}

/// The listing byte for a field's kind and storage.
fn kind_of(fields: &OwnedNode, field: &[u8]) -> Result<u8, String> {
    let definition = fields
        .at(&[&[0x05], field, &[0x02]])
        .and_then(|d| d.value.as_deref())
        .filter(|d| d.len() == 26)
        .ok_or_else(|| UNREADABLE.to_owned())?;
    let options = u32::from_be_bytes(definition[8..12].try_into().expect("4 bytes"));
    let mut kind = definition[0];
    if options & 0x2_0000 != 0 {
        kind |= 0x40;
    }
    if options & 0x1_0000 != 0 {
        kind |= 0x80;
    }
    Ok(kind)
}

fn listing(kind: u8, typ: u8, field: &[u8]) -> Vec<u8> {
    let mut value = vec![typ, kind];
    value.push(u8::try_from(field.len() + 1).expect("a short key"));
    value.push(u8::try_from(field.len()).expect("a short key"));
    value.extend_from_slice(field);
    value
}

/// The type of a trigger reached through an input of type `edge` whose
/// own closure has it with type `inherited`: `01` through `01`, `00`
/// through any `00`, `02` otherwise (`DBDepend::CombineExpansionDependencies`,
/// schema-path.md 5.10).
fn expand(edge: u8, inherited: u8) -> u8 {
    match (edge, inherited) {
        (1, 1) => 1,
        (_, 0) | (0, 1) => 0,
        _ => 2,
    }
}

/// The type of a trigger reached two ways: `00` both ways stays `00`, a
/// `00` and another type is `02`, `01` both ways stays `01`, and a `02`
/// either way is `02` (`DBDepend::CombineFieldDependencies`).
fn combine(a: u8, b: u8) -> u8 {
    match (a, b) {
        (0, 0) => 0,
        (1, 1) => 1,
        _ => 2,
    }
}

/// Each evaluated field's closure: its triggers and, through the evaluated
/// fields among them, theirs, typed by [`expand`] and [`combine`].
fn closures(direct: &BTreeMap<Vec<u8>, Triggers>) -> Result<BTreeMap<Vec<u8>, Triggers>, String> {
    fn visit(
        field: &[u8],
        direct: &BTreeMap<Vec<u8>, Triggers>,
        done: &mut BTreeMap<Vec<u8>, Triggers>,
        path: &mut BTreeSet<Vec<u8>>,
    ) -> Result<Triggers, String> {
        if let Some(closure) = done.get(field) {
            return Ok(closure.clone());
        }
        if !path.insert(field.to_vec()) || path.len() > 4096 {
            return Err("The table's calculations refer to each other in a circle.".into());
        }
        let mut closure = direct[field].clone();
        for (input, edge) in &direct[field] {
            if !direct.contains_key(input) || input == field {
                continue;
            }
            for (key, inherited) in visit(input, direct, done, path)? {
                let typ = expand(*edge, inherited);
                closure
                    .entry(key)
                    .and_modify(|t| *t = combine(*t, typ))
                    .or_insert(typ);
            }
        }
        path.remove(field);
        done.insert(field.to_vec(), closure.clone());
        Ok(closure)
    }
    let mut done = BTreeMap::new();
    for field in direct.keys() {
        visit(field, direct, &mut done, &mut BTreeSet::new())?;
    }
    Ok(done)
}

/// A field `dependent` (of kind byte `kind`) reads related field `input`.
type RelatedUse = (Vec<u8>, Vec<u8>, Vec<u8>);

/// Triggers derived from a field catalog's `fb` nodes, by field: the
/// match fields of its own table each lookup or related auto-entered
/// formula follows (5.13), type 1.
pub(crate) type Derived = BTreeMap<Vec<u8>, BTreeSet<Vec<u8>>>;

/// Rebuild `entry`'s evaluation order, event lists and field dependents
/// from `depends` (`fff0`), the `derived` triggers and the definitions in
/// `fields`.
pub(crate) fn rebuild(
    entry: &mut OwnedNode,
    depends: &OwnedNode,
    derived: &Derived,
    fields: &OwnedNode,
) -> Result<(), String> {
    let mut direct = read(depends)?;
    for (field, matches) in derived {
        let triggers = direct.entry(field.clone()).or_default();
        for key in matches {
            triggers.insert(key.clone(), 1);
        }
    }
    let closure = closures(&direct)?;
    let mut order: Vec<&Vec<u8>> = closure.keys().collect();
    order.sort_by_key(|f| (closure[*f].len(), f.len(), segment_number(f)));
    if order.len() + 1 > LAST_KEY {
        return Err("The table has more evaluated fields than OpenMaker writes.".into());
    }
    let kinds: BTreeMap<&Vec<u8>, u8> = order
        .iter()
        .map(|f| Ok((*f, kind_of(fields, f)?)))
        .collect::<Result<_, String>>()?;
    // A dependent that is itself a relational key field (it has a reach
    // node `00, F`) carries `0x20` in its listings under its input fields
    // (5.13: Assets' Assignments).
    let key_fields: BTreeSet<Vec<u8>> = entry
        .child(&[0])
        .map(|reach| reach.children.iter().map(|c| c.segment.clone()).collect())
        .unwrap_or_default();
    // A calculation unstored only because it reads its own table's global
    // or summary fields is listed without `0x40` under its other inputs,
    // with `0x50` under a global and `0x40` under a summary (5.16).
    let forced: BTreeSet<&Vec<u8>> = order
        .iter()
        .copied()
        .filter(|f| {
            direct
                .get(*f)
                .is_some_and(|triggers| unstored_by_global_or_summary(fields, f, triggers))
        })
        .collect();
    let mut lists: BTreeMap<Vec<u8>, Vec<OwnedNode>> = BTreeMap::new();
    // By occurrence: the related inputs' uses (5.12).
    let mut relational: BTreeMap<Vec<u8>, Vec<RelatedUse>> = BTreeMap::new();
    for (index, field) in order.iter().enumerate() {
        let key = vec![u8::try_from(index + 2).expect("bounded above")];
        let raise = raises_zero(fields, field);
        for (trigger, typ) in &closure[*field] {
            let typ = if raise && *typ == 0 { &2 } else { typ };
            if trigger.first() == Some(&0) {
                let (occurrence, input) = read_related(trigger).ok_or(UNREADABLE)?;
                let kind = vec![kind_of(fields, field)? & 0x0f];
                relational.entry(occurrence.to_vec()).or_default().push((
                    input.to_vec(),
                    (*field).clone(),
                    kind,
                ));
                continue;
            }
            // A field that reads itself (`Self`) is listed under itself
            // keyed `01` (schema-path.md 5.13, a07).
            let key = if trigger == *field {
                vec![1]
            } else {
                key.clone()
            };
            let mut kind = kinds[field];
            if forced.contains(field) {
                kind = (kind & !0x40) | input_flags(fields, trigger);
            }
            if !is_event(trigger) && key_fields.contains(*field) {
                kind |= 0x20;
            }
            lists
                .entry(trigger.clone())
                .or_default()
                .push(OwnedNode::leaf(key, listing(kind, *typ, field)));
        }
    }
    let mut order_node = OwnedNode::list(ORDER.to_vec(), Vec::new());
    for (index, field) in order.iter().enumerate() {
        order_node.children.push(OwnedNode::leaf(
            (*field).clone(),
            vec![1, u8::try_from(index + 1).expect("bounded above")],
        ));
    }
    order_node
        .children
        .sort_by(|a, b| a.segment.cmp(&b.segment));
    // Replace the order, the event lists, every field's dependents and the
    // related inputs' entries (`00, <occurrence>, ...` and
    // `<occurrence>, 00`), keeping each field's key marker `00`, the key
    // fields' reach under `00` and the rest of the entry.
    let is_field = |key: &[u8]| fields.at(&[&[0x05], key]).is_some();
    entry.children.retain_mut(|child| {
        if child.segment == ORDER || EVENTS.contains(&child.segment.as_slice()) {
            return false;
        }
        if child.segment == [0] {
            child.children.retain(|c| is_field(&c.segment));
            return !child.children.is_empty() || child.push;
        }
        if is_event(&child.segment) || child.segment.first() == Some(&0xff) {
            return true;
        }
        if !is_field(&child.segment) {
            return false;
        }
        child.children.retain(|c| c.segment == [0]);
        !child.children.is_empty()
    });
    entry
        .children
        .retain(|c| !(c.segment == [0] && c.children.is_empty()));
    for (occurrence, uses) in relational {
        let reach = entry.ensure(&[&[0], &occurrence]);
        for (input, dependent, kind) in uses {
            for list in [input.as_slice(), &[0xff][..]] {
                reach
                    .ensure(&[list])
                    .set_child(OwnedNode::leaf(dependent.clone(), kind.clone()));
            }
        }
        let mut record = vec![3, 0x20];
        record.push(u8::try_from(occurrence.len() + 1).expect("a short key"));
        record.push(u8::try_from(occurrence.len()).expect("a short key"));
        record.extend_from_slice(&occurrence);
        entry.put(&[&occurrence, &[0]], OwnedNode::leaf(Vec::new(), record));
    }
    // A table without evaluated fields keeps an empty order, as FileMaker
    // Pro gives a new table (schema-path.md 5.8, E1).
    entry.children.push(order_node);
    for (trigger, listed) in lists {
        if let Some(node) = entry.children.iter_mut().find(|c| c.segment == trigger) {
            node.children.extend(listed);
            node.children.sort_by(|a, b| a.segment.cmp(&b.segment));
        } else {
            entry.children.push(OwnedNode::list(trigger, listed));
        }
    }
    entry.children.sort_by(|a, b| a.segment.cmp(&b.segment));
    Ok(())
}

/// Whether rebuilding `entry` from `depends` gives back exactly what it
/// holds: the table has nothing the rebuild does not model.
pub(crate) fn reproduces(
    entry: &OwnedNode,
    depends: &OwnedNode,
    derived: &Derived,
    fields: &OwnedNode,
) -> bool {
    let mut rebuilt = entry.clone();
    if rebuild(&mut rebuilt, depends, derived, fields).is_err() {
        return false;
    }
    canonical(&rebuilt) == canonical(entry)
}

fn canonical(node: &OwnedNode) -> OwnedNode {
    let mut node = node.clone();
    // An empty order and no order read the same.
    node.children
        .retain(|c| !(c.segment == ORDER && c.children.is_empty()));
    node.children = node.children.iter().map(canonical).collect();
    node.children.sort_by(|a, b| a.segment.cmp(&b.segment));
    node.push = !node.children.is_empty() || node.push;
    node
}

/// The functions that make the engine evaluate a calculation on creation
/// and when its inputs are empty: `IsEmpty`, `GetAsBoolean`, `GetFieldName`
/// and `GetBaseTableName` (5.10, the function battery).
const WHEN_EMPTY_FUNCTIONS: [om_formula::Opcode; 4] = [
    om_formula::Opcode(0x4e),
    om_formula::Opcode(0x3e),
    om_formula::Opcode(3201),
    om_formula::Opcode(3216),
];

/// What a formula reads, as far as its dependency entries go.
pub(crate) struct FormulaInputs {
    pub triggers: Triggers,
    /// It reads a related field or an unstored calculation, so FileMaker
    /// does not store its result (schema-path.md 5.12).
    pub forces_unstored: bool,
    /// `Self` reads the field itself (an option's formula, 5.13).
    allow_self: bool,
    /// It reads an unstored calculation, which OpenMaker's record rules
    /// do not read as an input.
    reads_unstored: bool,
    /// It uses a `Get` selector, which adds no trigger.
    pub uses_get: bool,
}

/// Where a field a formula reads is defined: the base table of the
/// occurrence it is read through, and the field's 26-byte definition.
pub(crate) type Resolve<'a> = dyn Fn(u64, u64) -> Option<(Vec<u8>, Vec<u8>)> + 'a;

/// A calculation field's direct triggers, from its compiled formula
/// evaluated from occurrence `context` (5.10, 5.12):
/// - each field it reads, type 1 as a value and 0 where a field reference
///   is required, 2 when both: a field of its own table read through the
///   context occurrence by its key, a field read through another
///   occurrence (a related field) as `00 <occurrence> <field>`;
/// - creation and "when empty" (type 1) for `IsEmpty` and the other
///   functions of [`WHEN_EMPTY_FUNCTIONS`] or with "Do not evaluate if all
///   referenced fields are empty" off; a formula that reads no field gets
///   them with type 1 when it uses such a function and type 0 otherwise.
///
/// What the rules do not cover yet is refused with the reason.
pub(crate) fn formula_triggers(
    expr: &om_formula::Expr,
    context: u64,
    skip_when_empty: bool,
    resolve: &Resolve<'_>,
    field: &[u8],
) -> Result<FormulaInputs, String> {
    let mut inputs = FormulaInputs {
        triggers: Triggers::new(),
        forces_unstored: false,
        allow_self: false,
        reads_unstored: false,
        uses_get: false,
    };
    let mut function = false;
    walk(expr, context, resolve, field, &mut inputs, &mut function, 0)?;
    let triggers = &mut inputs.triggers;
    if triggers.is_empty() {
        let typ = u8::from(function);
        triggers.insert(CREATION.to_vec(), typ);
        triggers.insert(WHEN_EMPTY.to_vec(), typ);
    } else if function || !skip_when_empty {
        triggers.insert(CREATION.to_vec(), 1);
        triggers.insert(WHEN_EMPTY.to_vec(), 1);
    }
    Ok(inputs)
}

/// The field inputs of an auto-entered or validation formula of field
/// `field` (5.13): those of a calculation field's formula, with `Self` the
/// field itself, type 1, and no events (the caller adds creation for an
/// auto-entered value). A formula that names its own field or reads an
/// unstored calculation is refused with the reason; the caller checks the
/// related fields it reads.
pub(crate) fn option_inputs(
    expr: &om_formula::Expr,
    context: u64,
    resolve: &Resolve<'_>,
    field: &[u8],
) -> Result<Triggers, String> {
    let mut inputs = FormulaInputs {
        triggers: Triggers::new(),
        forces_unstored: false,
        allow_self: true,
        reads_unstored: false,
        uses_get: false,
    };
    walk(expr, context, resolve, field, &mut inputs, &mut false, 0)?;
    // OpenMaker's record rules read stored values only (`rules::RecordPlan`);
    // the caller checks the related fields it reads.
    if inputs.reads_unstored {
        return Err("it reads an unstored calculation.".into());
    }
    Ok(inputs.triggers)
}

/// Option `0x80000`: every field among the direct triggers that is read
/// with a type other than 0 is itself such a calculation (5.10, 5.12;
/// `is_pure` says so of a trigger key).
pub(crate) fn pure(triggers: &Triggers, is_pure: &dyn Fn(&[u8]) -> bool) -> bool {
    triggers
        .iter()
        .all(|(key, typ)| is_event(key) || *typ == 0 || is_pure(key))
}

/// The trigger key of a related field: `00`, the occurrence's key string
/// and the field's.
pub(crate) fn related_key(occurrence: &[u8], field: &[u8]) -> Vec<u8> {
    let mut key = vec![0];
    for part in [occurrence, field] {
        key.push(u8::try_from(part.len()).expect("a short key"));
        key.extend_from_slice(part);
    }
    key
}

/// The occurrence and field ids of a related trigger key.
pub(crate) fn related_input(key: &[u8]) -> Option<(u64, u64)> {
    let (occurrence, field) = read_related(key)?;
    Some((
        crate::schema_draft::id_of(occurrence)?,
        crate::schema_draft::id_of(field)?,
    ))
}

/// The occurrence and field of a related trigger key.
fn read_related(key: &[u8]) -> Option<(&[u8], &[u8])> {
    let rest = key.strip_prefix(&[0])?;
    let (&n, rest) = rest.split_first()?;
    let occurrence = rest.get(..usize::from(n))?;
    let rest = &rest[usize::from(n)..];
    let (&m, field) = rest.split_first()?;
    (field.len() == usize::from(m) && !occurrence.is_empty() && !field.is_empty())
        .then_some((occurrence, field))
}

/// Collect the fields `expr` reads into `inputs`, and whether it uses a
/// function of [`WHEN_EMPTY_FUNCTIONS`] into `function`.
fn walk(
    expr: &om_formula::Expr,
    context: u64,
    resolve: &Resolve<'_>,
    this: &[u8],
    inputs: &mut FormulaInputs,
    function: &mut bool,
    depth: usize,
) -> Result<(), String> {
    use om_formula::Expr;
    if depth > om_formula::MAX_NESTING + 8 {
        return Err("The formula is nested too deeply.".into());
    }
    let children: Vec<&Expr> = match expr {
        Expr::Field(f) | Expr::FieldRef(f) => {
            let typ = u8::from(matches!(expr, Expr::Field(_)));
            let key = read_field(f, context, resolve, this, inputs)?;
            inputs
                .triggers
                .entry(key)
                .and_modify(|t| *t = combine(*t, typ))
                .or_insert(typ);
            Vec::new()
        }
        Expr::Call { op, args } => {
            if WHEN_EMPTY_FUNCTIONS.contains(op) {
                *function = true;
            }
            if *op == om_formula::Opcode::SELF {
                if !inputs.allow_self {
                    return Err(
                        "OpenMaker cannot yet write a calculation field that uses Self.".into(),
                    );
                }
                inputs
                    .triggers
                    .entry(this.to_vec())
                    .and_modify(|t| *t = combine(*t, 1))
                    .or_insert(1);
            }
            args.iter().collect()
        }
        Expr::List(items) => items.iter().collect(),
        Expr::Case(pairs) => pairs.iter().flat_map(|(a, b)| [a, b]).collect(),
        Expr::Choose { test, results } => std::iter::once(test.as_ref()).chain(results).collect(),
        Expr::Let { bindings, body } => bindings
            .iter()
            .chain(std::iter::once(body.as_ref()))
            .collect(),
        Expr::While {
            initial,
            condition,
            logic,
            result,
        } => initial
            .iter()
            .chain(std::iter::once(condition.as_ref()))
            .chain(logic)
            .chain(std::iter::once(result.as_ref()))
            .collect(),
        // A Get selector adds no trigger (the function battery).
        Expr::Get(_) => {
            inputs.uses_get = true;
            Vec::new()
        }
        Expr::Variable(_) => {
            return Err(
                "OpenMaker cannot yet write a calculation field that uses $ variables.".into(),
            )
        }
        Expr::Evaluate { .. } | Expr::GetField { .. } => {
            return Err(
                "OpenMaker cannot yet write a calculation field that uses Evaluate \
                        or GetField."
                    .into(),
            )
        }
        Expr::CustomCall { .. } | Expr::PluginCall { .. } => {
            return Err(
                "OpenMaker cannot yet write a calculation field that uses custom, \
                        design or plug-in functions."
                    .into(),
            )
        }
        Expr::Symbol(_) => return Err("The formula has an unresolved name.".into()),
        Expr::Empty | Expr::Number(_) | Expr::Boolean(_) | Expr::Text(_) | Expr::Local(_) => {
            Vec::new()
        }
    };
    for child in children {
        walk(child, context, resolve, this, inputs, function, depth + 1)?;
    }
    Ok(())
}

/// The trigger key of a field the formula reads: its own key for a field
/// of its own table read through the context occurrence, a related key
/// otherwise. Summary and global fields are refused; a related field or an
/// unstored calculation makes the calculation unstored.
fn read_field(
    f: &om_formula::Field,
    context: u64,
    resolve: &Resolve<'_>,
    this: &[u8],
    inputs: &mut FormulaInputs,
) -> Result<Vec<u8>, String> {
    let missing = || "The formula refers to a field that does not exist.".to_owned();
    let occurrence = f.occurrence.id().ok_or_else(missing)?;
    let id = f.field.id().ok_or_else(missing)?;
    let (_, definition) = resolve(occurrence, id).ok_or_else(missing)?;
    let options = u32::from_be_bytes(definition[8..12].try_into().expect("4 bytes"));
    // A calculation field reads its own table's summary and global fields
    // as any other, and is not stored (5.16); an auto-entered or
    // validation formula reads its own table's global field as any other
    // field (5.17). Another table's, and a summary field in an
    // auto-entered or validation formula, are not measured.
    let summary = definition[0] == 3;
    let global = options & 0x1_0000 != 0;
    if (summary || global) && occurrence != context {
        return Err(
            "OpenMaker cannot yet write a formula that uses a summary or global field of \
                    another table."
                .into(),
        );
    }
    if summary && inputs.allow_self {
        return Err(
            "OpenMaker cannot yet write an auto-entered or validation formula that uses a \
                    summary field."
                .into(),
        );
    }
    // 5.17 measured a text global of one repetition; OpenMaker's record
    // rules read neither a container's data nor another repetition's
    // emptiness from the session's values.
    let repetitions = u16::from_be_bytes([definition[24], definition[25]]);
    if global && inputs.allow_self && (definition[1] == 6 || repetitions > 1) {
        return Err(
            "OpenMaker cannot yet write an auto-entered or validation formula that uses a \
                    container or repeating global field (not measured)."
                .into(),
        );
    }
    if (summary || global) && !inputs.allow_self {
        inputs.forces_unstored = true;
    }
    let key = crate::schema_draft::key_of(id);
    if definition[0] == 2 && options & 0x2_0000 != 0 {
        inputs.reads_unstored = true;
    }
    if occurrence != context {
        inputs.forces_unstored = true;
        return Ok(related_key(&crate::schema_draft::key_of(occurrence), &key));
    }
    if key == this {
        // OpenMaker's record rules read a field named in its own
        // auto-entered formula as a cycle; `Self` they evaluate.
        return Err(if inputs.allow_self {
            "it names its own field (use Self).".into()
        } else {
            "A calculation field cannot use its own value.".into()
        });
    }
    if definition[0] == 2 && options & 0x2_0000 != 0 {
        inputs.forces_unstored = true;
    }
    Ok(key)
}
