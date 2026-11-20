//! Recomputing stored calculations after a schema change, as FileMaker
//! Server does when Manage Database changes a formula, a result type, the
//! storage of a calculation, or a field a calculation reads
//! (schema-path.md 7.2 and "Implementation notes (recalculation)";
//! calc-values.md 8.8).
//!
//! The plan is per field, unlike [`RecordPlan`]: a calculation this
//! evaluator cannot run (a related field, a plug-in) is listed in
//! [`Recalculation::skipped`] and its stored values stay as they were,
//! while the table's other calculations are recomputed. A repeating
//! calculation is computed in each of its repetitions; a container result
//! is handed to the host to store ([`Recomputed::containers`]).
//! Auto-enter calculations are not recomputed (they apply when data is
//! entered) and nothing is validated.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use om_calc::ValueType;
use om_formula::Expr;
use om_hbam::catalog::{DataType, FieldKind};
use om_hbam::{KeyTree, Table};

use super::{
    cell_of, check_custom_functions, children, first_field_occurrence, is_aggregate, parse_code,
    parse_recomputed, schema, walk, Expression, InRepetition, RecordEnvironment, RecordPlan,
    ALWAYS_EVALUATE, FIELD_FORMULA, GLOBAL, MAX_CODE, MAX_DEPTH, MAX_NODES, UNSTORED,
};
use crate::containers;
use crate::host::{CalcSchema, FieldClass};
use crate::DataError;

/// The stored calculations of one table to recompute on every record, in
/// evaluation order.
pub struct Recalculation {
    /// The table's schema, for evaluating (no rules of its own).
    plan: RecordPlan,
    targets: Vec<Target>,
    skipped: Vec<Skipped>,
}

struct Target {
    id: u64,
    expression: Expression,
    /// "Do not evaluate if all referenced fields are empty" is off.
    always: bool,
    /// The field's repetitions, each computed in its own repetition.
    repetitions: u32,
    /// How the formula reads each field it reads, for the check above.
    readings: BTreeMap<u64, Readings>,
    /// Its result is a container, which the host stores
    /// ([`Recomputed::containers`]).
    container: bool,
}

/// How a formula reads a field of its own context, by every operation
/// that reads it ([`readings`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Readings {
    /// [`Reading::Current`]: in repetition n, repetition n of a repeating
    /// field, and a field that does not repeat in repetition 1 only.
    current: bool,
    /// [`Reading::First`]: its first repetition in every repetition.
    first: bool,
    /// [`Reading::Any`]: any of its repetitions.
    any: bool,
}

/// A stored calculation of the table: its name, the fields it reads (when
/// its formula decodes) and its plan or why there is none.
type Stored = (String, BTreeSet<u64>, Result<Target, String>);

/// A stored calculation that needed recomputing and is left as it was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skipped {
    pub field: u64,
    pub name: String,
    pub reason: String,
}

/// One record's recomputed cells.
#[derive(Debug, Default)]
pub struct Recomputed {
    /// The cells whose text changed (`None`: now empty).
    pub values: BTreeMap<u64, Option<String>>,
    /// The cells of container results that changed: what the host stores
    /// in the table's library and the cell then names.
    pub containers: BTreeMap<u64, ContainerResult>,
    /// Calculations that failed on this record; their cells keep their
    /// stored text.
    pub failed: Vec<(u64, DataError)>,
}

/// A recomputed container result as the host stores it.
#[derive(Clone, Debug, PartialEq)]
pub enum ContainerResult {
    /// No value: the cell goes, releasing a member it named.
    Empty,
    /// Text, stored as the cell's text (as text a script sets into a
    /// container field is, containers.md "Implementation notes (OpenMaker
    /// Server scripts)").
    Text(String),
    /// Container data: a member of a library the value names, or data to
    /// store as a new member (or one more reference to a member holding
    /// the same data).
    Data(om_calc::Container),
}

