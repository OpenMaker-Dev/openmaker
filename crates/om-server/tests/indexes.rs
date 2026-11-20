//! End to end: the index a client's find reads when the field has none
//! yet (query-program.md 7.1). FileMaker Server answers that find with no
//! records and builds the index afterwards. OpenMaker Server builds it first
//! and answers from it, unless `Server::with_late_index_builds` asks it to
//! answer as FileMaker Server does, for tests of clients.
//!
//! The Tasks file of `fixtures/tasks.rs`: its fields have no index nodes,
//! and record 2 is "Ship".

#[path = "fixtures/tasks.rs"]
mod fixture;
#[path = "fixtures/server.rs"]
mod server;

use om_collate::DateOrder;
use om_hbam::DataType;
use om_query::find::{self, IndexKind};
use om_server::Server;

/// Two finds of Tasks::Name `==Ship` in a row, each checking that the
/// value index exists first (`find::checked_program`): whether it did, and
/// what the find found.
fn find_ship_twice(port: u16) -> Vec<(bool, Vec<u64>)> {
    let (mut session, file_id) = server::logged_in(port, "indexes");
    let finders: Vec<Vec<u8>> = find::finders(
        fixture::TABLE,
        fixture::NAME,
        DataType::Text,
        "==Ship",
        DateOrder::MonthDayYear,
    )
    .unwrap()
    .into_iter()
    .map(|finder| finder.bytes)
    .collect();
    let program = find::checked_program(file_id, fixture::TABLE, fixture::NAME, &finders, None);
    let answers = (0..2)
        .map(|_| {
            let reply = session.query(&program).unwrap();
            (
                find::value_index_existed(&reply.slots).unwrap(),
                find::found(&reply.slots).unwrap().iter().collect(),
            )
        })
        .collect();
    session.disconnect().unwrap();
    answers
}

#[test]
fn a_find_on_a_missing_index_builds_it_first_unless_builds_are_late() {
    let file = fixture::TempFile::new("index-builds", &[], true);
    let port = server::start(&file);
    assert_eq!(find_ship_twice(port), [(false, vec![2]), (true, vec![2])]);

    let file = fixture::TempFile::new("late-index-builds", &[], true);
    let port = server::start_configured(&file, Server::with_late_index_builds);
    assert_eq!(find_ship_twice(port), [(false, vec![]), (true, vec![2])]);

    // In German (25), whose keys OpenMaker does not build (query-program.md
    // 7.2), the find is refused rather than answered from English keys,
    // which would miss what German keys find (#572), and nothing is stored
    // (#159).
    let mut root = fixture::tree(&[], true);
    fixture::set_field_language(&mut root, fixture::TABLE, fixture::NAME, 25);
    let file = fixture::TempFile::from_tree("unkeyed-index-builds", &root);
    let port = server::start(&file);
    let (mut session, file_id) = server::logged_in(port, "indexes");
    let finders: Vec<Vec<u8>> = find::finders(
        fixture::TABLE,
        fixture::NAME,
        DataType::Text,
        "==Ship",
        DateOrder::MonthDayYear,
    )
    .unwrap()
    .into_iter()
    .map(|finder| finder.bytes)
    .collect();
    let program = find::program(file_id, fixture::TABLE, &finders, None);
    let reply = session.query(&program).unwrap();
    // The query's error slot, 3 (`QueryError::UnkeyedIndex`): refused, not
    // an empty found set.
    assert_eq!(
        reply.slots,
        [0x80, 0x06, 0x00, 0x03, 0x02, 0x00, 0x03, 0x40],
        "the find is refused"
    );
    session.disconnect().unwrap();
    assert!(name_index_on_disk(&file).is_empty());
}

/// Two finds of Tasks::Name `ship` (read from the word index) and Count
/// `5` (the value index) in a row, each checking both indexes first
/// (`find::checked_find_program`): which existed, and what the find found.
fn find_ship_and_five_twice(port: u16) -> Vec<(Vec<bool>, Vec<u64>)> {
    let (mut session, file_id) = server::logged_in(port, "indexes");
    let mut checks = Vec::new();
    let mut finders = Vec::new();
    for (field, data_type, criterion) in [
        (fixture::NAME, DataType::Text, "ship"),
        (fixture::COUNT, DataType::Number, "5"),
    ] {
        let found = find::finders(
            fixture::TABLE,
            field,
            data_type,
            criterion,
            DateOrder::MonthDayYear,
        )
        .unwrap();
        for finder in found {
            checks.push((field, finder.reads()));
            finders.push(finder.bytes);
        }
    }
    assert_eq!(
        checks,
        [
            (fixture::NAME, IndexKind::Word),
            (fixture::COUNT, IndexKind::Value)
        ]
    );
    let program = find::checked_find_program(file_id, fixture::TABLE, &checks, &finders, None);
    let answers = (0..2)
        .map(|_| {
            let reply = session.query(&program).unwrap();
            (
                find::indexes_existed(&reply.slots, checks.len()).unwrap(),
                find::found(&reply.slots).unwrap().iter().collect(),
            )
        })
        .collect();
    session.disconnect().unwrap();
    answers
}

