# The store engine's file: reads, import and export

Where the time goes when a file is held in a store tree (`om_server::engine::paged`,
docs/server-storage.md 5, "The store engine's file") and read through its view,
imported from an `.fmp12` and exported again, measured against the file engine,
and what each change made of it.

## Reproduce

One step per process, so that each peak is its own, under a memory cap, one run
at a time, with `uptime` and `free -g` checked first:

```sh
cargo test --release -j 4 -p om-server --lib --no-run
BIN=target/release/deps/om_server-<hash>
run() {
  systemd-run --user --scope -p MemoryMax=8G -p MemorySwapMax=0 \
    env OM_PAGED_FILE=$PWD/target/m/f.fmp12 OM_PAGED_STORE=$PWD/target/m/store "$@" \
    $BIN --ignored paged_measurement --nocapture
}
systemd-run --user --scope -p MemoryMax=8G -p MemorySwapMax=0 \
  env OM_EXPORT_RECORDS=200000 OM_EXPORT_FILE=$PWD/target/m/f.fmp12 OM_EXPORT_STEP=generate \
  $BIN --ignored export_measurement --nocapture
run OM_PAGED_STEP=import                          # makes the page file
run OM_PAGED_STEP=reads OM_PAGED_CACHE_MIB=64     # and 1024
run OM_PAGED_STEP=mem-reads OM_PAGED_TRUSTING=0   # and 1
run OM_PAGED_STEP=decode
run OM_PAGED_STEP=export
run OM_PAGED_STEP=file-reads
```

Remove the generated file and the page file afterwards.

The profiles: `perf record -F 999` of the same steps. This host's kernel has
`perf_event_paranoid` at 4, which forbids sampling to an unprivileged user, so
perf ran as root (`sudo perf record ... -- sudo -u $USER env ... $BIN ...`), the
measured process as the user, under the same cap; the kernel setting was not
changed. The processor has no last-branch records here (a virtual machine), and
the release build has no frame pointers, so call chains did not unwind usefully:
the shares below are of self time by symbol, sorted into the categories under
"Method". `libc6-dbg` names the allocator's and `memcpy`'s symbols.

## Method

- **The file** is the Tasks fixture (`om-server/tests/fixtures/tasks.rs`) with
  one table of 200,000 records, eight 64-byte text cells each: 109.5 MB, 1,800,112
  nodes, as `export_measurement` generates it.
- **Import** (`import`) streams it into a new page file in this worktree's
  `target/` (ext4), in the default batches (4,096 nodes, 4 MiB), with a 64 MiB
  page cache and 4,096 pages (64 MiB) waiting for a checkpoint at most, and
  checkpoints at the end. Its tree is 8,490 pages of 16 KiB, three levels.
- **Reads** (`reads`): the page file opened again and its checkpoint pinned, then
  a walk of the table's records through the view (`View::walk`, 1,800,001 nodes)
  and 1,000 record subtrees at seeded places, twice in the same process: the first
  pass reads every page from the file and checks it (an `.fmp12` freshly written
  is in the system's cache, so a read is a copy from memory); the second finds
  what the page cache kept, all of it with a 1 GiB cache, little of it with 64 MiB
  (the tree is 139 MB). `OM_PAGED_PASSES` adds warm passes.
- **In memory** (`mem-reads`): the file imported into `MemStore` first (not timed),
  then the same two passes. `MemStore::new` checks each page's hash on its first
  read and keeps the checked page; `MemStore::trusting` checks none. The
  difference between the first pass through the checking store and the trusting
  store is the checks alone, without a disk or a page cache.
- **Decode** (`decode`): what import does before it writes, the stream read and
  each node's checks, with nothing applied.
- **Export** (`export`): the pinned tree written as an `.fmp12` and read back.
- **The file engine** (`file-reads`): the file loaded whole, then the same reads.
- **Timings** are the median of three runs. The host had other work on it
  (load averages from 2 to 9 during the runs); single runs moved by up to 10%.
- **Categories** of the profiles, by symbol: hashing (`ring`'s SHA-256);
  allocation (`malloc`, `free` and the allocator's internals, `RawVec`); copies
  (`memcpy`, `memmove`); the kernel (the reads' copies and page faults); page checks
  and parsing (`Page::verify`, `Page::parse`); key decoding (`last_segment`,
  `key::decode`, `item_key`); the view (`PagedView`, the view's iterators); the
  cache and page store (`Cache::get`, `read_page`, `MemStore::read`); import's
  checks (`writer::unwritable`, `check_edits`, `index_entry_cost`); the stream
  reader; and tree descents and compares (the rest of `om_store::tree`: `Page::item`,
  `Page::child`, the cursor, separator and key comparisons, `memcmp`).

## Before (at `deaee759a`)

| | Store engine | File engine |
|---|---:|---:|
| Walk of the table, first pass, 64 MiB cache | 949 ms | 73 ms |
| Walk, second pass, 64 MiB cache | 855 ms | |
| Walk, first pass, 1 GiB cache | 996 ms | |
| Walk, warm (1 GiB cache, all in it) | 403 ms | |
| Walk in memory, checking pages once / trusting | 911 / 443 ms | |
| 1,000 records, 64 MiB cache | 36 ms | 3.8 ms |
| 1,000 records, warm | 6.0 ms | |
| Import | 9.5 s (decode and checks 0.85 s) | 1.4 s (load) |
| Export | 2.25 s | |

Profiles (share of self time):

| Category | Reads, 1 GiB, cold and warm pass | Reads, 64 MiB | Reads, 1 cold and 7 warm passes | Import |
|---|---:|---:|---:|---:|
| Hashing | 24.5% | 37.0% | 8.4% | 6.9% |
| Allocation | 24.1% | 20.3% | 36.7% | 22.6% |
| Tree descents and compares | 21.7% | 18.0% | 30.3% | 53.0% |
| Kernel | 14.1% | 9.0% | 5.5% | 4.6% |
| Key decoding | 4.9% | 4.2% | 7.1% | 0.1% |
| The view | 4.8% | 3.1% | 5.4% | |
| Page checks and parsing | 2.1% | 3.4% | 0.6% | 0.4% |
| Copies | 1.2% | 0.7% | 2.4% | 1.5% |
| Cache and page store | 0.9% | 0.9% | 1.3% | 3.9% |
| Import's checks | | | | 2.5% |
| Stream reader | | | | 1.0% |

What that says:

- **A cold walk is half reading and checking pages**: 996 ms against 403 ms warm
  with every page in the cache, 911 ms against 443 ms in memory with and without
  the checks. With a cache smaller than the tree every pass is cold. The hash on a
  cache miss is the design's check (docs/server-storage.md 6.4) and stays; what a
  cold read costs is the cache's size.
- **A warm walk is still five times the file engine's**, and it goes to
  allocation (more than a third) and to the cursor's per-item work: every node
  read allocates its whole key (`item_key`), its value and its last segment, and
  compares its key with the walk's far bound. Re-opening the tree for each page of
  256 nodes is a descent of three levels per page, about 7,000 descents for the
  walk: too small to show by itself.
- **Scattered reads are cold reads**: 36 ms with the 64 MiB cache, 6 ms warm,
  against 3.8 ms for the file engine in memory.
- **Import is the tree's writing**: decoding and checking the file is 0.85 s of
  9.5 s, and 53% of the samples are descents and compares in `tree::apply`, which
  for each put looks up every ancestor, deletes the empty range under the key and
  looks the key up again before it sets it, although an import's keys arrive in
  order with their parents already put.

## Changes

Each change was measured with the same method before and after; its row gives the
medians.
