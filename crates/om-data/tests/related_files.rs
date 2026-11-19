//! A related hosted file imported into the root file's calculation schema
//! (`CalcSchema::import_file_schema`, calc-values.md 13): its formulas,
//! read through the root's occurrence of its table or solved in its own
//! occurrences, resolve its members, custom functions, formats, `$$`
//! variables, file `Get` selectors and ExecuteSQL catalog, never the
//! root's, although both files number their members alike.

use std::sync::Arc;

use om_data::calc::eval::{display, evaluate_text, Context, CustomFunction, Variables};
use om_data::calc::{Date, Locale, Value, ValueType};
use om_data::formula::{Formula, Key};
use om_data::host::sql::{SqlCatalog, SqlFieldInfo, SqlOccurrence};
use om_data::host::{
    CalcSchema, ExternalOccurrence, FieldClass, FieldDef, FileCalc, FileHost, HostReads,
    ImportedFileMap, JoinOp, JoinPredicate, MemorySource, OpenFile, Privileges, RecordValues,
    RelationshipDef, SessionInfo, SummaryDef, SummaryKind,
};
use om_data::meta::{ValueList, ValueSource};
use om_data::scripts::specs::SpecField;

/// The root file "Root": table Orders, its occurrence, and "Stock", its
/// occurrence of the related file's table.
const ORDERS: u64 = 1;
const ORDERS_TO: u64 = 2;
const STOCK_TO: u64 = 3;
/// The related file "Related" numbers its table Parts, its occurrence and
/// its custom function as the root numbers its own.
const PARTS: u64 = 1;
const PARTS_TO: u64 = 2;
/// `Tag ( x )` in both files.
const TAG: u64 = 5;

const CODE: u64 = 1;
const LABEL: u64 = 2;
const WEIGHT: u64 = 2;
const SHIPPED: u64 = 10;

/// "Do not evaluate if all referenced fields are empty" off.
const EVALUATE_ALWAYS: u32 = 0x20;

fn compile(schema: &CalcSchema, occurrence: u64, source: &str) -> Formula {
    let mut ctx = om_data::formula::Context::new(schema.names());
    ctx.occurrence = Some(occurrence);
    ctx.decimal_comma = schema.locale.decimal_separator == ',';
    let expr = om_data::formula::compile(source, &ctx)
        .unwrap_or_else(|e| panic!("{source}: {e:?}"))
        .expr;
    Formula {
        context: Some(Key::from_id(occurrence)),
        formatted: None,
        polish: expr,
    }
}

/// Add calculation `name` = `source` to the related file's Parts, with
/// result `ty`, evaluated always, solved from `context`: Parts, occurrence
/// 0 or none (calc-encoding.md 1).
fn add_calculation(
    schema: &mut CalcSchema,
    id: u64,
    name: &str,
    ty: ValueType,
    source: &str,
    context: Option<u64>,
) {
    let mut formula = compile(schema, PARTS_TO, source);
    formula.context = context.map(Key::from_id);
    let mut field = FieldDef::calculation(id, name, ty, formula, false);
    field.options = EVALUATE_ALWAYS;
    schema.add_field(PARTS, field);
}

fn tag(schema: &mut CalcSchema, body: &str) {
    let names = schema.names().clone();
    let parameters = vec!["x".to_owned()];
    let mut ctx = om_data::formula::Context::new(&names);
    ctx.parameters = &parameters;
    let body = om_data::formula::compile(body, &ctx).unwrap().expr;
    schema.add_custom_function(
        TAG,
        CustomFunction {
            name: "Tag".into(),
            parameters,
            body,
        },
    );
}

/// The related file, in German formats, with `sql` as its ExecuteSQL
/// catalog.
fn related(sql: SqlCatalog) -> CalcSchema {
    let mut s = CalcSchema::new("Related", Locale::de_de());
    s.add_table(PARTS, "Parts");
    s.add_occurrence(PARTS_TO, "Parts", PARTS);
    s.add_field(PARTS, FieldDef::data(CODE, "Code", ValueType::Text));
    s.add_field(PARTS, FieldDef::data(WEIGHT, "Weight", ValueType::Number));
    s.add_field(PARTS, FieldDef::data(SHIPPED, "Shipped", ValueType::Date));
    s.add_field(
        PARTS,
        FieldDef {
            class: FieldClass::Summary,
            summary: Some(SummaryDef::new(SummaryKind::List, SHIPPED)),
            ..FieldDef::data(11, "Shipped list", ValueType::Text)
        },
    );
    tag(&mut s, "\"related:\" & x");
    for (id, name, source) in [
        (3, "Shown", "Tag ( Weight * 2 )"),
        (
            4,
            "Mark",
            "Let ( $$Mark = \"related\" ; $$Mark & \"/\" & $local )",
        ),
        (5, "File", "Get ( FileName ) & \"|\" & Get ( FilePath )"),
        (6, "Decimal", "GetAsText ( Evaluate ( \"1,5 + 1\" ) )"),
        (
            7,
            "Names",
            "GetFieldName ( Parts::Code ) & \"=\" & Evaluate ( \"Parts::Code\" ) & \"|\" & \
             IsValidExpression ( \"Orders::Label\" )",
        ),
        (
            8,
            "Tables",
            "ExecuteSQL ( \"SELECT TableName, TableId, BaseTableName, BaseFileName FROM \
             FileMaker_Tables\" ; \"|\" ; \";\" )",
        ),
        (
            9,
            "Heavy",
            "ExecuteSQL ( \"SELECT Code FROM Parts WHERE Weight > 4\" ; \"\" ; \"\" )",
        ),
    ] {
        let formula = compile(&s, PARTS_TO, source);
        let mut field = FieldDef::calculation(id, name, ValueType::Text, formula, false);
        field.options = EVALUATE_ALWAYS;
        s.add_field(PARTS, field);
    }
    s.set_sql(sql);
    s
}