#[test]
fn a_find_checks_each_index_it_reads() {
    let file = fixture::TempFile::new("index-checks", &[], true);
    let port = server::start(&file);
    assert_eq!(
        find_ship_and_five_twice(port),
        [(vec![false, false], vec![2]), (vec![true, true], vec![2])]
    );

    let file = fixture::TempFile::new("late-index-checks", &[], true);
    let port = server::start_configured(&file, Server::with_late_index_builds);
    assert_eq!(
        find_ship_and_five_twice(port),
        [(vec![false, false], vec![]), (vec![true, true], vec![2])]
    );
}

/// Two JoinProjections into Tasks of Name = "Ship" in a row, each checking
/// Name's value index first (`find::checked_join_program`).
fn join_ship_twice(port: u16) -> Vec<(bool, Vec<u64>)> {
    let (mut session, file_id) = server::logged_in(port, "indexes");
    let rows = [(1, vec![(1, vec![om_collate::text_key("Ship")])])];
    let program = find::checked_join_program(
        file_id,
        fixture::TABLE,
        &[(fixture::NAME, IndexKind::Value)],
        &rows,
        &[(0, fixture::NAME, 1)],
        None,
    )
    .unwrap();
    let answers = (0..2)
        .map(|_| {
            let reply = session.query(&program).unwrap();
            (
                find::indexes_existed(&reply.slots, 1).unwrap()[0],
                find::found(&reply.slots).unwrap().iter().collect(),
            )
        })
        .collect();
    session.disconnect().unwrap();
    answers
}

/// OpenMaker Server builds the index a join reads before answering it,
/// also with late builds (which model FileMaker Server's finders only),
/// and the check before the join still sees it missing.
#[test]
fn a_join_checks_the_index_it_reads() {
    for (tag, late) in [("join-checks", false), ("late-join-checks", true)] {
        let file = fixture::TempFile::new(tag, &[], true);
        let port = if late {
            server::start_configured(&file, Server::with_late_index_builds)
        } else {
            server::start(&file)
        };
        assert_eq!(
            join_ship_twice(port),
            [(false, vec![2]), (true, vec![2])],
            "{tag}"
        );
    }
}