impl Recalculation {
    /// The stored calculations of `table` (its definitions in `tree`, within
    /// the file's calculation schema `file`) whose values `changed` makes
    /// stale: those among `changed` and, transitively, those that read a
    /// field in `changed` or another recomputed calculation. `changed`
    /// names fields whose definition changed in a way that changes stored
    /// values (formula, result type, storage, kind, repetitions; new and
    /// deleted fields), by id.
    pub fn plan(
        table: &Table,
        tree: &KeyTree<'_>,
        file: &CalcSchema,
        changed: &BTreeSet<u64>,
    ) -> Recalculation {
        let mut calc = file.clone();
        calc.add_table(table.id, table.name.clone());
        calc.clear_fields(table.id);
        for field in &table.fields {
            if let Some(def) = schema::field_def(tree, table.id, field) {
                calc.add_field(table.id, def);
            }
        }
        // Every stored calculation, with what it reads (when known) and
        // whether it can be evaluated here.
        let mut stored: BTreeMap<u64, Stored> = BTreeMap::new();
        for field in &table.fields {
            let Some(options) = field.decoded_options() else {
                continue;
            };
            if field.kind != FieldKind::Calculation || options.options & (UNSTORED | GLOBAL) != 0 {
                continue;
            }
            let formula = |key: u64| {
                tree.at(&[table.id, 3, 5, field.id, FIELD_FORMULA, key])
                    .and_then(|n| n.assembled())
            };
            let (context, code) = (formula(4), formula(6));
            let reads = match (&context, &code) {
                (Some(context), Some(code)) => references(code, context),
                _ => BTreeSet::new(),
            };
            let target = match (context, code) {
                (Some(context), Some(code)) => evaluable(
                    &calc,
                    table,
                    field.id,
                    field.data_type,
                    &options,
                    &code,
                    &context,
                ),
                _ => Err("its formula is missing.".into()),
            };
            stored.insert(field.id, (field.name.clone(), reads, target));
        }
        // What must be recomputed: the closure over "reads a stale field".
        let mut stale: BTreeSet<u64> = changed.clone();
        loop {
            let more: Vec<u64> = stored
                .iter()
                .filter(|(id, (_, reads, _))| {
                    !stale.contains(id) && reads.iter().any(|r| stale.contains(r))
                })
                .map(|(id, _)| *id)
                .collect();
            if more.is_empty() {
                break;
            }
            stale.extend(more);
        }
        let mut skipped = Vec::new();
        let mut ready: BTreeMap<u64, Target> = BTreeMap::new();
        for (id, (name, _, target)) in stored {
            if !stale.contains(&id) {
                continue;
            }
            match target {
                Ok(target) => {
                    ready.insert(id, target);
                }
                Err(reason) => skipped.push(Skipped {
                    field: id,
                    name,
                    reason,
                }),
            }
        }
        // A container result is stored by the host after the walk, so a
        // calculation reading one recomputed in the same pass would read
        // its old value; so would one reading such a skipped calculation,
        // and so on.
        // A container calculation reading another is a reader too.
        let producers: BTreeSet<u64> = ready
            .iter()
            .filter(|(_, t)| t.container)
            .map(|(id, _)| *id)
            .collect();
        let mut removed: BTreeSet<u64> = BTreeSet::new();
        loop {
            let readers: Vec<u64> = ready
                .iter()
                .filter(|(id, t)| {
                    t.expression
                        .dependencies
                        .iter()
                        .any(|d| (producers.contains(d) && d != *id) || removed.contains(d))
                })
                .map(|(id, _)| *id)
                .collect();
            if readers.is_empty() {
                break;
            }
            for id in readers {
                removed.insert(id);
                ready.remove(&id);
                skipped.push(Skipped {
                    field: id,
                    name: table.field(id).map_or_else(String::new, |f| f.name.clone()),
                    reason: "it reads a container calculation recomputed in the same change."
                        .into(),
                });
            }
        }
        // Formulas read the table through their context occurrence.
        for target in ready.values() {
            if let Some(occurrence) = target.expression.formula.occurrence() {
                if calc.occurrence(occurrence).is_none() {
                    calc.add_occurrence(occurrence, table.name.clone(), table.id);
                }
            }
        }
        let (targets, cyclic) = in_evaluation_order(ready);
        for id in cyclic {
            skipped.push(Skipped {
                field: id,
                name: table.field(id).map_or_else(String::new, |f| f.name.clone()),
                reason: "its calculation depends on itself.".into(),
            });
        }
        Recalculation {
            plan: RecordPlan {
                table: table.id,
                fields: BTreeMap::new(),
                evaluation_order: Vec::new(),
                schema: Arc::new(calc),
            },
            targets,
            skipped,
        }
    }

