//! Queries over an in-memory source: results and their text, NULL and
//! empty values, parameters, errors and `ExecuteSQL`/`ExecuteSQLe` through
//! the calculation hook.

use om_calc::eval::{self, CalcHost, Context, HostFunction, NullHost, VarScope, Variables};
use om_calc::{CalcError, Date, Locale, Number, Time, TimeStyle, Timestamp, Value};
use om_sql::source::Cursor;
use om_sql::{FieldClass, FieldDef, FieldType, SqlError, SqlSource, TableDef};

struct Table {
    def: TableDef,
    fields: Vec<FieldDef>,
    records: Vec<(u64, Vec<Value>)>,
}

/// Two tables: `People` (id, name, born, team) and `Teams` (id, name).
struct Memory {
    locale: Locale,
    tables: Vec<Table>,
    /// The query should stop (its script was aborted).
    stop: bool,
    /// The memory a query's intermediate rows may take, when not the
    /// default.
    budget: Option<usize>,
    /// An index may propose unordered, duplicated or nonmatching ids.
    index: Option<Vec<u64>>,
}

fn field(id: u64, name: &str, ty: FieldType) -> FieldDef {
    FieldDef {
        id,
        name: name.into(),
        ty,
        class: FieldClass::Normal,
        global: false,
        repetitions: 1,
        mod_count: 1,
    }
}

fn table(id: u64, name: &str) -> TableDef {
    TableDef {
        id,
        name: name.into(),
        table_id: 1_065_088 + id,
        base: Some((name.into(), "work".into())),
        mod_count: 1,
    }
}

fn date(y: i64, m: u32, d: u32) -> Value {
    Value::Date(Date::from_ymd(y, m, d).unwrap())
}

fn memory() -> Memory {
    let n = |v: i64| Value::number(v);
    let t = |s: &str| Value::text(s);
    Memory {
        locale: Locale::en_us(),
        tables: vec![
            Table {
                def: table(1, "People"),
                fields: vec![
                    field(1, "id", FieldType::Number),
                    field(2, "name", FieldType::Text),
                    field(3, "born", FieldType::Date),
                    field(4, "team", FieldType::Number),
                ],
                records: vec![
                    (1, vec![n(1), t("Ada"), date(1815, 12, 10), n(1)]),
                    (2, vec![n(2), t("alan"), date(1912, 6, 23), n(2)]),
                    (3, vec![n(3), t(" Grace"), date(1906, 12, 9), n(1)]),
                    (
                        5,
                        vec![
                            n(4),
                            t(""),
                            Value::empty(om_calc::ValueType::Date),
                            Value::empty(om_calc::ValueType::Number),
                        ],
                    ),
                ],
            },
            Table {
                def: table(2, "Teams"),
                fields: vec![
                    field(1, "id", FieldType::Number),
                    field(2, "name", FieldType::Text),
                ],
                records: vec![(1, vec![n(1), t("Engines")]), (2, vec![n(2), t("Codes")])],
            },
        ],
        stop: false,
        budget: None,
        index: None,
    }
}

impl Memory {
    fn table(&self, id: u64) -> &Table {
        self.tables.iter().find(|t| t.def.id == id).unwrap()
    }
}

impl SqlSource for Memory {
    fn locale(&self) -> &Locale {
        &self.locale
    }

    fn now(&self) -> Timestamp {
        let noon = Time::from_micros(12 * 3_600_000_000, TimeStyle::Elapsed);
        Timestamp::from_date_time(Date::from_ymd(2026, 10, 28).unwrap(), noon).unwrap()
    }

    fn user_name(&self) -> String {
        "Admin".into()
    }

    fn tables(&self) -> Vec<TableDef> {
        self.tables.iter().map(|t| t.def.clone()).collect()
    }

    fn fields(&self, table: &TableDef) -> Vec<FieldDef> {
        self.table(table.id).fields.clone()
    }

    fn records(&mut self, table: &TableDef) -> Result<Vec<u64>, SqlError> {
        Ok(self.table(table.id).records.iter().map(|r| r.0).collect())
    }

    fn value(
        &mut self,
        table: &TableDef,
        record: u64,
        field: &FieldDef,
        _rep: u32,
        _c: Cursor,
    ) -> Result<Value, CalcError> {
        let t = self.table(table.id);
        let r = t.records.iter().find(|r| r.0 == record).unwrap();
        let i = t.fields.iter().position(|f| f.id == field.id).unwrap();
        Ok(r.1[i].clone())
    }

