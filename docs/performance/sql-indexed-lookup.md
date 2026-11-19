# Indexed SQL candidate intersection

Measured on 2026-10-29 while reviewing PR #2, against baseline
`d013c9e16fbfc10ac9095c89fae21494172815b7`.

`Exec::found_sets` used `Vec::contains` to intersect each table record with
the ids returned by `SqlSource::lookup`. With N table records and M index
candidates this costs O(N*M), before the SQL predicate is evaluated. An
index on a common text value can consequently make its query slower than
scanning the table.

This change sorts the already owned candidate vector in place and uses
binary search for membership: O(M log M + N log M), without another
candidate-sized allocation. Iteration still follows the table's creation
order. Duplicate and absent index ids do not add rows, and the predicate
still rejects false positives. A missing index still uses the scan path.

## Measurement

The same `bench_indexed_lookup` example was built against the baseline and
the changed executor with `cargo build --release -p om-sql --example
bench_indexed_lookup --locked`. The two executables were saved separately.
The source implements the public `SqlSource` API, with constant-time value
access and an optional index. Each query runs through parsing, binding,
execution and result formatting:

```sql
SELECT ROWID FROM Items WHERE Bucket = 'match'
```

Each case warms the parser once, then measures five runs, checking the
entire output of every run against the expected record ids in order.
Index candidates arrive in reverse order with an extra duplicate, an
absent id and (when applicable) a false positive. Runs cover 10,000,
30,000 and 100,000 records, with 100%, 50%, 1% and one matching record,
both with and without an index. The example accepts a row count as its
first argument. Run it with:

```sh
cargo run --release -p om-sql --example bench_indexed_lookup
```

The confirmation run alternated the changed and baseline executables at
each size. Environment: Linux x86_64, 8-vCPU KVM VM, QEMU Virtual CPU 2.5+,
Rust 1.99.1, default release profile. Raw samples, including unchanged scan
controls, are in [sql-indexed-lookup.csv](sql-indexed-lookup.csv).

Median full-query time, milliseconds:

| Records | Matching | Baseline | Changed | Speedup |
| ---: | ---: | ---: | ---: | ---: |
| 10,000 | 50% | 8.699 | 3.618 | 2.4x |
| 30,000 | 50% | 73.746 | 9.381 | 7.9x |
| 100,000 | 50% | 766.842 | 33.486 | 22.9x |
| 100,000 | 100% | 1,037.938 | 74.929 | 13.9x |
| 100,000 | 1% | 17.355 | 1.776 | 9.8x |
| 100,000 | One record | 0.425 | 0.374 | Near timing floor |

The 100,000-record, 50% matching scan control was 40.144 ms before and
38.781 ms after. This is a shared VM, so small differences and submillisecond
results are not evidence of a meaningful speedup. The large gains concern
this in-memory query workload; they are not measurements of network,
disk, FileMaker Server, or production performance. The scan still visits
every table record and dense result formatting still costs time.

## Correctness

`indexed_candidates_preserve_order_and_recheck_predicates` uses
nonmonotonic creation order, unordered/duplicate/absent/false-positive
index ids, successive WHERE conjuncts, an empty index result and no index.
The benchmark independently checks complete output for every timed run.

The SQL test suite and host-level SQL/PSOS tests also exercise the change.
The artifact-dependent FileMaker vector test and SQL doctest remain
ignored; no compatibility claim is inferred from those skipped tests.