    /// Whether no stored value needs recomputing.
    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }

    /// The calculations recomputed, in evaluation order.
    pub fn fields(&self) -> Vec<u64> {
        self.targets.iter().map(|t| t.id).collect()
    }

    /// The stale calculations this evaluator cannot run, and why.
    pub fn skipped(&self) -> &[Skipped] {
        &self.skipped
    }

    /// Whether a calculation asks for `Get ( UUID )`, so that each record
    /// needs an environment of its own.
    pub fn uses_uuid(&self) -> bool {
        self.targets.iter().any(|t| {
            let mut found = false;
            walk(&t.expression.formula.polish, &mut |e| {
                found |= matches!(e, Expr::Get(101));
            });
            found
        })
    }

    /// Why `target` cannot be computed on `row`: a container field it
    /// reads has a cell naming a library member that `environment`'s
    /// reader cannot find (no library, or a member the library lost), in
    /// any repetition. The formula would read the reference's own text,
    /// not the data, so the calculation fails on the record and its value
    /// stays as it was (OpenMaker's choice: what FileMaker Server stores
    /// then is unmeasured).
    fn missing_container_data(
        &self,
        target: &Target,
        row: &BTreeMap<u64, String>,
        environment: &RecordEnvironment,
    ) -> Option<DataError> {
        let table = self.plan.schema.table(self.plan.table)?;
        for field in &target.expression.dependencies {
            let Some(def) = table
                .fields
                .get(field)
                .filter(|d| d.value_type == ValueType::Container)
            else {
                continue;
            };
            for n in 1..=def.repetitions.clamp(1, 255) {
                let Some(member) = cell_of(*field, n)
                    .and_then(|cell| row.get(&cell))
                    .and_then(|text| containers::reference(text))
                else {
                    continue;
                };
                let found = environment
                    .containers
                    .as_ref()
                    .map(|reader| reader.member(self.plan.table, &member));
                if !matches!(found, Some(Ok(Some(_)))) {
                    return Some(DataError::with_code(
                        super::error::UNAVAILABLE,
                        format!(
                            "the data of container field {} is missing from the library.",
                            def.name
                        ),
                    ));
                }
            }
        }
        None
    }

    /// The new values of one record, `row` being its stored cells as text
    /// (repetition cells under `F * 256 + n`).
    /// `budget` is what may still be held of container results for the
    /// host to store after the walk: a result past it fails on the record,
    /// which keeps its value.
    pub fn recompute(
        &self,
        row: &BTreeMap<u64, String>,
        environment: &RecordEnvironment,
        budget: &mut usize,
    ) -> Recomputed {
        // The file's own calculation with no session, as FileMaker Server
        // recomputes stored values (calc-functions.md 6.1, file context).
        let mut session = environment.session();
        session.file_context = true;
        let mut row = row.clone();
        let mut out = Recomputed::default();
        for target in &self.targets {
            if let Some(error) = self.missing_container_data(target, &row, environment) {
                out.failed.push((target.id, error));
                continue;
            }
            for repetition in 1..=target.repetitions {
                let Some(cell) = cell_of(target.id, repetition) else {
                    continue;
                };
                let skip = !target.always
                    && !target.readings.is_empty()
                    && !self.reads_a_value(target, repetition, &row);
                if target.container {
                    let result = if skip {
                        Ok(ContainerResult::Empty)
                    } else {
                        self.plan
                            .evaluate_repetition_value(
                                target.id,
                                repetition,
                                &target.expression,
                                &row,
                                &session,
                            )
                            .and_then(|(value, text)| container_result(value, text))
                    };
                    match result {
                        Ok(result) => {
                            if holds(row.get(&cell), &result, self.plan.table, environment) {
                                continue;
                            }
                            // What the host holds for the library until the
                            // walk ends, charged as it is queued.
                            let cost = held_cost(&result);
                            if cost > *budget {
                                out.failed.push((
                                    target.id,
                                    DataError::with_code(
                                        super::error::UNAVAILABLE,
                                        "the container data recomputed in one change passed its \
                                         limit."
                                            .to_string(),
                                    ),
                                ));
                                continue;
                            }
                            *budget -= cost;
                            out.containers.insert(cell, result);
                        }
                        Err(e) => out.failed.push((target.id, e)),
                    }
                    continue;
                }
                let text = if skip {
                    String::new()
                } else {
                    match self.plan.evaluate_repetition(
                        target.id,
                        repetition,
                        &target.expression,
                        &row,
                        &session,
                    ) {
                        Ok(text) => text,
                        Err(e) => {
                            out.failed.push((target.id, e));
                            continue;
                        }
                    }
                };
                if row.get(&cell).map_or("", String::as_str) != text {
                    out.values
                        .insert(cell, (!text.is_empty()).then(|| text.clone()));
                    row.insert(cell, text);
                }
            }
        }
        out
    }

    /// Whether repetition `repetition` of `target` reads a field that is
    /// not empty in `row`, as "Do not evaluate if all referenced fields
    /// are empty" asks: each field as the formula reads it there (inf:
    /// FileMaker's check in a repetition above 1 is unmeasured).
    fn reads_a_value(&self, target: &Target, repetition: u32, row: &BTreeMap<u64, String>) -> bool {
        let filled = |cell: Option<u64>| {
            cell.and_then(|cell| row.get(&cell))
                .is_some_and(|text| !text.is_empty())
        };
        let table = self.plan.schema.table(self.plan.table);
        target.readings.iter().any(|(field, how)| {
            let repetitions = table
                .and_then(|t| t.fields.get(field))
                .map_or(1, |f| f.repetitions.clamp(1, 255));
            (how.first && filled(cell_of(*field, 1)))
                || (how.current
                    && (repetition == 1 || repetitions > 1)
                    && repetition <= repetitions
                    && filled(cell_of(*field, repetition)))
                || (how.any && (1..=repetitions).any(|n| filled(cell_of(*field, n))))
        })
    }
}

