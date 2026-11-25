# Exporting a hosted file

How long an export of a hosted file takes on the file engine
(`om_server::engine::export`, docs/server-storage.md 5, "Exports"), how
much memory it takes beyond the loaded file, and how long it holds the
store's read lock, against a save of the same tree (`Store::save`), which
builds the whole file in memory and writes it under the store's write lock.

## Reproduce

One step per process, so that each peak is its own:

```sh
cargo test --release -j 4 -p om-server --lib --no-run
BIN=target/release/deps/om_server-<hash>
for step in generate export save; do
  systemd-run --user --scope -p MemoryMax=8G -p MemorySwapMax=0 \
    env OM_EXPORT_RECORDS=191000 OM_EXPORT_FILE=$PWD/target/measure/f.fmp12 \
    OM_EXPORT_STEP=$step \
    $BIN --ignored --exact engine::export::measurement::export_measurement --nocapture
done
```

`generate` writes the file again before each `export` and `save`, since a
save replaces it. `open` times what a backup held the lock for before
exports: one open of the saved file. Remove the generated file afterwards.

## Method

- **The files** are the Tasks fixture (`om-server/tests/fixtures/tasks.rs`)
  with one table of `OM_EXPORT_RECORDS` records, eight 64-byte text cells
  each, no indexes: the shape #673's save measurements used.
- **`export` and `save`** load the file into a store, put one new node in
  the tree (so that the export is written from the view, not copied from
  the file as it was loaded), reset the process's peak resident memory
  (`/proc/self/clear_refs`, 5) and run the one operation. The peak is
  `VmHWM` afterwards less the resident memory before it. The export's time
  is split into the part under the store's read lock (written and checked,
  `engine::export::prepare`) and the part after (flushed, renamed and the
  folder flushed). The save's time is all under the write lock: the
  rebuild, the write, the flush and the rename.
- **Runs:** three of each, one process at a time, on a Linux VM with 16
  x86-64 cores and an ext4 disk, shared with other work, on 2026-11-25.
  Each column of the table is the median of its three runs.

## Results

| File MB | Records | Export ms | of which under the read lock | Export peak MB | Save ms | Save peak MB |
|---:|---:|---:|---:|---:|---:|---:|
| 10.4 | 19,000 | 144 | 63 | 0.5 | 138 | 10.0 |
| 104.6 | 191,000 | 863 | 674 | 0.8 | 581 | 99.8 |
| 524.0 | 957,000 | 4,013 | 3,444 | 0.7 | 2,469 | 499.8 |

The loaded store took 47, 406 and 2,007 MB. An export's memory does not
grow with the file; a save's grows by the file's size. An export takes
longer than a save, by 4 % at 10 MB, 49 % at 105 MB and 63 % at 524 MB:
it writes and reads back page by page and walks the view twice. Reading
the export back through a 256 KB buffer instead of a page at a time took
the time under the lock for 105 MB from 674 to 664 ms, so the time is in
the walks, the encoding and the comparison, and the buffer was not kept.

**The lock a backup holds.** Before exports, a backup held the store's
read lock for one open of the saved file: 7 µs. Now it holds it while each
file's export is written and checked: 63 ms, 674 ms and 3.4 s for the three
files, during which a write to that file waits. Before the export it also
takes the lock once for the same open (7 µs), for the file's permissions
and saved version.
