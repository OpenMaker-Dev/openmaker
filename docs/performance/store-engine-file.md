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
  the checks. With a cache smaller than the tree most of every pass misses. The
  hash on a cache miss is the design's check (docs/server-storage.md 6.4) and
  stays; what a cold read costs is the cache's size.
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
| 5. Fewer reads of each item's header, its leaf lent | Walk, first pass / warm | 916 / 354 ms | 860 / 308 ms |
| 6. The leaf's edge compared on its second step | Walk, first pass / warm; listing | 853 / 307 ms; 319 ms | 873 / 313 ms; 310 ms |

1. **The append path.** Once an entry has put a key that the working tree's own
   bytes prove its greatest (every node on the tree's right edge a copy the entry
   changed, each in order as its bytes show and at or above the separator its
   parent holds for it, the last separators at or below the key), a put past that
   key needs none of the general put's lookups: its ancestors at or below the
   greatest key are that key or its ancestors, those above are missing, and
   nothing is at or under its own key. It sets the missing ancestors as lists,
   then the node: the primitives the general put applies, in the same order. The
   greatest key's ancestors are looked up once after each proof, as the general
   put looks them up, and an append that splits a page drops the proof, so that
   the next put proves the key and looks its ancestors up again. So a tree read
   from its pages that breaks the tree's rules (a key without its parent, keys
   out of order across or within pages) is not taken on trust: on a tree read
   from its pages an entry's first put takes the general put (an entry that makes
   the tree appends from its first put, having nothing to read). Deciding reads
   nothing, so the append path reads no page the general put would not. The tree
   is the same: seeded tests compare both paths' root hashes after every entry,
   over random ordered inputs with disorder mixed in (swaps, duplicates, deletes,
   an ancestor put again) in every split into entries of twelve short sequences
   in memory and in random splits of six long ones in memory and on the page
   file; over directed inputs (keys that spill, a key of 65,535 bytes and a value
   of 65,535, a path 64 deep, a tree at least four levels high, the greatest key
   deleted, the tree emptied and filled), in memory and on the page file; and on
   trees written page by page that break those rules, splits of their right
   leaves and root index included. The import's tree here is the same 8,490
   pages. Measured with the steps of "Reproduce", three runs each, before
   interleaving was used; the reads did not change.
2. **Lent keys.** `Cursor::next_lent` lends a key from its page when the page
   holds it whole, and the walk copies it into one of two buffers it swaps, rather
   than allocating every key. The last segment is still copied: the step owns it.
3. **Borrowed checks.** `tree::check_put` and `writer::unwritable_key` make the
   checks of `check_edits` and `unwritable` on borrowed parts, so import no longer
   copies each node's path and value to check them. A test compares
   `unwritable_key` with `unwritable` on the writer's refusal cases.
4. **The far bound per leaf.** When a leaf's own bytes prove its keys in order
   (a spilled key is known there only as far as its first 1,024 bytes, so two
   keys that agree that far are not proven) and show its last item (first, going
   backward) within the cursor's far bound, every item of it is, and each one's
   own comparison would have been decided by the page's bytes as well. The edge's
   comparison reads nothing, so it cannot fail, and leaving out the items' own
   comparisons leaves out no read: the walk gives what comparing each item gives,
   its errors included. On any other leaf each item is compared alone.
5. **Fewer reads of each item's header.** The cursor parsed each item's header three times
   (for the bound, the node and the key) and cloned its leaf's `Arc` for every
   item. It now lends the leaf and parses the header once for the item, and once
   more for its comparison where the item is compared alone (a leaf's first step,
   and a leaf whose edge is not shown within). The key is read before the value,
   as before.
6. **The edge on the second step.** Change 4 made a cursor that takes one step
   (`Children` makes one per child) compare two items; it compares one again. No
   difference was measured on the walk, the listing or the export (2.28, 2.23 and
   2.26 s interleaved, for the binaries of changes 1, 5 and 6).