/// Whether the stored calculation `field` can be evaluated on a record of
/// `table` alone, as [`RecordPlan`] requires of the calculations it
/// computes: a typed result, one repetition, fields of its own table that
/// are stored data or stored calculations.
fn evaluable(
    calc: &CalcSchema,
    table: &Table,
    field: u64,
    data_type: DataType,
    options: &om_hbam::FieldOptions,
    code: &[u8],
    context: &[u8],
) -> Result<Target, String> {
    if matches!(data_type, DataType::Unknown(_)) {
        return Err("its result type is unknown.".into());
    }
    // Repetition n of field F is the cell `F * 256 + n` (calc-values.md,
    // "Repetitions in records"), so only 255 have cells.
    if !(1..=255).contains(&options.max_repetitions) {
        return Err("calculations of more than 255 repetitions are not recomputed.".into());
    }
    let expression = parse_recomputed(code, context)?;
    if let Some(occurrence) = expression.formula.occurrence() {
        if calc
            .occurrence(occurrence)
            .is_some_and(|o| o.table != table.id)
        {
            return Err("it is evaluated from another table's occurrence.".into());
        }
    }
    check_custom_functions(
        calc,
        &expression.formula.polish,
        false,
        true,
        &mut BTreeSet::new(),
    )?;
    // A container input is read through the environment's container
    // reader (the table's library on the server): the field's data, as
    // FileMaker Server's engine reads it when a commit computes such a
    // calculation (script oracle s34-containers-calc).
    // Global, summary and unstored inputs make FileMaker store the
    // calculation unstored (calc-values.md 8.8), so they come only from a
    // file it did not write.
    let fields = calc.table(table.id).map(|t| &t.fields);
    for dependency in &expression.dependencies {
        let Some(def) = fields.and_then(|f| f.get(dependency)) else {
            return Err("it references a field that does not exist.".into());
        };
        if def.global || def.class == FieldClass::Summary || def.is_unstored_calculation() {
            return Err("it reads a global, summary or unstored calculation field.".into());
        }
    }
    let mut found = BTreeMap::new();
    readings(
        &expression.formula.polish,
        context,
        Reading::Current,
        &mut found,
    );
    Ok(Target {
        id: field,
        expression,
        always: options.options & ALWAYS_EVALUATE != 0,
        repetitions: u32::from(options.max_repetitions),
        readings: found,
        container: data_type == DataType::Container,
    })
}

/// What a container calculation's value `value` (shown as `text`)
/// stores: nothing, its text, or container data. Text in the form a cell
/// names a library member by (containers.md 1.4) cannot be stored as text:
/// the cell would name that member without counting it, so the
/// calculation fails on the record, which keeps its value.
fn container_result(value: om_calc::Value, text: String) -> Result<ContainerResult, DataError> {
    let text = match value {
        om_calc::Value::Container(c) if c.is_empty() => return Ok(ContainerResult::Empty),
        // A reference text without data stays text.
        om_calc::Value::Container(c) if c.library.is_none() && c.streams.is_empty() => {
            c.text.as_str().to_owned()
        }
        om_calc::Value::Container(c) => return Ok(ContainerResult::Data(c)),
        _ => text,
    };
    if text.is_empty() {
        return Ok(ContainerResult::Empty);
    }
    if containers::reference(&text).is_some() {
        return Err(DataError::with_code(
            super::error::UNAVAILABLE,
            "its text result has the form of a container's library reference.".to_string(),
        ));
    }
    Ok(ContainerResult::Text(text))
}

/// What `result` holds in memory until the host stores it: its data, its
/// text or file name, and its slot in the record's map. The map itself is
/// the host's to charge.
fn held_cost(result: &ContainerResult) -> usize {
    std::mem::size_of::<(u64, ContainerResult)>()
        + match result {
            ContainerResult::Empty => 0,
            ContainerResult::Text(text) => text.len(),
            ContainerResult::Data(c) => {
                c.text.as_str().len() + c.streams.iter().map(|(_, b)| b.len()).sum::<usize>()
            }
        }
}

/// Whether the cell text `stored` of a record of `table` already holds
/// `result`: no value, the same text, or a member of the table's library
/// with the same data (its own member, or one whose MD5 is the data's,
/// containers.md 2.2) as `environment`'s reader reads it.
fn holds(
    stored: Option<&String>,
    result: &ContainerResult,
    table: u64,
    environment: &RecordEnvironment,
) -> bool {
    let stored = stored.map_or("", String::as_str);
    match result {
        ContainerResult::Empty => stored.is_empty(),
        ContainerResult::Text(text) => {
            containers::reference(stored).is_none() && stored == text.as_str()
        }
        ContainerResult::Data(c) => {
            let Some(member) = containers::reference(stored) else {
                return false;
            };
            if let Some(library) = &c.library {
                if library.table == table {
                    return library.member == member;
                }
            }
            let md5 = match (&c.library, containers::tree(c)) {
                (_, Some((_, md5))) => md5,
                (Some(library), None) => library.md5,
                (None, None) => return false,
            };
            environment
                .containers
                .as_ref()
                .and_then(|reader| reader.member(table, &member).ok().flatten())
                .is_some_and(|info| info.md5 == md5)
        }
    }
}

