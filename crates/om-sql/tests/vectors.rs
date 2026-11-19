//! Every row of `docs/re/catalogs/execute-sql-vectors.tsv` against the
//! sample files it names (FileMaker Server 26's own answers). The files are
//! not in the repository:
//!
//! ```sh
//! OM_SQL_RE=/path/to/openmaker/re cargo test -p om-sql --features fmp12 --test vectors -- --ignored --nocapture
//! ```

#[path = "../examples/sql_vectors.rs"]
mod sql_vectors;

/// Rows om-sql answers differently, with the reason
/// (execute-sql.md, Implementation notes).
fn known_deviation(o: &sql_vectors::Outcome) -> Option<&'static str> {
    let e = &o.row.expression;
    (o.row.battery == "q13-collation" && (e.contains("'ı'") || e.contains("'Œ'")))
        .then_some("text order of U+0131 and U+0152 is not UTF-16 order (open)")
}

#[test]
#[ignore = "needs the FileMaker Server sample, the MBS examples and the Contacts templates"]
fn every_vector() {
    let outcomes = sql_vectors::run();
    let report = sql_vectors::report(&outcomes);
    let mut unexplained = Vec::new();
    for o in &outcomes {
        if o.got.is_some() && !o.passed() && known_deviation(o).is_none() {
            unexplained.push(o);
        }
    }
    for (battery, (rows, covered, passed)) in &report {
        println!("{battery:<18} {rows:>4} {covered:>8} {passed:>7}");
    }
    for o in &unexplained {
        let Some((text, code)) = &o.got else {
            continue;
        };
        println!(
            "line {}: {}\n  expected {:?} ({})\n  got      {:?} ({})",
            o.row.line, o.row.expression, o.row.result, o.row.error, text, code
        );
    }
    let covered: usize = report.values().map(|(_, c, _)| c).sum();
    assert!(covered > 1600, "sample files not found: {covered} rows ran");
    assert!(unexplained.is_empty(), "{} rows differ", unexplained.len());
}

/// The rows that keep a sample file's own long content only as its size,
/// digest and start (third-party text) still name a value `passed` can
/// check, and only that value passes.
#[test]
fn elided_results_are_checked_by_size_digest_and_start() {
    let rows: Vec<_> = sql_vectors::rows()
        .into_iter()
        .filter(|row| row.result.starts_with("[elided: "))
        .collect();
    assert_eq!(rows.len(), 2);
    for row in &rows {
        let kept = sql_vectors::elided(&row.result).expect("a well-formed stand-in");
        assert!(
            kept.bytes > 2048 && kept.sha256.len() == 64,
            "{}",
            row.result
        );
        assert!(!kept.matches(kept.starts), "{}", row.result);
    }
    let abc = sql_vectors::Elided {
        bytes: 3,
        sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        starts: "a",
    };
    assert!(abc.matches("abc") && !abc.matches("abd") && !abc.matches("abcd"));
}
