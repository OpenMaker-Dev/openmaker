# Value-list record sorting

Baseline: PR #2 at `d013c9e16fbfc10ac9095c89fae21494172815b7`.
Measured 2026-10-29, Linux x86_64, Rust 1.99.1, default release profile,
8-vCPU shared KVM VM. Final runs were pinned to CPU 7, without exclusive
CPU ownership.

The hosted provider calls `om_data::found::sort_rows` to order records.
For each record and custom-order column, it previously rebuilt collation
keys while scanning the value list for the first matching entry. This
repeated list normalization and membership search for every record.

The change builds one map of normalized keys to ranks per sort column.
The first collation-equivalent entry wins, as with the old linear search.
The map lives only for this call, so changed lists are reflected immediately.
Rank preparation and lookup become expected O(M + N) for M list entries
and N records, plus normalization cost, instead of O(N*M). Sorting the
records still costs O(N log N). The maps add O(M) temporary storage per
column. Empty and single-record inputs return before building keys.

## Benchmark

`crates/om-data/examples/bench_value_list_sort.rs` measures the production
public sorter, including rank preparation, value-key construction and
sorting. Every run checks the complete record order against an independent
numeric-rank result. Inputs include 0, 1, 1,000 and 10,000 records and
no custom list or lists of 4, 32 and 256 values. The four-entry case has
many values absent from the list; the other custom lists match every row.

```sh
cargo run --release -p om-data --example bench_value_list_sort
```

The identical example was compiled against the baseline source and the
changed source. Each case has one warmup and five measured runs per process.
Two processes per version ran in changed/baseline/baseline/changed order.
The table reports the median of ten samples. Raw samples are in
[value-list-sort.csv](value-list-sort.csv).

| Records | List entries | Before ms | After ms | Speedup |
| ---: | ---: | ---: | ---: | ---: |
| 10,000 | None | 7.776 | 7.674 | Control |
| 10,000 | 4 | 34.506 | 13.423 | 2.6x |
| 10,000 | 32 | 110.056 | 13.290 | 8.3x |
| 10,000 | 256 | 766.664 | 14.172 | 54.1x |

These are synthetic in-memory sort measurements, not network download or
production latency. Scheduling noise remains possible on this shared VM;
small control differences are not claimed as improvements.

## Validation

Regression cases cover first-match ranks for case/accent-equivalent list
entries, trimming, stable ties, unknown and empty values, descending order,
blanks last, changed lists, empty lists and ordinary sorting without a list.
Existing language, multi-column and summary ordering tests also pass.

`cargo test -p om-data --locked` passes 82 tests, with two artifact-dependent
tests ignored. The six hosted-provider finds/transactions tests also pass.
Clippy for every om-data target with warnings denied, workspace formatting
and diff checks pass. No security review or live service testing is included.