/// The root file with the related file imported and "Stock" bound to its
/// table, and their records: Orders A; Parts A (weight 3, shipped
/// 2026-10-31) and B (5).
fn files(sql: SqlCatalog) -> (CalcSchema, ImportedFileMap, MemorySource) {
    files_with(&related(sql))
}

/// [`files`] with `related` as the related file.
fn files_with(related: &CalcSchema) -> (CalcSchema, ImportedFileMap, MemorySource) {
    let mut root = CalcSchema::new("Root", Locale::en_us());
    root.add_table(ORDERS, "Orders");
    root.add_occurrence(ORDERS_TO, "Orders", ORDERS);
    root.add_field(ORDERS, FieldDef::data(CODE, "Code", ValueType::Text));
    root.add_field(ORDERS, FieldDef::data(LABEL, "Label", ValueType::Text));
    root.add_external_occurrence(ExternalOccurrence {
        id: STOCK_TO,
        name: "Stock".into(),
        source: "Related".into(),
    });
    root.add_relationship(RelationshipDef::new(
        1,
        ORDERS_TO,
        STOCK_TO,
        vec![JoinPredicate {
            op: JoinOp::Equal,
            left: CODE,
            right: CODE,
        }],
    ));
    tag(&mut root, "\"root:\" & x");
    let map = root.import_file_schema(related);
    assert!(root.bind_external_occurrence(STOCK_TO, map.tables[&PARTS]));

    let record = |pairs: &[(u64, Value)]| {
        let mut values = RecordValues::default();
        for (field, value) in pairs {
            values.set(*field, 1, value.clone());
        }
        values
    };
    let mut source = MemorySource::new();
    source.put_record(
        ORDERS,
        1,
        record(&[(CODE, Value::text("A")), (LABEL, Value::text("first"))]),
    );
    for (id, code, weight) in [(1, "A", 3), (2, "B", 5)] {
        source.put_record(
            map.tables[&PARTS],
            id,
            record(&[
                (CODE, Value::text(code)),
                (WEIGHT, Value::number(weight)),
                (SHIPPED, Value::Date(today())),
            ]),
        );
    }
    (root, map, source)
}

fn session() -> SessionInfo {
    SessionInfo {
        file_name: "Root".into(),
        file_path: "fmnet:/host.example/Root.fmp12".into(),
        ..SessionInfo::default()
    }
}

fn today() -> Date {
    Date::from_ymd(2026, 10, 31).unwrap()
}

fn eval(host: &mut FileHost<'_>, context: &Context, text: &str) -> String {
    let result = evaluate_text(text, host, context);
    assert_eq!(result.code(), 0, "{text}");
    display(&result.value, host)
}

#[test]
fn related_formulas_use_their_own_files_members() {
    let (root, map, source) = files(SqlCatalog::default());
    let session = session();
    let (mut locals, mut globals) = (Variables::new(), Variables::new());
    let mut host = FileHost::new(
        FileCalc::new(&root, &source, today()),
        &session,
        &mut locals,
        &mut globals,
    );
    let orders = Context::record(ORDERS_TO, 1);
    let parts = Context::record(map.occurrences[&PARTS_TO], 1);
    // Custom function 5 and occurrence 2 are each file's own.
    assert_eq!(eval(&mut host, &orders, "Tag ( \"x\" )"), "root:x");
    assert_eq!(eval(&mut host, &parts, "Tag ( \"x\" )"), "related:x");
    assert_eq!(eval(&mut host, &orders, "Stock::Shown"), "related:6");
    assert_eq!(eval(&mut host, &orders, "Sum ( Stock::Weight )"), "3");
    assert_eq!(eval(&mut host, &orders, "Label"), "first");
    assert_eq!(eval(&mut host, &parts, "Weight"), "3");
    assert_ne!(evaluate_text("Label", &mut host, &parts).code(), 0);
    // Names: each file's own, in Evaluate too.
    assert_eq!(
        eval(&mut host, &orders, "GetFieldName ( Stock::Code )"),
        "Stock::Code"
    );
    assert_eq!(eval(&mut host, &orders, "Stock::Names"), "Parts::Code=A|0");
    assert_eq!(
        eval(
            &mut host,
            &orders,
            "IsValidExpression ( \"Orders::Label\" )"
        ),
        "1"
    );
    assert_eq!(
        eval(&mut host, &orders, "GetBaseTableName ( Stock::Code )"),
        "Parts"
    );
}

