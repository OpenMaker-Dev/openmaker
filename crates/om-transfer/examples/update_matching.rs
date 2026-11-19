//! Benchmark ordinary update-matching imports. Run with:
//! `cargo run --release -p om-transfer --example update_matching`
//! Pass `verify` to serialize complete write traces for before/after comparison.
//! Fixtures and host construction are outside the timer. The timer includes
//! import_rows and its owned source destruction; the host only observes writes.
use om_calc::Locale;
use om_transfer::import::{
    import_rows, FieldWrite, ImportRequest, ImportSummary, ImportTarget, Source, TargetField,
};
use om_transfer::spec::{
    import_flag, ImportAction, ImportOptions, Key, MapEntry, Mapping, Profile,
};
use om_transfer::{FieldKind, FieldType, FileType, ImportCharset, RecordId, Result, TransferError};
use std::hint::black_box;
use std::time::Instant;

type WriteTrace = Vec<(Option<RecordId>, bool, Vec<FieldWrite>)>;

#[derive(Debug, Default)]
struct Target {
    existing: usize,
    columns: usize,
    created: u64,
    trace: Option<WriteTrace>,
    found: Vec<RecordId>,
    dates: bool,
    reject: bool,
}
impl ImportTarget for Target {
    fn fields(&mut self, _: u64) -> Result<Vec<TargetField>> {
        Ok((1..=self.columns as u64)
            .map(|id| TargetField {
                id,
                name: format!("Field{id}"),
                ty: if self.dates && id == 3 {
                    FieldType::Date
                } else {
                    FieldType::Text
                },
                kind: FieldKind::Normal,
                repetitions: if self.dates && id == 4 { 3 } else { 1 },
            })
            .collect())
    }
    fn found_set(&mut self, _: u64) -> Result<Vec<RecordId>> {
        Ok((0..self.existing as u64).collect())
    }
    fn value(&mut self, _: u64, record: RecordId, _: u64) -> Result<String> {
        Ok(format!("key{}", record / 2))
    }
    fn create(&mut self, _: u64, writes: &[FieldWrite], auto: bool) -> Result<RecordId> {
        black_box(writes);
        if let Some(trace) = &mut self.trace {
            trace.push((None, auto, writes.to_vec()));
        }
        if self.reject && writes.iter().any(|w| w.text == "reject") {
            return Err(TransferError::new(509, "Rejected"));
        }
        self.created += 1;
        Ok(self.existing as u64 + self.created)
    }
    fn update(
        &mut self,
        _: u64,
        record: RecordId,
        writes: &[FieldWrite],
        auto: bool,
    ) -> Result<()> {
        black_box(writes);
        if let Some(trace) = &mut self.trace {
            trace.push((Some(record), auto, writes.to_vec()));
        }
        if self.reject && writes.iter().any(|w| w.text == "reject") {
            return Err(TransferError::new(509, "Rejected"));
        }
        Ok(())
    }
    fn set_found_set(&mut self, _: u64, records: &[RecordId]) -> Result<()> {
        self.found = records.to_vec();
        Ok(())
    }
}
fn request(columns: usize, add: bool) -> ImportRequest {
    ImportRequest {
        options: ImportOptions {
            table: Key::from_id(1),
            action: ImportAction::UpdateMatching,
            flags: import_flag::AUTO_ENTER | if add { import_flag::ADD_REMAINING } else { 0 },
            charset: ImportCharset::Utf8,
            map: (1..=columns as u64)
                .map(|id| MapEntry {
                    field: Key::from_id(id),
                    mapping: if id == 1 {
                        Mapping::Match
                    } else {
                        Mapping::Import
                    },
                })
                .collect(),
        },
        profile: Profile::new(FileType::TabSeparated),
        profile_ex: None,
        field_flags: None,
    }
}
fn source(rows: usize, columns: usize, match_percent: usize) -> Source {
    Source {
        names: None,
        rows: (0..rows)
            .map(|r| {
                (0..columns)
                    .map(|c| {
                        vec![if c == 0 {
                            format!(
                                "key{}",
                                if r % 100 < match_percent {
                                    r % 500
                                } else {
                                    r + 1000
                                }
                            )
                        } else {
                            format!("Order {r}, column {c}: ordinary text")
                        }]
                    })
                    .collect()
            })
            .collect(),
    }
}
fn verify() {
    for add in [false, true] {
        for p in [0, 25, 100] {
            let mut t = Target {
                existing: 1000,
                columns: 12,
                trace: Some(Vec::new()),
                ..Default::default()
            };
            let result = import_rows(
                &mut t,
                &request(12, add),
                source(1200, 12, p),
                2,
                &Locale::default(),
            );
            println!("add={add} matches={p}\n{result:?}\n{t:?}");
        }
        let mut t = Target {
            existing: 4,
            columns: 5,
            trace: Some(Vec::new()),
            dates: true,
            reject: true,
            ..Default::default()
        };
        let cell = |s: &str| vec![s.to_owned()];
        let s = Source {
            names: None,
            rows: vec![
                vec![
                    cell("key0"),
                    cell("updated"),
                    cell("1/1/24"),
                    vec!["first".into(), "second".into()],
                ],
                vec![cell("new"), cell("added"), cell("2/2/2024"), vec![]],
                vec![cell("key0"), cell("again")],
                vec![cell("new"), cell("second new")],
                vec![cell("key1"), cell("reject")],
                vec![cell("fresh"), cell("reject")],
                vec![vec![]],
            ],
        };
        let result = import_rows(&mut t, &request(5, add), s, 7, &Locale::default());
        println!("edges add={add}\n{result:?}\n{t:?}");
    }
}
fn bench() {
    let args: Vec<String> = std::env::args().collect();
    let filter = args.get(2).map(String::as_str);
    for add in [false, true] {
        for p in [0, 25, 100] {
            let name = format!("{add}:{p}");
            if filter.is_some_and(|f| f != name) {
                continue;
            }
            let mut times = Vec::new();
            for run in 0..9 {
                let s = source(20000, 24, p);
                let mut t = Target {
                    existing: 1000,
                    columns: 24,
                    ..Default::default()
                };
                let req = request(24, add);
                let now = Instant::now();
                let result: ImportSummary =
                    import_rows(&mut t, &req, s, 1, &Locale::default()).unwrap();
                let elapsed = now.elapsed().as_secs_f64() * 1000.0;
                black_box(result);
                if run > 0 {
                    times.push(elapsed);
                }
            }
            println!("add={add} match_percent={p} milliseconds={times:?}");
        }
    }
}
fn main() {
    if std::env::args().nth(1).as_deref() == Some("verify") {
        verify()
    } else {
        bench()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_preserves_duplicate_updates_and_new_row_order() {
        for add in [false, true] {
            let mut target = Target {
                existing: 4,
                columns: 3,
                trace: Some(Vec::new()),
                ..Default::default()
            };
            let source = Source {
                names: None,
                rows: [
                    ["key0", "first", "a"],
                    ["new", "new-first", "b"],
                    ["key0", "last", "c"],
                    ["new", "new-second", "d"],
                ]
                .into_iter()
                .map(|row| row.into_iter().map(|s| vec![s.into()]).collect())
                .collect(),
            };
            let summary =
                import_rows(&mut target, &request(3, add), source, 1, &Locale::default()).unwrap();
            assert_eq!(summary.total, 4);
            assert_eq!(summary.updated, 4);
            assert_eq!(summary.added, if add { 2 } else { 0 });
            assert!(summary.errors.is_empty());
            assert_eq!(
                summary.found_set,
                if add { vec![0, 1, 5, 6] } else { vec![0, 1] }
            );
            assert_eq!(target.found, summary.found_set);
            let trace = target.trace.unwrap();
            assert_eq!(trace.len(), if add { 6 } else { 4 });
            for (record, auto, writes) in &trace {
                assert!(*auto);
                assert_eq!(writes.len(), if record.is_some() { 2 } else { 3 });
                assert_eq!(writes[0].field, if record.is_some() { 2 } else { 1 });
            }
            let update_text: Vec<_> = trace
                .iter()
                .filter(|(r, _, _)| r.is_some())
                .map(|(_, _, w)| w[0].text.as_str())
                .collect();
            assert_eq!(update_text, ["first", "first", "last", "last"]);
            let new_text: Vec<_> = trace
                .iter()
                .filter(|(r, _, _)| r.is_none())
                .map(|(_, _, w)| w[1].text.as_str())
                .collect();
            assert_eq!(
                new_text,
                if add {
                    vec!["new-first", "new-second"]
                } else {
                    vec![]
                }
            );
        }
    }

    #[test]
    fn matching_retains_dates_repetitions_missing_fields_and_validation_rows() {
        for add in [false, true] {
            let mut target = Target {
                existing: 4,
                columns: 5,
                dates: true,
                reject: true,
                trace: Some(Vec::new()),
                ..Default::default()
            };
            let cell = |s: &str| vec![s.to_owned()];
            let source = Source {
                names: None,
                rows: vec![
                    vec![
                        cell("key0"),
                        cell("ok"),
                        cell("1/1/24"),
                        vec!["first".into(), "second".into()],
                        cell(""),
                    ],
                    vec![cell("new"), cell("ok"), cell("2/2/2024"), vec![]],
                    vec![cell("key1"), cell("reject")],
                    vec![cell("fresh"), cell("reject")],
                ],
            };
            let summary =
                import_rows(&mut target, &request(5, add), source, 7, &Locale::en_us()).unwrap();
            assert_eq!(summary.updated, 2);
            assert_eq!(summary.added, usize::from(add));
            let errors: Vec<_> = summary
                .errors
                .iter()
                .map(|e| (e.row, e.code, e.message.as_str()))
                .collect();
            assert_eq!(
                errors,
                if add {
                    vec![
                        (9, 509, "Rejected"),
                        (9, 509, "Rejected"),
                        (10, 509, "Rejected"),
                    ]
                } else {
                    vec![(9, 509, "Rejected"), (9, 509, "Rejected")]
                }
            );
            assert_eq!(
                summary.found_set,
                if add { vec![0, 1, 5] } else { vec![0, 1] }
            );
            let trace = target.trace.unwrap();
            let writes = &trace[0].2;
            assert_eq!(
                writes
                    .iter()
                    .map(|w| (w.field, w.repetition, w.text.as_str()))
                    .collect::<Vec<_>>(),
                vec![
                    (2, 1, "ok"),
                    (3, 1, "01/01/2024"),
                    (4, 1, "first"),
                    (4, 2, "second"),
                    (4, 3, ""),
                    (5, 1, "")
                ]
            );
            if add {
                assert_eq!(
                    trace[2]
                        .2
                        .iter()
                        .map(|w| w.text.as_str())
                        .collect::<Vec<_>>(),
                    ["new", "ok", "2/2/2024", "", "", "", ""]
                );
            }
        }
    }
}