    fn interrupted(&self) -> bool {
        self.stop
    }

    fn lookup(
        &mut self,
        _: &TableDef,
        _: &FieldDef,
        _: &Value,
    ) -> Result<Option<Vec<u64>>, SqlError> {
        Ok(self.index.clone())
    }

    fn memory_budget(&self) -> usize {
        self.budget.unwrap_or(om_sql::source::MEMORY_BUDGET)
    }
}

fn run(sql: &str) -> String {
    match om_sql::execute(&mut memory(), sql, "|", "\n", &[]) {
        Ok(text) => text,
        Err(e) => format!("? ERROR: {}", e.message()),
    }
}

#[test]
fn indexed_candidates_preserve_order_and_recheck_predicates() {
    let mut source = memory();
    // Nonmonotonic creation order must survive even though index ids are
    // sorted for membership. The index includes a duplicate, an absent
    // record and false positives which the SQL predicate must reject.
    source.tables[0].records.swap(0, 2);
    source.index = Some(vec![5, 1, 99, 3, 2, 3]);
    let sql = "SELECT ROWID, name FROM People WHERE team = 1";
    assert_eq!(
        om_sql::execute(&mut source, sql, "|", "\n", &[]),
        Ok("3| Grace\n1|Ada".into())
    );
    // The next conjunct must intersect the already narrowed candidates,
    // not restore all records from the original table or index.
    assert_eq!(
        om_sql::execute(
            &mut source,
            "SELECT ROWID FROM People WHERE id > 1 AND team = 1",
            "",
            "",
            &[],
        ),
        Ok("3".into())
    );
    source.index = Some(Vec::new());
    assert_eq!(
        om_sql::execute(&mut source, sql, "", "", &[]),
        Ok(String::new())
    );
    // No index continues to use the scan path.
    source.index = None;
    assert_eq!(
        om_sql::execute(&mut source, sql, "|", "\n", &[]),
        Ok("3| Grace\n1|Ada".into())
    );
}

#[test]
fn queries_and_their_text() {
    assert_eq!(run("SELECT name FROM People WHERE id < 3"), "Ada\nalan");
    assert_eq!(
        run("SELECT COUNT(*), COUNT(name), MAX(born) FROM People"),
        "4|3|1912-06-23"
    );
    // Binary text order; the sort key drops leading spaces.
    assert_eq!(run("SELECT name FROM People WHERE name > 'B'"), "alan");
    assert_eq!(
        run("SELECT name FROM People ORDER BY name"),
        "\nAda\n Grace\nalan"
    );
    assert_eq!(
        run("SELECT p.name, t.name FROM People p JOIN Teams t ON p.team = t.id WHERE t.id = 1"),
        "Ada|Engines\n Grace|Engines"
    );
    assert_eq!(
        run("SELECT t.name, COUNT(p.ROWID) FROM Teams t LEFT JOIN People p ON p.team = t.id GROUP BY t.name"),
        "Codes|1\nEngines|2"
    );
    assert_eq!(run("SELECT ROWID FROM People WHERE born IS NULL"), "5");
    assert_eq!(
        run("SELECT name FROM People WHERE born = '12/10/1815'"),
        "Ada"
    );
    assert_eq!(
        run("SELECT id FROM People ORDER BY id DESC OFFSET 1 ROWS FETCH FIRST 2 ROWS ONLY"),
        "3\n2"
    );
    assert_eq!(
        run("SELECT name FROM Teams UNION SELECT name FROM Teams"),
        "Codes\nEngines"
    );
    assert_eq!(
        run("SELECT 1/3, -0.25, 1e9, DATE '2014-02-17', TIME '13:45:10.5' FROM Teams WHERE id = 1"),
        ".3333333333333333|-.25|1.0E+9|2014-02-17|13:45:10.5"
    );
    assert_eq!(
        run("SELECT TableName, TableId FROM FileMaker_Tables"),
        "People|1065089\nTeams|1065090"
    );
}

#[test]
fn a_source_that_asks_to_stop_ends_a_long_query() {
    // 4^5 rows: the source is asked every 256 rows.
    let sql = "SELECT COUNT(*) FROM People a, People b, People c, People d, People e";
    let mut source = memory();
    assert_eq!(
        om_sql::execute(&mut source, sql, "", "", &[]),
        Ok("1024".into())
    );
    source.stop = true;
    let error = om_sql::execute(&mut source, sql, "", "", &[]).unwrap_err();
    assert_eq!(error, SqlError::Engine(1));
    assert_eq!(error.message(), " (1): Operation aborted");
    assert_eq!(error.calc_error().code(), 1);
}