/// Whether `op` takes a field parameter it expands to every repetition
/// (`NPV`, `Last`; its first argument), as the evaluator reads it.
fn expands_field(op: om_formula::Opcode) -> bool {
    use om_formula::catalog::NArgs;
    matches!(
        op.info().map(|i| i.nargs),
        Some(NArgs::UnaryField | NArgs::BinaryField)
    )
}

/// How a field reference reads its field, by the operation that consumes
/// it (calc-values.md 8.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reading {
    /// Repetition n in repetition n: read plainly, or as one of several
    /// arguments of an aggregate.
    Current,
    /// Through `Extend`: its first repetition.
    First,
    /// As `GetRepetition`'s or `f[n]`'s field, or as an aggregate's only
    /// argument: any of its repetitions.
    Any,
}

/// How `expr` reads each field of the context `context` (its key as the
/// formula holds it), `reading` being how its consumer reads a field it
/// passes on (`Current` at the top).
fn readings(expr: &Expr, context: &[u8], reading: Reading, out: &mut BTreeMap<u64, Readings>) {
    match expr {
        Expr::Field(f) | Expr::FieldRef(f) => {
            if let Some(id) = f.field.id().filter(|_| f.occurrence.0 == context) {
                let entry = out.entry(id).or_default();
                match reading {
                    Reading::Current => entry.current = true,
                    Reading::First => entry.first = true,
                    Reading::Any => entry.any = true,
                }
            }
        }
        Expr::Call { op, args } => {
            let name = op.name().map(str::to_ascii_lowercase);
            match name.as_deref() {
                // Extend of an aggregate's only argument expands it.
                Some("extend") => {
                    let inner = match reading {
                        Reading::Any => Reading::Any,
                        _ => Reading::First,
                    };
                    for arg in args {
                        readings(arg, context, inner, out);
                    }
                }
                // As the evaluator expands them (om-calc's
                // `aggregate_arguments`): an aggregate's only argument, and
                // the field parameter of a function that takes one (`NPV`,
                // `Last`), read every repetition; each of several aggregate
                // arguments its current one.
                _ if is_aggregate(*op) || expands_field(*op) => {
                    let aggregate = is_aggregate(*op);
                    let expanded = if aggregate {
                        usize::from(args.len() == 1)
                    } else {
                        1
                    };
                    // Their other arguments are values of the current
                    // repetition (`GetRepetition`'s index, NPV's rate).
                    for (i, arg) in args.iter().enumerate() {
                        let each = if i < expanded {
                            Reading::Any
                        } else {
                            Reading::Current
                        };
                        readings(arg, context, each, out);
                    }
                }
                // The field, direct or computed, in any repetition; the
                // repetition number plainly.
                Some("getrepetition") => {
                    for (i, arg) in args.iter().enumerate() {
                        let each = if i == 0 {
                            Reading::Any
                        } else {
                            Reading::Current
                        };
                        readings(arg, context, each, out);
                    }
                }
                _ => {
                    for arg in args {
                        readings(arg, context, reading, out);
                    }
                }
            }
        }
        _ => {
            for part in children(expr) {
                readings(part, context, reading, out);
            }
        }
    }
}

/// The fields of the formula's own context that a stored Polish stream
/// reads, whether or not it can be evaluated here (empty when it does not
/// decode).
fn references(code: &[u8], context: &[u8]) -> BTreeSet<u64> {
    let mut reads = BTreeSet::new();
    if code.len() > MAX_CODE {
        return reads;
    }
    let limits = om_formula::polish::Limits {
        depth: MAX_DEPTH,
        nodes: MAX_NODES,
    };
    if let Ok(polish) = om_formula::polish::decode_with(code, limits) {
        walk(&polish, &mut |e| {
            if let Expr::Field(field) | Expr::FieldRef(field) = e {
                if field.occurrence.0 == context {
                    reads.extend(field.field.id());
                }
            }
        });
    }
    reads
}

/// `targets` ordered so that each comes after the targets it reads; the
/// ids caught in a cycle are returned apart.
fn in_evaluation_order(mut targets: BTreeMap<u64, Target>) -> (Vec<Target>, Vec<u64>) {
    let mut order = Vec::with_capacity(targets.len());
    loop {
        let next: Vec<u64> = targets
            .iter()
            .filter(|(_, t)| {
                t.expression
                    .dependencies
                    .iter()
                    .all(|d| !targets.contains_key(d))
            })
            .map(|(id, _)| *id)
            .collect();
        if next.is_empty() {
            break;
        }
        for id in next {
            order.extend(targets.remove(&id));
        }
    }
    (order, targets.into_keys().collect())
}

/// How long one evaluation of a record limit may run.
const LIMIT_BUDGET: std::time::Duration = std::time::Duration::from_millis(100);

