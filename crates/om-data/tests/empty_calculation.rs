//! "Do not evaluate if all referenced fields are empty" on unstored
//! calculations (calc-values.md 8.8) follows the fields a formula actually
//! reads while it runs, and a field inspection function it runs forces its
//! result, rather than an upfront scan of every field in its tree.

use std::cell::RefCell;
use std::sync::Arc;

use om_data::calc::eval::{evaluate_text, Context, CustomFunction, Variables};
use om_data::calc::{Date, Locale, Value, ValueType};
use om_data::formula::{self, Expr, Formula, Key};
use om_data::host::{
    CachedSource, CalcData, CalcNeed, CalcSchema, FieldDef, FileCalc, FileHost, RecordValues,
    SessionInfo,
};

const ROWS_TABLE: u64 = 0x8001;
const KIDS_TABLE: u64 = 0x8002;
const ROWS: u64 = 0xd00001;
/// Related to the record by two child records, one with Text "a".
const KIDS: u64 = 0xd00002;
/// Related to no record.
const NO_KIDS: u64 = 0xd00003;
/// A related set the cache does not hold: reading it asks for it.
const UNFETCHED: u64 = 0xd00004;

/// Field option `0x20`: evaluate even when every referenced field is empty.
const EVALUATE_ALWAYS: u32 = 0x20;

fn compile(schema: &CalcSchema, text: &str, parameters: &[String]) -> Expr {
    let mut context = formula::Context::new(schema.names());
    context.occurrence = Some(ROWS);
    context.parameters = parameters;
    formula::compile(text, &context).unwrap().expr
}

/// Rows, on whose record Input, Amount and Photo are empty and Note is
/// "nonempty"; custom functions `InspectInput` (`IsEmpty ( Input )`),
/// `ReadNote` (`Note`) and `Echo ( x )` (`x`).
fn schema() -> CalcSchema {
    let mut schema = CalcSchema::new("Empty inputs", Locale::en_us());
    schema.add_table(ROWS_TABLE, "Rows");
    schema.add_occurrence(ROWS, "Rows", ROWS_TABLE);
    for (id, name, ty) in [
        (1, "Input", ValueType::Text),
        (2, "Note", ValueType::Text),
        (3, "Amount", ValueType::Number),
        (4, "Photo", ValueType::Container),
    ] {
        schema.add_field(ROWS_TABLE, FieldDef::data(id, name, ty));
    }
    schema.add_table(KIDS_TABLE, "Kids");
    for (id, name) in [
        (KIDS, "Kids"),
        (NO_KIDS, "NoKids"),
        (UNFETCHED, "Unfetched"),
    ] {
        schema.add_occurrence(id, name, KIDS_TABLE);
    }
    schema.add_field(KIDS_TABLE, FieldDef::data(1, "Text", ValueType::Text));
    let functions: [(u64, &str, &[&str], &str); 3] = [
        (1, "InspectInput", &[], "IsEmpty ( Input )"),
        (2, "ReadNote", &[], "Note"),
        (3, "Echo", &["x"], "x"),
    ];
    for (id, name, parameters, body) in functions {
        let parameters: Vec<String> = parameters.iter().map(|p| p.to_string()).collect();
        let body = compile(&schema, body, &parameters);
        schema.add_custom_function(
            id,
            CustomFunction {
                name: name.into(),
                parameters,
                body,
            },
        );
    }
    schema
}

/// An unstored text calculation of Rows; "Do not evaluate if all
/// referenced fields are empty" is selected.
fn calculation(schema: &CalcSchema, id: u64, name: &str, text: &str) -> FieldDef {
    let formula = Formula {
        context: Some(Key::from_id(ROWS)),
        formatted: None,
        polish: compile(schema, text, &[]),
    };
    let field = FieldDef::calculation(id, name, ValueType::Text, formula, false);
    assert!(!field.evaluates_always());
    field
}

/// The record's data, with the related sets of Kids and NoKids.
fn data() -> CalcData {
    let mut row = RecordValues::default();
    row.set(2, 1, Value::text("nonempty"));
    let mut data = CalcData::default();
    data.records.insert((ROWS_TABLE, 1), Some(Arc::new(row)));
    let mut kid = RecordValues::default();
    kid.set(1, 1, Value::text("a"));
    data.records.insert((KIDS_TABLE, 1), Some(Arc::new(kid)));
    data.records
        .insert((KIDS_TABLE, 2), Some(Arc::new(RecordValues::default())));
    data.related.insert((ROWS, 1, KIDS), Arc::from([1, 2]));
    data.related.insert((ROWS, 1, NO_KIDS), Arc::from([]));
    data
}

/// Each named field read on the record, and what reading them asked the
/// source for beyond the data it holds.
fn read(schema: &CalcSchema, names: &[&str]) -> (Vec<String>, Vec<CalcNeed>) {
    let cache = RefCell::new(data());
    let source = CachedSource::new(&cache);
    let session = SessionInfo::default();
    let (mut locals, mut globals) = (Variables::new(), Variables::new());
    let mut host = FileHost::new(
        FileCalc::new(schema, &source, Date::from_ymd(2026, 10, 29).unwrap()),
        &session,
        &mut locals,
        &mut globals,
    );
    let values = names
        .iter()
        .map(|name| {
            let reference = format!("${{{name}}}");
            let value = evaluate_text(&reference, &mut host, &Context::record(ROWS, 1)).value;
            value.to_text(&host.file.cx()).into_string()
        })
        .collect();
    (values, source.missing())
}

