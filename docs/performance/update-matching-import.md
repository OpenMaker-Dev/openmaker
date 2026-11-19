# Update-matching import work

Baseline: `d013c9e16fbfc10ac9095c89fae21494172815b7`, measured 2026-10-29.
An update-matching import built update writes before checking whether a
source row matched a target. Unmatched rows discarded those writes, or
built a second set of create writes when "add remaining" was enabled.
Moving update-write construction below the unmatched branch eliminates
unused cloning, repetition padding and stored-value conversion.

The matching keys, target calls, their order and error reporting are
unchanged. The optimization only benefits unmatched rows.

## Reproduction

```sh
cargo build --release -p om-transfer --example update_matching --locked
taskset -c 7 target/release/examples/update_matching bench
target/release/examples/update_matching verify > writes.txt
cargo test -p om-transfer --all-targets --locked
```

Build the identical example against the baseline and changed library,
save both executables and compare their `verify` output with `cmp`.
For one benchmark scenario, append a filter such as `bench false:0` or
`bench true:25` (add-remaining flag and percentage of matched source rows).

The workload has 20,000 already-parsed rows, 24 text columns and 1,000
existing target records, with two target records per match key. Setup is
outside the timer. The timer includes `import_rows`, found-set construction
and destruction of its owned source. A lightweight host observes write
slices with `black_box`; file parsing and backend persistence are excluded.

Both versions were built with Cargo's default release profile and Rust
1.99.1 on an 8-vCPU x86_64 KVM VM, then pinned to CPU 7. Two rounds ran in
changed/baseline/baseline/changed order, with one warmup and eight measured
samples per case and process. Each table cell is the median of 16 samples.
The VM is shared and the CPU is not exclusive.

| Add remaining | Matched | Before ms | After ms | Less engine time |
| --- | ---: | ---: | ---: | ---: |
| No | 0% | 68.385 | 35.329 | 48.3% |
| No | 25% | 69.120 | 46.692 | 32.4% |
| No | 100% | 70.030 | 67.867 | Control |
| Yes | 0% | 100.860 | 71.007 | 29.6% |
| Yes | 25% | 95.296 | 69.857 | 26.7% |
| Yes | 100% | 69.039 | 68.437 | Control |

[Raw samples](update-matching-import.csv) retain every measurement.
Small all-matched control differences are not claimed as improvements.
These are synthetic engine timings, not end-user or production latency.

## Correctness

The complete 7,865,773-byte verification output is identical before and
after (SHA256 `fa4a76887c79fceb3f9bc26fb9a9b8868d30e0ddf1479abb99d2d3d86239175d`).
It covers complete write traces, summaries and found sets for 0%, 25% and
100% matching with add-remaining on and off, duplicate target keys,
repeated source keys, new keys repeated within the import, date completion,
repetitions and padding, missing cells, ordinary validation errors,
source-row numbering and auto-enter flags.

Two example tests assert those ordering and conversion contracts. All 51
om-transfer tests, including the example tests, pass. The two loopback PSOS
transfer tests also pass. Clippy for all om-transfer targets with warnings
denied, workspace formatting and diff checks pass. Security review and live
service testing are outside this work.