#[test]
fn joins_hold_only_the_rows_they_keep() {
    // 4^6 = 4,096 combinations, of which the WHERE keeps 4: the product is
    // filtered row by row, never held whole, so 4 rows' worth of memory is
    // enough. CROSS JOIN chains are part of it.
    let mut source = memory();
    source.budget = Some(4 * (24 + 16 * 6));
    let sql = "SELECT COUNT(*) FROM People a, People b, People c, People d CROSS JOIN People e \
               CROSS JOIN People f WHERE a.id = b.id AND b.id = c.id AND c.id = d.id \
               AND d.id = e.id AND e.id = f.id";
    assert_eq!(
        om_sql::execute(&mut source, sql, "", "", &[]),
        Ok("4".into())
    );
    // Results larger than the budget end the query with error 2 ("Memory
    // allocation failure") rather than exhaust the server's memory: all
    // 4,096 rows of an unfiltered product, or the 16 of a join the budget
    // cannot hold.
    let error = om_sql::execute(
        &mut source,
        "SELECT COUNT(*) FROM People a, People b, People c, People d, People e, People f",
        "",
        "",
        &[],
    )
    .unwrap_err();
    assert_eq!(error, SqlError::Engine(2));
    assert_eq!(error.message(), " (2): Memory allocation failure");
    assert_eq!(error.calc_error().code(), 2);
    source.budget = Some(10 * (24 + 16 * 2));
    let error = om_sql::execute(
        &mut source,
        "SELECT a.name FROM People a CROSS JOIN People b",
        "",
        "",
        &[],
    )
    .unwrap_err();
    assert_eq!(error, SqlError::Engine(2));
    // The default holds them.
    source.budget = None;
    assert_eq!(
        om_sql::execute(
            &mut source,
            "SELECT COUNT(*) FROM People a, People b, People c, People d, People e, People f",
            "",
            "",
            &[],
        ),
        Ok("4096".into())
    );
}

#[test]
fn empty_is_null() {
    assert_eq!(run("SELECT COUNT(*) FROM People WHERE '' IS NULL"), "4");
    assert_eq!(run("SELECT COUNT(*) FROM People WHERE name = ''"), "0");
    assert_eq!(
        run("SELECT COALESCE(name, 'none') FROM People WHERE id = 4"),
        "none"
    );
}

#[test]
fn errors_name_their_code_and_position() {
    assert_eq!(
        run("SELECT x FROM Nope"),
        "? ERROR: FQL0002/(1:14): The table named \"Nope\" does not exist."
    );
    assert_eq!(run("SELECT nope FROM People"), "? ERROR: FQL0007/(1:7): The column named \"nope\" does not exist in any table in the column reference's scope.");
    assert_eq!(
        run("SELECT name FROM People WHERE name = 1"),
        "? ERROR: FQL0018/(1:35): An expression contains incompatible data types."
    );
    assert_eq!(
        run("SELEC 1"),
        "? ERROR: FQL0001/(1:1): There is an error in the syntax of the query."
    );
    assert_eq!(
        run("SELECT 1/0 FROM Teams"),
        "? ERROR:  (15): Divide by zero"
    );
    // Statements that would change the file are checked, then refused.
    assert_eq!(
        run("DELETE FROM People"),
        "? ERROR:  (3): Unavailable command"
    );
    assert_eq!(run("DELETE FROM FileMaker_Tables"), "? ERROR: FQL0048/(1:12): This statement contains an invalid operation on FileMaker system table \"FileMaker_Tables\".");
    assert_eq!(run("SELECT COUNT(*) FROM People"), "4");
}

#[test]
fn separators_and_parameters() {
    let mut m = memory();
    let rows = "SELECT id, name FROM People WHERE id <= 2";
    assert_eq!(
        om_sql::execute(&mut m, rows, "", "", &[]).unwrap(),
        "1,Ada\r2,alan"
    );
    let args = [Value::text("2abc")];
    assert_eq!(
        om_sql::execute(
            &mut m,
            "SELECT name FROM People WHERE id = ?",
            "",
            "",
            &args
        )
        .unwrap(),
        "alan"
    );
    let args = [date(2014, 2, 17), Value::Number(Number::from_i64(3))];
    assert_eq!(
        om_sql::execute(
            &mut m,
            "SELECT ?, ? + 1 FROM Teams WHERE id = 1",
            "|",
            "",
            &args
        )
        .unwrap(),
        "2/17/2014|4"
    );
}

