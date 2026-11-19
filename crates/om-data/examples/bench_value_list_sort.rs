//! `cargo run --release -p om-data --example bench_value_list_sort`
//! Measures the public sorter used by hosted record sorting. Every run
//! checks the complete record order against an independent numeric rank.

use std::time::Instant;

use om_data::found::{sort_rows, SortField, SortOrder};
use om_data::{Field, FieldKind, Record};

fn main() {
    let fields = [Field {
        name: "Status".into(),
        kind: FieldKind::Text,
    }];
    println!("rows,list_values,median_ms,samples_ms");
    for count in [0usize, 1, 1_000, 10_000] {
        for values in [0usize, 4, 32, 256] {
            let categories = values.max(32);
            let records: Vec<_> = (0..count)
                .map(|i| Record {
                    id: i as i64,
                    values: vec![Some(format!("Status {:04}", (i * 17) % categories))],
                })
                .collect();
            let sort = SortField {
                value_list: (values > 0).then(|| {
                    (0..values)
                        .rev()
                        .map(|i| format!("Status {i:04}"))
                        .collect()
                }),
                ..SortField::new("Status", SortOrder::Ascending)
            };
            let mut expected: Vec<_> = (0..count).collect();
            expected.sort_by_key(|i| {
                let category = (i * 17) % categories;
                if category < values {
                    (0, values - 1 - category)
                } else {
                    (1, category)
                }
            });
            let expected: Vec<_> = expected.into_iter().map(|i| i as i64).collect();
            let mut samples = Vec::new();
            for run in 0..6 {
                let mut rows: Vec<_> = records.iter().collect();
                let start = Instant::now();
                sort_rows(&fields, &mut rows, std::slice::from_ref(&sort));
                let elapsed = start.elapsed().as_secs_f64() * 1000.0;
                assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), expected);
                if run > 0 {
                    samples.push(elapsed);
                }
            }
            let mut sorted = samples.clone();
            sorted.sort_by(f64::total_cmp);
            println!("{count},{values},{:.3},{samples:?}", sorted[2]);
        }
    }
}
