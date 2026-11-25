# How often a commit checks an index whole

Baseline: `fbe0ebfb7`, the operations' transaction (`c3d6d25fe`) with a log
line for each index check. Changed: the verified-index marks kept by
version in the derived state (docs/server-storage.md 5, "The
verified-index marks"), the branch of
[#782](https://github.com/OpenMaker-Dev/openmaker/issues/782). Measured
2026-11-25.

A commit keeps a stored index current only where it agrees with its
records; whether it does is checked against every record of the table
once (`Store::check_index`) and kept as a mark. The question is whether
the marks' new home makes commits check their indexes whole more often.

## Reproduction

```sh
cargo build --release --locked -p om-server --example index_check_perf
systemd-run --user --scope -p MemoryMax=6G target/release/examples/index_check_perf
```

`crates/om-server/examples/index_check_perf.rs`: the tests' Tasks file
(`tests/fixtures/tasks.rs`) with records 3 to 100,000 added and a value
index on Name that agrees with them, hosted over loopback TLS. After the
load it commits a new Name into one record eight times, then into 1,000
records three times, each record changed once; each commit's time is
from the request to its reply, the save of the whole file included. The
server logs each index check with its time (`Store::index_agrees`,
debug), which the example prints on stderr.

Both were built with Cargo's release profile and Rust 1.99.1, and run one
at a time, three runs each, on a 16-vCPU Intel Xeon Gold 6134 KVM VM
shared with other work (load average 6 to 9).

| Build | Run | Index checks | Check ms | First commit ms | Commits 2 to 8, median ms | 1,000 records, median ms |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| before | 1 | 1 | 213 | 280 | 66 | 812 |
| before | 2 | 1 | 197 | 264 | 66 | 798 |
| before | 3 | 1 | 221 | 293 | 72 | 821 |
| after | 1 | 1 | 218 | 286 | 68 | 841 |
| after | 2 | 1 | 224 | 299 | 72 | 795 |
| after | 3 | 1 | 189 | 256 | 67 | 790 |

Before and after, the first commit after the load checks Name's index
against its 100,000 records once, about 200 ms of its time, and no later
commit checks it again: the mark is carried from version to version. The
other differences are within the spread of runs.

## The checking transaction's checks of the marks

In debug builds with the test view on, the checking transaction checks
every mark that says an index agrees against the tree, by checking the
index whole again (`checking::marks_hold`), at each operation's end,
after its save and after a roll-back. Its cost to the tests was measured
by timing `cargo test -j 4 -p om-server -p om-author --no-fail-fast` with
the test binaries built, alternating a build whose `marks_hold` returned
at once when `OM_SKIP_MARK_CHECK=1` was set (a measuring patch, not
kept) with and without the variable, twice each (load average 6 to 7):

| Mark checks | Run 1 s | Run 2 s |
| --- | ---: | ---: |
| off | 62.0 | 62.3 |
| on | 60.4 | 59.0 |

The difference is within the spread of runs: the suites' tables are
small, so each check is cheap, and every mark is checked rather than a
sample.

## Limits

One table and one index. A write that forgets marks (a schema change, an
upload into an index or a record) makes the next commit check again, as
before; that is not measured here. A roll-back now puts back the marks
from before the change where it forgot them all, which saves a check
after a failed save; not measured either.
