# A commit's time with the operations' transaction

Baseline: `cee82185c`, before the operations' transaction
(docs/server-storage.md 5, "The operations' transaction"). Changed: the
branch of [#776](https://github.com/OpenMaker-Dev/openmaker/issues/776),
built at `221e99208` (its later commits change docs, test switches,
debug-only code and how op87 and op88 count a library's members, none of
the code a commit runs). Measured 2026-11-25.

A commit's records still go through the store's own helpers; what the
transaction adds to every write operation in a release build is a flag of
writes that reach the derived state and a check of it when the operation
ends. The question is whether a user would see that in a commit.

## Reproduction

```sh
cargo build --release --locked -p om-server --example commit_perf
TMPDIR=$PWD/target/bench-tmp systemd-run --user --scope -p MemoryMax=6G \
  taskset -c 12-15 target/release/examples/commit_perf
```

Build the same example (`crates/om-server/examples/commit_perf.rs`) in a
worktree of the baseline, and run the two executables alternately, one at
a time.

The file is the tests' Tasks file (`tests/fixtures/tasks.rs`) with
records 3 to 100,000 added. Over loopback TLS, each sample uploads a block
of records no earlier sample changed (op14) with a new Name, and times the
commit (op70) alone, from the request to its reply, which includes the
save of the whole file. Each run has one warm-up and seven measured
samples of 1 record, then the same of 1,000 records.

Both were built with Cargo's release profile and Rust 1.99.1, and run on a
16-vCPU Intel Xeon Gold 6134 KVM VM shared with other work (load average
3 to 8 during the runs). Twelve runs were not pinned, six of each, in
the order before, after, after, before, then after, before, before, after
twice; twenty were pinned to CPUs 12 to 15, in pairs whose order swapped
from one pair to the next.

| Runs | Committed | Before ms (median, IQR) | After ms (median, IQR) |
| --- | ---: | ---: | ---: |
| Pinned, 70 samples each | 1 | 138.1 (131.1-147.9) | 136.6 (127.7-145.2) |
| Pinned, 70 samples each | 1,000 | 145.7 (135.1-153.0) | 141.2 (134.4-149.9) |
| Not pinned, 42 samples each | 1 | 129.5 (123.3-139.5) | 138.1 (125.5-162.3) |
| Not pinned, 42 samples each | 1,000 | 138.2 (135.0-155.4) | 138.2 (132.4-143.9) |

[Raw samples](commit-through-transaction.csv) keep every measurement.

The time is the save of the whole file, about the same for 1 record as for
1,000, and the differences go both ways within the spread of runs (the
unpinned 1-record medians differ by 9 ms; pinned, by 1.5 ms the other
way). The transaction adds nothing a user would see to a commit.

## Limits

The commit's records are not on the transaction yet, so this measures the
transaction's cost to the operations around them, not a commit's writes
through it. Not measured: the debug builds' checking transaction, which
tests only use.
