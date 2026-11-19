//! Release throughput probe for ordinary numeric UniqueValues lists.
//!
//! Run identically on the baseline and changed revision:
//! cargo run --release -p om-calc --example unique_values_perf -- 10000 5
//! Arguments: maximum list length (default 10000), rounds (default 5).
//! Input construction and correctness checks are outside the timed region.
use om_calc::functions::{lookup, Implementation};
use om_calc::{Ctx, Date, Locale, Value};
use std::hint::black_box;
use std::time::Instant;

fn main() {
    let mut cli = std::env::args().skip(1);
    let max_n: usize = cli.next().map_or(10000, |s| s.parse().unwrap());
    let rounds: usize = cli.next().map_or(5, |s| s.parse().unwrap());
    let locale = Locale::en_us();
    let cx = Ctx::new(&locale, Date::from_ymd(2026, 10, 29).unwrap());
    let Implementation::Pure(unique) = lookup("UniqueValues").unwrap().implementation else {
        unreachable!()
    };
    println!("case,n,round,repetitions,ms_per_call");
    for case in ["unique", "duplicates"] {
        for n in [100, 1000, 5000, 10000].into_iter().filter(|n| *n <= max_n) {
            // 7919 is coprime to every size, producing a permutation. The
            // duplicate control cycles over ten values with the same n.
            let input = (0..n)
                .map(|i| {
                    let value = if case == "duplicates" {
                        i % 10
                    } else {
                        (i * 7919) % n
                    };
                    value.to_string()
                })
                .collect::<Vec<_>>()
                .join("\r");
            let expected = if case == "unique" {
                format!("{input}\r")
            } else {
                "0\r1\r2\r3\r4\r5\r6\r7\r8\r9\r".to_string()
            };
            let args = [Value::text(input), Value::number(2)];
            assert_eq!(unique(&args, &cx).unwrap().to_text(&cx).as_str(), expected);
            black_box(unique(&args, &cx).unwrap());
            let repetitions = if n < 1000 {
                100
            } else if n < 5000 {
                10
            } else {
                1
            };
            for round in 0..rounds {
                let start = Instant::now();
                for _ in 0..repetitions {
                    black_box(unique(black_box(&args), black_box(&cx)).unwrap());
                }
                let ms = start.elapsed().as_secs_f64() * 1000.0 / repetitions as f64;
                println!("{case},{n},{round},{repetitions},{ms:.6}");
            }
        }
    }
}
