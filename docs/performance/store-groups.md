# What a group costs in the store engine

What a group (server-storage.md 6.9) costs `om_store::store::Store` on
the real file system: a group of N parts, each one 4 KiB put under the
group's one declared path, staged and then closed. For each N: the time
to stage and the pages the parts wrote, the pages they held for the
close, the page file's size while the group is open, the time to close
and the pages the close wrote, and the process's peak memory after the
staging and after the close.

## Reproduce

```sh
cargo build --release -p om-store --example group_cost
uptime; free -g
systemd-run --user --scope -p MemoryMax=8G -p MemorySwapMax=0 \
  target/release/examples/group_cost <parts> [folder]
```

One N per process, one process at a time. The folder (default
`target/group-cost`) is removed first and at the end; for 10,000 parts it
needs about 10 GiB free.

## Method

- A new store with the default configuration (256 MiB page cache, a
  budget of 16,384 pages waiting for a checkpoint, a reserve of 256
  pages, 64 MiB log segments). One file is added, flushed and
  checkpointed before the timed staging.
- **Stage**: the opening part and N - 1 parts, each a change with one
  put of 4,096 bytes at `[g, i]` (`i` the part's number, 4 bytes
  big-endian), so the puts are in key order; a flush after every 100
  parts and at the end, as a client sending them in batches would. A
  checkpoint is taken whenever an append is refused for one (none was).
- **Pages written** are what each entry's unit wrote, with the fixed room
  its trees take in the tables (`Appended::pages`, which is
  `Reservation::taken`), summed over the parts; the close's are its own.
- **Held** is `Space::groups` after the staging: what the parts set aside
  for the close (format 4.8).
- **Close**: a checkpoint, then the closing entry appended and flushed.
- **Peak memory** is the process's `VmHWM` after the staging and after
  the close, so the second includes the first.

## Results

2026-11-25, branch `t3/store-groups` (pull request for #778). The
development VM: ext4 on a virtual disk shared with other agents' builds,
load average 3.7 to 3.8 before each run, 39 GiB of memory available.

| Parts | Stage | Pages the parts wrote | Held for the close | Page file while open | Close | Pages the close wrote | Peak memory, staged / closed |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 6.1 ms | 7 | 59 pages (0.9 MiB) | 8.0 MiB (as before) | 14.7 ms | 3 | 4.6 / 4.6 MiB |
| 100 | 112.6 ms | 397 | 5,900 (92 MiB) | 104 MiB | 14.6 ms | 109 | 11.4 / 11.4 MiB |
| 10,000 | 18,417 ms | 47,991 | 590,000 (9.0 GiB) | 9.9 GiB | 370.1 ms | 10,672 | 234 / 383 MiB |

- **The close** writes about one page for each 4 KiB part: what loading
  the same items in key order in one entry writes (four items to a leaf,
  and the index pages above), each page once. It does not depend on how
  the parts were cut. Its time is mostly the flush for small groups and
  the apply for large ones.
- **The parts** write about 4.8 pages each: the staging tree's leaf and
  the index pages above it, and the store tree's pages for the group's
  row, each written anew by every part.
- **The holds** dominate the cost of a large group. Each part holds
  `entry_bound` of its edits (59 pages for one 4 KiB put), about 55
  times what its share of the close writes, and the page file grows to
  provide them: 9.9 GiB for 40 MiB of data. The same staging run with no
  holds (the bound forced to 0, not a configuration) took 6,276 ms for
  10,000 parts, with a 187 MiB page file, and 566 ms and 1,868 ms for
  1,000 and 3,000 parts; with holds 1,349 ms and 5,479 ms. So the holds
  also make staging grow faster than linearly; the file's growth and
  the map's size are the likely causes, not measured apart.
- **Memory**: staging holds the pages written since the last checkpoint
  and the page cache; the close adds about 150 MiB for 10,000 parts, the
  decoded edits and the tree's working copy.
- Without the flush every 100 parts, the 10,000 parts staged in 44.8 s
  and peaked at 907 MiB: the store keeps every unpublished entry's
  version until a flush.