Review made changes 1 and 4 exact on trees the writer never makes and on reads
that fail (above, as they now are). Measured interleaved against the code before,
ten rounds each with load averages from 4 to 6, the corrections cost nothing
measurable: import 3.81 s before and 3.84 s after, the warm walk 325 and 332 ms,
its first pass 873 and 861 ms, the listing 327 and 321 ms. A second round made
an append that splits a page drop its proof, so the next put proves the greatest
key and looks its ancestors up again, about 8,500 times in this import: import
3.71 s before and 3.82 s after (ten rounds each, load averages 4 to 6), the warm
walk 327 and 322 ms.

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

The reads profile is of the whole step: in each pass a walk of the table and
1,000 scattered record subtrees, the first pass reading every page; after, at
`2657a3d6a`, whose step does not list the records yet. Both are of the code
before review. `Cursor::next_lent`, whose symbol names the view's closure, is
counted with the tree. The import's hashing includes the measurement's own check
of the tree after it, which hashes every page once more.

## What is left, and why it stays

- **A warm walk is four times the file engine's**, and allocation is 41% of the
  reads profile, mostly each node's value and last segment, allocated because
  the view's answers are owned (a step can borrow only from the view, and a page
  in the cache can be evicted while the step lives) and freed by the reader.
  Making them lent would change the `View` contract that every engine and reader
  shares. Reading each item from its leaf takes 20%, finding each node's depth
  and last segment in its key 7%, and the view's own steps 6%.
- **A cursor kept between a walk's pages** would save the descent the view makes
  for each page of 256 nodes: about 1% of a warm walk's samples, so it was not
  made.
- **Cold reads** are the hash on a cache miss, which stays (docs/server-storage.md
  6.4): with a cache smaller than the tree most of a walk's pages miss on every
  pass (773 ms for the second pass with 64 MiB, against 833 ms for the first),
  and scattered reads are mostly misses (36 ms for 1,000 records with 64 MiB).
- **Import** is now the tree's writing: each node's path and value copied from
  the stream into an owned edit, its key encoded and its item built (allocation,
  31%), finding the tree's right edge and splitting pages (24%), and hashing every
  page written. Going further would mean edits that borrow from the stream through
  `tree::apply`, a change to `om-store`'s `Edit`.
