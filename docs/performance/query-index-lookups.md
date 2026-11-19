# Query program index lookups

Measured on 2026-11-10 for #306, against baseline
`d57188d` (`main` with the program limits of #334), a commit of the project's earlier, private history
(not in this repository).

A query program (op86) runs while the hosted file's store is held for
reading, so writers of the file wait for it, and any signed-in account may
send one. Four instructions cost the program's size times the size of an
index:

- JoinProjection compared every key of the target's value index with every
  source row.
- Each SimpleFind finder scanned its field's whole index.
- ExistsInIndex looked up each index key in the client's key list.
- A range find merged the growing result with the records of each key in
  the range, copying the result every time. That is quadratic in the range.

Index keys are stored in key order (query-program.md 7.1). Each
instruction now finds its keys by binary search, reads only the entries
its answer needs and merges their records into one list in place. Reading
an index counts towards the program's work limit (11.4): each key once for
the order check (the same pass leaves out the `ff 80` entry of long keys,
7.1, at no further cost), the keys each binary search compares, and each
entry read.

## Method

A throwaway harness, outside the repository, runs `om-server` in process on
loopback. It hosts the server's test fixture (`crates/om-server/tests/fixtures/tasks.rs`)
with records 3 to N added by `add_tasks`, so Name's value index holds N
keys ("Task <id>"). The harness signs in as Clerk, an account without Full
Access. Warm-up programs build Name's value index on demand. It then sends
one program and times it from the request to the reply, as the client sees
it. A program larger than one request (2 MiB) is uploaded in parts and
timed from its last part. The shapes, each against Name:

| shape | program |
|---|---|
| join | JoinProjection, `size` source rows, `=`, each row's key `fe` (matches nothing; the issue's reproduction) |
| finders | SimpleFind with `size` IndexMatch finders, each the key of a "Task <n>" no record holds |
| exists | ExistsInIndex with `size` such keys |
| range | SimpleFind with `size` IndexRange finders over every key, `[01, 01 ffff]` |
| ne2 | JoinProjection, `size` rows, two `≠` predicates: each row reads the whole index twice |
| notrow | SimpleFind with `size` NotRowIdMatch finders: each reads the table's record list |
| hits | JoinProjection, `size` rows, `=`, keys the index holds |
| noop | JoinProjection, `size` rows and no predicate: reads no index (control) |

The same harness was compiled against an export of the baseline and against
the change, release profile. For the four shapes, each case ran three
times in one process per version, base and changed alternating case by
case. Environment: Linux x86_64, 8-vCPU shared KVM VM (QEMU Virtual CPU
2.5+), Rust 1.99.1. Other agents' builds kept the load average at 18 to 24
throughout, so single samples vary by up to a factor of ten. The tables
give medians. Raw samples are in
[query-index-lookups.csv](query-index-lookups.csv).

## The four shapes

Median milliseconds, request to reply:

| shape | index keys | program size | before | after |
| --- | ---: | ---: | ---: | ---: |
| join | 20,000 | 20,000 rows (223 KB) | 1,219 | 44 |
| join | 40,000 | 10,000 rows (110 KB) | 1,239 | 64 |
| join | 40,000 | 20,000 rows (223 KB) | 2,537 | 42 |
| finders | 20,000 | 10,000 finders (400 KB) | 358 | 55 |
| finders | 40,000 | 5,000 finders (200 KB) | 747 | 48 |
| finders | 40,000 | 10,000 finders (400 KB) | 1,665 | 67 |
| exists | 20,000 | 20,000 keys (460 KB) | 612 | 20 |
| exists | 40,000 | 10,000 keys (230 KB) | 848 | 51 |
| exists | 40,000 | 20,000 keys (460 KB) | 1,722 | 55 |
| range | 20,000 | 1 finder | 29 | 2.9 |
| range | 40,000 | 1 finder | 103 | 6.1 |
| range | 80,000 | 1 finder | 438 | 13 |
| range | 40,000 | 8 finders | 756 | 46 |
| range | 40,000 | 20 finders | 1,864 | 87 |
| ne2 | 40,000 | 20 rows (275 B) | 3,781 | 213 |

Before, each shape grew with the program times the index, and a single
range find grew with the square of the index. After, the first three
shapes stay under 70 ms at either index size, and the range find grows
with the keys it reads.

## Worst case at the default limits

Programs sized to run into the work limit (16,777,216 nodes), against the
40,000-key index, after the change (two runs each):

| shape | program | answer | ms |
| --- | --- | --- | ---: |
| ne2 | 2,000 rows (22 KB) | refused, `ffff` | 820, 833 |
| range | 1,000 finders (24 KB) | refused | 1,007, 1,031 |
| notrow | 100,000 finders (2.3 MB) | refused | 569, 586 |
| finders | 1,000,000 finders (43 MB, in parts) | refused | 1,093, 1,205 |
| exists | 2,000,000 keys (52 MB, in parts) | refused | 1,164, 1,468 |
| join | 1,300,000 rows (15.6 MB, in parts) | refused | 3,295 to 5,342 |
| hits | 1,300,000 rows (42 MB, in parts) | refused | 3,344, 3,507 |

The base had no bound on these shapes. ne2 took 917 ms for 5 rows and
3,706 ms for 20, about 185 ms a row. 20 full-range finders took 1,791 ms.

Spending the work limit on index reads takes about a second (0.6 to
1.5 s). The two longest cases are dominated by the program's size, not by
index reads. A program of 1,300,000 rows with no predicate (`noop`) took
3.9 to 30 s, on the base and after alike. At 400,000 rows, `noop` and the
`=` join both took 1.07 s. Sending and parsing a program near the size
limit of query-program.md 11.4 (four million nodes) is a cost of its own,
which this change does not alter.

These are end-to-end timings on a loaded shared VM, not FileMaker Server
comparisons. FileMaker Server's own cost for these programs is unmeasured.
