//! End-to-end query benchmark for index candidate intersection.
//! Run with `cargo run --release -p om-sql --example bench_indexed_lookup`.
//! The source returns candidate ids in reverse order, with a duplicate and
//! an absent id. Results must retain table order and recheck the predicate.

use std::hint::black_box;
use std::time::Instant;

use om_calc::{CalcError, Date, Locale, Time, TimeStyle, Timestamp, Value};
use om_sql::source::Cursor;
use om_sql::{FieldClass, FieldDef, FieldType, SqlError, SqlSource, TableDef};

struct Indexed {
    locale: Locale,
    rows: u64,
    stride: u64,
    indexed: bool,
}

impl SqlSource for Indexed {
    fn locale(&self) -> &Locale {
        &self.locale
    }

    fn now(&self) -> Timestamp {
        Timestamp::from_date_time(
            Date::from_ymd(2026, 10, 29).unwrap(),
            Time::from_micros(0, TimeStyle::Elapsed),
        )
        .unwrap()
    }

    fn tables(&self) -> Vec<TableDef> {
        vec![TableDef {
            id: 1,
            name: "Items".into(),
            table_id: 1,
            base: Some(("Items".into(), "benchmark".into())),
            mod_count: 1,
        }]
    }

    fn fields(&self, _: &TableDef) -> Vec<FieldDef> {
        vec![FieldDef {
            id: 1,
            name: "Bucket".into(),
            ty: FieldType::Text,
            class: FieldClass::Normal,
            global: false,
            repetitions: 1,
            mod_count: 1,
        }]
    }

    fn records(&mut self, _: &TableDef) -> Result<Vec<u64>, SqlError> {
        Ok((1..=self.rows).collect())
    }

    fn value(
        &mut self,
        _: &TableDef,
        record: u64,
        _: &FieldDef,
        _: u32,
        _: Cursor,
    ) -> Result<Value, CalcError> {
        Ok(Value::text(if record % self.stride == 0 {
            "match"
        } else {
            "other"
        }))
    }

    fn lookup(
        &mut self,
        _: &TableDef,
        _: &FieldDef,
        _: &Value,
    ) -> Result<Option<Vec<u64>>, SqlError> {
        Ok(self.indexed.then(|| {
            let mut ids: Vec<_> = (1..=self.rows)
                .rev()
                .filter(|id| id % self.stride == 0)
                .collect();
            ids.extend([1, self.rows + 1, self.stride]);
            ids
        }))
    }
}

fn main() {
    let sizes: Vec<u64> = match std::env::args().nth(1) {
        Some(n) => vec![n.parse().expect("row count")],
        None => vec![10_000, 30_000, 100_000],
    };
    let query = "SELECT ROWID FROM Items WHERE Bucket = 'match'";
    println!("rows,stride,indexed,median_ms,samples_ms");
    for rows in sizes {
        for stride in [1, 2, 100, rows] {
            for indexed in [false, true] {
                let mut source = Indexed {
                    locale: Locale::en_us(),
                    rows,
                    stride,
                    indexed,
                };
                let expected = (1..=rows)
                    .filter(|id| id % stride == 0)
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>()
                    .join("\n");
                // Warm the parser and validate the complete result, not
                // only its length; every measured run is checked too.
                assert_eq!(
                    om_sql::execute(&mut source, query, "", "\n", &[]).unwrap(),
                    expected
                );
                let mut samples = Vec::new();
                for _ in 0..5 {
                    let start = Instant::now();
                    let result =
                        om_sql::execute(&mut source, black_box(query), "", "\n", &[]).unwrap();
                    samples.push(start.elapsed().as_secs_f64() * 1000.0);
                    assert_eq!(result, expected);
                }
                let mut sorted = samples.clone();
                sorted.sort_by(f64::total_cmp);
                println!("{rows},{stride},{indexed},{:.3},{samples:?}", sorted[2]);
            }
        }
    }
}
