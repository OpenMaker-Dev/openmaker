# HBAM whole-file rebuild benchmark

Run `cargo bench -p om-hbam --bench rebuild`. This uses Cargo's optimized
bench profile and no additional dependencies. The default cases are 1,000,
10,000 and 50,000 records, each with eight 64-byte text cells. The benchmark
checks every record ID and cell before timing, then compares every timed
result against the complete expected file bytes.

Set `HBAM_BENCH_INPUT=/path/to/file.fmp12` to additionally rebuild a local file.
That case verifies the entire decoded tree after rebuilding. Input parsing,
fixture construction, comparisons, file writes and output destruction are
outside the timed region; allocation and encoding inside `writer::rebuild`
are included. Each case has one untimed rebuild and seven timed samples.

Set `HBAM_BENCH_OUTPUT=/tmp/hbam-after` to save complete result files. For a
before/after comparison, install this same benchmark and Cargo bench stanza
in a checkout of the baseline and run it with output `/tmp/hbam-before`.
Compare each corresponding file using `cmp`, not only its length or checksum.
On Linux, prefix both invocations with `taskset -c 3` to use the same CPU.

## Measurement on 2026-10-29

Baseline: `aebaa2a80dee9fa1715d30e2c7ea49623fe06617`, a commit of the project's earlier, private history
(not in this repository).
Candidate changes only the index-page prefix search, from successively
serializing every candidate prefix to exponential bracketing and bisection.
It still selects the largest fitting prefix and uses the original serializer.

Environment: Rust 1.99.1, Cargo 1.99.1, x86_64 Linux KVM VM exposing eight
QEMU Virtual CPU version 2.5+ cores, CPU 3 affinity, standard release settings.
Three runs per version, seven samples per case, interleaved in order
candidate/baseline/baseline/candidate/candidate/baseline. All 21 samples per
case/version are included in the medians, including first samples.

| Workload | Output bytes | Baseline median ms | Candidate median ms | Speedup |
| --- | ---: | ---: | ---: | ---: |
| 1,000 records | 557,056 | 1.5676 | 0.9913 | 1.58x |
| 10,000 records | 5,484,544 | 34.0476 | 13.8256 | 2.46x |
| 50,000 records | 27,385,856 | 189.4367 | 95.0506 | 1.99x |
| Shipped Windows Contacts/en template | 417,792 | 0.9212 | 0.5725 | 1.61x |

Raw samples are in `rebuild-measurements.csv` (nanoseconds). Corresponding
baseline/candidate outputs for all four workloads were compared in full and
were byte-identical. These are whole-file in-memory rebuild results, not
end-to-end save latency or a throughput claim for production deployments.
The VM is shared and timings show scheduling noise, including a 688.7 ms
baseline sample for 50,000 records. No disk I/O is timed.

Validation: 30 unit tests pass, including exact page bodies, first separator
and level against the original greedy packer across 78 valid ordered fixture
sets. Sets cover flat/nested keys, maximum supported key width, begin/end
fences and page-capacity boundaries. The existing optional
`rebuild_keeps_every_key_and_value` test passes for all 138 locally available
shipped templates and samples. `cargo clippy -p om-hbam --all-targets -- -D
warnings` passes. FileMaker application acceptance was not run for this
change; the four complete output comparisons establish unchanged bytes for
those workloads only.
