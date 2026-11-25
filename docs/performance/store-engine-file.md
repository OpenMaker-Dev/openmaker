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
- **Listing** (at the end of `reads`, and in `file-reads`): the table's 200,000
  records listed through `View::children`, a page of children at a time.
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

Each change was measured before and after with the same steps. From the second
on, the binaries before and after ran interleaved, one process at a time, so that
the host's load moved both alike: five rounds, ten for changes 4 and 5. A walk's
"warm" is the mean of a run's three warm passes (`OM_PAGED_PASSES=4`, 1 GiB
cache). The rows give medians.

| Change | Measure | Before | After |
|---|---|---:|---:|
| 1. An append path in `tree::apply` | Import | 9.53 s | 4.31 s |
| 2. A walk's keys lent from their pages | Walk, first pass / warm | 998 / 402 ms | 955 / 391 ms |
| 3. Import's checks on borrowed parts | Decode and checks / import | 838 ms / 4.24 s | 346 ms / 3.54 s |
| 4. The far bound compared once per leaf | Walk, first pass / warm | 988 / 401 ms | 932 / 358 ms |
| 5. Each walked item read once, its leaf lent | Walk, first pass / warm | 916 / 354 ms | 860 / 308 ms |
| 6. The leaf's edge compared on its second step | Walk, first pass / warm; listing | 853 / 307 ms; 319 ms | 873 / 313 ms; 310 ms |

1. **The append path.** A put whose key is past the tree's greatest key finds its
   ancestors at or below that key present (they are that key or its ancestors),
   those above it missing, and nothing at or under its own key. It sets the
   missing ancestors as lists, then the node: the primitives the general put
   applies, in the same order, without the lookups that decide them (an import's
   keys arrive in that order). The tree is the same: a seeded test compares the
   root hashes of both paths over random ordered inputs with disorder mixed in
   (swaps, duplicates, deletes, an ancestor put again), in every split into
   batches of short sequences and random splits of long ones, on `MemStore` and on
   the page file; the import's tree here is the same 8,490 pages. Measured with
   the steps of "Reproduce", three runs each, before interleaving was used; the
   reads did not change.
2. **Lent keys.** `Cursor::next_lent` lends a key from its page when the page
   holds it whole, and the walk copies it into one of two buffers it swaps, rather
   than allocating every key. The last segment is still copied: the step owns it.
3. **Borrowed checks.** `tree::check_put` and `writer::unwritable_key` make the
   checks of `check_edits` and `unwritable` on borrowed parts, so import no longer
   copies each node's path and value to check them. A test compares
   `unwritable_key` with `unwritable` on the writer's refusal cases.
4. **The far bound per leaf.** A leaf's keys are in order, so when its last item
   (first, going backward) is within the cursor's far bound, every item of it is;
   each item is compared alone only in a leaf whose edge is not within, or whose
   comparison failed.
5. **One read of each item.** The cursor parsed each item's header three times
   (for the bound, the node and the key) and cloned its leaf's `Arc` for every
   item; it now parses it once and lends the leaf.
6. **The edge on the second step.** Change 4 made a cursor that takes one step
   (`Children` makes one per child) compare two items; it compares one again. No
   difference was measured on the walk, the listing or the export (2.28, 2.23 and
   2.26 s interleaved, for the binaries of changes 1, 5 and 6).

## After (all six changes)

| | Before | After | File engine |
|---|---:|---:|---:|
| Walk of the table, first pass, 64 MiB cache | 893 ms | 833 ms | 72 ms |
| Walk, again, 64 MiB cache | 831 ms | 773 ms | |
| Walk, first pass, 1 GiB cache | 959 ms | 850 ms | |
| Walk, warm (1 GiB cache, all in it) | 393 ms | 309 ms | |
| Walk in memory, checking pages once / trusting | 911 / 443 ms | 811 / 322 ms | |
| 1,000 records, 64 MiB cache | 36 ms | 36 ms | 3.4 ms |
| 1,000 records, warm | 5.8 ms | 5.7 ms | |
| 200,000 records listed, 64 MiB cache / warm | | 737 / 311 ms | 6 ms |
| Import | 9.5 s (decode and checks 0.85 s) | 3.5 s (0.39 s) | 1.4 s (load) |
| Export | 2.28 s | 2.26 s | |

The reads on the page file (the walk, the records and the listing) ran
interleaved with the binary of change 1, whose reads are those before the
changes, five rounds, as did the export. The rest is three runs each as in
"Before", at change 5: change 6 changed neither import nor, measurably, the reads.
A later run of every step at the final code had the host loaded by other work
(load averages 5 to 7, the file engine's load 1.65 s against 1.37 s) and is not
used.

Profiles (share of self time), as before:

| Category | Reads, 1 cold and 7 warm passes, before / after | Import, before / after |
|---|---:|---:|
| Allocation | 36.7% / 41.5% | 22.6% / 31.0% |
| Tree descents and compares | 30.3% / 19.8% | 53.0% / 24.0% |
| The view | 5.4% / 6.2% | |
| Hashing | 8.4% / 11.1% | 6.9% / 16.9% |
| Key decoding | 7.1% / 7.0% | 0.1% / 0.0% |
| Kernel | 5.5% / 6.1% | 4.6% / 9.7% |
| Copies | 2.4% / 2.7% | 1.5% / 2.6% |
| Cache and page store | 1.3% / 2.1% | 3.9% / 0.2% |
| Page checks and parsing | 0.6% / 0.7% | 0.4% / 2.1% |
| Import's checks | | 2.5% / 3.0% |
| Stream reader | | 1.0% / 2.2% |

`Cursor::next_lent`, whose symbol names the view's closure, is counted with the
tree. The import's hashing includes the measurement's own check of the tree
after it, which hashes every page once more.

## What is left, and why it stays

- **A warm walk is four times the file engine's**, and 41% of it is allocation:
  each node's value and last segment, allocated because the view's answers are
  owned (a step can borrow only from the view, and a page in the cache can be
  evicted while the step lives) and freed by the reader. Making them lent would
  change the `View` contract that every engine and reader shares. Reading each
  item from its leaf takes 20%, finding each node's depth and last segment in its
  key 7%, and the view's own steps 6%.
- **A cursor kept between a walk's pages** would save the descent the view makes
  for each page of 256 nodes: about 1% of a warm walk's samples, so it was not
  made.
- **Cold reads** are the hash on a cache miss, which stays (docs/server-storage.md
  6.4): with a cache smaller than the tree every pass is cold, and scattered
  reads are cold reads (36 ms for 1,000 records with 64 MiB).
- **Import** is now the tree's writing: each node's path and value copied from
  the stream into an owned edit, its key encoded and its item built (allocation,
  31%), finding the tree's right edge and splitting pages (24%), and hashing every
  page written. Going further would mean edits that borrow from the stream through
  `tree::apply`, a change to `om-store`'s `Edit`.
- **Listing a node's children** costs a descent from the root for every child:
  311 ms for 200,000 records against the file engine's 6 ms. It was not part of
  this measurement at first and is a change to `om-store`'s `Children`:
  [#767](https://github.com/OpenMaker-Dev/openmaker/issues/767).
