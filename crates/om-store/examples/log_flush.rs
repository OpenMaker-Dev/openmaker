//! What making a batch durable costs on the real file system
//! (`docs/performance/store-log-flush.md`): appends and bytes per second
//! through `Log::flush_batch` for batches of 1, 10 and 100 small entries,
//! and the two flushes a batch costs measured apart.
//!
//! ```sh
//! cargo run --release -p om-store --example log_flush -- <folder> [seconds] [runs]
//! ```
//!
//! `<folder>` must exist, on the disk to measure; the example makes store
//! folders in it and leaves them for the caller to remove. Each case runs
//! `runs` times (default 5) for `seconds` (default 3); the median run is
//! reported, with the fastest and slowest.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use om_store::log::{Config, Log, Position, BATCH_HEADER, ENTRY_OVERHEAD};
use om_store::os::{OsClock, OsFs, OsRandom};
use om_store::state::State;
use om_store::sys::{Clock, FileOptions, Fs};

/// The payload of each entry: a small change.
const PAYLOAD: usize = 100;

fn store(folder: &str, name: &str) -> (OsFs, State) {
    let fs = OsFs::new(PathBuf::from(folder).join(name));
    fs.create_root().expect("the store folder");
    let lock = fs.lock().expect("the lock");
    let state = State::create(&fs, &OsRandom::new(), lock).expect("a new state");
    (fs, state)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let folder = args.next().expect("a folder on the disk to measure");
    let seconds: u64 = args.next().map_or(3, |s| s.parse().expect("seconds"));
    let runs: usize = args.next().map_or(5, |s| s.parse().expect("runs"));
    let run = Duration::from_secs(seconds);
    let clock = OsClock::new();

    println!(
        "batch | batches/s | appends/s | payload MB/s | log MB/s | ms per batch (fastest, slowest)"
    );
    println!("---:|---:|---:|---:|---:|---:");
    for per in [1usize, 10, 100] {
        let mut results = Vec::new();
        for r in 0..runs {
            let (fs, state) = store(&folder, &format!("flush-{per}-{r}"));
            let fs: Arc<dyn Fs> = Arc::new(fs);
            let (mut log, _) =
                Log::open(fs, state, Config::DEFAULT, Position::START).expect("the log");
            log.prepare().expect("the first segment");
            let payload = [7u8; PAYLOAD];
            let batch = |log: &mut Log| {
                for _ in 0..per {
                    log.append(1, 1, 1, &payload).expect("append");
                }
                log.flush_batch(None).expect("flush");
            };
            for _ in 0..20 {
                batch(&mut log);
            }
            let start = clock.monotonic();
            let mut batches = 0u64;
            while clock.monotonic() - start < run {
                batch(&mut log);
                batches += 1;
            }
            let secs = (clock.monotonic() - start).as_secs_f64();
            results.push(batches as f64 / secs);
        }
        results.sort_by(f64::total_cmp);
        let rate = results[results.len() / 2];
        let appends = rate * per as f64;
        let log_bytes =
            rate * (BATCH_HEADER as f64 + per as f64 * (ENTRY_OVERHEAD as usize + PAYLOAD) as f64);
        println!(
            "{per} | {rate:.0} | {appends:.0} | {:.3} | {:.3} | {:.3} ({:.3}, {:.3})",
            appends * PAYLOAD as f64 / 1e6,
            log_bytes / 1e6,
            1e3 / rate,
            1e3 / results[results.len() - 1],
            1e3 / results[0],
        );
    }

    // The two flushes apart.
    let (fs, mut state) = store(&folder, "apart");
    let file = fs
        .create("segment", FileOptions::PLAIN)
        .expect("a segment file");
    file.allocate(64 << 20).expect("its space");
    file.flush().expect("flushed");
    drop(file);
    let bytes = vec![7u8; BATCH_HEADER as usize + ENTRY_OVERHEAD as usize + PAYLOAD];
    let mut n = 0u64;
    let mut time = |what: &str, f: &mut dyn FnMut(u64)| {
        let mut each = Vec::new();
        for _ in 0..runs {
            let start = clock.monotonic();
            let mut done = 0u64;
            while clock.monotonic() - start < run {
                f(n);
                n += 1;
                done += 1;
            }
            each.push((clock.monotonic() - start).as_secs_f64() * 1e3 / done as f64);
        }
        each.sort_by(f64::total_cmp);
        println!(
            "{what}: {:.3} ms ({:.3}, {:.3})",
            each[each.len() / 2],
            each[0],
            each[each.len() - 1]
        );
    };
    let sync = fs.open("segment", FileOptions::SYNC).expect("open");
    time("a synchronous write of a batch of one entry", &mut |n| {
        sync.write_at(n * bytes.len() as u64 % (60 << 20), &bytes)
            .expect("write")
    });
    time("the same, followed by a flush of the file", &mut |n| {
        sync.write_at(n * bytes.len() as u64 % (60 << 20), &bytes)
            .expect("write");
        sync.flush().expect("flush");
    });
    let plain = fs.open("segment", FileOptions::PLAIN).expect("open");
    time("a plain write and a flush of the file", &mut |n| {
        plain
            .write_at(n * bytes.len() as u64 % (60 << 20), &bytes)
            .expect("write");
        plain.flush().expect("flush");
    });
    time("a marker advance in state", &mut |n| {
        state
            .update(|v| {
                v.marker.index = n + 1;
                v.marker.hash[0] = n as u8;
            })
            .expect("update");
    });
}