#[test]
fn related_formulas_keep_their_own_variables_formats_and_file() {
    let (root, map, source) = files(SqlCatalog::default());
    let session = session();
    let (mut locals, mut globals) = (Variables::new(), Variables::new());
    let mut host = FileHost::new(
        FileCalc::new(&root, &source, today()),
        &session,
        &mut locals,
        &mut globals,
    );
    let orders = Context::record(ORDERS_TO, 1);
    let parts = Context::record(map.occurrences[&PARTS_TO], 1);
    // `$$` variables are per file; `$` ones belong to the script frame.
    assert_eq!(
        eval(
            &mut host,
            &orders,
            "Let ( [ $$Mark = \"root\" ; $local = \"shared\" ] ; Stock::Mark & \"|\" & $$Mark )"
        ),
        "related/shared|root"
    );
    assert_eq!(eval(&mut host, &orders, "$$Mark"), "root");
    assert_eq!(eval(&mut host, &parts, "$$Mark"), "related");
    assert_eq!(eval(&mut host, &parts, "$local"), "shared");
    // Formats: a formula's, and a summary's computed by its table's file
    // although the root's occurrence reads it.
    assert_eq!(eval(&mut host, &orders, "Stock::Decimal"), "2,5");
    assert_eq!(
        eval(&mut host, &orders, "Stock::Shipped list"),
        "31.10.2026"
    );
    assert_eq!(
        eval(
            &mut host,
            &orders,
            "GetSummary ( Stock::Shipped list ; Stock::Shipped list )"
        ),
        "31.10.2026"
    );
    assert_eq!(
        eval(&mut host, &orders, "GetAsText ( Evaluate ( \"1.5 + 1\" ) )"),
        "2.5"
    );
    // The file.
    assert_eq!(
        eval(&mut host, &orders, "Stock::File"),
        "Related|fmnet:/host.example/Related.fmp12"
    );
    assert_eq!(eval(&mut host, &orders, "Get ( FileName )"), "Root");
    assert_eq!(
        eval(&mut host, &parts, "Get ( FileLocaleElements )"),
        Locale::de_de().elements_json(true)
    );
    assert_eq!(
        eval(&mut host, &orders, "Get ( FileLocaleElements )"),
        Locale::en_us().elements_json(true)
    );
}

