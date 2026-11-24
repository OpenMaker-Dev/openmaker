//! Times the page file on the real file system: a tree of about a million
//! small items loaded in key order, a checkpoint after 10,000 entries of
//! one put each (its time and the bytes it writes), random reads with the
//! page cache cold and warm, and the file's size against the live pages
//! under a steady workload of puts and deletes. Run with
//! `cargo run --release -p om-store --example pagefile_timing [folder]
//! [items]`; the folder (default `target/pagefile-timing`) is removed
//! first and at the end.

use std::time::Instant;

use om_store::os::OsFs;
use om_store::page::StoreError;
use om_store::pagefile::{entry_bound, Config, EntryId, PageFile, Purpose, Reservation, TreeName};
use om_store::sys::Fs;
use om_store::tree::{self, Edit, Root, Tree};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

fn path(record: u32) -> Vec<Vec<u8>> {
    vec![vec![0x80, 0x01], vec![5], record.to_be_bytes().to_vec()]
}

fn entry(index: u64) -> EntryId {
    EntryId {
        index,
        term: 1,
        hash: om_store::page::Hash::of(&index.to_le_bytes()),
    }
}

/// Applies `edits` at `index` through a reservation of `pages`, extended by
/// as much again whenever the entry needs more (the apply that ran out
/// changed nothing). Returns the new root and the pages the entry took.
fn apply(pf: &PageFile, root: Root, index: u64, edits: &[Edit], pages: u64) -> (Root, u64) {
    let res: Reservation<'_> = match pf.reserve(index, pages, Purpose::Entry) {
        Ok(res) => res,
        Err(e) => panic!("entry {index}: {e}"),
    };
    loop {
        match tree::apply(&res, root, index, edits) {
            Ok(new) => return (new, res.taken()),
            Err(tree::Error::Store(StoreError::Full)) => res.extend(pages).unwrap(),
            Err(e) => panic!("{e}"),
        }
    }
}

fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1 << 20) as f64
}

