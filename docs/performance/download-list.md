# Record-batch download benchmark

Baseline: `aebaa2a80dee9fa1715d30e2c7ea49623fe06617` (`main`).

Op92 previously cloned every child below the requested parent, then filtered
with a linear search of requested keys. The change selects matching children
before cloning and uses binary search over the ordered decoded key list.
The snapshot retains parent metadata and child order. Encoding and transfer
paging still happen after releasing the store read lock.

## Reproduce

```sh
cargo run --release -p om-server --example download_list_perf --locked
cargo test -p om-server --all-targets --locked
```

For the baseline, copy the same example into a checkout of the baseline SHA.
Build both with the same toolchain and run their release executables with
`taskset -c 7`. This benchmark opens an ephemeral loopback TLS server with
synthetic data and credentials. It requires no external service or fixture.

Each table has 1,000 or 10,000 records, each containing a 512-byte payload
and an eight-byte ID. Each nonempty selection also requests one missing key.
The client/server request, decoding, encoding, TLS transport and continuation
paging are timed. File creation, server setup, login, fixture construction and
full reply-byte assertions are outside the timed region. The 10,000-record
full selection crosses the server's 1 MiB transfer page boundary.

## Measurements

2026-10-29, Rust 1.99.1, x86_64 Linux KVM VM, eight QEMU virtual CPUs.
Both release binaries pinned to CPU 7, run in candidate/baseline/baseline/
candidate order. Every case has one warmup and seven samples per run. The
medians below combine all 14 measured samples per version. Raw samples are
in `download-list-samples.csv`. The shared VM has scheduling noise; these
results describe synthetic loopback latency, not production network latency.

| Records | Requested existing | Baseline ms | Candidate ms | Speedup |
| --- | --- | ---: | ---: | ---: |
| 1,000 | 0 | 0.300 | 0.026 | 11.76x |
| 1,000 | 10 | 0.381 | 0.112 | 3.41x |
| 1,000 | 100 | 0.982 | 0.779 | 1.26x |
| 1,000 | 1,000 | 7.942 | 7.311 | 1.09x |
| 10,000 | 0 | 6.576 | 0.028 | 237.94x |
| 10,000 | 10 | 6.222 | 0.349 | 17.82x |
| 10,000 | 100 | 7.222 | 1.129 | 6.40x |
| 10,000 | 10,000 | 179.954 | 69.782 | 2.58x |

Every warmup and measured response is compared byte-for-byte against the
original clone-and-filter result. The synthetic regression test also covers
serial-key width boundaries, absent keys, empty/full selections, and an absent
parent. Valid wire key lists are sorted; the decoded tree enforces that order.
The unchanged server tests plus the new test pass (25 passed, nine existing
fixture-dependent tests ignored). Clippy passes with warnings denied.
FileMaker desktop acceptance and production load were not exercised.
