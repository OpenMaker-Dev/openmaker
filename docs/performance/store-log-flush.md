# Making a log batch durable

What `om_store::log::Log::flush_batch` costs on the real file system: how
many batches, appends and bytes per second one caller gets when it appends
a batch of small entries and waits for it to be durable, and what the two
flushes a batch costs (server-storage.md 6.3) take apart. There is no
group commit yet (one caller, one batch at a time), so these are the
latencies a commit pays, not the store's throughput under many clients.

## Reproduce

```sh
cargo run --release -p om-store --example log_flush -- <folder> [seconds] [runs]
```

`<folder>` must exist on the disk to measure. Each case runs `runs` times
(default 5) for `seconds` (default 3), each time in a new store folder; the
median run is reported with the fastest and slowest. The example leaves
its store folders in `<folder>`.

## Method

- Each entry has a 100-byte payload, so it takes 164 bytes in the log; a
  batch adds a 96-byte header. Segments are the default 64 MiB, made
  before the timed loop, so no batch waits for an allocation.
- A **batch** is `per` appends and one `flush_batch`: one synchronous
  write of the batch to the current segment (`O_DSYNC`), then a marker
  advance in `state` (one write of 178 bytes and one `fdatasync`).
- **Apart**: the same synchronous write of a one-entry batch to a 64 MiB
  file made beforehand; that write followed by `fdatasync` as well; a plain
  write followed by `fdatasync`; and a marker advance alone.

## Results

2026-11-24, branch `t3/implement-store-change-log` (pull request for
#708), the development VM: ext4 on a virtual disk shared with other
agents' builds. The disk was busy: `/proc/pressure/io` showed 19 to 31%
of the time with some task stalled on I/O in the 10 seconds before and
after the run. Two-second runs, seven of each.

| Entries per batch | Batches/s | Appends/s | Payload MB/s | Log MB/s | ms per batch (fastest, slowest) |
|---:|---:|---:|---:|---:|---:|
| 1 | 927 | 927 | 0.09 | 0.24 | 1.08 (0.95, 1.52) |
| 10 | 1,313 | 13,133 | 1.31 | 2.28 | 0.76 (0.67, 1.13) |
| 100 | 459 | 45,864 | 4.59 | 7.57 | 2.18 (1.74, 3.77) |

| Apart | ms (fastest, slowest) |
|---|---:|
| synchronous write of a one-entry batch | 0.47 (0.40, 0.61) |
| the same, then `fdatasync` | 0.61 (0.49, 0.88) |
| plain write, then `fdatasync` | 0.50 (0.45, 0.78) |
| marker advance in `state` | 0.62 (0.46, 0.81) |

- A small batch costs about one synchronous write and one marker advance,
  about 1 ms on this disk: the log's flush and `state`'s are each about
  half of it.
- Larger batches amortize both: 100 entries per batch gave about 46,000
  appends per second from one caller. The group commit that shares one
  batch among waiting commits (with the engine) is what turns this into
  throughput under load.
- 10 entries per batch came out faster than 1 in four of the five sets
  run that evening (the table is the last); it is unexplained and may be
  the shared disk.
- The order of the three single-write variants was not stable across
  the five sets: in one, a plain write and `fdatasync` took 3.0 ms against
  0.38 ms for a synchronous write and 0.66 ms for a synchronous write and
  `fdatasync`; in another, all three were about 0.5 ms. The synchronous
  write alone was the fastest in three sets and within 0.08 ms of the
  fastest in the other two. The log relies on it alone
  (server-storage-format.md 3.8), since a flush after it adds a second
  device flush. A quiet machine and a real drive are needed before these
  are compared further.