#[test]
fn imported_account_privileges_belong_to_the_authenticated_owning_file() {
    const REPORT: &str = "Get ( AccountPrivilegeSetName ) & \"|\" & Get ( CurrentPrivilegeSetName ) & \"|\" & Substitute ( Get ( AccountExtendedPrivileges ) ; Char ( 13 ) ; \",\" ) & \"|\" & Substitute ( Get ( CurrentExtendedPrivileges ) ; Char ( 13 ) ; \",\" ) & \"|\" & Get ( LayoutAccess ) & \"|\" & Get ( RecordAccess )";
    let root_account = Privileges {
        account_set_name: "Root account".into(),
        extended: vec!["root-only".into()],
        layout_access: Some(1),
        record_access: Some(1),
    };
    let elevated = Privileges {
        account_set_name: "[Full Access]".into(),
        extended: vec!["root-elevated".into()],
        layout_access: Some(2),
        record_access: Some(2),
    };
    for (metadata, answer) in [
        (
            Some(Privileges {
                account_set_name: "Related account".into(),
                extended: vec!["zeta".into(), "alpha".into()],
                layout_access: Some(0),
                record_access: Some(1),
            }),
            "Related account|Related account|zeta,alpha|zeta,alpha|0|1",
        ),
        (
            Some(Privileges {
                account_set_name: "[Full Access]".into(),
                extended: vec!["related-full".into()],
                layout_access: Some(2),
                record_access: Some(2),
            }),
            "[Full Access]|[Full Access]|related-full|related-full|2|2",
        ),
        (
            Some(Privileges {
                account_set_name: "Partial metadata".into(),
                ..Privileges::default()
            }),
            "Partial metadata|Partial metadata||||",
        ),
        (None, "|||||"),
    ] {
        // Unread metadata answers empty, which is not FileMaker's answer.
        let known = metadata
            .as_ref()
            .is_some_and(|p| p.layout_access.is_some() && p.record_access.is_some());
        let mut child = related(SqlCatalog::default());
        child.authenticated_privileges = metadata;
        for (id, name, context) in [
            (40, "Privileges", Some(PARTS_TO)),
            (41, "ZeroPrivileges", Some(0)),
            (42, "AbsentPrivileges", None),
        ] {
            add_calculation(&mut child, id, name, ValueType::Text, REPORT, context);
        }
        add_calculation(
            &mut child,
            43,
            "NestedPrivileges",
            ValueType::Text,
            "Tag ( Evaluate ( \"AbsentPrivileges\" ) )",
            None,
        );
        let (mut root, map, source) = files_with(&child);
        root.authenticated_privileges = Some(root_account.clone());
        // A second imported file with the same member ids must keep its own
        // unavailable metadata, even when the first import is known.
        let unknown = root.import_file_schema(&related(SqlCatalog::default()));
        for current in [None, Some(elevated.clone())] {
            let session = SessionInfo {
                privilege_set_name: root_account.account_set_name.clone(),
                privileges: Some(root_account.clone()),
                current_privileges: current.clone(),
                ..session()
            };
            let (mut locals, mut globals) = (Variables::new(), Variables::new());
            let mut host = FileHost::new(
                FileCalc::new(&root, &source, today()),
                &session,
                &mut locals,
                &mut globals,
            );
            let orders = Context::record(ORDERS_TO, 1);
            let root_answer = if current.is_some() {
                "Root account|[Full Access]|root-only|root-elevated|2|2"
            } else {
                "Root account|Root account|root-only|root-only|1|1"
            };
            assert_eq!(eval(&mut host, &orders, REPORT), root_answer);
            assert_eq!(
                host.reads,
                HostReads {
                    session: true,
                    unknown: false
                }
            );
            // The root owns an expression in its external occurrence too.
            assert_eq!(
                eval(&mut host, &Context::record(STOCK_TO, 1), REPORT),
                root_answer
            );
            for name in ["Privileges", "ZeroPrivileges", "AbsentPrivileges"] {
                assert_eq!(eval(&mut host, &orders, &format!("Stock::{name}")), answer);
            }
            assert_eq!(host.reads.unknown, !known, "{answer}");
            host.reads = HostReads::default();
            assert_eq!(
                eval(&mut host, &orders, "Stock::NestedPrivileges"),
                format!("related:{answer}")
            );
            // Both occurrence-derived ownership and explicit ownership (which
            // overrides a root occurrence) select the related account.
            for context in [
                Context::record(map.occurrences[&PARTS_TO], 1),
                Context {
                    file: Some(1),
                    ..orders.clone()
                },
                Context {
                    file: Some(1),
                    ..Context::new()
                },
            ] {
                assert_eq!(eval(&mut host, &context, REPORT), answer);
            }
            assert_eq!(
                eval(
                    &mut host,
                    &Context::record(unknown.occurrences[&PARTS_TO], 1),
                    REPORT
                ),
                "|||||"
            );
            assert!(host.reads.unknown);
            assert_eq!(eval(&mut host, &orders, REPORT), root_answer);
        }
    }
}

#[test]
fn execute_sql_reads_the_file_owning_the_formula() {
    let (root, map, source) = files(SqlCatalog::default());
    let session = session();
    let (mut locals, mut globals) = (Variables::new(), Variables::new());
    let mut host = FileHost::new(
        FileCalc::new(&root, &source, today()),
        &session,
        &mut locals,
        &mut globals,
    );
    let orders = Context::record(ORDERS_TO, 1);
    let parts = Context::record(map.occurrences[&PARTS_TO], 1);
    let tables = "ExecuteSQL ( \"SELECT TableName, TableId, BaseTableName, BaseFileName FROM \
                  FileMaker_Tables\" ; \"|\" ; \";\" )";
    assert_eq!(
        eval(&mut host, &orders, tables),
        "Orders|2|Orders|Root;Stock|3|Parts|Related"
    );
    assert_eq!(
        eval(&mut host, &orders, "Stock::Tables"),
        "Parts|2|Parts|Related"
    );
    assert_eq!(eval(&mut host, &parts, tables), "Parts|2|Parts|Related");
    assert_eq!(eval(&mut host, &orders, "Stock::Heavy"), "B");
    assert_eq!(
        eval(
            &mut host,
            &orders,
            "ExecuteSQL ( \"SELECT Code FROM Stock WHERE Weight > 4\" ; \"\" ; \"\" )"
        ),
        "B"
    );
}