/// Defines each case's calculation, reads them all and checks each value;
/// what reading them asked the source for.
fn check(cases: &[(&str, &str, &str)]) -> Vec<CalcNeed> {
    let mut schema = schema();
    for (i, (name, text, _)) in cases.iter().enumerate() {
        let field = calculation(&schema, 10 + i as u64, name, text);
        schema.add_field(ROWS_TABLE, field);
    }
    let names: Vec<&str> = cases.iter().map(|(n, ..)| *n).collect();
    let (values, missing) = read(&schema, &names);
    for ((name, text, want), got) in cases.iter().zip(values) {
        assert_eq!(got, *want, "{name}: {text}");
    }
    missing
}

/// A formula that runs `IsEmpty`, `IsValid`, `GetAsBoolean`, `GetFieldName`
/// or `GetBaseTableName` is evaluated over empty fields; one that does not
/// is empty unless it evaluates always.
#[test]
fn field_inspection_forces_evaluation_over_empty_fields() {
    check(&[
        (
            "Fallback",
            "If ( IsEmpty ( Input ) ; \"Fallback\" ; Input )",
            "Fallback",
        ),
        ("Doubled", "Amount * 2", ""),
        ("Empty test", "IsEmpty ( Input )", "1"),
        ("Valid test", "IsValid ( Input )", "1"),
        ("Boolean test", "GetAsBoolean ( Input )", "0"),
        (
            "Field name",
            "GetFieldName ( Input ) & Input",
            "Rows::Input",
        ),
        ("Table name", "GetBaseTableName ( Input ) & Input", "Rows"),
    ]);
    let mut always = schema();
    let mut doubled = calculation(&always, 10, "Doubled", "Amount * 2");
    doubled.options |= EVALUATE_ALWAYS;
    always.add_field(ROWS_TABLE, doubled);
    assert_eq!(read(&always, &["Doubled"]).0, ["0"]);
}

/// The branches an `If`, `Case`, `Choose` or `and` does not take read
/// nothing: their fields neither count nor are fetched, and their
/// inspection functions do not force the result.
#[test]
fn only_the_branches_that_run_count() {
    let missing = check(&[
        (
            "Unchosen inspection",
            "If ( 0 ; IsEmpty ( Input ) ; Input & \"suffix\" )",
            "",
        ),
        ("Unchosen value", "If ( 0 ; Note ; Input & \"suffix\" )", ""),
        (
            "Chosen constant",
            "If ( 1 ; \"constant\" ; Input )",
            "constant",
        ),
        ("Case", "Case ( Input ; Note ; Amount & \"case\" )", ""),
        ("Choose", "Choose ( 1 ; Note ; Input & \"choose\" )", ""),
        (
            "Short circuit",
            "Input & ( Input and IsEmpty ( Note ) )",
            "",
        ),
        (
            "Unchosen relation",
            "If ( IsEmpty ( Input ) ; \"fallback\" ; Unfetched::Text )",
            "fallback",
        ),
    ]);
    assert_eq!(missing, [], "an unchosen branch must not fetch");
    // The same relation in the branch that runs is fetched.
    let missing = check(&[(
        "Chosen relation",
        "If ( IsEmpty ( Note ) ; \"fallback\" ; Unfetched::Text )",
        "?",
    )]);
    assert_eq!(
        missing,
        [CalcNeed::RelatedIds {
            from: ROWS,
            record: 1,
            to: UNFETCHED
        }]
    );
}

/// A container counts as a value even when empty, one nonempty field is
/// enough, and an aggregate reads its field on every related record, empty
/// without one.
#[test]
fn containers_nonempty_fields_and_aggregates() {
    check(&[
        ("Photo label", "Photo & \"container\"", "container"),
        ("Mixed", "Input & Amount & Note", "nonempty"),
        ("Kid count", "Count ( Kids::Text )", "1"),
        ("Kid list", "Input & List ( Kids::Text )", "a"),
        ("No kid count", "Count ( NoKids::Text )", ""),
        ("No kid list", "List ( NoKids::Text ) & \"list\"", ""),
    ]);
}

/// A nested unstored calculation, `Evaluate` text, `GetField` and a custom
/// function's body keep their reads and inspection functions to
/// themselves; a nested calculation's value and a custom function's
/// arguments are the caller's reads.
#[test]
fn nested_formulas_keep_their_reads() {
    check(&[
        ("Inspected", "If ( IsEmpty ( Input ) ; \"\" ; Input )", ""),
        ("Outer", "Input & \"outer\" & Inspected", ""),
        (
            "Fallback",
            "If ( IsEmpty ( Input ) ; \"Fallback\" ; Input )",
            "Fallback",
        ),
        ("Outer fallback", "Input & Fallback", "Fallback"),
        (
            "Dynamic inspection",
            "Input & Evaluate ( \"IsEmpty ( Input )\" )",
            "",
        ),
        ("Dynamic read", "Input & Evaluate ( \"Note\" )", ""),
        ("Dynamic field", "Input & GetField ( \"Note\" )", ""),
        ("Custom inspection", "Input & InspectInput", ""),
        ("Custom read", "Input & ReadNote", ""),
        ("Custom argument", "Input & Echo ( Note )", "nonempty"),
        (
            "Custom inspected argument",
            "Echo ( IsEmpty ( Input ) )",
            "1",
        ),
    ]);
}
