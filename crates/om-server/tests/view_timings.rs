//! Timings of the reads views serve (docs/server-storage.md 5, "Views"),
//! for release builds (ignored by default):
//!
//! ```sh
//! cargo test --release -p om-server --test view_timings -- --ignored --nocapture --test-threads 1
//! ```
//!
//! A Tasks file of `fixtures/tasks.rs` with 200,000 records (`OM_VIEW_RECORDS`
//! for another count, 2,000 to 2,000,000), written under the build's
//! temporary folder and removed afterwards. Each is the median of
//! `OM_VIEW_RUNS` runs (9; 1 to 101), timed by the client:
//!
//! - two finds on Count, whose value index the file holds (op86), on a
//!   server that loaded the file with it: one value (200 records) and a
//!   range (half the table);
//! - a download of the table's records `[T, 05]` in pages (op90), of a
//!   hundred records spread over the table and of a thousand in a row
//!   (op92);
//! - a find on Name, whose value index the server builds on demand: each run
//!   on a server of its own, its folder read-only so that storing the index
//!   fails at once instead of saving the whole file.
//!
//! `OM_VIEW_PARTS` names the parts to run, of `finds`, `downloads` and
//! `demand` (all by default).

#[path = "fixtures/tasks.rs"]
mod fixture;
#[path = "fixtures/server.rs"]
mod server;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use om_collate::DateOrder;
use om_hbam::slot::uint_segment;
use om_hbam::{serial, DataType, OwnedNode, RidList};
use om_query::find;
use om_scripts::text_node;

/// The number the environment variable `name` gives, `default` without
/// one; a value that does not parse or lies outside `range` is refused.
fn env_number(name: &str, default: u64, range: std::ops::RangeInclusive<u64>) -> u64 {
    let Ok(text) = std::env::var(name) else {
        return default;
    };
    match text.parse() {
        Ok(n) if range.contains(&n) => n,
        _ => panic!(
            "{name}={text}: a whole number from {} to {}",
            range.start(),
            range.end()
        ),
    }
}

/// The Tasks tree with `count` records: Name `Task <n> ...`, Count `n % 1000`
/// and Doubled twice that.
fn tree(count: u64) -> OwnedNode {
    let mut root = fixture::tree(&[], true);
    let table = uint_segment(fixture::TABLE);
    let mut records = Vec::with_capacity(count as usize);
    let mut ids = RidList::new();
    for id in 1..=count {
        let n = id % 1000;
        let cells = [
            (
                fixture::NAME,
                format!("Task {id:06} of the generated file, for timing reads"),
            ),
            (fixture::COUNT, n.to_string()),
            (fixture::DOUBLED, (2 * n).to_string()),
        ];
        let mut children: Vec<OwnedNode> = cells
            .iter()
            .map(|(field, text)| text_node(&uint_segment(*field), text))
            .collect();
        children.sort_by(|a, b| a.segment.cmp(&b.segment));
        records.push(OwnedNode::list(serial::encode(id), children));
        ids.insert(id);
    }
    root.put(&[&table, &[5]], OwnedNode::list(vec![5], records));
    root.put(&[&table, &[0x0d]], ids.to_node(Vec::new()));
    let mut last = serial::encode(count);
    last.insert(0, last.len() as u8);
    root.put(&[&table, &[0]], OwnedNode::leaf(Vec::new(), last));
    root
}

/// The folder the file is written to, removed when dropped.
struct Folder(PathBuf);