#[test]
fn execute_sql_lists_the_owning_files_catalog_with_its_own_ids() {
    let mut sql = SqlCatalog {
        occurrences: vec![SqlOccurrence {
            id: PARTS_TO,
            name: "Parts".into(),
            table: Some(PARTS),
            mod_count: 7,
        }],
        ..SqlCatalog::default()
    };
    sql.tables.insert(PARTS, 4);
    sql.fields.insert(
        (PARTS, CODE),
        SqlFieldInfo {
            mod_count: 9,
            ..SqlFieldInfo::default()
        },
    );
    let (root, map, source) = files(sql);
    let session = session();
    let (mut locals, mut globals) = (Variables::new(), Variables::new());
    let mut host = FileHost::new(
        FileCalc::new(&root, &source, today()),
        &session,
        &mut locals,
        &mut globals,
    );
    let orders = Context::record(ORDERS_TO, 1);
    let parts = Context::record(map.occurrences[&PARTS_TO], 1);
    let sql = |query: &str| format!("ExecuteSQL ( \"{query}\" ; \"|\" ; \";\" )");
    assert_eq!(
        eval(
            &mut host,
            &parts,
            &sql("SELECT TableName, TableId, BaseTableName, ModCount FROM FileMaker_Tables")
        ),
        "Parts|2|Parts|7"
    );
    assert_eq!(
        eval(
            &mut host,
            &parts,
            &sql("SELECT * FROM FileMaker_BaseTables")
        ),
        "Parts|1|<Internal>|4"
    );
    // A field's options come from the file owning its table, from either
    // file's formulas.
    let code =
        sql("SELECT ModCount FROM FileMaker_Fields WHERE TableName = 'XX' AND FieldName = 'Code'");
    assert_eq!(eval(&mut host, &parts, &code.replace("XX", "Parts")), "9");
    assert_eq!(eval(&mut host, &orders, &code.replace("XX", "Stock")), "9");
    // The root lists only its own base table.
    assert_eq!(
        eval(
            &mut host,
            &orders,
            &sql("SELECT BaseTableName, BaseTableId FROM FileMaker_BaseTables")
        ),
        "Orders|1"
    );
}

#[test]
fn declared_results_convert_in_their_own_files_formats() {
    // The related file's German formats read the text its calculations
    // return as their declared types, whichever file's formula reads them.
    let mut child = related(SqlCatalog::default());
    let number = ValueType::Number;
    add_calculation(
        &mut child,
        20,
        "Text number",
        number,
        "\"1,5\"",
        Some(PARTS_TO),
    );
    let date = ValueType::Date;
    add_calculation(
        &mut child,
        21,
        "Text date",
        date,
        "\"31.10.2026\"",
        Some(PARTS_TO),
    );
    let (root, _, source) = files_with(&child);
    let session = session();
    let (mut locals, mut globals) = (Variables::new(), Variables::new());
    let mut host = FileHost::new(
        FileCalc::new(&root, &source, today()),
        &session,
        &mut locals,
        &mut globals,
    );
    let orders = Context::record(ORDERS_TO, 1);
    assert_eq!(eval(&mut host, &orders, "Stock::Text number + 0"), "1.5");
    let day = "Day ( Stock::Text date ) & \".\" & Month ( Stock::Text date )";
    assert_eq!(eval(&mut host, &orders, day), "31.10");
}

/// [`related`] with calculations that have no occurrence of their own:
/// `Zero…` store occurrence 0, `Absent…` none (calc-encoding.md 1).
fn related_without_contexts() -> CalcSchema {
    let mut child = related(SqlCatalog::default());
    let mut id = 30;
    for (prefix, context) in [("Zero", Some(0)), ("Absent", None)] {
        for (name, source) in [
            ("File", "Get ( FileName )"),
            ("Locale", "GetAsNumber ( \"1,5\" ) * 2"),
            (
                "Names",
                "Evaluate ( \"Tag ( 1 )\" ) & \"|\" & GetField ( \"Code\" )",
            ),
            ("Mark", "$$Mark"),
            (
                "Tables",
                "ExecuteSQL ( \"SELECT TableName FROM FileMaker_Tables\" ; \"\" ; \"\" ) & \
                 \"|\" & TableNames ( \"\" )",
            ),
        ] {
            let name = format!("{prefix}{name}");
            add_calculation(&mut child, id, &name, ValueType::Text, source, context);
            id += 1;
        }
    }
    child
}

