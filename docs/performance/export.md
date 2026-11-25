# Copies of a hosted file

How long each whole-file copy of a hosted file holds the store's read lock
on the file engine, during which every write to the file waits, how long
it takes, and how much memory it takes beyond the loaded file: a backup's
copy, the desktop's Save a Copy as and its compacted copy
(`om_server::engine::export`, docs/server-storage.md 5, "Exports"). Each
is measured as the code was before exports, as it was at `0b36912` (every
copy written from the view), and as it is now (a version on disk copied,
the view walked only for a version the disk lacks and for a compacted
copy).

## Reproduce

One step per process, so that each peak is its own:

```sh
cargo test --release -j 4 -p om-server --lib --no-run
BIN=target/release/deps/om_server-<hash>
run() {
  systemd-run --user --scope -p MemoryMax=8G -p MemorySwapMax=0 \
    env OM_EXPORT_RECORDS=191000 OM_EXPORT_FILE=$PWD/target/measure/f.fmp12 \
    OM_EXPORT_STEP=$1 \
    $BIN --ignored --exact engine::export::measurement::export_measurement --nocapture
}
run generate
for step in backup-before copy-before compacted-before walk copy; do run $step; done
```

Remove the generated file afterwards.

## Method

- **The files** are the Tasks fixture (`om-server/tests/fixtures/tasks.rs`)
  with one table of `OM_EXPORT_RECORDS` records, eight 64-byte text cells
  each, no indexes: the shape #673's save measurements used.
- **Each step** loads the file into a store, puts one new node in the tree
  and saves it, so that the disk holds the version, resets the process's
  peak resident memory (`/proc/self/clear_refs`, 5), makes one copy beside
  the file, and reports the time under the lock, the time in all and the
  peak (`VmHWM` less the resident memory before the copy).
- **The steps.** `copy`: the file opened under the lock, then copied from
  its descriptor in 8 MB parts (the kernel's `copy_file_range`: 15 calls
  for the 105 MB file, under `strace -c`) into an unnamed file, checked to
  be as long as the file was when opened, flushed, linked to its name and
  the folder flushed (a backup and a desktop Copy now). `walk`: the view
  written and checked under the lock, then flushed and named (a compacted
  copy now and at `0b36912`; a backup and a desktop Copy of a saved
  version at `0b36912`). The code before exports, re-enacted with the calls it made,
  since it no longer exists: `backup-before`, the file opened under the
  lock, then copied with `io::copy` in 8 MB parts, its permissions set and
  flushed; `copy-before`, the locked descriptor cloned under the lock, then
  read by position into the copy and flushed; `compacted-before`, the whole
  file rebuilt in memory under the lock (`om_hbam::writer::rebuild`), then
  written and flushed. A desktop copy is published by linking an unnamed
  file rather than by a rename; that is not timed apart.
- **Runs:** three of each, one process at a time, on a Linux VM with 16
  x86-64 cores and an ext4 disk, shared with other work (load 3 to 8), on
  2026-11-25. Each figure is the median of its three runs. The `copy`
  runs of the "Now" column for a backup and a desktop Copy were made with
  `backup-before` run beside them, which then took 12 µs and 188 ms
  (105 MB) and 12 µs and 1,135 ms (524 MB): the spread between runs on
  this machine is about as large as the difference between the columns.

## Results

Time under the store's read lock, then time in all:

| Copy | File | Before exports | At `0b36912` | Now |
|---|---:|---:|---:|---:|
| Backup | 104.6 MB | 11 µs, 196 ms | 740 ms, 915 ms | 11 µs, 230 ms |
| Backup | 524.0 MB | 12 µs, 911 ms | 3,499 ms, 4,138 ms | 19 µs, 1,075 ms |
| Desktop Copy | 104.6 MB | 1 µs, 262 ms | 740 ms, 915 ms | 11 µs, 230 ms |
| Desktop Copy | 524.0 MB | 2 µs, 906 ms | 3,499 ms, 4,138 ms | 19 µs, 1,075 ms |
| Compacted copy | 104.6 MB | 290 ms, 498 ms | 740 ms, 915 ms | 740 ms, 915 ms |
| Compacted copy | 524.0 MB | 1,386 ms, 2,152 ms | 3,499 ms, 4,138 ms | 3,499 ms, 4,138 ms |

Peak memory above the loaded store (406 and 2,008 MB): 0.1 to 0.6 MB for
every copy and walk, and 99.6 and 499.6 MB for the compacted copy before
exports, which held the whole file in memory.

- A backup and a desktop Copy hold the lock for an open again, and take
  about what they took before: the kernel copies the file, and the copy is
  checked only for its length. Made without a name and linked to it, with
  the folder flushed after the link, the 105 MB copy took 17 µs under the
  lock and 249 ms in all, the old copy beside it in the same runs 12 µs
  and 193 ms (three runs each, after the table's). A desktop Copy now opens the file by its
  path, checked against the one the host holds locked (about 10 µs),
  where it cloned the locked descriptor (1 to 2 µs).
- A compacted copy holds the lock 2.5 times as long as before and takes no
  memory beyond the file's: the walk and its check against the view,
  where before the rebuild in memory, which took the file's size in
  memory.
- At `0b36912` every copy of a saved version walked the view under the
  lock; a copy of a version no save wrote was copied, and checked against
  the view under the lock.
