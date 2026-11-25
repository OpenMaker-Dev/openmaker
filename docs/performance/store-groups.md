# What a group costs in the store engine

What a group (server-storage.md 6.9) costs `om_store::store::Store` on
the real file system, for both kinds:
- a **staged** group changes an existing file: its parts are staged and
  its close applies them;
- a **new file** group makes a new file: its parts write the file's tree
  at once and its close publishes it.

Each group is N parts, each one put under the group's declared path,
in key order. For each N the note gives:
- the time to stage, and the pages the parts wrote;
- the pages held for the group's end;
- the page file's size while the group is open;
- the time to close, and the pages the close wrote;
- the process's peak memory after the staging and after the close.

## Reproduce

```sh
cargo build --release -p om-store --example group_cost
uptime; free -g
systemd-run --user --scope -p MemoryMax=8G -p MemorySwapMax=0 \
  target/release/examples/group_cost <staged|new> <parts> [value bytes] \
  [checkpoint every n parts] [folder]
```

One group per process, one process at a time. Values are 4,096 bytes by
default, and by default no checkpoint is taken but those the budget asks
for. The folder (default `target/group-cost`) is removed first and at
the end; a staged group of 10,000 parts needs about 10 GiB free.

## Method

- **The store.** A new store with the default configuration: a 256 MiB
  page cache, a budget of 16,384 pages waiting for a checkpoint, a
  reserve of 256 pages, and 64 MiB log segments. One file is added,
  flushed and checkpointed before the timed staging.
