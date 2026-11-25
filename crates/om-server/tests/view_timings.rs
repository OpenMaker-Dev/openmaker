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
//!
//! `logins_and_thumbnails` times logins on a Tasks file with a thousand
//! accounts more and thumbnails of a picture in its library, each the
//! median of `OM_VIEW_RUNS` runs:
//!
//! ```sh
//! cargo test --release -p om-server --test view_timings -- --ignored --nocapture logins_and_thumbnails
//! ```
//!
//! `engines` hosts a generated file on the server binary on either engine
//! (`--engine file` or `store`) and times its start, its memory and a
//! client's reads; its doc has the steps
//! (docs/performance/store-engine-file.md, "Hosting on the store engine").

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
    let text = match std::env::var(name) {
        Ok(text) => text,
        Err(std::env::VarError::NotPresent) => return default,
        Err(std::env::VarError::NotUnicode(text)) => panic!(
            "{name}={text:?}: a whole number from {} to {}",
            range.start(),
            range.end()
        ),
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

/// The folder the file is written to, removed when dropped: one of its own
/// for each timing (`name`), so that timings run at once do not share it.
struct Folder(PathBuf);

impl Folder {
    fn new(name: &str) -> Folder {
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("view-timings-{name}-{}", std::process::id()));
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
    report_in(what, times, 1000.0, "ms");
}

/// [`report`] in `unit`s, `scale` of them to a second.
fn report_in(what: &str, times: Vec<Duration>, scale: f64, unit: &str) {
    let all: Vec<String> = times
        .iter()
        .map(|t| format!("{:.1}", t.as_secs_f64() * scale))
        .collect();
    println!(
        "{what}: median {:.1} {unit} (runs: {} {unit})",
        median(times).as_secs_f64() * scale,
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
    let folder = Folder::new("large");
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

/// How many accounts [`logins_and_thumbnails`]'s file has besides the
/// fixture's.
const ACCOUNTS: u64 = 1_000;

/// Account `n` of [`ACCOUNTS`]: its record key, after every key the
/// fixture's accounts use, its name and its password.
fn generated_account(n: u64) -> (Vec<u8>, String, String) {
    (
        serial::encode(0x100 + n),
        format!("User {n:04}"),
        format!("password {n}"),
    )
}

/// The Tasks tree with [`ACCOUNTS`] more accounts in [Full Access], each
/// stored as the fixture stores Admin, and Photo's picture in Tasks'
/// library ([`fixture::add_photo`]).
fn accounts_tree() -> OwnedNode {
    use om_crypto::account::StoredPassword;
    let mut root = fixture::tree(&[], true);
    for n in 0..ACCOUNTS {
        let (key, name, password) = generated_account(n);
        let salt = [(n % 251) as u8; 10];
        let members = [
            (6u8, StoredPassword::encode(&password, salt)),
            (0x0b, vec![1, 1]),
        ];
        for (member, value) in members {
            root.put(
                &[&[23], &[1], &[5], &key, &[member]],
                OwnedNode::leaf(Vec::new(), value),
            );
        }
        root.put(&[&[23], &[1], &[5], &key, &[16]], text_node(&[], &name));
    }
    fixture::add_photo(&mut root);
    root
}

/// A login (op80) on a file with a thousand accounts, as the last of them
/// (the account looked up after every other), and refused for an unknown
/// account and for a wrong password, each the mean of 200 logins on one
/// connection; and a thumbnail (op96) of Photo's picture, in microseconds, once cached and
/// once rendered for a box not asked for before. The login limits' clock
/// is moved on an hour before each refused login, so none is refused for
/// the limits.
#[test]
#[ignore = "a timing for release builds; writes a file under the build's temporary folder"]
fn logins_and_thumbnails() {
    let report_us = |what: &str, times| report_in(what, times, 1e6, "us");
    use om_protocol::container::{self, DownloadThumbnail, ThumbnailReply};
    use om_protocol::LoginEnvironment;

    const LOGINS: u32 = 200;
    let runs = env_number("OM_VIEW_RUNS", 9, 1..=101) as usize;
    let folder = Folder::new("logins");
    let file = fixture::TempFile(folder.0.join(fixture::FILE));
    fixture::write_tree(&file.0, &accounts_tree());
    let (port, handle) = server::start_configured_with_handle(&file, |server| server);
    handle.stop_login_clock();
    let (mut session, file_id) = server::logged_in(port, "timings");
    let environment = LoginEnvironment::for_remote_file("127.0.0.1", fixture::FILE);
    let (_, last, password) = generated_account(ACCOUNTS - 1);
    let mut login = |account: &str, password: &str, expected: u16| {
        let times = (0..runs)
            .map(|_| {
                let mut spent = Duration::ZERO;
                for _ in 0..LOGINS {
                    if expected != 0 {
                        handle.advance_login_clock(Duration::from_secs(3600));
                    }
                    let started = Instant::now();
                    let reply = session
                        .login(account, password, fixture::INTERNAL_CREATION, &environment)
                        .unwrap();
                    spent += started.elapsed();
                    assert_eq!(reply.error_code, expected, "{account}");
                }
                spent / LOGINS
            })
            .collect();
        times
    };
    report_us(
        "login as the last of 1,000 accounts (op80)",
        login(&last, &password, 0),
    );
    report_us(
        "login as an unknown account (op80)",
        login("Nobody", &password, 212),
    );
    report_us(
        "login with a wrong password (op80)",
        login(&last, "wrong", 212),
    );
    session
        .login(&last, &password, fixture::INTERNAL_CREATION, &environment)
        .unwrap();

    let mut thumbnail = |width: u32| {
        let request = DownloadThumbnail {
            reserved: [0; 4],
            height: 32,
            width,
            member: vec![serial::encode(fixture::PHOTO_MEMBER)],
            table: vec![uint_segment(fixture::TABLE)],
            flags: 0,
            offset: 0,
            max_chunk: 0x000f_ffe6,
        };
        let started = Instant::now();
        let reply = session
            .perform(&container::build_download_thumbnail(10, file_id, &request))
            .unwrap();
        let elapsed = started.elapsed();
        let reply = container::parse_download_thumbnail_reply(&reply).unwrap();
        assert!(
            matches!(reply, ThumbnailReply::Chunk { status: 0, .. }),
            "{reply:?}"
        );
        elapsed
    };
    thumbnail(64);
    let cached = (0..runs).map(|_| thumbnail(64)).collect();
    report_us("thumbnail of Photo, cached (op96)", cached);
    let rendered = (0..runs as u32).map(|run| thumbnail(65 + run)).collect();
    report_us("thumbnail of Photo, rendered (op96)", rendered);
    session.disconnect().unwrap();
    handle.shutdown(Duration::from_secs(5));
}

/// The text `export_measurement`'s generated file holds in field `field`
/// of record `record`.
fn generated_text(record: u64, field: u64) -> String {
    format!("{record:012}-{field:02}-")
        .chars()
        .cycle()
        .take(64)
        .collect()
}

/// The field `name` of process `pid`'s status, in MB.
fn process_mb(pid: u32, name: &str) -> f64 {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    status
        .lines()
        .find_map(|l| l.strip_prefix(name))
        .and_then(|v| v.trim().trim_end_matches(" kB").trim().parse::<f64>().ok())
        .unwrap()
        / 1024.0
}

/// A measurement, not a test: the server binary on either engine
/// (docs/server-storage.md 11) hosting a file `export_measurement`
/// generates (`OM_TIMING_FILE`, named `Tasks.fmp12`; one table, eight
/// 64-byte text cells a record). For a release build, one run at a time,
/// under a memory cap (docs/performance/store-engine-file.md):
///
/// ```sh
/// OM_TIMING_FILE=$PWD/target/m/a/Tasks.fmp12 OM_TIMING_STEP=index \
///   cargo test --release -p om-server --test view_timings -- --ignored --nocapture engines
/// ```
///
/// The steps: `generate`, a file of `OM_TIMING_RECORDS` records as
/// `export_measurement` writes it, with the table's record list (which that
/// file leaves as the fixture's two records) listing every record, and
/// Creator holding [`COMMON_CREATOR`] in nine records of ten; `index`, the
/// file hosted by the file engine and found on Name and on Creator, which
/// builds their value indexes and stores them in the file, as a file with
/// indexes is served; `serve`, the file hosted by the engine
/// `OM_TIMING_ENGINE` (`file` or `store`, its folder `OM_TIMING_STORE`; the
/// state folder `OM_TIMING_STATE`), the file's folder made read-only first
/// so that the file engine's save of an index built on demand fails at once
/// rather than write the whole file. It reports the time from the process's
/// start to its listening line (the files hosted), its resident memory once
/// hosted and idle, then the client's times of a find on Name through its
/// stored index (median of 5), a find for duplicates on Name, which reads
/// every key and entry of its index (median of 3), a find on Code whose index is built on
/// demand (the first, then once more), a sort of every record by Name, and
/// a download of the table's records `[T, 05]` in pages (op90), and the
/// process's peak resident memory and its resident memory after them. After
/// the finds on Name, an ExistsInIndex of [`BULK_KEYS`] keys no record
/// holds, each just below the key most records hold (median of 5), with the
/// peak resident memory during it alone. `OM_TIMING_SERVER` names another
/// `om-server` binary to measure; `OM_TIMING_READS=indexes` stops after the
/// ExistsInIndex, for the reads of stored indexes alone.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "a measurement for release builds; set OM_TIMING_FILE and OM_TIMING_STEP"]
fn engines() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};

    fn var(name: &str) -> String {
        std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
    }
    let file = PathBuf::from(var("OM_TIMING_FILE"));
    assert_eq!(file.file_name().unwrap(), fixture::FILE);
    let folder = file.parent().unwrap().to_path_buf();
    let step = var("OM_TIMING_STEP");
    if step == "generate" {
        // As `export_measurement` generates it, but for the record list.
        let count: u64 = var("OM_TIMING_RECORDS").parse().unwrap();
        let mut tree = fixture::tree(&[], true);
        let list = (1..=count)
            .map(|id| {
                let cells = (1..=8)
                    .map(|f| match f == fixture::CREATOR && id % 10 != 0 {
                        true => text_node(&uint_segment(f), COMMON_CREATOR),
                        false => text_node(&uint_segment(f), &generated_text(id, f)),
                    })
                    .collect();
                OwnedNode::list(serial::encode(id), cells)
            })
            .collect();
        let table = uint_segment(fixture::TABLE);
        tree.put(&[&table, &[0x05]], OwnedNode::list(vec![0x05], list));
        // The table's record list and last id, which that file keeps as
        // the fixture's (two records), so that finds see every record.
        let mut ids = RidList::new();
        for id in 1..=count {
            ids.insert(id);
        }
        tree.put(&[&table, &[0x0d]], ids.to_node(Vec::new()));
        let mut last = serial::encode(count);
        last.insert(0, last.len() as u8);
        tree.put(&[&table, &[0]], OwnedNode::leaf(Vec::new(), last));
        std::fs::create_dir_all(&folder).unwrap();
        fixture::write_tree(&file, &tree);
        println!(
            "generated {count} records: {:.1} MB",
            std::fs::metadata(&file).unwrap().len() as f64 / 1e6
        );
        return;
    }
    let engine = match step.as_str() {
        "index" => "file".to_string(),
        "serve" => var("OM_TIMING_ENGINE"),
        other => panic!("OM_TIMING_STEP={other}: generate, index or serve"),
    };
    let state = std::env::var("OM_TIMING_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| folder.with_extension("state"));
    std::fs::create_dir_all(&state).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let read_only = |read_only: bool| {
        let mut permissions = std::fs::metadata(&folder).unwrap().permissions();
        permissions.set_readonly(read_only);
        std::fs::set_permissions(&folder, permissions).unwrap();
    };
    if step == "serve" {
        read_only(true);
    }
    let binary = std::env::var_os("OM_TIMING_SERVER")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_om-server")));
    let mut command = Command::new(binary);
    command
        .args(["--bind", &format!("127.0.0.1:{port}"), "--engine", &engine])
        .arg("--state-dir")
        .arg(&state);
    if engine == "store" {
        command.arg("--store-dir").arg(var("OM_TIMING_STORE"));
    }
    /// The server process, killed when dropped, a panic included.
    struct Running(std::process::Child);
    impl Drop for Running {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let started = Instant::now();
    let mut server = Running(
        command
            .arg(&file)
            .env("RUST_LOG", "info")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let pid = server.0.id();
    let mut log = BufReader::new(server.0.stderr.take().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        assert_ne!(log.read_line(&mut line).unwrap(), 0, "the server ended");
        if line.contains("imported into the store")
            || line.contains("held by the store")
            || line.contains("the store engine")
        {
            print!("server: {line}");
        }
        if line.contains("listening on") {
            break;
        }
    }
    let hosted = started.elapsed();
    // The rest of the log is read, so that the server never waits on it.
    std::thread::spawn(move || {
        let mut line = String::new();
        while log.read_line(&mut line).is_ok_and(|n| n > 0) {
            line.clear();
        }
    });
    let (mut session, file_id) = server::logged_in(port, "timings");
    let table = uint_segment(fixture::TABLE);
    let records = |reply: &om_protocol::QueryReply| find::found(&reply.slots).unwrap().len();
    let name = format!("=={}", generated_text(123, fixture::NAME));
    let by_name = finder_program(file_id, fixture::NAME, DataType::Text, &name);
    if step == "index" {
        let started = Instant::now();
        assert_eq!(records(&session.query(&by_name).unwrap()), 1);
        println!(
            "Name's index built, stored and saved in {:.1} s",
            started.elapsed().as_secs_f64()
        );
        let creator = format!("=={}", generated_text(10, fixture::CREATOR));
        let by_creator = finder_program(file_id, fixture::CREATOR, DataType::Text, &creator);
        let started = Instant::now();
        assert_eq!(records(&session.query(&by_creator).unwrap()), 1);
        println!(
            "Creator's index built, stored and saved in {:.1} s",
            started.elapsed().as_secs_f64()
        );
        session.disconnect().unwrap();
        return;
    }
    std::thread::sleep(Duration::from_secs(2));
    println!(
        "{engine} engine: hosted {:.2} s after the start; resident {:.0} MB once hosted (peak {:.0} MB)",
        hosted.as_secs_f64(),
        process_mb(pid, "VmRSS:"),
        process_mb(pid, "VmHWM:")
    );
    let timed = |run: &mut dyn FnMut()| {
        let started = Instant::now();
        run();
        started.elapsed()
    };
    let times = (0..5)
        .map(|_| {
            timed(&mut || {
                assert_eq!(records(&session.query(&by_name).unwrap()), 1);
            })
        })
        .collect();
    report("find on Name, its index stored", times);
    // A find for duplicates on Name: every key of its stored value index
    // listed and every entry's records read (a text range would read the
    // word index, which the file does not store, and time its build).
    let by_duplicates = finder_program(file_id, fixture::NAME, DataType::Text, "!");
    let times = (0..3)
        .map(|_| timed(&mut || assert_eq!(records(&session.query(&by_duplicates).unwrap()), 0)))
        .collect();
    report(
        "find for duplicates on Name, every key and entry read",
        times,
    );
    // ExistsInIndex of keys no record holds, each just below the key of the
    // value most records hold, so that each lookup misses next to its entry
    // (the value's key is checked first: a hit, its records read once).
    // Before the find on Code: on the file engine an index built on demand
    // that its read-only folder cannot store is built again for each later
    // query, which would be timed too.
    let common = om_collate::text_key(COMMON_CREATOR);
    let exists = |session: &mut om_session::Session, keys: &[Vec<u8>]| {
        let reply = session
            .query(&exists_program(file_id, fixture::CREATOR, keys))
            .unwrap();
        let tree = om_hbam::KeyTree::from_slots(om_hbam::SlotReader::new(&reply.slots)).unwrap();
        match tree.node(&[&[1], &[0]]).and_then(|node| node.value) {
            Some([1, 1]) => true,
            Some([1, 0]) => false,
            other => panic!("ExistsInIndex answered {other:02x?}"),
        }
    };
    assert!(exists(&mut session, std::slice::from_ref(&common)));
    let last = common.iter().rposition(|&b| b > 0).unwrap();
    let missing: Vec<Vec<u8>> = (0..BULK_KEYS)
        .map(|i| {
            let mut key = common[..=last].to_vec();
            key[last] -= 1;
            key.extend(i.to_be_bytes());
            key
        })
        .collect();
    // The peak during it alone (proc(5), clear_refs), and the process's
    // peak so far kept for the one reported after the reads.
    let peak = process_mb(pid, "VmHWM:");
    std::fs::write(format!("/proc/{pid}/clear_refs"), "5").unwrap();
    let resident = process_mb(pid, "VmRSS:");
    let times = (0..5)
        .map(|_| timed(&mut || assert!(!exists(&mut session, &missing))))
        .collect();
    report(
        &format!("ExistsInIndex of {BULK_KEYS} missing keys below the common Creator"),
        times,
    );
    println!(
        "{engine} engine: resident {resident:.0} MB before it, peak during it {:.0} MB",
        process_mb(pid, "VmHWM:")
    );
    if std::env::var("OM_TIMING_READS").is_ok_and(|reads| reads == "indexes") {
        session.disconnect().unwrap();
        return;
    }
    let code = format!("=={}", generated_text(456, fixture::CODE));
    let by_code = finder_program(file_id, fixture::CODE, DataType::Text, &code);
    let first = timed(&mut || assert_eq!(records(&session.query(&by_code).unwrap()), 1));
    let again = timed(&mut || assert_eq!(records(&session.query(&by_code).unwrap()), 1));
    report(
        "find on Code, its index built on demand: first",
        vec![first],
    );
    report(
        "find on Code, its index built on demand: again",
        vec![again],
    );
    // SortList of every record by Name (query-program.md 10).
    let leaf = |key: u8, value: Vec<u8>| OwnedNode::leaf(vec![key], value);
    let mut spec = vec![0, 0, 0, 1];
    spec.extend([3, 2, 0x80, 0x01, 2, 1, fixture::NAME as u8, 0, 1]);
    spec.extend([0; 10]);
    let mut children = vec![
        leaf(0x00, vec![14]),
        OwnedNode::list(vec![0x01], vec![leaf(0xff, Vec::new())]),
        leaf(0x02, vec![1, 3, 1, 1]),
        leaf(0x04, file_id.to_be_bytes().to_vec()),
        leaf(0x05, vec![2, 0x80, 0x01]),
        leaf(0x0a, spec),
        leaf(0x0b, vec![2, 0x80, 0x01]),
    ];
    children.sort_by(|a, b| a.segment.cmp(&b.segment));
    let sort = OwnedNode::list(
        Vec::new(),
        vec![OwnedNode::list(
            vec![1],
            vec![OwnedNode::list(serial::encode(1), children)],
        )],
    )
    .to_wire()
    .unwrap();
    // A program past its limits (`om_query::program::limits`) is refused
    // with the error slot, which is reported rather than failed.
    let mut refused = false;
    let sorted = timed(&mut || {
        let reply = session.query(&sort).unwrap();
        refused = reply.slots.len() < 64;
    });
    let what = match refused {
        true => "sort of every record by Name, refused (past a program limit)",
        false => "sort of every record by Name",
    };
    report(what, vec![sorted]);
    let downloaded = timed(&mut || {
        let reply = session.download(&[&table, &[5]]).unwrap();
        assert!(reply.slots.len() > 1000);
    });
    report("download of [T, 05] in pages (op90)", vec![downloaded]);
    println!(
        "{engine} engine: resident {:.0} MB after the reads, peak {:.0} MB",
        process_mb(pid, "VmRSS:"),
        process_mb(pid, "VmHWM:").max(peak)
    );
    session.disconnect().unwrap();
    drop(server);
    read_only(false);
}

/// What Creator (field 6) holds in nine records of ten of a file `engines`
/// generates (the others hold their own text): one value whose index entry
/// lists most of the table's records.
const COMMON_CREATOR: &str = "Admin";

/// How many keys `engines`' ExistsInIndex looks up.
const BULK_KEYS: u32 = 10_000;

/// ExistsInIndex (op 18, query-program.md 8.4) of `keys` in the value index
/// of `field` of Tasks, its answer at `[3, 1]`.
fn exists_program(file_id: u32, field: u64, keys: &[Vec<u8>]) -> Vec<u8> {
    use om_query::program::{leaf, opcode};
    let mut leaves = vec![
        OwnedNode::leaf(vec![leaf::OPCODE], vec![opcode::EXISTS_IN_INDEX]),
        OwnedNode::leaf(vec![leaf::FILE_ID], file_id.to_be_bytes().to_vec()),
        OwnedNode::leaf(vec![leaf::SRC], vec![1, 2, 1, 1]),
        OwnedNode::leaf(vec![leaf::FIELD_KEY], vec![1, field as u8]),
        OwnedNode::leaf(vec![leaf::TABLE], vec![2, 0x80, 0x01]),
        OwnedNode::leaf(vec![leaf::DEST], vec![1, 3, 1, 1]),
    ];
    leaves.sort_by(|a, b| a.segment.cmp(&b.segment));
    let mut keys: Vec<OwnedNode> = keys
        .iter()
        .map(|key| OwnedNode::leaf(key.clone(), Vec::new()))
        .collect();
    keys.sort_by(|a, b| a.segment.cmp(&b.segment));
    keys.dedup_by(|a, b| a.segment == b.segment);
    OwnedNode::list(
        Vec::new(),
        vec![
            OwnedNode::list(vec![1], vec![OwnedNode::list(serial::encode(1), leaves)]),
            OwnedNode::list(vec![2], vec![OwnedNode::list(vec![1], keys)]),
        ],
    )
    .to_wire()
    .unwrap()
}
