//! What a group costs on the real file system (`docs/server-storage.md`
//! section 6.9): a group of N parts of one 4 KiB put each, staged and
//! closed. It prints the time to stage (every part appended, a flush after
//! every 100 and at the end, and the checkpoints the dirty budget asked
//! for), the time to close (the
//! closing entry appended and flushed), the pages each wrote, the pages
//! the parts held, the page file's size, and the process's peak memory
//! after each. Run one N per process:
//! `cargo run --release -p om-store --example group_cost -- <parts>
//! [folder]`; the folder (default `target/group-cost`) is removed first
//! and at the end.

use std::sync::Arc;
use std::time::{Duration, Instant};

use om_store::entry::{Change, FileEvent, Group, GroupPath, Payload, Step};
use om_store::os::{OsFs, OsRandom};
use om_store::store::{Config, Store, WriteError};
use om_store::sys::Fs;
use om_store::tree::Edit;

const VALUE: usize = 4 << 10;
const PAGE: u64 = 16 << 10;
/// Parts appended between flushes while staging, as a client sending them
/// in batches would.
const FLUSH_EVERY: u64 = 100;

/// The process's peak resident memory, in KiB (`VmHWM`).
fn peak_kib() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
        .unwrap_or(0)
}

/// Appends `payload`, checkpointing first whenever the pages waiting for
/// one are past their budget; returns the pages the entry wrote.
fn append(store: &mut Store, file: u64, payload: &Payload, checkpoints: &mut u64) -> u64 {
    loop {
        match store.append(file, payload) {
            Ok(appended) => return appended.pages,
            Err(WriteError::CheckpointNeeded(_)) => {
                store.flush().expect("flush");
                store.checkpoint(None).expect("checkpoint");
                *checkpoints += 1;
            }
            Err(e) => panic!("append: {e}"),
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let parts: u64 = args
        .next()
        .and_then(|a| a.parse().ok())
        .expect("the number of parts");
    let folder = args.next().unwrap_or_else(|| "target/group-cost".into());
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).expect("the folder");
    let fs: Arc<dyn Fs> = Arc::new(OsFs::new(&folder));
    let config = Config {
        log: Default::default(),
        pages: Default::default(),
    };
    let (mut store, _) = Store::create(fs, &OsRandom::new(), config).expect("create");
    let file = store.next_index();
    let added = Payload::File(FileEvent::Added {
        name: b"f".to_vec(),
        flags: 0,
        page0: Vec::new(),
    });
    store.append(file, &added).expect("add");
    store.flush().expect("flush");
    store.checkpoint(None).expect("checkpoint");
    let before = store.space();
    let group = store.next_index();
    let declared = vec![b"g".to_vec()];
    let mut value = vec![0u8; VALUE];
    let mut checkpoints = 0;
    let mut staged_pages = 0;
    let stage = Instant::now();
    for i in 0..parts {
        for (j, b) in value.iter_mut().enumerate() {
            *b = (i as usize).wrapping_mul(31).wrapping_add(j) as u8;
        }
        let mut path = declared.clone();
        path.push((i as u32).to_be_bytes().to_vec());
        let step = if i == 0 {
            Step::Open(vec![GroupPath {
                file,
                path: declared.clone(),
            }])
        } else {
            Step::Part
        };
        let payload = Payload::Change(Change {
            group: Some(Group { id: group, step }),
            edits: vec![Edit::leaf(path, value.clone())],
            ..Change::default()
        });
        staged_pages += append(&mut store, file, &payload, &mut checkpoints);
        if (i + 1) % FLUSH_EVERY == 0 {
            store.flush().expect("flush");
        }
    }
    store.flush().expect("flush");
    let staged = stage.elapsed();
    let peak_staged = peak_kib();
    let held = store.space().groups;
    let end_staged = store.space().end;
    store.checkpoint(None).expect("checkpoint");
    let close = Payload::Change(Change {
        group: Some(Group {
            id: group,
            step: Step::Close,
        }),
        ..Change::default()
    });
    let closing = Instant::now();
    let mut close_checkpoints = 0;
    let close_pages = append(&mut store, file, &close, &mut close_checkpoints);
    store.flush().expect("flush");
    let closed = closing.elapsed();
    let peak_closed = peak_kib();
    let after = store.space();
    let ms = |d: Duration| d.as_secs_f64() * 1e3;
    println!(
        "parts {parts}: stage {:.1} ms ({checkpoints} checkpoints), {staged_pages} pages written; \
         held {held} pages ({:.1} MiB); page file {end_staged} pages ({:.1} MiB, {} at the start); \
         peak {:.1} MiB",
        ms(staged),
        (held * PAGE) as f64 / (1 << 20) as f64,
        (end_staged * PAGE) as f64 / (1 << 20) as f64,
        before.end,
        peak_staged as f64 / 1024.0,
    );
    println!(
        "parts {parts}: close {:.1} ms ({close_checkpoints} checkpoints), {close_pages} pages \
         written; held after {}; peak {:.1} MiB",
        ms(closed),
        after.groups,
        peak_closed as f64 / 1024.0,
    );
    drop(store);
    let _ = std::fs::remove_dir_all(&folder);
}
