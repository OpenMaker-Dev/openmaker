//! What opening a hosted file with N related files costs (#740): the
//! connections and sessions the host sees, the keepalive threads, the
//! Opens, and the time until the file is open and until its calculation
//! schema has its related files. On loopback, and through a relay that
//! delays every chunk 20 ms each way, as a link with that latency would.
//! Synthetic Tasks files on one `om-server`.
//!
//! ```text
//! cargo test -p om-provider --test related_connections -- --ignored --nocapture
//! ```

#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
#[path = "common/related.rs"]
mod related;

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use om_data::DataProvider;
use related::{reference, Host};

/// The tests here count this process's keepalive threads: one at a time.
static ALONE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Root, naming `n` related files, and the files.
fn solution(n: u64) -> Vec<(String, om_hbam::OwnedNode)> {
    let mut root = fixture::tree(&[], true);
    let mut files = Vec::new();
    for i in 1..=n {
        let name = format!("File {i}");
        reference(
            &mut root,
            (0xd1_0000 + i, &format!("Related {i}")),
            i,
            &name,
        );
        files.push((format!("{name}.fmp12"), fixture::tree(&[], true)));
    }
    let mut all = vec![("Root.fmp12".to_string(), root)];
    all.extend(files);
    all
}

/// Threads of this process whose name starts as `name` does (Linux keeps
/// 15 bytes of it), counted through `/proc`; `None` where there is none to
/// count them by (macOS, Windows).
fn threads(name: &str) -> Option<usize> {
    let name = &name[..name.len().min(15)];
    std::fs::read_dir("/proc/self/task").ok().map(|tasks| {
        tasks
            .filter_map(|task| std::fs::read_to_string(task.ok()?.path().join("comm")).ok())
            .filter(|comm| comm.trim_end() == name)
            .count()
    })
}