/// What one evaluation of a record limit may build: text values of 1 MiB at
/// most, so that no single text function (whose checks for interruption
/// come between passes over its input) takes long or much memory, and
/// 1,000 nested calls. A limit past them fails, which allows nothing.
const LIMIT_CALC: om_calc::eval::CalcLimits = om_calc::eval::CalcLimits {
    text: 1 << 20,
    nesting: 1_000,
};

/// A custom privilege set's calculation limiting a record action
/// (session-identity.md 2.1: a table's `05, T, 06`-`09`), evaluated on a
/// record's stored values as a validation calculation is: a true result
/// allows the action. A calculation that fails, or a field it reads that
/// the record lacks, allows nothing (FileMaker counts a formula that does
/// not load as no [C]; one that fails is inferred to count the same).
pub struct RecordLimit {
    plan: RecordPlan,
    expression: Expression,
}

impl RecordLimit {
    /// The limit `code` (a compiled stream) of `table` (its definitions in
    /// `tree`, within the file's calculation schema `file`), evaluated in
    /// occurrence `context`, else the table's first occurrence (a set
    /// stores none: its formulas name their fields' occurrence themselves,
    /// session-identity.md 2.8). Refused when it cannot be evaluated here: it
    /// does not decode, is evaluated from another table's occurrence, calls
    /// a custom function this evaluator lacks, or reads a container,
    /// summary or unstored calculation field.
    pub fn new(
        table: &Table,
        tree: &KeyTree<'_>,
        file: &CalcSchema,
        code: &[u8],
        context: Option<&[u8]>,
    ) -> Result<Self, String> {
        let mut calc = file.clone();
        calc.add_table(table.id, table.name.clone());
        calc.clear_fields(table.id);
        for field in &table.fields {
            if let Some(def) = schema::field_def(tree, table.id, field) {
                calc.add_field(table.id, def);
            }
        }
        let context = match context
            .map(<[u8]>::to_vec)
            .or_else(|| first_field_occurrence(code))
        {
            Some(context) => context,
            None => {
                let occurrence = calc
                    .occurrences_in_order()
                    .into_iter()
                    .find(|o| o.table == table.id)
                    .ok_or("the table has no occurrence.")?;
                let bytes = occurrence.id.to_be_bytes();
                let first = bytes
                    .iter()
                    .position(|b| *b != 0)
                    .unwrap_or(bytes.len() - 1);
                bytes[first..].to_vec()
            }
        };
        let expression = parse_code(code, &context)?;
        // Every field it reads must be read through the table's own
        // occurrence, its first in catalog order: through another one (a
        // self-join) the fields would be the related record's, which this
        // evaluator does not follow (`parse_code` refuses a second one).
        let own = calc
            .occurrences_in_order()
            .into_iter()
            .find(|o| o.table == table.id)
            .map(|o| o.id);
        if let Some(occurrence) = expression.formula.occurrence() {
            if Some(occurrence) != own {
                return Err(
                    "it reads fields through an occurrence other than its table's own.".into(),
                );
            }
        }
        check_custom_functions(
            &calc,
            &expression.formula.polish,
            false,
            false,
            &mut BTreeSet::new(),
        )?;
        let fields = calc.table(table.id).map(|t| &t.fields);
        for dependency in &expression.dependencies {
            let Some(def) = fields.and_then(|f| f.get(dependency)) else {
                return Err("it references a field that does not exist.".into());
            };
            // A global's value is the session's, which this evaluator is
            // not given: it would read empty.
            if def.value_type == ValueType::Container
                || def.global
                || def.class == FieldClass::Summary
                || def.is_unstored_calculation()
            {
                return Err(
                    "it reads a container, global, summary or unstored calculation field.".into(),
                );
            }
        }
        Ok(Self {
            plan: RecordPlan {
                table: table.id,
                fields: BTreeMap::new(),
                evaluation_order: Vec::new(),
                schema: Arc::new(calc),
            },
            expression,
        })
    }