#[test]
fn formulas_without_an_occurrence_are_still_their_files() {
    // Read through the root's occurrence of the related table, which they
    // are then solved in, they keep the related file's name, formats,
    // names, `$$` variables, ExecuteSQL catalog and Design answers.
    let (root, _, source) = files_with(&related_without_contexts());
    let session = session();
    let (mut locals, mut globals) = (Variables::new(), Variables::new());
    let mut host = FileHost::new(
        FileCalc::new(&root, &source, today()),
        &session,
        &mut locals,
        &mut globals,
    );
    let orders = Context::record(ORDERS_TO, 1);
    let stock = Context::record(STOCK_TO, 1);
    // The related file's `$$Mark` is "related", the root's "root".
    assert_eq!(
        eval(
            &mut host,
            &orders,
            "Let ( $$Mark = \"root\" ; Stock::Mark )"
        ),
        "related/"
    );
    for prefix in ["Zero", "Absent"] {
        let field = |name: &str| format!("Stock::{prefix}{name}");
        assert_eq!(eval(&mut host, &orders, &field("File")), "Related");
        assert_eq!(eval(&mut host, &orders, &field("Locale")), "3");
        assert_eq!(eval(&mut host, &orders, &field("Names")), "related:1|A");
        assert_eq!(eval(&mut host, &orders, &field("Mark")), "related");
        assert_eq!(eval(&mut host, &orders, &field("Tables")), "Parts|Parts");
        // The root's formula in the same occurrence compiles the same text
        // with its own names.
        let both = format!("Evaluate ( \"Tag ( 1 )\" ) & \"|\" & {prefix}Names");
        assert_eq!(eval(&mut host, &stock, &both), "root:1|related:1|A");
        // ExecuteSQL reading them from the root.
        let query = format!(
            "ExecuteSQL ( \"SELECT {prefix}File, {prefix}Mark FROM Stock WHERE Code = 'A'\" ; \
             \"|\" ; \"\" )"
        );
        assert_eq!(eval(&mut host, &orders, &query), "Related|related");
    }
    assert_eq!(eval(&mut host, &orders, "$$Mark"), "root");
    assert_eq!(eval(&mut host, &orders, "Get ( FileName )"), "Root");
}

fn custom_list(id: u64, name: &str, values: &str) -> ValueList {
    ValueList {
        id,
        name: name.into(),
        source: ValueSource::Custom(vec![values.into()]),
        options: 0,
        sort_locale: 0,
        separator: " ".into(),
    }
}

#[test]
fn design_functions_answer_for_the_file_asked_about() {
    // The related file adds "Twin" (id 4), another occurrence of Parts
    // related to it, a value list and a comment.
    const TWIN_TO: u64 = 4;
    let mut child = related(SqlCatalog::default());
    child.add_occurrence(TWIN_TO, "Twin", PARTS);
    child.add_relationship(RelationshipDef::new(
        1,
        PARTS_TO,
        TWIN_TO,
        vec![JoinPredicate {
            op: JoinOp::Equal,
            left: CODE,
            right: CODE,
        }],
    ));
    child.set_table_comment(PARTS, "Parts on hand");
    child.add_value_list(custom_list(7, "Sizes", "S\rM"));
    child.add_value_list(ValueList {
        id: 8,
        name: "Shipped".into(),
        source: ValueSource::Field {
            primary: SpecField {
                occurrence: Some(PARTS_TO),
                field: SHIPPED,
                repetition: 1,
            },
            secondary: None,
            start: None,
        },
        ..custom_list(8, "", "")
    });
    let (mut root, map, source) = files_with(&child);
    root.add_layout(1, "Main", Some(ORDERS_TO));
    root.add_value_list(custom_list(1, "Colors", "Red\rBlue"));
    root.set_table_comment(ORDERS, "Orders taken");
    let session = SessionInfo {
        window_name: "Root".into(),
        ..session()
    };
    let (mut locals, mut globals) = (Variables::new(), Variables::new());
    let mut host = FileHost::new(
        FileCalc::new(&root, &source, today()),
        &session,
        &mut locals,
        &mut globals,
    );
    let orders = Context::record(ORDERS_TO, 1);
    let parts = Context::record(map.occurrences[&PARTS_TO], 1);
    let fields = "Code\rWeight\rShown\rMark\rFile\rDecimal\rNames\rTables\rHeavy\rShipped\r\
                  Shipped list";
    let weight = eval(&mut host, &orders, "FieldType ( \"\" ; \"Stock::Weight\" )");
    assert!(weight.starts_with("Standard Number"), "{weight}");
    for (context, text, answer) in [
        // The root file: its own members, names and ids only.
        (&orders, "TableNames ( \"\" )", "Orders\rStock"),
        (&orders, "TableIDs ( \"\" )", "2\r3"),
        (&orders, "BaseTableNames ( \"Root\" )", "Orders"),
        (&orders, "BaseTableIDs ( \"\" )", "1"),
        (&orders, "ValueListNames ( \"\" )", "Colors"),
        (&orders, "ValueListIDs ( \"\" )", "1"),
        (&orders, "ValueListItems ( \"\" ; \"Related::Sizes\" )", ""),
        (&orders, "FieldNames ( \"\" ; \"Related::Parts\" )", ""),
        (&orders, "FieldNames ( \"\" ; \"Stock\" )", fields),
        (
            &orders,
            "BaseTableComment ( \"\" ; \"Related::Parts\" )",
            "",
        ),
        (
            &orders,
            "BaseTableComment ( \"\" ; \"Orders\" )",
            "Orders taken",
        ),
        (&orders, "LayoutNames ( \"\" )", "Main"),
        (
            &orders,
            "RelationInfo ( \"\" ; \"Orders\" )",
            "Source:Related\rTable:Stock\rOptions:\rStock::Code = Orders::Code",
        ),
        // The related file, by its name or as its formulas' own file: its
        // members under its own names and ids.
        (&orders, "TableNames ( \"Related\" )", "Parts\rTwin"),
        (&orders, "TableIDs ( \"related.FMP12\" )", "2\r4"),
        (&parts, "TableIDs ( \"\" )", "2\r4"),
        (&parts, "BaseTableNames ( \"\" )", "Parts"),
        (&parts, "BaseTableIDs ( \"\" )", "1"),
        (&parts, "ValueListNames ( \"\" )", "Sizes\rShipped"),
        (&parts, "ValueListIDs ( \"\" )", "7\r8"),
        (
            &orders,
            "ValueListItems ( \"Related\" ; \"sizes\" )",
            "S\rM",
        ),
        // In the related file's formats.
        (
            &orders,
            "ValueListItems ( \"Related\" ; \"Shipped\" )",
            "31.10.2026",
        ),
        (&parts, "ValueListItems ( \"\" ; \"Colors\" )", ""),
        (
            &parts,
            "ValueListItems ( \"Root\" ; \"Colors\" )",
            "Red\rBlue",
        ),
        (&orders, "FieldNames ( \"Related\" ; \"Twin\" )", fields),
        (
            &orders,
            "FieldIDs ( \"Related\" ; \"Parts\" )",
            "1\r2\r3\r4\r5\r6\r7\r8\r9\r10\r11",
        ),
        (&parts, "FieldType ( \"\" ; \"Weight\" )", &weight),
        (
            &orders,
            "FieldType ( \"Related\" ; \"Twin::Weight\" )",
            &weight,
        ),
        // Orders::Code is the root's.
        (&orders, "FieldType ( \"Related\" ; \"Code\" )", ""),
        (
            &orders,
            "BaseTableComment ( \"Related\" ; \"Parts\" )",
            "Parts on hand",
        ),
        (
            &orders,
            "RelationInfo ( \"Related\" ; \"Parts\" )",
            "Source:Related\rTable:Twin\rOptions:\rTwin::Code = Parts::Code",
        ),
        // A related file's layouts and windows are not imported.
        (&orders, "WindowNames ( \"\" )", "Root"),
        (&parts, "WindowNames ( \"\" )", ""),
        (&parts, "LayoutNames ( \"\" )", ""),
        (&orders, "LayoutNames ( \"Related\" )", ""),
        (&orders, "TableNames ( \"Elsewhere\" )", ""),
    ] {
        assert_eq!(eval(&mut host, context, text), answer, "{text}");
    }
}