/// A loopback relay to `port` that delays every chunk `delay` each way;
/// its port.
fn relay(port: u16, delay: Duration) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let local = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for client in listener.incoming() {
            let Ok(client) = client else { continue };
            let Ok(server) = TcpStream::connect(("127.0.0.1", port)) else {
                continue;
            };
            let _ = client.set_nodelay(true);
            let _ = server.set_nodelay(true);
            let directions = [
                (client.try_clone().unwrap(), server.try_clone().unwrap()),
                (server, client),
            ];
            for (mut from, mut to) in directions {
                let (sent, delayed) = mpsc::channel::<(Instant, Vec<u8>)>();
                std::thread::spawn(move || {
                    let mut buffer = vec![0; 64 * 1024];
                    while let Ok(n @ 1..) = from.read(&mut buffer) {
                        if sent.send((Instant::now(), buffer[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                });
                std::thread::spawn(move || {
                    for (at, bytes) in delayed {
                        let due = at + delay;
                        let now = Instant::now();
                        if due > now {
                            std::thread::sleep(due - now);
                        }
                        if to.write_all(&bytes).is_err() {
                            break;
                        }
                    }
                    let _ = to.shutdown(Shutdown::Write);
                });
            }
        }
    });
    local
}

/// One row: Root with `n` related files opened, directly or through a
/// relay delaying `delay` each way.
fn measure(n: u64, delay: Option<Duration>) -> String {
    let files = solution(n);
    let tag = format!("connections-{n}-{}", delay.map_or(0, |d| d.as_millis()));
    let host = Host::new(
        &tag,
        files
            .iter()
            .map(|(name, tree)| (name.as_str(), tree.clone()))
            .collect(),
    );
    let mut connection = host.connection("Root.fmp12");
    if let Some(delay) = delay {
        connection.port = relay(host.port, delay);
    }
    let mut provider = host.provider();
    let started = Instant::now();
    provider.open(&connection).unwrap();
    let opened = started.elapsed();
    let schema = provider.calc_schema().unwrap().unwrap();
    let hydrated = started.elapsed();
    assert_eq!(
        schema.tables().count(),
        2 * (n as usize + 1),
        "Root's tables and every related file's: {:?}",
        provider.related_file_warnings()
    );
    let sessions = host.handle.clients().len();
    let keepalives =
        threads("om-session keepalive").map_or_else(|| "n/a".to_owned(), |n| n.to_string());
    let (opens, logins) = host.captures.opens_and_logins();
    provider.close();
    let connections = std::fs::read_dir(&host.captures.0).unwrap().count();
    format!(
        "| {n} | {} | {connections} | {sessions} | {keepalives} | {} | {} | {:.2} | {:.2} |",
        delay.map_or("loopback".to_string(), |d| format!(
            "{} ms each way",
            d.as_millis()
        )),
        opens.values().sum::<usize>(),
        logins.values().sum::<usize>(),
        opened.as_secs_f64(),
        hydrated.as_secs_f64(),
    )
}

/// A hosted file's related files on its host open on its connection, as
/// FileMaker Pro opens them (network-protocol.md 5.5): one connection and
/// session, one keepalive, an Open and a Login per file.
#[test]
fn related_files_share_the_opened_files_connection() {
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let files = solution(8);
    let host = Host::new(
        "share-8",
        files
            .iter()
            .map(|(name, tree)| (name.as_str(), tree.clone()))
            .collect(),
    );
    let mut provider = host.open("Root.fmp12");
    let schema = provider.calc_schema().unwrap().unwrap();
    assert_eq!(schema.tables().count(), 2 * 9);
    assert_eq!(host.handle.clients().len(), 1, "one session");
    // One keepalive, where the threads can be counted.
    if let Some(keepalives) = threads("om-session keepalive") {
        assert_eq!(keepalives, 1);
    }
    let (opens, logins) = host.captures.opens_and_logins();
    assert_eq!((opens.len(), logins.len()), (9, 9), "{opens:?} {logins:?}");
    assert!(opens.values().chain(logins.values()).all(|n| *n == 1));
    provider.close();
    assert_eq!(std::fs::read_dir(&host.captures.0).unwrap().count(), 1);
}

/// With Root and two related files open on one connection, its keepalive
/// sends what FileMaker Pro 26.0.3 sent with two files open: one Tickle
/// record, session 1, not one per file (network-protocol.md 6.1).
#[test]
fn the_tickle_carries_one_record_for_the_connection() {
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let files = solution(2);
    let host = Host::new(
        "tickle-2",
        files
            .iter()
            .map(|(name, tree)| (name.as_str(), tree.clone()))
            .collect(),
    );
    let mut provider = host
        .provider()
        .with_keepalive(Some(Duration::from_millis(100)));
    provider.open(&host.connection("Root.fmp12")).unwrap();
    provider.calc_schema().unwrap();
    assert_eq!(host.handle.clients()[0].files.len(), 3);
    let deadline = Instant::now() + Duration::from_secs(10);
    let tickles = loop {
        let tickles = host.captures.tickles();
        if !tickles.is_empty() {
            break tickles;
        }
        assert!(Instant::now() < deadline, "no Tickle was sent");
        std::thread::sleep(Duration::from_millis(50));
    };
    for records in &tickles {
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(records[0].session_id, 1, "{records:?}");
    }
    provider.close();
}

/// On demand (#983): building Root's schema opens no related file and
/// warns of none; a calculation reading one file's occurrence opens that
/// file only, and the schema grows with the ids it gave kept.
#[test]
fn related_files_open_when_something_reads_them() {
    use om_data::host::CalcNeed;
    use std::sync::Arc;
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut files = solution(6);
    fixture::index_layouts(&mut files[0].1);
    fixture::index_layout_definitions(&mut files[0].1);
    // Listed metadata can arrive, or settle as absent, between related
    // growth calls. Neither change may be lost when the root is reused.
    files[0].1.remove(&[&[4], &[5], &[2]]);
    let host = Host::new(
        "demand-6",
        files
            .iter()
            .map(|(name, tree)| (name.as_str(), tree.clone()))
            .collect(),
    );
    let mut provider = host.provider().with_related_on_demand(true);
    provider.open(&host.connection("Root.fmp12")).unwrap();
    let schema = provider.calc_schema().unwrap().unwrap();
    let formula = |schema: &om_data::host::CalcSchema| {
        Arc::clone(
            schema
                .field(fixture::OCCURRENCE, fixture::DOUBLED)
                .unwrap()
                .formula
                .as_ref()
                .unwrap(),
        )
    };
    let original = formula(&schema);
    let opened = || {
        let (opens, logins) = host.captures.opens_and_logins();
        assert_eq!(opens, logins, "one Login per Open");
        opens.into_keys().collect::<Vec<_>>()
    };
    assert_eq!(opened(), ["Root.fmp12"]);
    for i in 1..=6 {
        assert!(schema.is_pending(0xd1_0000 + i), "File {i} waits");
    }
    assert!(provider.related_file_warnings().is_empty());
    assert!(!provider.take_grown_schema());

    // A calculation reads File 3's occurrence: File 3 opens, alone.
    let read = |provider: &mut om_provider::NativeProvider, occurrence| {
        provider
            .calc_data(&[CalcNeed::Occurrence { occurrence }])
            .unwrap();
    };
    read(&mut provider, 0xd1_0003);
    assert!(provider.take_grown_schema());
    let grown = provider.calc_schema().unwrap().unwrap();
    assert!(
        Arc::ptr_eq(&original, &formula(&grown)),
        "root not decoded again"
    );
    let three = grown.table_of(0xd1_0003).expect("File 3's Tasks").id;
    assert!(grown.is_pending(0xd1_0001) && grown.is_pending(0xd1_0005));
    assert_eq!(opened(), ["File 3.fmp12", "Root.fmp12"]);

    // Another file opens after it; File 3's table keeps its id.
    read(&mut provider, 0xd1_0005);
    let again = provider.calc_schema().unwrap().unwrap();
    assert!(Arc::ptr_eq(&original, &formula(&again)));
    assert_eq!(again.table_of(0xd1_0003).map(|t| t.id), Some(three));
    assert!(again.table_of(0xd1_0005).is_some());
    assert_eq!(opened(), ["File 3.fmp12", "File 5.fmp12", "Root.fmp12"]);

    // Read again, an open file opens nothing.
    assert!(provider.take_grown_schema());
    read(&mut provider, 0xd1_0003);
    assert!(!provider.take_grown_schema());
    assert_eq!(opened().len(), 3);

    // A member supplies new root metadata; an absent member must settle
    // too. Both invalidate the decoded root before its next import.
    provider
        .calc_data(&[CalcNeed::Layout { layout: 1 }])
        .unwrap();
    let with_layout = provider.calc_schema().unwrap().unwrap();
    assert!(with_layout.layout_named("Tasks").unwrap().layout.is_some());
    assert!(!with_layout.is_layout_pending(1));
    let loaded = formula(&with_layout);
    assert!(!Arc::ptr_eq(&original, &loaded));
    provider
        .calc_data(&[CalcNeed::Layout { layout: 2 }])
        .unwrap();
    let settled = provider.calc_schema().unwrap().unwrap();
    assert!(!settled.is_layout_pending(2));
    let absent = formula(&settled);
    assert!(!Arc::ptr_eq(&loaded, &absent));

    read(&mut provider, 0xd1_0001);
    let final_schema = provider.calc_schema().unwrap().unwrap();
    assert!(Arc::ptr_eq(&absent, &formula(&final_schema)));
    assert!(!final_schema.is_layout_pending(1) && !final_schema.is_layout_pending(2));
    assert!(final_schema.layout_named("Tasks").unwrap().layout.is_some());
    assert_eq!(final_schema.table_of(0xd1_0003).unwrap().id, three);
    assert_eq!(
        final_schema.table_of(0xd1_0005).unwrap().id,
        again.table_of(0xd1_0005).unwrap().id
    );
    let page = provider.records(fixture::LAYOUT as u32, 0, 10).unwrap();
    let doubled = page
        .fields
        .iter()
        .position(|f| f.name == "Doubled")
        .unwrap();
    assert_eq!(
        page.records
            .iter()
            .map(|r| r.values[doubled].as_deref())
            .collect::<Vec<_>>(),
        [Some("6"), Some("10")]
    );
    // A generic schema drop must not carry the decoded root into the new
    // metadata lifetime, even when the definitions happen to be unchanged.
    provider.database_schema().unwrap();
    let refreshed = provider.calc_schema().unwrap().unwrap();
    assert!(!Arc::ptr_eq(&absent, &formula(&refreshed)));
    assert_eq!(refreshed.table_of(0xd1_0003).unwrap().id, three);
    assert_eq!(
        opened(),
        ["File 1.fmp12", "File 3.fmp12", "File 5.fmp12", "Root.fmp12"]
    );
    for file in ["File 1.fmp12", "File 3.fmp12", "File 5.fmp12"] {
        let catalogs: Vec<_> = host
            .captures
            .transactions()
            .into_iter()
            .filter_map(|(name, t)| {
                if name != file
                    || t.first().copied() != Some(om_protocol::Opcode::DownloadWithLock as u8)
                {
                    return None;
                }
                let path = om_protocol::parse_download_request(&t).unwrap().key_path;
                (path.len() == 2 && path[1] == [3])
                    .then(|| om_hbam::slot::be_uint(&path[0]).unwrap())
            })
            .collect();
        assert_eq!(
            catalogs,
            [fixture::TABLE],
            "one demanded catalog, no write-only Notes: {file}"
        );
    }
    provider.close();
}

/// A script's destination keeps the caller's demand policy: preparing its
/// first schema must not open files that no layout or calculation has read.
#[test]
fn a_peer_keeps_related_files_pending_until_they_are_read() {
    use om_data::host::CalcNeed;
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut files = solution(2);
    files.push(("Launcher.fmp12".into(), fixture::tree(&[], true)));
    let host = Host::new(
        "peer-demand",
        files
            .iter()
            .map(|(name, tree)| (name.as_str(), tree.clone()))
            .collect(),
    );
    let mut launcher = host.provider().with_related_on_demand(true);
    launcher.open(&host.connection("Launcher.fmp12")).unwrap();
    let mut peer = launcher.open_peer("Root.fmp12").unwrap();
    let schema = peer.calc_schema().unwrap().unwrap();
    assert!(schema.is_pending(0xd1_0001));
    assert!(schema.is_pending(0xd1_0002));
    let opened = || {
        let (opens, logins) = host.captures.opens_and_logins();
        assert_eq!(opens, logins, "one Login per Open");
        assert!(opens.values().all(|count| *count == 1));
        opens.into_keys().collect::<Vec<_>>()
    };
    assert_eq!(opened(), ["Launcher.fmp12", "Root.fmp12"]);

    peer.calc_data(&[CalcNeed::Occurrence {
        occurrence: 0xd1_0001,
    }])
    .unwrap();
    let grown = peer.calc_schema().unwrap().unwrap();
    assert!(!grown.is_pending(0xd1_0001));
    assert!(grown.is_pending(0xd1_0002));
    assert_eq!(opened(), ["File 1.fmp12", "Launcher.fmp12", "Root.fmp12"]);
    let table = grown.table_of(0xd1_0001).unwrap().id;
    let data = peer
        .calc_data(&[CalcNeed::Record { table, record: 1 }])
        .unwrap();
    assert!(matches!(
        data.records[&(table, 1)]
            .as_ref()
            .unwrap()
            .get(fixture::NAME, 1),
        Some(om_data::calc::Value::Text(value)) if value.as_str() == "Write tests"
    ));
    let catalogs: Vec<_> = host
        .captures
        .transactions()
        .into_iter()
        .filter(|(file, t)| {
            file == "File 1.fmp12"
                && t.first().copied() == Some(om_protocol::Opcode::DownloadWithLock as u8)
        })
        .map(|(_, t)| om_protocol::parse_download_request(&t).unwrap().key_path)
        .filter(|path| path.len() == 2 && path[1] == [3])
        .collect();
    assert_eq!(
        catalogs,
        [vec![om_hbam::slot::uint_segment(fixture::TABLE), vec![3]]]
    );
    peer.close();
    launcher.close();
}

/// On demand, a related set opens every file of its relationship path, an
/// intermediate one too (#983's review): Root's Tasks relate to File 1's,
/// which relate to File 2's, and the ids of File 2's records related to a
/// Root record open both files.
#[test]
fn a_related_read_opens_the_files_of_its_whole_path() {
    use om_data::host::CalcNeed;
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut files = solution(3);
    fixture::add_relationship(
        &mut files[0].1,
        1,
        (fixture::OCCURRENCE, fixture::NAME),
        0,
        (0xd1_0001, fixture::NAME),
    );
    fixture::add_relationship(
        &mut files[0].1,
        2,
        (0xd1_0001, fixture::NAME),
        0,
        (0xd1_0002, fixture::NAME),
    );
    let host = Host::new(
        "demand-path",
        files
            .iter()
            .map(|(name, tree)| (name.as_str(), tree.clone()))
            .collect(),
    );
    let mut provider = host.provider().with_related_on_demand(true);
    provider.open(&host.connection("Root.fmp12")).unwrap();
    provider.calc_schema().unwrap();
    provider
        .calc_data(&[CalcNeed::RelatedIds {
            from: fixture::OCCURRENCE,
            record: 1,
            to: 0xd1_0002,
        }])
        .unwrap();
    let (opens, _) = host.captures.opens_and_logins();
    assert_eq!(
        opens.into_keys().collect::<Vec<_>>(),
        ["File 1.fmp12", "File 2.fmp12", "Root.fmp12"],
        "the intermediate file opens too, File 3 does not"
    );
    let schema = provider.calc_schema().unwrap().unwrap();
    assert!(schema.table_of(0xd1_0001).is_some() && schema.table_of(0xd1_0002).is_some());
    assert!(schema.is_pending(0xd1_0003));
    provider.close();
}

/// On demand, the root's layout catalog is read as FileMaker Pro 26 reads
/// it (#1018): the index at open, a layout's member when first needed
/// (#1019). The schema lists both of Root's layouts from the index, layout
/// 2's member waits, and a calculation reading its objects reads it.
#[test]
fn a_layout_member_is_read_when_first_needed() {
    use om_data::host::CalcNeed;
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut files = solution(1);
    // Every file FileMaker makes has the layout catalog's index.
    fixture::index_layouts(&mut files[0].1);
    fixture::index_layout_definitions(&mut files[0].1);
    let host = Host::new(
        "demand-layouts",
        files
            .iter()
            .map(|(name, tree)| (name.as_str(), tree.clone()))
            .collect(),
    );
    let mut provider = host.provider().with_related_on_demand(true);
    provider.open(&host.connection("Root.fmp12")).unwrap();
    let layouts: Vec<u32> = provider.layouts().unwrap().iter().map(|l| l.id).collect();
    assert_eq!(layouts, [1, 2], "both listed from the index");
    let schema = provider.calc_schema().unwrap().unwrap();
    assert!(schema.is_layout_pending(1) && schema.is_layout_pending(2));
    let notes = schema.layout_named("Notes").expect("listed").clone();
    assert_eq!(
        notes.occurrence,
        Some(fixture::NOTES_OCCURRENCE),
        "from the index"
    );
    assert!(notes.layout.is_none());
    let catalog = |host: &Host| {
        host.captures
            .downloaded_tables()
            .into_iter()
            .filter(|(file, first)| file == "Root.fmp12" && *first == 4)
            .count()
    };
    assert_eq!(catalog(&host), 1, "the index only");

    provider
        .calc_data(&[CalcNeed::Layout { layout: 2 }])
        .unwrap();
    assert!(provider.take_grown_schema());
    let grown = provider.calc_schema().unwrap().unwrap();
    assert!(!grown.is_layout_pending(2) && grown.is_layout_pending(1));
    assert!(grown.layout_named("Notes").unwrap().layout.is_some());
    assert_eq!(catalog(&host), 2, "the index and layout 2's member");
    // Its member read once, layout 2 is decoded without another download.
    provider.render_layout(2).unwrap();
    assert_eq!(catalog(&host), 2);
    // Its theme, 1, which this file's theme catalog does not hold and
    // whose index lists no members (#1129): asked for once.
    provider.render_layout(2).unwrap();
    let themes = host
        .captures
        .downloaded_tables()
        .into_iter()
        .filter(|(file, first)| file == "Root.fmp12" && *first == 6)
        .count();
    assert_eq!(themes, 2, "the index, and theme 1 once");
    provider.close();
}

/// On demand, a related file's table field catalog `[T, 3]` is read when
/// first needed, as FileMaker Pro reads one (#772, stage B;
/// network-protocol.md 5.5). File 1 opens for Root's read of its Tasks with
/// that table's catalog only. Root's occurrence of File 1's Notes is then
/// bound to a table without fields, and waits. A record read of that table
/// reads Notes' catalog once, then its record, and every id stays.
#[test]
fn a_related_tables_fields_are_read_when_first_needed() {
    use om_data::host::CalcNeed;
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Root's occurrence of File 1's Notes, through File 1's data source.
    const NOTES_THERE: u64 = 0xd1_0101;
    let mut files = solution(1);
    fixture::add_occurrence(
        &mut files[0].1,
        NOTES_THERE,
        "Related 1 Notes",
        fixture::NOTES,
    );
    files[0].1.put(
        &[
            &[3],
            &[17],
            &[5],
            &om_hbam::slot::uint_segment(NOTES_THERE),
            &[2],
        ],
        om_hbam::OwnedNode::leaf(
            Vec::new(),
            [related::key(1), related::key(fixture::NOTES)].concat(),
        ),
    );
    let host = Host::new(
        "lazy-fields",
        files
            .iter()
            .map(|(name, tree)| (name.as_str(), tree.clone()))
            .collect(),
    );
    // File 1's downloads of `table`'s field catalog `[T, 3]`.
    let catalogs = |table: u64| {
        host.captures
            .transactions()
            .into_iter()
            .filter(|(file, t)| {
                file == "File 1.fmp12"
                    && t.first().copied() == Some(om_protocol::Opcode::DownloadWithLock as u8)
            })
            .map(|(_, t)| om_protocol::parse_download_request(&t).unwrap().key_path)
            .filter(|path| {
                path.len() == 2 && om_hbam::slot::be_uint(&path[0]) == Some(table) && path[1] == [3]
            })
            .count()
    };
    let mut provider = host.provider().with_related_on_demand(true);
    provider.open(&host.connection("Root.fmp12")).unwrap();
    provider
        .calc_data(&[CalcNeed::Occurrence {
            occurrence: 0xd1_0001,
        }])
        .unwrap();
    let schema = provider.calc_schema().unwrap().unwrap();
    assert!(!schema.is_pending(0xd1_0001));
    let tasks = schema.table_of(0xd1_0001).unwrap().id;
    let notes = schema
        .table_of(NOTES_THERE)
        .expect("bound to File 1's Notes");
    assert!(notes.fields.is_empty(), "its fields not read");
    let notes = notes.id;
    assert!(schema.is_pending(NOTES_THERE), "bound, and still waiting");
    assert_eq!(catalogs(fixture::TABLE), 1, "Tasks' catalog, at the open");
    assert_eq!(catalogs(fixture::NOTES), 0, "Notes' not yet");

    // A record of Notes, read by its table's id: its catalog is read first,
    // rather than the record being read as having no fields.
    let data = provider
        .calc_data(&[CalcNeed::Record {
            table: notes,
            record: 1,
        }])
        .unwrap();
    let grown = provider.calc_schema().unwrap().unwrap();
    assert!(!grown.is_pending(NOTES_THERE));
    assert_eq!(
        grown.table_of(NOTES_THERE).unwrap().id,
        notes,
        "the same id"
    );
    assert_eq!(grown.table_of(0xd1_0001).unwrap().id, tasks, "the same id");
    assert!(grown.field(NOTES_THERE, fixture::BODY).is_some());
    assert_eq!(catalogs(fixture::NOTES), 1, "Notes' catalog, once");
    assert_eq!(catalogs(fixture::TABLE), 1);
    let record = data
        .records
        .get(&(notes, 1))
        .cloned()
        .flatten()
        .expect("Notes' record 1");
    assert!(record.get(fixture::BODY, 1).is_some(), "{record:?}");
    provider.close();
}

/// FileMaker Pro 26 reads a file's node `[2]` once, before Login, and uses
/// it for the session (network-protocol.md 5.6.9b). The root's open and a
/// related file's open each read it once, for the Login challenge, and the
/// calculation schema takes what it holds from that read: Root's document
/// preferences `[2, 18]` name its OnFirstWindowOpen script.
#[test]
fn a_files_node_2_is_read_once_before_login() {
    use om_data::host::CalcNeed;
    use om_hbam::{serial, OwnedNode};
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut files = solution(1);
    // The document preferences of a file whose OnFirstWindowOpen script is
    // script 1: the default options, field view order, OnFirstWindowOpen,
    // no OnLastWindowClose, and layout 1 as the unchecked "Switch to
    // layout" (file-metadata.md 3.1).
    let key = serial::encode(1);
    let mut preferences = 0x8403u32.to_be_bytes().to_vec();
    preferences.extend([1, 1, key.len() as u8 + 1, key.len() as u8]);
    preferences.extend(&key);
    preferences.extend([0, 0x02, 0x01, 0x01]);
    files[0]
        .1
        .put(&[&[2], &[0x18]], OwnedNode::leaf(Vec::new(), preferences));
    let host = Host::new(
        "status-once",
        files
            .iter()
            .map(|(name, tree)| (name.as_str(), tree.clone()))
            .collect(),
    );
    let mut provider = host.provider().with_related_on_demand(true);
    provider.open(&host.connection("Root.fmp12")).unwrap();
    let schema = provider.calc_schema().unwrap().unwrap();
    assert_eq!(
        schema
            .file_options
            .as_ref()
            .and_then(|o| o.first_window_open),
        Some(1),
        "from the node read before Login"
    );
    provider
        .calc_data(&[CalcNeed::Occurrence {
            occurrence: 0xd1_0001,
        }])
        .unwrap();
    let reads = |file: &str| {
        host.captures
            .downloaded_tables()
            .into_iter()
            .filter(|(name, first)| name == file && *first == 2)
            .count()
    };
    assert_eq!(reads("Root.fmp12"), 1, "the root's [2], once");
    assert_eq!(reads("File 1.fmp12"), 1, "the related file's [2], once");
    provider.close();
}

/// On demand, the root's theme catalog is read as FileMaker Pro 26 reads
/// it (#1129): its index `[6, 1]` at open, and a theme's member
/// `[6, 5, T]` once, when a layout naming it is first decoded. Root has
/// themes 1 and 2. Layout 1 names theme 1; layout 2 names theme 3, which
/// the index does not list. Each layout decodes as it does from the whole
/// catalog, which an open that is not on demand reads.
#[test]
fn a_themes_member_is_read_when_a_layout_naming_it_is_first_decoded() {
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut files = solution(1);
    fixture::index_layouts(&mut files[0].1);
    fixture::add_themes(&mut files[0].1, &[1, 2]);
    let notes = om_hbam::slot::uint_segment(fixture::NOTES_LAYOUT);
    files[0].1.put(
        &[&[4], &[5], &notes, &[0x0b]],
        om_hbam::OwnedNode::leaf(Vec::new(), vec![1, 3]),
    );
    let host = Host::new(
        "demand-themes",
        files
            .iter()
            .map(|(name, tree)| (name.as_str(), tree.clone()))
            .collect(),
    );
    let reads = |host: &Host| {
        host.captures
            .downloaded_tables()
            .into_iter()
            .filter(|(file, first)| file == "Root.fmp12" && *first == 6)
            .count()
    };

    let mut whole = host.provider();
    whole.open(&host.connection("Root.fmp12")).unwrap();
    let expected = [
        whole.render_layout(1).unwrap().unwrap(),
        whole.render_layout(2).unwrap().unwrap(),
    ];
    whole.close();
    assert_eq!(reads(&host), 1, "the whole catalog, read once");
    assert_eq!(expected[0].layout.theme_key, Some(1));
    assert_ne!(
        expected[0].layout.theme_styles,
        Default::default(),
        "layout 1 has its theme's styles"
    );
    assert_eq!(expected[1].layout.theme_styles, Default::default());

    let mut provider = host.provider().with_related_on_demand(true);
    provider.open(&host.connection("Root.fmp12")).unwrap();
    assert_eq!(reads(&host), 2, "the index only");
    let first = provider.render_layout(1).unwrap().unwrap();
    assert_eq!(reads(&host), 3, "the index and theme 1's member");
    assert_eq!(first, expected[0], "as from the whole catalog");
    provider.render_layout(1).unwrap();
    assert_eq!(reads(&host), 3, "theme 1 is read once");
    let second = provider.render_layout(2).unwrap().unwrap();
    assert_eq!(second, expected[1]);
    assert_eq!(
        reads(&host),
        3,
        "theme 3, not listed in the index, is not asked for"
    );
    provider.close();
}

/// The calculation schema of a hosted file opened on demand lists its
/// scripts from the script catalog's index `[17, 1]`, as FileMaker Pro 26
/// reads it at open, not from every script's steps (#1052). A script to
/// run is read alone, its member `[17, 5, S]` (#1053). The whole catalog,
/// read for the Scripts menu, then serves the schema's later builds, and
/// corrects a separator, which the index lists as a script.
#[test]
fn the_calculation_schema_lists_scripts_from_the_script_index() {
    use om_scripts::{Script, ScriptKind};
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut files = solution(1);
    let root = &mut files[0].1;
    let separator = Script {
        id: 3,
        name: "-".into(),
        kind: ScriptKind::Separator,
        ..Default::default()
    };
    for script in [
        fixture::script(2, "Report", vec![fixture::set_variable("$x", "1")]),
        separator,
        fixture::script(4, "Close", vec![fixture::set_variable("$y", "2")]),
    ] {
        let node = script.encode();
        root.put(&[&[17], &[5], &node.segment.clone()], node);
    }
    fixture::index_scripts(root);
    let host = Host::new(
        "demand-script-index",
        files
            .iter()
            .map(|(name, tree)| (name.as_str(), tree.clone()))
            .collect(),
    );
    let mut provider = host.provider().with_related_on_demand(true);
    provider.open(&host.connection("Root.fmp12")).unwrap();
    let listed = |schema: &om_data::host::CalcSchema| -> Vec<(String, ScriptKind)> {
        schema
            .scripts()
            .iter()
            .map(|s| (s.name.clone(), s.kind))
            .collect()
    };
    let script_reads = |host: &Host| {
        host.captures
            .downloaded_tables()
            .into_iter()
            .filter(|(file, first)| file == "Root.fmp12" && *first == 17)
            .count()
    };
    let schema = provider.calc_schema().unwrap().unwrap();
    let named = |kinds: [ScriptKind; 3]| -> Vec<(String, ScriptKind)> {
        ["Report", "-", "Close"]
            .into_iter()
            .map(String::from)
            .zip(kinds)
            .collect()
    };
    assert_eq!(
        listed(&schema),
        named([ScriptKind::Script; 3]),
        "from the index"
    );
    assert_eq!(script_reads(&host), 1, "the index only");

    // A member read alone, as a script that runs is: its steps, and what
    // it is (a separator, here); none for an id the file does not have.
    let report = provider.script(2).unwrap().unwrap();
    assert_eq!((report.name.as_str(), report.steps.len()), ("Report", 1));
    let dash = provider.script(3).unwrap().unwrap();
    assert_eq!(dash.kind, ScriptKind::Separator);
    assert!(provider.script(99).unwrap().is_none());
    assert_eq!(script_reads(&host), 4, "the index and three members");

    let catalog = provider.scripts().unwrap();
    assert_eq!(catalog.scripts.len(), 3);
    assert_eq!(script_reads(&host), 5, "the whole catalog, once");
    assert!(provider.take_grown_schema(), "the separator corrected");
    let schema = provider.calc_schema().unwrap().unwrap();
    assert_eq!(
        listed(&schema),
        named([
            ScriptKind::Script,
            ScriptKind::Separator,
            ScriptKind::Script
        ])
    );
    provider.close();
}

/// A layout the catalog's index lists whose member the host does not have
/// is asked for once: it then reads as having no objects, and decoding it
/// says the host has no such layout (#1019).
#[test]
fn an_indexed_layout_with_no_member_is_asked_for_once() {
    use om_data::host::CalcNeed;
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut files = solution(1);
    fixture::index_layouts(&mut files[0].1);
    fixture::index_layout_definitions(&mut files[0].1);
    files[0].1.remove(&[&[4], &[5], &[2]]);
    let host = Host::new(
        "demand-absent-layout",
        files
            .iter()
            .map(|(name, tree)| (name.as_str(), tree.clone()))
            .collect(),
    );
    let mut provider = host.provider().with_related_on_demand(true);
    provider.open(&host.connection("Root.fmp12")).unwrap();
    assert!(provider
        .calc_schema()
        .unwrap()
        .unwrap()
        .is_layout_pending(2));
    let reads = |host: &Host| host.captures.downloaded_tables().len();
    provider
        .calc_data(&[CalcNeed::Layout { layout: 2 }])
        .unwrap();
    assert!(provider.take_grown_schema(), "it waits no longer");
    let schema = provider.calc_schema().unwrap().unwrap();
    assert!(!schema.is_layout_pending(2));
    assert!(schema.layout_named("Notes").unwrap().layout.is_none());
    let after = reads(&host);
    provider
        .calc_data(&[CalcNeed::Layout { layout: 2 }])
        .unwrap();
    assert!(!provider.take_grown_schema());
    assert_eq!(reads(&host), after, "not asked for again");
    assert!(provider.render_layout(2).is_err());
    provider.close();
}

/// A calculation the provider computes itself (a record page's unstored
/// one) that reads another layout's fields reads that layout's member and
/// is computed again, rather than reading the layout as having none (#1019:
/// the page showed no field names).
#[test]
fn a_page_calculation_reads_the_member_of_a_layout_it_names() {
    const FIELDS: u64 = 40;
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut files = solution(1);
    let root = &mut files[0].1;
    // Layout 3 "Names" on Tasks, showing Tasks::Name.
    fixture::index_layouts(root);
    let catalog = root.at(&[&[4]]).cloned().unwrap();
    let mut member = om_data::layout_catalog::plan(
        &catalog,
        &om_data::layout_catalog::NewLayout {
            name: "Names".into(),
            occurrence: fixture::OCCURRENCE,
            theme: vec![1],
            width: 1000.0,
        },
    )
    .expect("new layout")
    .member;
    member.segment = om_hbam::slot::uint_segment(3);
    let member = om_data::layout_placement::plan(
        &member,
        &om_data::layout_placement::Placement {
            occurrence: fixture::OCCURRENCE,
            field: fixture::NAME,
            frame: om_layout::Rect {
                top: 20.0,
                left: 20.0,
                bottom: 40.0,
                right: 130.0,
                ..Default::default()
            },
            label: None,
        },
    )
    .expect("placed")
    .member;
    root.put(&[&[4], &[5], &member.segment.clone()], member);
    let mut names = fixture::names();
    names.add_field(fixture::TABLE, FIELDS, "Names Fields");
    let table = fixture::TABLE;
    fixture::add_field(root, table, FIELDS, "Names Fields", 2, 1, 0, 0x2_0020, 0);
    fixture::add_formula(
        root,
        &names,
        (table, fixture::OCCURRENCE),
        FIELDS,
        5,
        r#"FieldNames ( Get ( FileName ) ; "Names" )"#,
    );
    fixture::index_layouts(root);
    fixture::index_layout_definitions(root);
    let host = Host::new(
        "demand-layout-calc",
        files
            .iter()
            .map(|(name, tree)| (name.as_str(), tree.clone()))
            .collect(),
    );
    let mut provider = host.provider().with_related_on_demand(true);
    provider.open(&host.connection("Root.fmp12")).unwrap();
    let page = provider.records(fixture::LAYOUT as u32, 0, 100).unwrap();
    let column = page
        .fields
        .iter()
        .position(|f| f.name == "Names Fields")
        .unwrap();
    let values: Vec<Option<&str>> = page
        .records
        .iter()
        .map(|r| r.values[column].as_deref())
        .collect();
    assert!(!values.is_empty());
    assert!(values.iter().all(|v| *v == Some("Name")), "{values:?}");
    provider.close();
}

/// A related file's open reads no font catalog `[25]`: only styled text of
/// its records needs it. The first record whose text names a font reads
/// it, once, and the text keeps its font family (#1098).
#[test]
fn a_related_files_fonts_are_read_when_its_styled_text_needs_them() {
    use om_data::host::CalcNeed;
    use om_hbam::{serial, slot::uint_segment, OwnedNode};
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut files = solution(1);
    let related = &mut files[1].1;
    // Font 22 is "Courier"; both records' Name is styled text in it: one
    // run from the start, its character style naming font 22 only.
    related.put(
        &[&[25], &[5], &[22], &[0x10]],
        om_scripts::text_node(&[], "Courier"),
    );
    let mut runs = 1u32.to_be_bytes().to_vec();
    runs.extend([0u8; 8]);
    runs.extend(1u32.to_be_bytes());
    runs.extend(1u32.to_be_bytes());
    runs.extend(12u32.to_be_bytes());
    runs.extend([0, 1, 0, 22, 0, 0, 0, 0, 0, 0, 0, 0]);
    // The text chunked, as styled text is stored: a header (bytes,
    // characters, 4), the masked text, then the formatting `ff`.
    let text = om_scripts::text_node(&[], "Styled").value.unwrap();
    let mut header = (text.len() as u32).to_be_bytes().to_vec();
    header.extend(6u32.to_be_bytes());
    header.extend(4u32.to_be_bytes());
    for record in [1, 2] {
        let mut name = OwnedNode::default();
        name.put(&[&[0]], OwnedNode::leaf(Vec::new(), header.clone()));
        name.put(&[&[1]], OwnedNode::leaf(Vec::new(), text.clone()));
        name.put(&[&[0xff], &[1]], OwnedNode::leaf(Vec::new(), runs.clone()));
        related.put(
            &[
                &uint_segment(fixture::TABLE),
                &[5],
                &serial::encode(record),
                &uint_segment(fixture::NAME),
            ],
            name,
        );
    }
    let host = Host::new(
        "demand-related-fonts",
        files
            .iter()
            .map(|(name, tree)| (name.as_str(), tree.clone()))
            .collect(),
    );
    let fonts_read = |host: &Host| {
        host.captures
            .downloaded_tables()
            .into_iter()
            .filter(|(file, first)| file == "File 1.fmp12" && *first == 25)
            .count()
    };
    let mut provider = host.provider().with_related_on_demand(true);
    provider.open(&host.connection("Root.fmp12")).unwrap();
    let related = 0xd1_0001;
    provider
        .calc_data(&[CalcNeed::Occurrence {
            occurrence: related,
        }])
        .unwrap();
    assert!(provider.take_grown_schema(), "File 1 opened");
    assert_eq!(fonts_read(&host), 0, "its open reads no fonts");
    let schema = provider.calc_schema().unwrap().unwrap();
    let table = schema.table_of(related).unwrap().id;
    let name = |data: &om_data::host::CalcData, record: u64| {
        let values = data.records[&(table, record)].clone().unwrap();
        match values.get(fixture::NAME, 1).cloned().unwrap() {
            om_data::calc::Value::Text(text) => (text.as_str().to_owned(), text.style_at(0).font),
            other => panic!("{other:?}"),
        }
    };

    let data = provider
        .calc_data(&[CalcNeed::Record { table, record: 1 }])
        .unwrap();
    assert_eq!(
        name(&data, 1),
        ("Styled".to_owned(), Some("Courier".to_owned()))
    );
    assert_eq!(fonts_read(&host), 1, "read when styled text named a font");

    let data = provider
        .calc_data(&[CalcNeed::Record { table, record: 2 }])
        .unwrap();
    assert_eq!(
        name(&data, 2),
        ("Styled".to_owned(), Some("Courier".to_owned()))
    );
    assert_eq!(fonts_read(&host), 1, "once");
    provider.close();
}

#[test]
#[ignore = "a measurement: prints connections, sessions, threads and times for N = 8, 32 and 60 related files"]
fn related_files_connections_and_time() {
    let _alone = ALONE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut rows = vec![
        "| N | Link | Connections | Sessions while open | Keepalive threads | Opens | Logins | Open (s) | Related files open (s) |".to_string(),
        "|---:|---|---:|---:|---:|---:|---:|---:|---:|".to_string(),
    ];
    for delay in [None, Some(Duration::from_millis(20))] {
        for n in [8, 32, 60] {
            rows.push(measure(n, delay));
        }
    }
    println!("{}", rows.join("\n"));
}