- **Listing a node's children** cost a descent from the root for every child:
  311 ms for 200,000 records against the file engine's 6 ms. Since
  [#767](https://github.com/OpenMaker-Dev/openmaker/issues/767) a listing seeks
  each child from where it found the one before, in the same leaf when it is
  there, and decodes only the child's own segment, with keys built in room
  reserved for them: 1,000,000 records listed in 0.74 and 0.81 s against 2.41
  and 1.97 s before (`reads`, release build, a 1 GiB cache that holds the
  tree, after a cold and a warm walk; interleaved, load 8 to 17; the file
  engine 37 ms). A warm walk of the same table takes 1.5 s.

## Checking the range of each leaf a read enters

Since [#1180](https://github.com/OpenMaker-Dev/openmaker/issues/1180) a cursor
compares each leaf it enters with the separators around it at every level
(docs/server-storage-format.md 2.6): at most two key comparisons a level for
each leaf entered, none while it stays in one. Measured on the 1,000,000-record
file of "Hosting on the store engine" (655.8 MB, Name's and Creator's indexes
stored; imported: 11,200,625 nodes, 59,583 leaves and 334 index pages, four
levels), `paged_measurement` `reads` with a 1 GiB cache, release builds of the
same code with the check and with it taken out. Key comparisons were counted by
a thread-local counter in `Pages::cmp_stored`, `Pages::cmp_known` and the
check's comparison decided by the pages' own bytes (a measurement-only patch,
as for [#767](https://github.com/OpenMaker-Dev/openmaker/issues/767)), in
builds apart from the timed ones; the timed runs alternated
the two builds, one process at a time, three rounds, load 5.6 to 5.9:

| | Without the check | With it |
|---|---:|---:|
| Key comparisons, 1,000,000 records listed (`View::children`) | 14,958,373 | 15,201,214 (+242,841, 1.6%) |
| Key comparisons, a walk of the table (9,000,001 nodes, `View::walk` through `walk_page`) | 1,047,886 | 1,467,579 (+419,693) |
| 1,000,000 records listed | 957, 890, 868 ms | 884, 947, 889 ms |
| The walk, warm | 1,478, 1,468, 1,503 ms | 1,569, 1,539, 1,528 ms |
| The walk, first pass | 4,424, 4,333, 4,402 ms | 4,294, 4,389, 4,495 ms |

The listing enters each of the table's leaves once and compares about five
separators for each (three index levels, fewer at the first and last child of
a page); its time does not move beyond the runs' spread. The walk in pages
seeks each page of 256 nodes anew, and checks the leaf each page lands in as
well as each leaf it steps into: a warm walk took about 4% longer, more than
the spread of these runs; its first pass, which reads and checks every page,
did not move beyond it.

## Hosting on the store engine

The server binary hosting one file on each engine (`--engine file` and
`--engine store`, docs/server-storage.md 11), on this branch's release build:
the time from the process's start to its listening line (its files hosted),
its resident memory once hosted, a client's reads, and its memory after them.

### Reproduce

`tests/view_timings.rs`, `engines`, one step per process, one run at a time,
under the same cap, with `uptime` and `free -g` checked first:

```sh
cargo test --release -j 4 -p om-server --test view_timings --no-run
BIN=target/release/deps/view_timings-<hash>
step() {
  systemd-run --user --scope -p MemoryMax=8G -p MemorySwapMax=0 \
    env OM_TIMING_FILE=$PWD/target/m/a/Tasks.fmp12 "$@" \
    $BIN --ignored --nocapture --exact engines
}
step OM_TIMING_STEP=generate OM_TIMING_RECORDS=200000   # and 1000000 in target/m/b
step OM_TIMING_STEP=index
step OM_TIMING_STEP=serve OM_TIMING_ENGINE=file
step OM_TIMING_STEP=serve OM_TIMING_ENGINE=store OM_TIMING_STORE=$PWD/target/m/a.store  # first start: imports
step OM_TIMING_STEP=serve OM_TIMING_ENGINE=store OM_TIMING_STORE=$PWD/target/m/a.store  # second start
```

Remove `target/m` afterwards.

### Method

- **The files** are `export_measurement`'s (above): the Tasks fixture with
  one table of 200,000 or 1,000,000 records, eight 64-byte text cells each,
  109.5 MB and 547.6 MB, with the table's record list `[T, 0d]` listing every
  record (`export_measurement` leaves the fixture's two there, so a find saw
  only those). `index` hosts the file on the file engine and finds on Name,
  which builds Name's value index, stores it and saves the file: 138.8 MB and
  694.0 MB, as a file with indexes is served. The 1,000,000-record file of
  the results is generated as `engines` generates it now: Creator holds
  `Admin` in nine records of ten (494.7 MB), and `index` also finds on
  Creator, which stores Creator's index too (655.8 MB).
- **`serve`** starts `om-server` on 127.0.0.1 with the one file, its folder
  made read-only first, so that the file engine's store of an index built on
  demand fails at once instead of writing the whole file (as `demand` above).
  The store engine never writes the folder. Memory is the server process's
  `VmRSS` and `VmHWM`, read from `/proc` two seconds after the listening line,
  and again after the reads. The reads, from one client logged in as Admin:
  a find on Name through its stored index, one record found (median of 5);
  an ExistsInIndex on Creator of 10,000 keys no record holds, each just
  below `Admin`'s key, so that each lookup misses next to the entry of
  900,000 records (median of 5, with the peak resident memory during it
  alone, `clear_refs`); a find on Code, which has no index, one record
  found, its index built on demand, then the same find again; a SortList of
  every record by Name; and a download of the table's records `[T, 05]` in
  pages (op90). The ExistsInIndex runs before the find on Code: on the file
  engine an index built on demand that the read-only folder cannot store is
  built again for every later query, about 7 s each here
  ([#812](https://github.com/OpenMaker-Dev/openmaker/issues/812)).
- **The machine:** an Intel Xeon Gold 6134 (16 threads) virtual machine with
  50 GB of memory, ext4, shared with other work: each step waits for a
  one-minute load average under 5 for up to ten minutes and starts anyway
  after that. The load was 2.9 to 3.9 at the start of the 200,000-record
  steps and 4.4 to 5.0 at the 1,000,000-record ones.
- **The builds:** a field's index read through listings
  ([#783](https://github.com/OpenMaker-Dev/openmaker/issues/783)). The
  200,000-record figures are from its first build, which listed each entry
  with its records; the 1,000,000-record ones from the build that lists
  keys and reads an entry's records only when an instruction takes it. In
  parentheses: the find on Name before #783, which read the index whole
  (on the files of the 200,000-record run), and the ExistsInIndex on the
  first build, on the same file just before.

### Results

| | file engine | store engine, first start | store engine, second start |
|---|---|---|---|
| **200,000 records (138.8 MB)** | | | |
| Start to hosted | 3.01 s | 5.26 s (import 5.05 s) | 0.43 s (hash 0.36 s) |
| Resident once hosted (peak) | 549 MB (702 MB) | 220 MB (220 MB) | 13 MB (13 MB) |
| Find on Name, stored index (before: whole) | 0.5 ms (8.2 ms) | 0.6 ms (219 ms) | 0.6 ms (243 ms) |
| Find on Code, index built: first, again | 2,333 ms, 2,113 ms | 2,090 ms, 2,040 ms | 2,633 ms, 2,176 ms |
| Sort of every record by Name | 3,281 ms | 3,005 ms | 3,449 ms |
| Download of `[T, 05]` (op90) | 658 ms | 975 ms | 938 ms |
| Resident after the reads (peak) | 665 MB (1,292 MB) | 553 MB (811 MB) | 550 MB (808 MB) |
| **1,000,000 records (655.8 MB, two indexes)** | | | |
| Start to hosted | 13.22 s | 41.15 s (import 40.16 s) | 1.98 s (hash 1.72 s) |
| Resident once hosted (peak) | 2,717 MB (3,467 MB) | 567 MB (567 MB) | 13 MB (13 MB) |
| Find on Name, stored index (before: whole) | 2.2 ms (54.3 ms) | 2.7 ms (2,608 ms) | 2.5 ms (2,480 ms) |
| ExistsInIndex, 10,000 keys missing below `Admin` (before: entries with their records) | 48.1 ms (50.2 ms) | 45.0 ms (629.6 ms) | 74.4 ms (589.0 ms) |
| Its peak over the resident before it (before) | 1 MB (1 MB) | 1 MB (1 MB) | 1 MB (1 MB) |
| Find on Code, index built: first, again | 12,350 ms, 11,528 ms | 15,752 ms, 16,037 ms | 15,924 ms, 15,474 ms |
| Sort of every record by Name | refused after 13,281 ms | refused after 13,692 ms | refused after 14,697 ms |
| Download of `[T, 05]` (op90) | 3,838 ms | 6,550 ms | 6,610 ms |
| Resident after the reads (peak) | 3,485 MB (4,326 MB) | 1,600 MB (2,066 MB) | 1,048 MB (1,513 MB) |

The store folder holds 214 MB for the first file and 988 MB for the second.
Its derived state took 1.3 to 2.1 ms to compute at each start, whatever the
records: it reads the catalogs, the accounts and the locale.

### What the numbers say

- **Hosted.** A second start reads the `.fmp12` once to hash it (0.37 s and
  1.72 s, the file in the system's cache) and holds nothing of it: 13 MB
  resident for either file, against the file engine's 549 MB and 2.7 GB, which
  grow with the file. A first start imports it, 1.9 and 3.0 times as long as
  the file engine's load. The import's peak, 220 MB and 567 MB, is what the
  page cache (256 MiB) and the pages waiting for a checkpoint (the default
  budget) can hold, and does not grow past them with the file (inferred from
  the configuration, not measured on a larger file); it stays resident once
  hosted.
- **Memory grows with the file in the reads, not in the hosting.** Each of
  these holds something whole, sized by the table: a find that builds an
  index holds the index it builds, and on the store engine builds it again
  for each find ([#793](https://github.com/OpenMaker-Dev/openmaker/issues/793));
  a sort holds a key per record; a download holds the pages it has not sent,
  up to `--download-memory-mib`. The peak during the reads is 808 to 811 MB
  at 200,000 records and 1.5 to 2.1 GB at 1,000,000, and the allocator keeps
  most of it resident afterwards (550 MB and 1.0 to 1.6 GB). A find through
  a stored index no longer adds to it.
- **Finds through a stored index** read the keys they need, listed from a
  bound a page at a time, and the records of the entries they take
  ([#783](https://github.com/OpenMaker-Dev/openmaker/issues/783)): 0.5 to
  2.7 ms on either engine, where reading the whole index took 219 to 243 ms
  and 2.5 to 2.6 s on the store engine. The file engine is faster too (8.2
  to 0.5 ms, 54 to 2.2 ms): it lent the index before, but checked the order
  of every key once per find.
- **A full read of an index** lists every key and reads every entry: a find
  for duplicates on Name, of 1,000,000 records. Each page of keys and each
  entry was sought from the root, the version's root page read again for
  each; since [#767](https://github.com/OpenMaker-Dev/openmaker/issues/767)
  a listing seeks each child from where it found the one before, and the
  view keeps the place its last read left, from which an entry at a key the
  listing has just given, and the next page, are sought. Release build, a
  256 MiB cache, `index-reads` four passes each and the server two starts
  each, interleaved, load 5 to 13:

  | 1,000,000 keys, store engine | Before | The listing | And the entries |
  |---|---:|---:|---:|
  | Every key listed | 4.6 to 5.7 s | 2.1 to 2.4 s | 2.1 to 2.3 s |
  | Every entry read by its key | 5.9 to 6.6 s | 4.7 to 5.1 s | 3.5 to 3.9 s |
  | The upkeep's check through the listing | 8.6 to 9.8 s | 4.9 to 5.4 s | 3.7 to 4.2 s |
  | One walk of the index | 2.4 to 2.6 s | 2.4 to 2.8 s | 2.3 to 2.6 s |
  | The find for duplicates, end to end (median of 3) | 6.9 and 7.2 s | 4.7 and 4.9 s | 3.6 and 3.7 s |

  The find is 1.5 times a walk of the index, which is itself 27 times the
  file engine's (0.09 s). A profile of the server during it: loading pages
  into the cache, with each page's hash checked, about a quarter (as in a
  walk); reading the pages and comparing keys, about as much; the
  allocator, a sixth; encoding and decoding keys, a tenth; building each
  entry as an owned tree, a twentieth.
- **Lookups that miss read no entry's records.** Each of the ExistsInIndex's
  10,000 keys lands just before `Admin`'s entry, whose records (900,000, in
  about 250 nodes) the first listing built with each page and dropped: 0.59
  to 0.63 s on the store engine. Now each lookup reads one key: 45 to 74 ms
  (median), 34 to 41 ms the fastest run. The runs alternate between those
  two by about 40 ms, on both engines and both builds (the file engine's
  are 10 ms and 48 ms): a stall of the 170 KB request's transport, not of
  the index. The file engine lends its entries and is unchanged. Memory is
  not what changed: each entry built was freed at once, and the peak during
  the check stays within 1 MB of what the process held.
- **A find that builds an index** costs about the same on both engines at
  200,000 records and 1.3 times as much on the store engine at 1,000,000
  (its records read through pages). The file engine here rebuilds it too,
  because its folder is read-only and the index's store failed; with a
  writable folder it stores the index and the next find reads it.
- **The sort** of 1,000,000 records by a 64-character field is refused on
  both engines, after seconds of work
  ([#795](https://github.com/OpenMaker-Dev/openmaker/issues/795)). It reads
  the index from its first key until every record has one, which here is the
  whole index. Through listings that cost the store engine more than the
  whole copy did (3.0 to 3.4 s against 2.3 to 2.5 s at 200,000 records),
  because its view read each entry's records with a seek of its own from
  the root (with each page, `PagedView::subtrees_page`, in the first build;
  by the entry's key, `View::subtree`, later); since
  [#767](https://github.com/OpenMaker-Dev/openmaker/issues/767) they are
  sought from where the listing stands (not measured again for the sort).
  The file engine's is unchanged (3.3 s against 3.2 s, one run each).
- **The download** is 1.3 times the file engine's at 200,000 records and 1.7
  times at 1,000,000.