#[test]
fn a_related_file_open_in_the_session_answers_its_layouts_and_windows() {
    // The related file is also open, with a layout and a window, behind
    // the root file's.
    let mut child = related(SqlCatalog::default());
    child.add_layout(3, "Parts List", Some(PARTS_TO));
    let (mut root, map, source) = files_with(&child);
    root.add_layout(1, "Main", Some(ORDERS_TO));
    let session = SessionInfo {
        window_name: "Root".into(),
        other_files: vec![OpenFile {
            schema: Arc::new(child),
        }],
        open_position: 0,
        windows: vec![
            ("Root".into(), "Root".into()),
            ("Related".into(), "Parts".into()),
        ],
        ..session()
    };
    let (mut locals, mut globals) = (Variables::new(), Variables::new());
    let mut host = FileHost::new(
        FileCalc::new(&root, &source, today()),
        &session,
        &mut locals,
        &mut globals,
    );
    let orders = Context::record(ORDERS_TO, 1);
    let parts = Context::record(map.occurrences[&PARTS_TO], 1);
    for (context, text, answer) in [
        (&orders, "DatabaseNames", "Root\rRelated"),
        (&orders, "WindowNames", "Root\rParts"),
        (&orders, "WindowNames ( \"\" )", "Root"),
        (&orders, "WindowNames ( \"related.fmp12\" )", "Parts"),
        // "" is the related file for its own formulas.
        (&parts, "WindowNames ( \"\" )", "Parts"),
        // Its layouts come from the open file, by its name or as its
        // formulas' own file.
        (&orders, "LayoutNames ( \"Related\" )", "Parts List"),
        (&parts, "LayoutNames ( \"\" )", "Parts List"),
        (&parts, "LayoutNames ( \"Root\" )", "Main"),
        (&orders, "LayoutNames ( \"\" )", "Main"),
        // The rest from the imported schema, in the formula's own context.
        (&parts, "TableNames ( \"\" )", "Parts"),
        (&orders, "BaseTableNames ( \"Related\" )", "Parts"),
    ] {
        assert_eq!(eval(&mut host, context, text), answer, "{text}");
    }
    // An unqualified field of the formula's own table.
    let weight = eval(&mut host, &parts, "FieldType ( \"\" ; \"Weight\" )");
    assert!(weight.starts_with("Standard Number"), "{weight}");
}

