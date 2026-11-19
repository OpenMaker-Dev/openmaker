# Typed UniqueValues duplicate lookup

`UniqueValues` with numeric, date, time, or timestamp datatypes previously
compared every input against a vector of all distinct values already retained.
A list of n distinct values required n(n-1)/2 typed comparisons. An ordered set
reduces membership work to O(n log u), where u is the number of distinct values.
The output is still appended in input order, keeping the first spelling and
styles, and the existing typed comparator still defines duplicate equality.
Number and moment variants have distinct ranks to make the set's order total;
a single call only produces one of those variants, so user-visible ordering
is unchanged.

This change does not make the entire function O(n log n): text construction
still normalizes accumulated text on each append. That separate cost remains.

## Measurement

Baseline: `d013c9e`, a commit of the project's earlier, private history (not in this
repository). Measured 2026-10-29 with Rust 1.99.1,
`rustc -C opt-level=3`, x86_64 QEMU VM, eight virtual CPUs. The complete
baseline om-calc source was rebuilt in a scratch directory with both its
original text-function module and the changed module. Both implementations
therefore used identical supporting code and release dependencies in one
binary. Existing release dependency artifacts were reused; the existing
om-calc library was not reused.

The process was pinned to CPU 5, without CPU exclusivity. Inputs were prepared
before timing. Each side received one warmup, then five paired rounds with
before/after execution order alternating. Each round averaged 100 calls at
n=100, 10 at n=1,000, and one at n=5,000/10,000. Results were consumed with
`black_box`; output allocation and destruction are included. No builds ran in
this agent while measurements were active, but the shared VM was not idle.
The 10,000-value run had substantial scheduling variation; treat absolute
numbers as local observations rather than production latency estimates.

The distinct workload is the permutation `(i * 7919) % n`, formatted as decimal
numbers separated by CR. The duplicate control is `i % 10` with the same n.
The full text and style runs matched between implementations on all measured
inputs and 396 additional combinations of datatype sign, locale, empty and
invalid values, aliases, long decimals, Unicode, and styles.

Median elapsed milliseconds per call (ratio of medians):

| Input | n | Before | After | Speedup |
| --- | ---: | ---: | ---: | ---: |
| Distinct numbers | 100 | 0.7302 | 0.1510 | 4.84x |
| Distinct numbers | 1,000 | 70.1844 | 3.7089 | 18.92x |
| Distinct numbers | 5,000 | 1,637.9448 | 55.3403 | 29.60x |
| Distinct numbers | 10,000 | 6,991.8312 | 209.3819 | 33.39x |
| Ten repeated numbers | 100 | 0.0813 | 0.0803 | 1.01x |
| Ten repeated numbers | 1,000 | 0.7532 | 0.7674 | 0.98x |
| Ten repeated numbers | 5,000 | 3.9439 | 4.0034 | 0.99x |
| Ten repeated numbers | 10,000 | 7.9232 | 7.6631 | 1.03x |

[Raw paired samples](unique-values-d013c9e.csv) retain every observation,
including scheduling outliers.

## Reproduction

The committed example uses the public function lookup and asserts the expected
output before timing. Build the same example at the baseline and changed
revision, then run the resulting executables with the same CPU affinity and
alternate their order. For a single revision:

```sh
cargo build --release -p om-calc --example unique_values_perf
taskset -c 5 target/release/examples/unique_values_perf 10000 5
```

The arguments are maximum list size and number of rounds. For a short check,
use `1000 3`. Input generation, expected-output assertions and warmup are
outside the measured region. The example prints every sample as CSV.

The focused unit regressions cover retained spelling and input order, empty
and invalid equivalence classes, signed zero and exponent aliases, long exact
decimals, date/time/timestamp aliases, negative datatype selectors, first-value
style retention, separator style inheritance, and German decimal separators.
Existing oracle integration tests remain the broader compatibility check.

## Independent Cargo validation

The parent reviewer also built the identical example against the baseline
and changed crate with `cargo build --release -p om-calc --example
unique_values_perf --locked`, including Cargo-built dependencies. Separate
executables ran on CPU 7 with maximum size 5,000 and three measured rounds.
This confirms the gain through the public function API and normal build.

| Input | n | Before ms | After ms |
| --- | ---: | ---: | ---: |
| Distinct numbers | 100 | 0.6543 | 0.1408 |
| Distinct numbers | 1,000 | 64.8818 | 3.5776 |
| Distinct numbers | 5,000 | 1,589.1587 | 55.2529 |
| Ten repeated numbers | 100 | 0.0784 | 0.0765 |
| Ten repeated numbers | 1,000 | 0.7387 | 0.8138 |
| Ten repeated numbers | 5,000 | 3.7270 | 3.8071 |

[Cargo samples](unique-values-cargo.csv) include every observation. The
5,000-distinct-value improvement is 28.8x. Duplicate-heavy inputs do not
benefit and may incur a little overhead: the 1,000-value confirmation was
about 10% slower, a difference of 0.075 ms per call, versus about 2% slower
in the five-round paired run. These are shared-VM results, not production
latency estimates.

`cargo test -p om-calc --all-targets --locked` passes all 206 tests, including
the tracked calculation vectors and three new regressions. Clippy for all
om-calc targets with warnings denied, workspace formatting and diff checks
also pass. No security review or live service testing was performed.