impl Folder {
    fn new() -> Folder {
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("view-timings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Folder(root)
    }

    fn set_read_only(&self, read_only: bool) {
        let mut permissions = std::fs::metadata(&self.0).unwrap().permissions();
        permissions.set_readonly(read_only);
        std::fs::set_permissions(&self.0, permissions).unwrap();
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        self.set_read_only(false);
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn median(mut times: Vec<Duration>) -> Duration {
    times.sort();
    times[times.len() / 2]
}

fn report(what: &str, times: Vec<Duration>) {
    let all: Vec<String> = times
        .iter()
        .map(|t| format!("{:.1}", t.as_secs_f64() * 1000.0))
        .collect();
    println!(
        "{what}: median {:.1} ms (runs: {} ms)",
        median(times).as_secs_f64() * 1000.0,
        all.join(", ")
    );
}

fn finder_program(file_id: u32, field: u64, data_type: DataType, criterion: &str) -> Vec<u8> {
    let finders: Vec<Vec<u8>> = find::finders(
        fixture::TABLE,
        field,
        data_type,
        criterion,
        DateOrder::MonthDayYear,
    )
    .unwrap()
    .into_iter()
    .map(|finder| finder.bytes)
    .collect();
    find::program(file_id, fixture::TABLE, &finders, None)
}

#[test]
#[ignore = "a timing for release builds; writes a large file under the build's temporary folder"]
fn downloads_and_finds_on_a_large_file() {
    // At least 2,000 records, so that the thousand in a row fit in the
    // table's second half; at most two million (about 1.5 GB as a tree).
    let count = env_number("OM_VIEW_RECORDS", 200_000, 2_000..=2_000_000);
    let runs = env_number("OM_VIEW_RUNS", 9, 1..=101) as usize;
    // What the finds on Count give, Count being the record id modulo 1000.
    let equal = (1..=count).filter(|id| id % 1000 == 500).count();
    let below = (1..=count).filter(|id| id % 1000 < 500).count();
    let folder = Folder::new();
    let file = fixture::TempFile(folder.0.join(fixture::FILE));
    let started = Instant::now();
    fixture::write_tree(&file.0, &tree(count));
    println!(
        "{count} records, {:.1} MB, written in {:.1} s",
        std::fs::metadata(&file.0).unwrap().len() as f64 / 1e6,
        started.elapsed().as_secs_f64()
    );
    let table = uint_segment(fixture::TABLE);

    // The first find on Count builds its value index, which is stored and
    // saved; the server measured loads the file with it, as a file with
    // indexes is served.
    let (port, handle) = server::start_configured_with_handle(&file, |server| server);
    let (mut session, file_id) = server::logged_in(port, "timings");
    let count_find = finder_program(file_id, fixture::COUNT, DataType::Number, "==500");
    let reply = session.query(&count_find).unwrap();
    let found = find::found(&reply.slots).unwrap().len();
    assert_eq!(found, equal);
    session.disconnect().unwrap();
    handle.shutdown(Duration::from_secs(5));
    let (port, handle) = server::start_configured_with_handle(&file, |server| server);
    let (mut session, file_id) = server::logged_in(port, "timings");
    let count_find = finder_program(file_id, fixture::COUNT, DataType::Number, "==500");

    let parts = std::env::var("OM_VIEW_PARTS").unwrap_or_else(|_| "finds,downloads,demand".into());
    let part = |name: &str| parts.split(',').any(|p| p == name);
    let time = |what: &str, run: &mut dyn FnMut()| {
        let times = (0..runs)
            .map(|_| {
                let started = Instant::now();
                run();
                started.elapsed()
            })
            .collect();
        report(what, times);
    };
    // Finds first: a download frees much memory, which allocations made
    // after it reuse.
    if part("finds") {
        time("find on the indexed Count, ==500", &mut || {
            let reply = session.query(&count_find).unwrap();
            assert_eq!(find::found(&reply.slots).unwrap().len(), found);
        });
        let range_find = finder_program(file_id, fixture::COUNT, DataType::Number, "<500");
        time("find on the indexed Count, <500", &mut || {
            let reply = session.query(&range_find).unwrap();
            assert_eq!(find::found(&reply.slots).unwrap().len(), below);
        });
    }
    if !part("downloads") {
        session.disconnect().unwrap();
        handle.shutdown(Duration::from_secs(5));
        if part("demand") {
            demand(&folder, &file, runs);
        }
        return;
    }
    time("download of [T, 05] in pages (op90)", &mut || {
        let reply = session.download(&[&table, &[5]]).unwrap();
        assert!(reply.slots.len() > count as usize * 50);
    });
    // A hundred records spread over the table, as a list view fetches them.
    let ids: Vec<Vec<u8>> = (1..=100).map(|n| serial::encode(n * count / 100)).collect();
    let keys: Vec<&[u8]> = ids.iter().map(Vec::as_slice).collect();
    time("download of 100 records of [T, 05] (op92)", &mut || {
        let reply = session.download_list(&[&table, &[5]], &keys, 0).unwrap();
        assert!(reply.slots.len() > 100 * 50);
    });
    // A thousand records in a row, as a long list view fetches them.
    let ids: Vec<Vec<u8>> = (1..=1000).map(|n| serial::encode(count / 2 + n)).collect();
    let keys: Vec<&[u8]> = ids.iter().map(Vec::as_slice).collect();
    time(
        "download of 1,000 records in a row of [T, 05] (op92)",
        &mut || {
            let reply = session.download_list(&[&table, &[5]], &keys, 0).unwrap();
            assert!(reply.slots.len() > 1000 * 50);
        },
    );
    session.disconnect().unwrap();
    handle.shutdown(Duration::from_secs(5));
    if part("demand") {
        demand(&folder, &file, runs);
    }
}

/// The finds on Name whose index each server builds on demand.
fn demand(folder: &Folder, file: &fixture::TempFile, runs: usize) {
    folder.set_read_only(true);
    let name_find = |file_id| {
        let name = "==Task 000123 of the generated file, for timing reads";
        finder_program(file_id, fixture::NAME, DataType::Text, name)
    };
    let mut times = Vec::new();
    for _ in 0..runs {
        let (port, handle) = server::start_configured_with_handle(file, |server| server);
        let (mut session, file_id) = server::logged_in(port, "timings");
        let program = name_find(file_id);
        let started = Instant::now();
        let reply = session.query(&program).unwrap();
        times.push(started.elapsed());
        assert_eq!(find::found(&reply.slots).unwrap().len(), 1);
        session.disconnect().unwrap();
        handle.shutdown(Duration::from_secs(5));
    }
    report("find on Name, its index built on demand", times);
}