/// A related file's value list is read under the account that file was
/// opened with, by its own id: hidden there, it has no items here, whatever
/// the root file's account may read.
#[test]
fn a_related_files_value_list_is_hidden_by_that_files_account() {
    use om_data::host::privilege_set::{
        AccessLevel, Actions, Allow, CustomRules, ItemAccess, Quad, TableRules,
    };
    use om_data::host::security::SetAccess;
    use om_data::host::AccountInfo;
    let mut child = related(SqlCatalog::default());
    child.add_value_list(custom_list(7, "Sizes", "S\rM"));
    child.add_value_list(custom_list(9, "Grades", "A\rB"));
    // Its account's custom set: Sizes (`09, 21 07` = `d5`) no access,
    // other lists view only.
    let mut value_lists = ItemAccess {
        default: Some(Quad(0xd6)),
        ..ItemAccess::default()
    };
    value_lists.items.insert(vec![7], Quad(0xd5));
    let read = TableRules {
        actions: Actions {
            create: Allow::No,
            view: Allow::Yes,
            edit: Allow::No,
            delete: Allow::No,
        },
        fields: AccessLevel::Modifiable,
        field_levels: Default::default(),
    };
    child.authenticated_account = AccountInfo {
        account_type: Some("FileMaker File".into()),
        password_expiry: None,
        set_access: Some(SetAccess::Custom(Box::new(CustomRules {
            default: read,
            tables: Default::default(),
            layouts: ItemAccess::default(),
            scripts: ItemAccess::default(),
            value_lists,
            prints: true,
            exports: true,
            menus: om_data::host::privilege_set::MenuCommands::All,
        }))),
    };
    let (mut root, _, source) = files_with(&child);
    root.add_value_list(custom_list(1, "Colors", "Red\rBlue"));
    let session = session();
    let (mut locals, mut globals) = (Variables::new(), Variables::new());
    let mut host = FileHost::new(
        FileCalc::new(&root, &source, today()),
        &session,
        &mut locals,
        &mut globals,
    );
    let orders = Context::record(ORDERS_TO, 1);
    for (text, answer) in [
        ("ValueListItems ( \"Related\" ; \"Sizes\" )", ""),
        ("ValueListItems ( \"Related\" ; \"Grades\" )", "A\rB"),
        ("ValueListItems ( \"\" ; \"Colors\" )", "Red\rBlue"),
        ("ValueListNames ( \"Related\" )", "Sizes\rGrades"),
    ] {
        assert_eq!(eval(&mut host, &orders, text), answer, "{text}");
    }
}

#[test]
fn reimporting_a_file_gives_disjoint_ids_the_same_way_each_time() {
    use std::collections::BTreeSet;
    let child = related(SqlCatalog::default());
    let ids = |map: &ImportedFileMap| -> BTreeSet<u64> {
        [
            &map.tables,
            &map.occurrences,
            &map.functions,
            &map.value_lists,
        ]
        .into_iter()
        .flat_map(|m| m.values().copied())
        .collect()
    };
    let run = || {
        let mut root = CalcSchema::new("Root", Locale::en_us());
        let first = root.import_file_schema(&child);
        let second = root.import_file_schema(&child);
        assert!(ids(&first).is_disjoint(&ids(&second)));
        (ids(&first), ids(&second))
    };
    assert_eq!(run(), run());
}

#[test]
fn imported_account_facts_keep_owning_file_and_unknown_provenance() {
    use om_data::host::AccountInfo;
    const REPORT: &str = "Get ( AccountType ) & \"|\" & Get ( AccountPasswordDaysRemaining )";
    for known in [true, false] {
        let mut child = related(SqlCatalog::default());
        if known {
            child.authenticated_account = AccountInfo {
                account_type: Some("FileMaker File".into()),
                password_expiry: Some(1),
                set_access: None,
            };
        }
        add_calculation(
            &mut child,
            44,
            "AccountFacts",
            ValueType::Text,
            REPORT,
            Some(PARTS_TO),
        );
        let (root, map, source) = files_with(&child);
        let session = SessionInfo {
            account_info: AccountInfo {
                account_type: Some("FileMaker File".into()),
                password_expiry: Some(-1),
                set_access: None,
            },
            ..session()
        };
        let (mut locals, mut globals) = (Variables::new(), Variables::new());
        let mut host = FileHost::new(
            FileCalc::new(&root, &source, today()),
            &session,
            &mut locals,
            &mut globals,
        );
        let orders = Context::record(ORDERS_TO, 1);
        assert_eq!(eval(&mut host, &orders, REPORT), "FileMaker File|-1");
        host.reads = HostReads::default();
        let want = if known { "FileMaker File|0" } else { "|" };
        assert_eq!(eval(&mut host, &orders, "Stock::AccountFacts"), want);
        assert_eq!(host.read_provenance().unknown, !known);
        host.reads = HostReads::default();
        assert_eq!(
            eval(
                &mut host,
                &Context::record(map.occurrences[&PARTS_TO], 1),
                REPORT
            ),
            want
        );
        assert_eq!(host.read_provenance().unknown, !known);
    }
}