/// Statements nested just within `parse::MAX_DEPTH` run on a thread with
/// the 2 MiB stack OpenMaker Server runs scripts on (#296). Building,
/// planning and running them recurse over their tree with large frames (in
/// a debug build, over 20 KB for each operator of a chain), and move to the
/// heap when the thread's stack runs low.
#[test]
fn statements_nested_up_to_the_limit_run_on_a_server_thread() {
    let max = om_sql::parse::MAX_DEPTH;
    let nested = |open: &str, inner: &str, close: &str, n: usize| {
        format!("{}{inner}{}", open.repeat(n), close.repeat(n))
    };
    // A level of each costs about 1, 5 and 20 of the limit's levels.
    let operators = max - 50;
    let parentheses = max / 5 - 10;
    let subqueries = max / 20 - 4;
    std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || {
            assert_eq!(
                run(&format!("SELECT 1{} FROM Teams", "+1".repeat(operators))),
                format!("{0}\n{0}", operators + 1)
            );
            assert_eq!(
                run(&format!(
                    "SELECT name FROM Teams WHERE {}",
                    nested("(", "id = 1", ")", parentheses)
                )),
                "Engines"
            );
            assert_eq!(
                run(&format!(
                    "SELECT name FROM Teams WHERE {}",
                    // `id = 1 AND` runs each subquery once, not once per row.
                    nested(
                        "id = 1 AND id IN (SELECT id FROM Teams WHERE ",
                        "id = 1",
                        ")",
                        subqueries
                    )
                )),
                "Engines"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

/// A calculation host whose file is [`Memory`].
struct Host {
    calc: NullHost,
    file: Memory,
}

impl CalcHost for Host {
    fn locale(&self) -> &Locale {
        &self.calc.locale
    }

    fn today(&self) -> Date {
        self.calc.today
    }

    fn variables(&mut self, scope: VarScope) -> &mut Variables {
        self.calc.variables(scope)
    }

    fn call_reporting(
        &mut self,
        f: HostFunction<'_>,
        args: &[Value],
        cx: &Context,
    ) -> (Value, Option<CalcError>) {
        if let Some(r) = om_sql::calc::host_call(&mut self.file, f, args) {
            return r;
        }
        match self.call(f, args, cx) {
            Ok(v) => (v, None),
            Err(e) => (Value::invalid(), Some(e)),
        }
    }
}

#[test]
fn execute_sql_in_calculations() {
    let mut host = Host {
        calc: NullHost::new(Locale::en_us(), Date::from_ymd(2026, 10, 28).unwrap()),
        file: memory(),
    };
    let mut eval = |text: &str| {
        let r = eval::evaluate_text(text, &mut host, &Context::new());
        (
            r.value
                .to_text(&om_calc::Ctx::new(
                    &Locale::en_us(),
                    Date::from_ymd(2026, 10, 28).unwrap(),
                ))
                .into_string(),
            r.code(),
        )
    };
    assert_eq!(
        eval("ExecuteSQL ( \"SELECT name FROM Teams\" ; \"\" ; \"\" )"),
        ("Engines\rCodes".into(), 0)
    );
    assert_eq!(
        eval("ExecuteSQL ( \"SELECT nope FROM Teams\" ; \"\" ; \"\" )"),
        ("?".into(), 8309)
    );
    let (text, code) = eval("ExecuteSQLe ( \"SELECT nope FROM Teams\" ; \"\" ; \"\" )");
    assert!(text.starts_with("? ERROR: FQL0007/(1:7): "), "{text}");
    assert_eq!(code, 8309);
    assert_eq!(
        eval("ExecuteSQL ( \"SELECT name FROM Teams WHERE id = ?\" ; \"\" ; \"\" ; 2 )"),
        ("Codes".into(), 0)
    );
    assert_eq!(
        eval("ExecuteSQL ( \"SELECT ? FROM Teams WHERE id = 1\" ; \"\" ; \"\" ; 1/0 )"),
        ("?".into(), 15)
    );
    assert_eq!(
        eval("IsEmpty ( ExecuteSQL ( \"SELECT name FROM Teams WHERE id = 9\" ; \"\" ; \"\" ) )"),
        ("1".into(), 0)
    );
}
