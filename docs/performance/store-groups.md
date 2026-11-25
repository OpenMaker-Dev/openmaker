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

2026-11-25, branch `t3/store-groups` (pull request #796, round 1 of its
review). The development VM: ext4 on a virtual disk shared with other
agents' builds, load average 3.7 to 6.9 before each run, 38 to 39 GiB of
memory available.

Parts of one 4,096-byte value:

| Kind | Parts | Stage | Pages the parts wrote | Held | Page file while open | Close | Pages the close wrote | Peak memory, staged / closed |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| staged | 1 | 1.1 ms | 7 | 148 pages (2.3 MiB) | 8.0 MiB | 1.1 ms | 3 | 4.6 / 4.6 MiB |
| staged | 100 | 300 ms | 397 | 5,989 (94 MiB) | 117 MiB | 10.0 ms | 109 | 11.3 / 11.3 MiB |
| staged | 10,000 | 21,477 ms | 47,991 | 590,089 (9.0 GiB) | 9.9 GiB | 338 ms | 10,672 | 233 / 383 MiB |
| new file | 1 | 5.2 ms | 9 | 505 (7.9 MiB) | 16.2 MiB | 0.9 ms | 1 | 4.7 / 4.7 MiB |
| new file | 100 | 86.6 ms | 397 | 505 | 20.2 MiB | 0.9 ms | 1 | 11.2 / 11.2 MiB |
| new file | 10,000 | 6,513 ms | 46,519 | 505 | 188 MiB | 20.4 ms | 1 | 181 / 182 MiB |
| new file | 26,856 (110 MB) | 22,524 ms | 131,927 | 505 | 482 MiB | 623 ms | 1 | 475 / 476 MiB |

A new file group of about 110 MB, other values and checkpoints:

| Values | Parts | Checkpoint every | Stage | Page file while open | Close | Peak memory |
|---:|---:|---:|---:|---:|---:|---:|
| 4,096 bytes | 26,856 | 1,000 parts | 24,270 ms | 482 MiB | 5.7 ms | 329 MiB |
| 4,000 bytes | 27,500 | none asked | 17,648 ms | 132 MiB | 11.8 ms | 138 MiB |
| 4,000 bytes | 27,500 | 1,000 parts | 20,865 ms | 132 MiB | 7.6 ms | 138 MiB |
| 4,096 bytes | 53,712 (220 MB) | 1,000 parts | 44,032 ms | 977 MiB | 18.3 ms | 354 MiB |

- **A new file group's close is constant.** The publication writes one
  page, the store tree's leaf, whatever the group's size, and the group
  holds 505 pages for it from its open. Its time is the flush's; at
  110 MB with no checkpoint while staging, the flush waited on the disk
  writing back the large checkpoint taken just before the close (623 ms,
  against 5.7 ms with a checkpoint every 1,000 parts).
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
  page once. Its memory adds about 150 MiB for 10,000 parts, the decoded
  edits and the tree's working copy; the staged bound, 64 MiB of encoded
  edits, keeps it to about 240 MiB.
- **A staged group's holds dominate its cost.** Each part holds
  `entry_bound` of its edits (59 pages for one 4 KiB put), about 55 times
  what its share of the close writes, and the page file grows to provide
  them: 9.9 GiB for 40 MiB of data
  ([#786](https://github.com/OpenMaker-Dev/openmaker/issues/786)). The
  same staging with the bound forced to 0 (an experiment in an earlier
  build, not a setting) took 6.3 s for 10,000 parts with a 187 MiB page
  file. The group's own end hold is 89 pages for a group in one file.
- **Without a flush every 100 parts** (an earlier build), 10,000 staged
  parts took 44.8 s and peaked at 907 MiB: the store keeps every
  unpublished entry's version until a flush.