/// #301: a program that would make a spot deeper than a program can be
/// aborted the whole server (the reply's encoding overflowed the
/// connection's stack), and so did a sort by one field named 3,400 times.
/// Both are refused with the error slot, and the server goes on answering.
#[test]
fn programs_past_the_limits_are_refused_and_the_server_goes_on() {
    use om_hbam::{serial, OwnedNode};

    let file = fixture::TempFile::new("query-limits", &[], true);
    let port = server::start(&file);
    let (mut session, file_id) = server::logged_in(port, "limits");
    let leaf = |key: u8, value: Vec<u8>| OwnedNode::leaf(vec![key], value);
    let program = |opcode: u8, leaves: Vec<OwnedNode>| {
        let mut children = vec![
            leaf(0x00, vec![opcode]),
            leaf(0x04, file_id.to_be_bytes().to_vec()),
        ];
        children.extend(leaves);
        children.sort_by(|a, b| a.segment.cmp(&b.segment));
        let instruction = OwnedNode::list(serial::encode(1), children);
        OwnedNode::list(
            Vec::new(),
            vec![OwnedNode::list(vec![1], vec![instruction])],
        )
        .to_wire()
        .unwrap()
    };
    let refused = [0x80, 0x06, 0x00, 0x03, 0x02, 0xff, 0xff, 0x40];

    // CopyNode of the instruction list `[01]` into `[03]` and 10,000 empty
    // segments.
    let mut deep = vec![1, 3];
    deep.extend([0; 10_000]);
    let copy = program(4, vec![leaf(0x01, vec![1, 1]), leaf(0x02, deep)]);
    assert_eq!(session.query(&copy).unwrap().slots, refused);

    // SortList of every record by Code, empty in each, 3,400 times.
    let mut spec = vec![0, 0];
    spec.extend(3_400u16.to_be_bytes());
    for _ in 0..3_400 {
        spec.extend([3, 2, 0x80, 0x01, 2, 1, fixture::CODE as u8, 0, 1]);
        spec.extend([0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    }
    let sort = program(
        14,
        vec![
            OwnedNode::list(vec![0x01], vec![leaf(0xff, Vec::new())]),
            leaf(0x02, vec![1, 3, 1, 1]),
            leaf(0x05, vec![2, 0x80, 0x01]),
            leaf(0x0a, spec),
            leaf(0x0b, vec![2, 0x80, 0x01]),
        ],
    );
    assert_eq!(session.query(&sort).unwrap().slots, refused);
    session.disconnect().unwrap();

    assert_eq!(find_ship_twice(port), [(false, vec![2]), (true, vec![2])]);
}

/// Commit `name` into Tasks::Name of record `id` as FileMaker Pro does:
/// lock, upload to the temporary area (op14), commit (op70), unlock.
fn commit_name(session: &mut om_session::Session, id: u64, name: &str) {
    commit_cells(session, id, &[(fixture::NAME, name)]).unwrap();
}

/// [`commit_name`] of a record holding `cells` of Tasks, its new content
/// (the upload is the whole record), with the commit's outcome.
fn commit_cells(
    session: &mut om_session::Session,
    id: u64,
    cells: &[(u64, &str)],
) -> Result<(), om_session::SessionError> {
    use om_hbam::slot::uint_segment;
    use om_hbam::{serial, OwnedNode};
    let table = uint_segment(fixture::TABLE);
    let path: [&[u8]; 3] = [&table, &[5], &serial::encode(id)];
    assert!(matches!(
        session.lock(&path, 0x31, true).unwrap(),
        om_protocol::write::LockReply::Granted { .. }
    ));
    let leaves = |value: bool| {
        cells
            .iter()
            .map(|(field, text)| {
                let bytes = if value {
                    om_scsu::encode_scsu(text)
                } else {
                    Vec::new()
                };
                OwnedNode::leaf(uint_segment(*field), bytes)
            })
            .collect()
    };
    let record = OwnedNode::list(serial::encode(id), leaves(true));
    session.upload_records(&table, vec![record]).unwrap();
    let changed = OwnedNode::list(serial::encode(id), leaves(false));
    let committed = session.commit_records(&table, vec![changed], 0x31);
    session.kill_all_locks(0x31).unwrap();
    committed
}

/// A field indexed in German (25), whose keys OpenMaker does not build
/// (query-program.md 7.2), with the value index FileMaker built (`Ship`
/// keys alike in English and German): a FileMaker Pro client's commit that
/// would change its text is refused and leaves the file as it was, as
/// English keys would not be FileMaker's; one that changes another field
/// of the record is saved (#159).
#[test]
fn text_indexed_in_a_language_without_keys_is_not_keyed_in_english() {
    use om_hbam::slot::uint_segment;
    use om_query::keys::{record_value_entries, TextKeys};
    let mut root = fixture::tree(&[], true);
    fixture::set_field_language(&mut root, fixture::TABLE, fixture::NAME, 25);
    let (t, f) = (uint_segment(fixture::TABLE), uint_segment(fixture::NAME));
    let ship = record_value_entries(
        DataType::Text,
        &["Ship".to_owned()],
        DateOrder::MonthDayYear,
        TextKeys::Collated,
    );
    for key in ship {
        let records: om_hbam::RidList = [2].into_iter().collect();
        root.put(&[&t, &[1], &f, &[0], &key], records.to_node(Vec::new()));
    }
    let file = fixture::TempFile::from_tree("unkeyed-commits", &root);
    let index = name_index_on_disk(&file);
    assert_eq!(index.len(), 1);
    let port = server::start(&file);
    let (mut session, _) = server::logged_in(port, "unkeyed");
    commit_cells(
        &mut session,
        2,
        &[(fixture::NAME, "Ship"), (fixture::COUNT, "7")],
    )
    .unwrap();
    let refused = commit_cells(
        &mut session,
        2,
        &[(fixture::NAME, "Äpfel"), (fixture::COUNT, "7")],
    );
    assert!(refused.is_err());
    session.disconnect().unwrap();
    assert_eq!(name_on_disk(&file, 2).as_deref(), Some("Ship"));
    assert_eq!(name_index_on_disk(&file), index);
}

/// Record `id`'s Name in the file on disk.
fn name_on_disk(file: &fixture::TempFile, id: u64) -> Option<String> {
    use om_hbam::slot::uint_segment;
    let hbam = om_hbam::HbamFile::open(&file.0).unwrap();
    let path: [&[u8]; 4] = [
        &uint_segment(fixture::TABLE),
        &[5],
        &om_hbam::serial::encode(id),
        &uint_segment(fixture::NAME),
    ];
    let node = hbam.subtree(&path).unwrap()?;
    om_hbam::Cell::from_node(&node).text()?.ok()
}

/// Name's value index in the file on disk: each key's length (or the key
/// itself when short) with its records.
fn name_index_on_disk(file: &fixture::TempFile) -> Vec<(Vec<u8>, Vec<u64>)> {
    use om_hbam::slot::uint_segment;
    let hbam = om_hbam::HbamFile::open(&file.0).unwrap();
    let path: [&[u8]; 4] = [
        &uint_segment(fixture::TABLE),
        &[1],
        &uint_segment(fixture::NAME),
        &[0],
    ];
    let Some(index) = hbam.subtree(&path).unwrap() else {
        return Vec::new();
    };
    index
        .children
        .iter()
        .map(|entry| {
            let records = om_hbam::RidList::from_node(entry).unwrap().iter().collect();
            (entry.segment.0.to_vec(), records)
        })
        .collect()
}

/// A text value whose index key is longer than a key may be (query-program.md
/// 7.1: FileMaker keeps a key's first 219 bytes and also lists the record
/// under `ff 80`), and a find on its field that builds the field's index,
/// leave the file saveable: later commits are saved, by any account, also
/// into the now indexed field, and finds for long and short values find
/// the right records (issue #303).
#[test]
fn long_values_and_finds_leave_the_file_saveable() {
    let long = "x".repeat(300);
    let near = format!("{}y", "x".repeat(299));
    let longer = "y".repeat(250);
    // Server script: Name := Get ( ScriptParameter ) in Find mode, Perform
    // Find; the error and the found count.
    let find = fixture::script(
        1,
        "Find by parameter",
        vec![
            fixture::on(86, true),
            om_scripts::ScriptStep::new(22),
            fixture::set_field(fixture::NAME, "Get ( ScriptParameter )"),
            om_scripts::ScriptStep::new(28),
            fixture::exit_with("Get ( LastError ) & \":\" & Get ( FoundCount )"),
        ],
    );
    let file = fixture::TempFile::new("long-values", &[find], true);
    let port = server::start(&file);
    let (mut session, file_id) = server::logged_in(port, "long values");

    // Name has no index yet: the commit stores the value, and the find
    // builds Name's value index, holding record 1's key cut short.
    commit_name(&mut session, 1, &long);
    assert_eq!(name_on_disk(&file, 1).as_deref(), Some(long.as_str()));
    let finders: Vec<Vec<u8>> = find::finders(
        fixture::TABLE,
        fixture::NAME,
        DataType::Text,
        "==Ship",
        DateOrder::MonthDayYear,
    )
    .unwrap()
    .into_iter()
    .map(|finder| finder.bytes)
    .collect();
    let program = find::checked_program(file_id, fixture::TABLE, fixture::NAME, &finders, None);
    let reply = session.query(&program).unwrap();
    assert_eq!(
        find::found(&reply.slots)
            .unwrap()
            .iter()
            .collect::<Vec<_>>(),
        [2]
    );
    let cut = om_collate::text_key(&"x".repeat(109));
    assert_eq!(cut.len(), 219);
    assert_eq!(
        name_index_on_disk(&file),
        [
            (om_collate::text_key("Ship"), vec![2]),
            (cut.clone(), vec![1]),
            (vec![0xff, 0x80], vec![1]),
        ]
    );

    // Later commits are saved, a long value into the indexed field too.
    commit_name(&mut session, 2, "Shipped");
    assert_eq!(name_on_disk(&file, 2).as_deref(), Some("Shipped"));
    commit_name(&mut session, 2, &longer);
    assert_eq!(name_on_disk(&file, 2).as_deref(), Some(longer.as_str()));

    // Finds for long values find their records, and only those.
    let found = |session: &mut om_session::Session, criterion: &str| {
        session
            .perform_script_on_server(1, criterion, true)
            .unwrap()
            .result
    };
    assert_eq!(found(&mut session, &format!("=={long}")), "0:1");
    assert_eq!(found(&mut session, &long), "0:1");
    assert_eq!(found(&mut session, &format!("=={near}")), "401:0");
    assert_eq!(found(&mut session, &format!("=={longer}")), "0:1");
    assert_eq!(found(&mut session, "==Write tests"), "401:0");
    // Both records are under `ff 80`, which is no value: not duplicates.
    assert_eq!(found(&mut session, "!"), "401:0");

    // A record that no longer holds a long value leaves `ff 80`.
    commit_name(&mut session, 1, "Short");
    assert_eq!(found(&mut session, "short"), "0:1");
    assert_eq!(found(&mut session, &format!("=={long}")), "401:0");
    let index = name_index_on_disk(&file);
    assert!(index.contains(&(vec![0xff, 0x80], vec![2])), "{index:?}");
    assert!(index.contains(&(om_collate::text_key("Short"), vec![1])));
    session.disconnect().unwrap();
}
