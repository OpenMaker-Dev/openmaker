//! The references in one formula (calc-encoding.md 2.5, 4.1, 5.2, 7): the
//! fields and custom functions it stores by id, and the functions that look
//! a name up when it runs. Walked with a stack of its own, so a deeply
//! nested formula cannot exhaust the thread's; the number of nodes walked
//! is returned for the build's work budget.

use om_formula::Expr;

use super::{Kind, NameUse};

/// The plug-in id of FileMaker's built-in Design functions
/// (calc-encoding.md 7).
const DESIGN: [u8; 4] = *b"FMDR";
/// The plug-in id of the built-in mobile functions (`Location` and its
/// kin), which name no design object.
const MOBILE: [u8; 4] = *b"FMMB";

/// One reference a formula makes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum FormulaRef {
    /// A field through a table occurrence.
    Field {
        occurrence: u64,
        field: u64,
    },
    CustomFunction(u64),
    /// A name looked up when the formula runs.
    ByName {
        how: NameUse,
        function: Option<String>,
        could_refer_to: Vec<Kind>,
    },
    /// A stored reference whose key does not read.
    Unreadable(&'static str),
}

/// What a Design function's text arguments can name, by the function's
/// English name (calc-functions.md, Design functions: each takes a file
/// name and an object's name). An unknown one can name anything.
fn design_kinds(name: &str) -> Vec<Kind> {
    let lower = name.to_ascii_lowercase();
    let starts = |prefix: &str| lower.starts_with(prefix);
    if starts("valuelist") {
        vec![Kind::ValueList, Kind::Field, Kind::Occurrence]
    } else if starts("script") {
        vec![Kind::Script]
    } else if starts("layoutobject") {
        vec![Kind::Layout, Kind::LayoutObject]
    } else if starts("layout") {
        vec![Kind::Layout]
    } else if starts("field") || starts("getnextserialvalue") {
        vec![Kind::Field, Kind::Occurrence, Kind::Layout]
    } else if starts("table") || starts("basetable") || starts("relationinfo") {
        vec![Kind::Occurrence, Kind::Table]
    } else if starts("databasenames") || starts("windownames") {
        Vec::new()
    } else {
        Kind::ANY.to_vec()
    }
}

/// The built-in functions that look an object up by a name or an
/// expression when they run, by their English names, and what the name
/// can be (calc-functions.md and the Claris help page of each; the table
/// in docs/authoring.md gives each one's source). `Evaluate` and
/// `GetField` have Polish forms of their own (`dc`, `dd`).
const NAMING_FUNCTIONS: [(&str, NameUse, &[Kind]); 8] = [
    // The SQL names tables by occurrence and their fields.
    (
        "ExecuteSQL",
        NameUse::ExecuteSql,
        &[Kind::Field, Kind::Occurrence],
    ),
    (
        "ExecuteSQLe",
        NameUse::ExecuteSql,
        &[Kind::Field, Kind::Occurrence],
    ),
    // The expression it checks can name anything a calculation can.
    ("IsValidExpression", NameUse::ExpressionCheck, &Kind::ANY),
    // `GetLayoutObjectAttribute ( objectName ; attributeName ... )`.
    (
        "GetLayoutObjectAttribute",
        NameUse::ObjectByName,
        &[Kind::LayoutObject],
    ),
    // `GetRecordIDsFromFoundSet ( type { ; tableOccurrenceOrPortal } )`:
    // a table occurrence or a portal's object name.
    (
        "GetRecordIDsFromFoundSet",
        NameUse::OccurrenceByName,
        &[Kind::Occurrence, Kind::LayoutObject],
    ),
    // `GetFieldsOnLayout ( layoutName )`: names a layout; the fields it
    // returns are found by the layout, not named.
    ("GetFieldsOnLayout", NameUse::LayoutByName, &[Kind::Layout]),
    // `GetTableDDL ( tableOccurrenceNames ; ignoreError )`: names table
    // occurrences; their fields come with them, not by name.
    (
        "GetTableDDL",
        NameUse::OccurrenceByName,
        &[Kind::Occurrence],
    ),
    // `GetToolDefinitions ( provider ; functionName ... )`: custom
    // functions only (calc-functions.md).
    (
        "GetToolDefinitions",
        NameUse::FunctionByName,
        &[Kind::CustomFunction],
    ),
];

/// Every reference `expr` makes, in the order met, and how many nodes were
/// walked.
pub(super) fn references(expr: &Expr) -> (Vec<FormulaRef>, usize) {
    let mut out = Vec::new();
    let mut nodes = 0;
    let mut stack = vec![expr];
    while let Some(expr) = stack.pop() {
        nodes += 1;
        match expr {
            Expr::Field(f) | Expr::FieldRef(f) => match (f.occurrence.id(), f.field.id()) {
                (Some(occurrence), Some(field)) => {
                    out.push(FormulaRef::Field { occurrence, field })
                }
                _ => out.push(FormulaRef::Unreadable(
                    "a field reference whose key does not read",
                )),
            },
            Expr::CustomCall { function, .. } => match function.id() {
                Some(id) => out.push(FormulaRef::CustomFunction(id)),
                None => out.push(FormulaRef::Unreadable(
                    "a custom function call whose key does not read",
                )),
            },
            Expr::Evaluate { .. } => out.push(FormulaRef::ByName {
                how: NameUse::Evaluate,
                function: None,
                could_refer_to: Kind::ANY.to_vec(),
            }),
            Expr::GetField { .. } => out.push(FormulaRef::ByName {
                how: NameUse::GetField,
                function: None,
                could_refer_to: vec![Kind::Field, Kind::Occurrence],
            }),
            Expr::Call { op, .. } => {
                let name = op.name().unwrap_or_default();
                if let Some((name, how, kinds)) = NAMING_FUNCTIONS
                    .iter()
                    .find(|(known, ..)| name.eq_ignore_ascii_case(known))
                {
                    out.push(FormulaRef::ByName {
                        how: *how,
                        function: Some((*name).to_owned()),
                        could_refer_to: kinds.to_vec(),
                    });
                }
            }
            Expr::PluginCall { function, .. } if function.plugin == MOBILE => {}
            Expr::PluginCall { function, .. } if function.plugin == DESIGN => {
                let name = om_formula::catalog::builtin(*function).map(|b| b.name);
                out.push(FormulaRef::ByName {
                    how: NameUse::DesignFunction,
                    function: name.map(str::to_owned),
                    could_refer_to: name.map_or_else(|| Kind::ANY.to_vec(), design_kinds),
                });
            }
            Expr::PluginCall { function, .. } => out.push(FormulaRef::ByName {
                how: NameUse::PluginFunction,
                function: Some(format!(
                    "{} {}",
                    String::from_utf8_lossy(&function.plugin),
                    function.function
                )),
                could_refer_to: Kind::ANY.to_vec(),
            }),
            _ => {}
        }
        // Operands are pushed last first, so they are met in source order.
        let at = stack.len();
        expr.for_each_child(|c| stack.push(c));
        stack[at..].reverse();
    }
    (out, nodes)
}
