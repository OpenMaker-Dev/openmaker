//! Statements and FileMaker's syntax error positions (execute-sql.md 3):
//! every FQL0001 of the vector catalog that comes from the lexer or the
//! parser, and the statement forms of section 3.2.

use om_sql::parse::{self, ast};

const VECTORS: &str = include_str!("../../../docs/re/catalogs/execute-sql-vectors.tsv");

fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('r') => out.push('\r'),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some('u') => {
                let hex: String = it.by_ref().take(4).collect();
                out.extend(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32));
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// The statement of an `ExecuteSQL ( "..." ; ...)` calculation: its first
/// text literal (`\"` is a quote, `¶` a CR).
fn statement(calculation: &str) -> Option<String> {
    let start = calculation.find('"')?;
    let mut out = String::new();
    let mut it = calculation[start + 1..].chars();
    while let Some(c) = it.next() {
        match c {
            '\\' => out.push(it.next()?),
            '"' => return Some(out),
            '¶' => out.push('\r'),
            other => out.push(other),
        }
    }
    None
}

#[test]
fn syntax_errors_are_where_filemaker_reports_them() {
    let mut checked = 0;
    for line in VECTORS.lines().skip(1) {
        let cols: Vec<&str> = line.split('\t').collect();
        let expected = unescape(cols[3]);
        let Some(rest) = expected.strip_prefix("? ERROR: FQL0001/(") else {
            continue;
        };
        let want = &rest[..rest.find(')').expect("position")];
        let Some(sql) = statement(&unescape(cols[0])).filter(|_| cols[0].starts_with("ExecuteSQL"))
        else {
            continue;
        };
        // FETCH NEXT is rejected after parsing, at its 0-based column.
        if sql.contains("FETCH NEXT") {
            assert!(parse::parse(&sql).is_ok(), "{sql}");
            continue;
        }
        let got = match parse::parse(&sql) {
            Ok(_) => "no error".to_string(),
            Err(e) => format!("{}:{}", e.pos.line, e.pos.column + 1),
        };
        assert_eq!(got, want, "{sql:?}");
        checked += 1;
    }
    assert!(checked >= 115, "{checked}");
}

#[test]
fn positions_count_code_points_and_lines() {
    let pos = |sql: &str| parse::parse(sql).map(|_| ()).unwrap_err().pos;
    assert_eq!((pos("").line, pos("").column), (1, 0));
    assert_eq!(pos("SELECT '😀😀' x y FROM t").column, 14);
    let p = pos("SELECT x\r\nFROM t WHERE");
    assert_eq!((p.line, p.column), (2, 7));
    // A CR alone starts no line.
    assert_eq!(pos("SELECT x\rFROM t WHERE").line, 1);
}

#[test]
fn statement_forms() {
    let parsed = parse::parse("SELECT DISTINCT a.x AS y, COUNT(*) FROM t a LEFT JOIN u ON a.k = u.k WHERE a.x > 1 GROUP BY a.x HAVING COUNT(*) > 1 ORDER BY 1 DESC OFFSET 1 ROWS FETCH FIRST 2 ROWS WITH TIES").unwrap();
    let ast::Statement::Query(q) = parsed else {
        panic!("a query");
    };
    let ast::QueryTerm::Spec(spec) = &q.body.first else {
        panic!("a query spec");
    };
    assert!(spec.distinct);
    assert!(matches!(
        &spec.from[0],
        ast::TableRef::Join {
            kind: ast::JoinKind::Left,
            ..
        }
    ));
    assert!(spec.group_by.is_some() && spec.having.is_some());
    assert!(q.order_by[0].descending);
    assert!(q.fetch.as_ref().is_some_and(|f| f.with_ties && !f.next));
    for sql in [
        "INSERT INTO t (a, b) VALUES (1, 'x'), (2, 'y')",
        "UPDATE t SET a = 1 WHERE b = 2",
        "DELETE FROM t WHERE CURRENT OF c",
        "CREATE TABLE t (a VARCHAR(10) NOT NULL, b INT DEFAULT 1)",
        "ALTER TABLE t ADD COLUMN c DATE",
        "DROP INDEX ON t.a",
        "DECLARE c CURSOR FOR SELECT a FROM t",
        "FETCH NEXT FROM c",
        "SELECT a FROM t WHERE EXISTS SELECT b FROM u",
        "SELECT CASE x WHEN 1 THEN 'a' WHEN y > 2 THEN 'b' ELSE NULL END FROM t",
        "SELECT GETAS(c, 'JPEG') j FROM t",
        "SELECT a FROM t UNION ALL SELECT b FROM u UNION SELECT c FROM v",
        "SELECT x + INTERVAL 1 DAY, TRIM(LEADING 'x' FROM y), EXTRACT(YEAR FROM d) FROM t",
    ] {
        assert!(parse::parse(sql).is_ok(), "{sql}");
    }
}

/// Text nested deeper than `parse::MAX_DEPTH` is a syntax error where the
/// depth passed the limit, on a thread with the 2 MiB stack OpenMaker
/// Server runs scripts on (#296). Without the limit, 2,000 nested
/// parentheses overflowed the stack while parsing, and an operator chain
/// as deep made a tree whose drop did, aborting the process.
#[test]
fn statements_nested_past_the_limit_are_syntax_errors() {
    let nested = |open: &str, inner: &str, close: &str, n: usize| {
        format!("{}{inner}{}", open.repeat(n), close.repeat(n))
    };
    let statements = [
        format!(
            "SELECT a FROM t WHERE a = 'x' OR {}",
            nested("(", "1=1", ")", 2_000)
        ),
        format!(
            "SELECT a FROM t WHERE {}",
            nested("a IN (SELECT a FROM t WHERE ", "1=1", ")", 500)
        ),
        // No parentheses, but each operator nests the expression's tree.
        format!("SELECT 1{} FROM t", "+1".repeat(100_000)),
    ];
    std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || {
            for sql in &statements {
                let e = parse::parse(sql).map(|_| ()).unwrap_err();
                // At the limit, well inside the nesting, not at its end.
                assert_eq!(e.pos.line, 1);
                assert!((e.pos.column as usize) < sql.len() / 4, "{}", e.pos.column);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