fn main() {
    let mut args = std::env::args().skip(1);
    let folder = args
        .next()
        .unwrap_or_else(|| "target/pagefile-timing".into());
    let items: u32 = args.next().map_or(1_000_000, |n| n.parse().unwrap());
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).unwrap();
    let fs = OsFs::new(&folder);
    fs.create_root().unwrap();
    let lock = fs.lock().unwrap();
    let config = Config {
        cache_bytes: 256 << 20,
        dirty_budget: 65_536,
        ..Config::default()
    };
    let name = TreeName::new(b"file/1".to_vec()).unwrap();
    let pf = PageFile::create(&fs, &lock, config).unwrap();

    // The tree: `items` records of 16 bytes, 1,000 puts an entry, in key
    // order.
    let start = Instant::now();
    let res = pf.reserve(1, 2, Purpose::Entry).unwrap();
    let mut root = res.create_tree(&name).unwrap();
    drop(res);
    let mut index = 1;
    for chunk in (0..items).collect::<Vec<_>>().chunks(1_000) {
        index += 1;
        let edits: Vec<Edit> = chunk
            .iter()
            .map(|&r| Edit::leaf(path(r), (r as u128).to_le_bytes().to_vec()))
            .collect();
        root = apply(&pf, root, index, &edits, 64).0;
    }
    let loaded = start.elapsed();
    let report = tree::check(&pf, root).unwrap();
    println!(
        "load: {items} items in {} entries, {:.1} s; height {}, {} pages ({:.1} MiB)",
        index - 1,
        loaded.as_secs_f64(),
        report.height,
        report.pages.len(),
        mib(report.pages.len() as u64 * 16_384)
    );
    let start = Instant::now();
    let done = pf
        .checkpoint(entry(index), &[(name.clone(), root)], None)
        .unwrap();
    println!(
        "first checkpoint: {:.2} s, {} tree pages and {} meta pages, {:.1} MiB",
        start.elapsed().as_secs_f64(),
        done.pages,
        done.meta_pages,
        mib(done.bytes)
    );

    // 10,000 entries of one put each, then a checkpoint.
    let mut rng = Rng(7);
    let height = Tree::open(&pf, root).unwrap().height();
    let start = Instant::now();
    let mut taken = 0;
    for _ in 0..10_000 {
        index += 1;
        let r = (rng.next() % u64::from(items)) as u32;
        let edits = [Edit::leaf(path(r), rng.next().to_le_bytes().to_vec())];
        let bound = entry_bound(&edits, height);
        let (new, t) = apply(&pf, root, index, &edits, bound);
        root = new;
        taken += t;
    }
    let puts = start.elapsed();
    let dirty = pf.space().dirty;
    let start = Instant::now();
    let done = pf
        .checkpoint(entry(index), &[(name.clone(), root)], None)
        .unwrap();
    let took = start.elapsed();
    println!(
        "10,000 one-put entries: {:.1} us each, {taken} pages written in memory ({:.1} a put, \
         the bound reserved {} each), {dirty} waiting",
        puts.as_secs_f64() * 1e6 / 10_000.0,
        taken as f64 / 10_000.0,
        entry_bound(&[Edit::leaf(path(0), vec![0; 8])], height)
    );
    println!(
        "checkpoint after them: {:.2} s, {} tree pages and {} meta pages, {:.1} MiB written",
        took.as_secs_f64(),
        done.pages,
        done.meta_pages,
        mib(done.bytes)
    );

    // Reads with the cache cold (a new open: the store's cache is empty,
    // the system's is not), then warm.
    drop(pf);
    let (pf, _) = PageFile::open(&fs, &lock, config).unwrap();
    let tree = Tree::open(&pf, root).unwrap();
    for (what, n) in [("cold", 100_000), ("warm", 100_000)] {
        let before = pf.cache_counts();
        let start = Instant::now();
        for _ in 0..n {
            let r = (rng.next() % u64::from(items)) as u32;
            assert!(tree.get(&path(r)).unwrap().is_some());
        }
        let elapsed = start.elapsed();
        let after = pf.cache_counts();
        println!(
            "random gets, cache {what}: {:.2} us each; {} pages read from the file",
            elapsed.as_secs_f64() * 1e6 / f64::from(n),
            after.1 - before.1
        );
    }
    drop(tree);

    // A steady workload: each round 1,000 entries of 20 edits (puts of new
    // records and deletes of old ones, keeping the count), a checkpoint and
    // a reclaim. The file's pages against the pages the newest version and
    // the two checkpoints on disk reach.
    let mut low = 0u32;
    let mut high = items;
    for round in 1..=20 {
        for _ in 0..1_000 {
            index += 1;
            let mut edits = Vec::with_capacity(20);
            for _ in 0..10 {
                edits.push(Edit::leaf(path(high), vec![1; 16]));
                high += 1;
                edits.push(Edit::delete(path(low)));
                low += 1;
            }
            for _ in 0..4 {
                let r = low + (rng.next() % u64::from(high - low)) as u32;
                edits.push(Edit::leaf(path(r), rng.next().to_le_bytes().to_vec()));
            }
            root = apply(&pf, root, index, &edits, 256).0;
        }
        pf.checkpoint(entry(index), &[(name.clone(), root)], None)
            .unwrap();
        let mut freed = 0;
        loop {
            let done = pf.reclaim(1 << 20);
            freed += done.freed;
            if done.drained {
                break;
            }
        }
        if round % 5 == 0 {
            let space = pf.space();
            let live = tree::check(&pf, root).unwrap().pages.len() as u64;
            println!(
                "steady round {round}: file {} pages ({:.1} MiB), {} free, {} dying, newest \
                 version {live} pages; {freed} freed this round",
                space.end,
                mib(space.end * 16_384),
                space.free,
                space.dying
            );
        }
    }
    drop(pf);
    drop(lock);
    let _ = std::fs::remove_dir_all(&folder);
}