    /// Whether the limit allows the action on the record whose stored cells
    /// as text are `row` (repetition cells under `F * 256 + n`).
    pub fn allows(&self, row: &BTreeMap<u64, String>, environment: &RecordEnvironment) -> bool {
        // A budget the formula cannot raise (SetRecursion raises only its
        // loop allowance; the evaluator checks for interruption as it
        // runs): a stopped evaluation allows nothing. The host evaluates
        // under the file's write lock.
        let deadline = std::time::Instant::now() + LIMIT_BUDGET;
        let outer = environment.interrupted.clone();
        let mut environment = environment.clone();
        environment.limits = om_calc::eval::CalcLimits {
            text: environment.limits.text.min(LIMIT_CALC.text),
            nesting: environment.limits.nesting.min(LIMIT_CALC.nesting),
        };
        environment.interrupted = Some(Arc::new(move || {
            outer.as_ref().is_some_and(|stop| stop()) || std::time::Instant::now() >= deadline
        }));
        let session = environment.session();
        let today = crate::host::session::today(0);
        let cx = om_calc::Ctx::new(&self.plan.schema.locale, today);
        let first = InRepetition::Calculation(1);
        self.plan
            .run(0, &self.expression, row, &session, None, None, first)
            .is_ok_and(|(result, _)| result.is_true(&cx))
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{field, initials, CONTEXT};
    use super::*;
    use om_calc::Locale;
    use om_hbam::KeySegment;

    fn tree(formulas: &[(u8, Vec<u8>)]) -> KeyTree<'static> {
        let mut tree = KeyTree::default();
        for (id, code) in formulas {
            let code: &'static [u8] = Box::leak(code.clone().into_boxed_slice());
            let id: &'static [u8] = Box::leak(vec![*id].into_boxed_slice());
            let mut path = vec![
                KeySegment(&[0x80, 2]),
                KeySegment(&[3]),
                KeySegment(&[5]),
                KeySegment(id),
                KeySegment(&[5]),
                KeySegment(&[4]),
            ];
            tree.insert(&path, om_hbam::slot::ChunkKind::Leaf, Some(CONTEXT))
                .unwrap();
            *path.last_mut().unwrap() = KeySegment(&[6]);
            tree.insert(&path, om_hbam::slot::ChunkKind::Leaf, Some(code))
                .unwrap();
        }
        tree
    }

    fn environment() -> RecordEnvironment {
        RecordEnvironment {
            account_name: "Admin".into(),
            user_name: "Admin".into(),
            date: "10/29/2026".into(),
            time: "1:00:00 PM".into(),
            timestamp: "10/29/2026 1:00:00 PM".into(),
            uuid: "4C32B318-4200-4E07-90A2-4C2BB78F3250".into(),
            containers: None,
            interrupted: None,
            limits: om_calc::eval::CalcLimits::DEFAULT,
            lookups: Default::default(),
            serials: Default::default(),
            globals: None,
            context: Default::default(),
            held: None,
            related_needs: None,
        }
    }