- **Stage.**
  - A staged group: the opening part and N - 1 parts, each a change with
    one put at `[g, i]` (`i` the part's number, 4 bytes big-endian).
  - A new file group: its opening `group` entry and N parts, each with
    the same put, in the new file.
  - Either way, a flush after every 100 parts and at the end, as a
    client sending them in batches would, and a checkpoint whenever an
    append is refused for one, or every n parts when asked.
- **Pages written** are what each entry's unit wrote, with the fixed
  room its trees take in the tables (`Appended::pages`, which is
  `Reservation::taken`), summed over the parts; the close's are its own.
- **Held** is `Space::groups` after the staging: what the group set
  aside for its end and, for a staged group, for its close's
  application (format 4.8).
- **Close.** A checkpoint, then the closing entry (a staged close, or a
  publication) appended and flushed.
- **Peak memory** is the process's `VmHWM` after the staging and after
  the close, so the second includes the first.

## Results

2026-11-25, branch `t3/store-groups` (pull request #796, round 2 of its
review). The development VM: ext4 on a virtual disk shared with other
agents' builds, load average 7 to 12 before the runs, 38 to 39 GiB of
memory available.

Parts of one 4,096-byte value:

| Kind | Parts | Stage | Pages the parts wrote | Held | Page file while open | Close | Pages the close wrote | Peak memory, staged / closed |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| staged | 1 | 43.5 ms | 7 | 431 pages (6.7 MiB) | 12.0 MiB | 0.9 ms | 3 | 4.8 / 4.8 MiB |
| staged | 100 | 105 ms | 397 | 6,272 (98 MiB) | 117 MiB | 2.9 ms | 109 | 11.4 / 11.4 MiB |
| staged | 10,000 | 19,366 ms | 47,991 | 590,372 (9.0 GiB) | 9.9 GiB | 325 ms | 10,672 | 233 / 383 MiB |
| new file | 1 | 14.9 ms | 9 | 509 (8.0 MiB) | 16.2 MiB | 17.1 ms | 5 | 4.8 / 4.8 MiB |
| new file | 100 | 62.9 ms | 397 | 509 | 20.2 MiB | 18.0 ms | 5 | 11.2 / 11.2 MiB |
| new file | 10,000 | 6,261 ms | 46,519 | 509 | 188 MiB | 107 ms | 5 | 181 / 182 MiB |
| new file | 26,856 (110 MB) | 19,800 ms | 131,927 | 509 | 483 MiB | 13.9 ms | 5 | 476 / 477 MiB |

A new file group of about 110 MB, other values and checkpoints:

| Values | Parts | Checkpoint every | Stage | Page file while open | Close | Peak memory |
|---:|---:|---:|---:|---:|---:|---:|
| 4,096 bytes | 26,856 | 1,000 parts | 23,101 ms | 483 MiB | 20.2 ms | 329 MiB |
| 4,000 bytes | 27,500 | none asked | 21,344 ms | 132 MiB | 28.7 ms | 139 MiB |
| 4,000 bytes | 27,500 | 1,000 parts | 22,398 ms | 132 MiB | 25.3 ms | 139 MiB |
| 4,096 bytes | 53,712 (220 MB) | 1,000 parts | 46,700 ms | 979 MiB | 19.1 ms | 354 MiB |

What a staged close holds in memory for its edits, apart from the page
file, at the bounds a staged group is held to (1,048,576 edits, 64 MiB
as they encode): the staged bytes decoded as the close decodes them and
applied to an empty tree in one unit over an in-memory page store
(`cargo run --release -p om-store --example group_close_memory --
<edits> <value bytes> [empty segments]`, each edit a put at `[g, <empty
segments>, i]`; the staged bytes are held whole here, which the close
does not do):

| Edits | Value | Path segments | Encoded | Peak decoded | Peak applied |
|---:|---:|---:|---:|---:|---:|
| 1,048,576 | 0 bytes | 26 | 66.1 MB | 971 MiB | 1,086 MiB |
| 1,048,576 | 0 bytes | 25 | 64.0 MB | 971 MiB | 1,072 MiB |
| 1,048,576 | 0 bytes | 22 | 57.7 MB | 966 MiB | 1,065 MiB |
| 480,000 | 0 bytes | 64 | 66.7 MB | 832 MiB | 879 MiB |
| 1,048,576 | 44 bytes | 2 | 61.9 MB | 362 MiB | 491 MiB |
| 1,048,576 | 0 bytes | 2 | 15.7 MB | 251 MiB | 332 MiB |
| 16,320 | 4,096 bytes | 2 | 67.1 MB | 134 MiB | 362 MiB |
| 1,023 | 65,535 bytes | 2 | 67.1 MB | 130 MiB | 134 MiB |

- **A new file group's close is constant.** The publication writes the
  store tree's leaf and the table room of the tree's new name, 5 pages,
  whatever the group's size, and the group holds 509 pages for it from
  its open. Its time is the flush's. In an earlier build, the 110 MB run
  that took no checkpoint while staging closed in 623 ms, its flush
  waiting on the disk writing back the large checkpoint taken just
  before; here it took 13.9 ms.
- **A new file's parts cost what the same changes to a file cost.**
  About 4.9 pages a part: the file's last leaf and the overflow page of
  the value, and the store tree's leaf for the group's row and the last
  entry's position.
- **Memory is not flat in the group's size; it is bounded by the
  store's caches.** Memory grows with the pages written since the last
  checkpoint and with the page cache, up to the budget of 16,384 pages
  and the 256 MiB cache. With a checkpoint every 1,000 parts, 110 MB and
  220 MB peaked at 329 and 354 MiB.
- **The page file is near the data's size only for values that do not
  spill.** With 4,000-byte values a 110 MB group left a 132 MiB page
  file: the data, the 8 MiB the store started with, and the hold. A
  4,096-byte value with its key is just past a quarter page, so it
  spills: 1 KiB in its leaf and a 16 KiB overflow page for the rest,
  about four times its size. That is the tree format's rule for every
  file, not the group's
  ([#802](https://github.com/OpenMaker-Dev/openmaker/issues/802)).
- **A staged group's close** writes about one page for each 4 KiB part:
  what loading the same items in key order in one entry writes, each
  page once. Its memory follows the shape of the edits, not their bytes:
  a million edits of paths with many empty segments decode to about
  15 times their encoded size. For the worst shape found under the
  bounds, a million puts at paths of 26 segments (66 MB encoded), the
  decoded edits peaked at 971 MiB and the apply at 1,086 MiB; for the
  4 KiB puts at the bound, 134 and 362 MiB. These are measured peaks of
  this example, not a bound on the close: the close also holds a part's
  bytes while it decodes them, and its tree writer works over the file's
  tree, not an empty one.
- **A staged group's holds dominate its cost.** Each part holds
  `entry_bound` of its edits (59 pages for one 4 KiB put), about 55 times
  what its share of the close writes, and the page file grows to provide
  them: 9.9 GiB for 40 MiB of data
  ([#786](https://github.com/OpenMaker-Dev/openmaker/issues/786)). A
  million small edits would hold about a terabyte, so the close's worst
  memory shape cannot be staged on an ordinary disk. The same staging with
  the bound forced to 0 (an experiment in an earlier build, not a
  setting) took 6.3 s for 10,000 parts with a 187 MiB page file. The
  group's own end hold is 372 pages for a group in one file: the session
  row with a reply of the largest size is most of it.
- **Without a flush every 100 parts** (an earlier build), 10,000 staged
  parts took 44.8 s and peaked at 907 MiB: the store keeps every
  unpublished entry's version until a flush.