    /// A custom set's record limit (#156 slice b): `Field1 = "Allowed"`
    /// built as FileMaker Pro stores the #137 fixture's (an `=`, the field
    /// through the table's occurrence, the text, the end). It answers on
    /// the record, and is refused through another occurrence of the table
    /// (a self-join) or reading a global.
    #[test]
    fn record_limits_answer_on_the_record_and_refuse_what_they_cannot_follow() {
        let table = Table {
            id: 0x8002,
            name: "Example".into(),
            fields: vec![
                field(1, FieldKind::Normal, DataType::Text, 0, 0, 0),
                field(2, FieldKind::Normal, DataType::Text, 0, GLOBAL, 0),
            ],
        };
        let tree = tree(&[]);
        let mut file = CalcSchema::new("test", Locale::en_us());
        file.add_occurrence(0xd0_0002, "Example", 0x8002);
        file.add_occurrence(0xd0_0003, "Self", 0x8002);
        let limit = |occurrence: &[u8], field: u8| {
            let mut code = vec![0x44, 0x16, 0, 4];
            code.extend(occurrence);
            code.extend([2, 1, field, 0, 1]);
            let text = om_scsu::encode_scsu("Allowed");
            code.extend([0x13, text.len() as u8]);
            code.extend(text);
            code.push(0);
            RecordLimit::new(&table, &tree, &file, &code, None)
        };
        let allowed = limit(CONTEXT, 1).unwrap();
        let row = |text: &str| BTreeMap::from([(1, text.to_owned())]);
        assert!(allowed.allows(&row("Allowed"), &environment()));
        assert!(!allowed.allows(&row("Denied"), &environment()));
        assert!(limit(&[3, 0xd0, 0, 3], 1).is_err());
        assert!(limit(CONTEXT, 2).is_err());
        // A limit that builds text until one function takes long: refused
        // quickly by the text limit, not run under the host's lock (the
        // review's probe, 8 MiB of text by doubling).
        let source = "Let ( t = While ( [ i = 0 ; t = \"a\" ] ; i < 23 ; \
                      [ t = t & t ; i = i + 1 ] ; t ) ; PatternCount ( t ; \"\" ) = 0 )";
        let names = crate::formula::NameTable::default();
        let compiled =
            crate::formula::compile(source, &crate::formula::Context::new(&names)).unwrap();
        let code = crate::formula::polish::encode(&compiled.expr);
        let probe = RecordLimit::new(&table, &tree, &file, &code, Some(CONTEXT)).unwrap();
        let started = std::time::Instant::now();
        assert!(!probe.allows(&row("Allowed"), &environment()));
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn recomputes_the_changed_calculation_and_what_reads_it() {
        let table = Table {
            id: 0x8002,
            name: "Example".into(),
            fields: vec![
                field(1, FieldKind::Normal, DataType::Text, 0, 0, 0),
                field(2, FieldKind::Normal, DataType::Text, 0, 0, 0),
                field(5, FieldKind::Calculation, DataType::Text, 0, 0, 0),
                field(6, FieldKind::Calculation, DataType::Text, 0, 0, 0),
                field(7, FieldKind::Calculation, DataType::Text, 0, 0, 0),
            ],
        };
        // 5 reads 1 and 2 (the existing tests' Initial: "W" for Hello
        // World); 6 reads 5; 7 reads 1 only.
        let tree = tree(&[
            (5, initials(1, 2)),
            (6, initials(5, 5)),
            (7, initials(1, 1)),
        ]);
        let file = CalcSchema::new("test", Locale::en_us());
        let recalc = Recalculation::plan(&table, &tree, &file, &BTreeSet::from([5]));
        assert_eq!(recalc.fields(), [5, 6]);
        assert!(recalc.skipped().is_empty());
        let row = BTreeMap::from([
            (1, "Hello".to_string()),
            (2, "World".to_string()),
            (5, "old".to_string()),
            (6, "old".to_string()),
            (7, "H".to_string()),
        ]);
        let mut budget = usize::MAX;
        let out = recalc.recompute(&row, &environment(), &mut budget);
        assert!(out.failed.is_empty(), "{:?}", out.failed);
        assert_eq!(
            out.values,
            BTreeMap::from([(5, Some("W".into())), (6, Some("W".into()))])
        );
        // A deleted or retyped input makes its readers stale too.
        let recalc = Recalculation::plan(&table, &tree, &file, &BTreeSet::from([1]));
        assert_eq!(recalc.fields(), [5, 7, 6]);
    }

    /// Each field reference reads as the operation consuming it reads
    /// fields (calc-values.md 8.6), for "do not evaluate if all referenced
    /// fields are empty": the review of #380's cases among them.
    #[test]
    fn readings_follow_the_operation_that_reads_each_field() {
        let mut names = crate::formula::NameTable::default();
        names.add_occurrence(0xd0_0002, "Example", 0x8002);
        names.add_field(0x8002, 1, "r");
        names.add_field(0x8002, 2, "e");
        let read = |source: &str| {
            let mut ctx = crate::formula::Context::new(&names);
            ctx.occurrence = Some(0xd0_0002);
            let compiled = crate::formula::compile(source, &ctx).unwrap();
            let mut out = BTreeMap::new();
            readings(&compiled.expr, CONTEXT, Reading::Current, &mut out);
            out
        };
        let how = |current, first, any| Readings {
            current,
            first,
            any,
        };
        let (current, first, any) = (
            how(true, false, false),
            how(false, true, false),
            how(false, false, true),
        );
        for (source, field, want) in [
            ("r", 1, current),
            ("Upper ( r )", 1, current),
            ("Sum ( r ; 1 )", 1, current),
            ("List ( r ; e )", 2, current),
            ("Sum ( r )", 1, any),
            ("Last ( r )", 1, any),
            ("NPV ( r ; e )", 1, any),
            ("NPV ( r ; e )", 2, current),
            ("Sum ( GetRepetition ( r ; e ) )", 1, any),
            ("Sum ( GetRepetition ( r ; e ) )", 2, current),
            ("Extend ( r )", 1, first),
            ("Sum ( Extend ( r ) )", 1, any),
            ("r[2]", 1, any),
            ("GetRepetition ( If ( 1 ; r ; r ) ; 1 ) & e", 1, any),
            ("GetRepetition ( If ( 1 ; r ; r ) ; 1 ) & e", 2, current),
        ] {
            assert_eq!(
                read(source).get(&field),
                Some(&want),
                "{source}: field {field}"
            );
        }
    }

    #[test]
    fn a_calculation_it_cannot_run_is_skipped_alone() {
        let table = Table {
            id: 0x8002,
            name: "Example".into(),
            fields: vec![
                field(1, FieldKind::Normal, DataType::Text, 0, 0, 0),
                field(2, FieldKind::Normal, DataType::Text, 0, GLOBAL, 0),
                field(5, FieldKind::Calculation, DataType::Text, 0, 0, 0),
                field(6, FieldKind::Calculation, DataType::Text, 0, 0, 0),
            ],
        };
        // 5 reads the global 2, which only the session knows.
        let tree = tree(&[(5, initials(1, 2)), (6, initials(1, 1))]);
        let file = CalcSchema::new("test", Locale::en_us());
        let recalc = Recalculation::plan(&table, &tree, &file, &BTreeSet::from([5, 6]));
        assert_eq!(recalc.fields(), [6]);
        assert_eq!(recalc.skipped().len(), 1);
        assert_eq!(recalc.skipped()[0].field, 5);
        // Unchanged calculations are left alone.
        let recalc = Recalculation::plan(&table, &tree, &file, &BTreeSet::from([2]));
        assert!(recalc.fields().is_empty());
        assert_eq!(recalc.skipped().len(), 1, "5 reads the global");
    }
}
