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
use om_query::keys::TextKeys;
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

/// A read that fails, as reads that reach a damaged page fail
/// (docs/server-storage.md 5, "Views", and 6.12), reaches the client as an
/// error, never as a reply that looks whole: a download that reaches it
/// (op90, op92) is answered with a CORBA system exception (`PERSIST_STORE`),
/// a find that reads it fails with the error slot's code `ffff`, and what
/// does not reach it is served as before, on the same connection.
#[cfg(debug_assertions)]
#[test]
fn a_failed_read_reaches_the_client_as_an_error() {
    use om_hbam::serial;
    use om_hbam::slot::uint_segment;
    let file = fixture::TempFile::new("failed-reads", &[], true);
    let (port, handle) = server::start_configured_with_handle(&file, |server| server);
    let table = uint_segment(fixture::TABLE);
    let (one, ship) = (serial::encode(1), serial::encode(2));
    // Record 2, "Ship", and what is below it cannot be read.
    handle.failing_reads(fixture::FILE, &[&table, &[5], &ship]);
    let (mut session, file_id) = server::logged_in(port, "failed-reads");
    let first = session.download(&[&table, &[5], &one]).unwrap();
    assert!(!first.slots.is_empty());
    let paths: [&[&[u8]]; 3] = [&[&table, &[5]], &[&table, &[5], &ship], &[&table]];
    for path in paths {
        let error = session.download(path).unwrap_err();
        assert!(error.to_string().contains("SystemException"), "{error}");
    }
    let keys: [&[u8]; 2] = [&one, &ship];
    let error = session
        .download_list(&[&table, &[5]], &keys, 0)
        .unwrap_err();
    assert!(error.to_string().contains("SystemException"), "{error}");
    let listed = session.download_list(&[&table, &[5]], &[&one], 0).unwrap();
    assert!(!listed.slots.is_empty());
    // A find on Name reads every record to build the index on demand.
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
    assert_eq!(
        reply.slots,
        [0x80, 0x06, 0x00, 0x03, 0x02, 0xff, 0xff, 0x40],
        "the find fails"
    );
    assert!(session.download(&[&table, &[5], &one]).is_ok());
    session.disconnect().unwrap();
}

/// Finds of Tasks::Name `==Ship` (its value index) on a session's
/// connection: the program.
fn ship_finder(file_id: u32) -> Vec<u8> {
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
    find::program(file_id, fixture::TABLE, &finders, None)
}

/// A find by an account whose set may view no record of Tasks reads Name's
/// index as absent without reading it, and NodeExists and GetKey answer
/// for the table, its index container and the index from their heads:
/// under the test view, which refuses a find an index, or a node above
/// one, read whole (`Server::checking_reads`), each answers as the file
/// engine does, where the index or a node above it read whole and then
/// filtered by the view limits failed the comparison.
#[test]
fn a_find_on_a_table_the_account_may_not_view_reads_no_index() {
    use om_hbam::slot::uint_segment;
    let mut root = fixture::tree(&[], true);
    fixture::add_custom_clerk(&mut root);
    // Temp's set: no Tasks record created, viewed, edited or deleted.
    let tasks = uint_segment(fixture::TABLE);
    root.put(
        &[&[23], &[2], &[5], &[4], &[5], &tasks, &[2]],
        om_hbam::OwnedNode::leaf(Vec::new(), vec![0x00, 0x03]),
    );
    let file = fixture::TempFile::from_tree("hidden-index", &root);
    let port = server::start_configured(&file, |server| server.admitting_custom_sets());
    // Admin's find builds and stores Name's index.
    let (mut admin, admin_file) = server::logged_in(port, "admin");
    let reply = admin.query(&ship_finder(admin_file)).unwrap();
    assert_eq!(
        find::found(&reply.slots)
            .unwrap()
            .iter()
            .collect::<Vec<_>>(),
        [2]
    );
    let (mut temp, temp_file) = server::logged_in_as(
        port,
        "temp",
        fixture::FILE,
        fixture::TEMP,
        fixture::TEMP_PASSWORD,
    );
    for _ in 0..2 {
        let reply = temp.query(&ship_finder(temp_file)).unwrap();
        assert!(find::found(&reply.slots).unwrap().is_empty());
    }
    // The table and its index container are there for it, the container
    // emptied, and Name's index is not: answered from the nodes' heads,
    // never from what is below them, which the test view refuses a find.
    use om_query::program::opcode::{GET_KEY, NODE_EXISTS};
    let name = uint_segment(fixture::NAME);
    let container: [&[u8]; 2] = [&tasks, &[1]];
    let index: [&[u8]; 4] = [&tasks, &[1], &name, &[0]];
    let mut answer = |op, path: &[&[u8]]| file_node_answer(&mut temp, temp_file, op, path);
    assert_eq!(answer(NODE_EXISTS, &container), Some(vec![1, 1]));
    assert_eq!(answer(GET_KEY, &container), None);
    assert_eq!(answer(NODE_EXISTS, &index), Some(vec![1, 0]));
    assert_eq!(answer(NODE_EXISTS, &[&tasks]), Some(vec![1, 1]));
    temp.disconnect().unwrap();
    admin.disconnect().unwrap();
}

/// What NodeExists (`op`, or GetKey) answers at `path` for `session`: the
/// value at the result spot's `00`.
fn file_node_answer(
    session: &mut om_session::Session,
    file_id: u32,
    op: u8,
    path: &[&[u8]],
) -> Option<Vec<u8>> {
    use om_hbam::OwnedNode;
    use om_query::program::leaf;
    let path: Vec<u8> = path
        .iter()
        .flat_map(|segment| std::iter::once(segment.len() as u8).chain(segment.iter().copied()))
        .collect();
    let mut query = OwnedNode::list(Vec::new(), Vec::new());
    for (key, value) in [
        (leaf::OPCODE, vec![op]),
        (leaf::SRC, path),
        (leaf::DEST, vec![1, 3, 1, 1]),
        (leaf::FILE_ID, file_id.to_be_bytes().to_vec()),
    ] {
        query.put(&[&[1], &[1], &[key]], OwnedNode::leaf(Vec::new(), value));
    }
    let slots = session.query(&query.to_wire().unwrap()).unwrap().slots;
    let tree = om_hbam::KeyTree::from_slots(om_hbam::SlotReader::new(&slots)).unwrap();
    tree.node(&[&[1], &[0]])
        .and_then(|node| node.value.map(<[u8]>::to_vec))
}

/// NodeExists and GetKey of Name's value index read the node alone, not
/// the index under it: under the test view, which refuses a find an index
/// read whole (`Server::checking_reads`), they answer as the file engine
/// does (the index exists; it is a node, so GetKey gives no value).
#[test]
fn node_exists_on_an_index_reads_its_node_alone() {
    use om_hbam::slot::uint_segment;
    use om_query::program::opcode;
    let file = fixture::TempFile::new("index-node", &[], true);
    let port = server::start(&file);
    let (mut session, file_id) = server::logged_in(port, "index-node");
    let reply = session.query(&ship_finder(file_id)).unwrap();
    assert_eq!(
        find::found(&reply.slots)
            .unwrap()
            .iter()
            .collect::<Vec<_>>(),
        [2]
    );
    let table = uint_segment(fixture::TABLE);
    let name = uint_segment(fixture::NAME);
    let index: [&[u8]; 4] = [&table, &[1], &name, &[0]];
    let answer =
        |session: &mut om_session::Session, op: u8| file_node_answer(session, file_id, op, &index);
    assert_eq!(answer(&mut session, opcode::NODE_EXISTS), Some(vec![1, 1]));
    assert_eq!(answer(&mut session, opcode::GET_KEY), None);
    session.disconnect().unwrap();
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

/// In an index written under other key rules (an earlier OpenMaker
/// build's, or FileMaker's where OpenMaker's still differ), a record a
/// write changes leaves every entry its new value does not give (#448).
/// Here Name's value index lists record 2, "Ship", under a key the current
/// rules do not give, and not under "Ship"'s. Changing it to "Shipped"
/// would otherwise leave it under the old key, where a find would still
/// return it. Other records keep their entries as stored, and a commit
/// whose save fails puts the stored index back exactly.
#[test]
fn a_changed_record_leaves_entries_another_rule_wrote() {
    use om_hbam::slot::uint_segment;
    use om_hbam::{OwnedNode, RidList};
    let stale = vec![0x01, 0x10, 0x20];
    let written = om_collate::text_key("Write tests");
    let mut root = fixture::tree(&[], true);
    let entry = |key: &[u8], id: u64| [id].into_iter().collect::<RidList>().to_node(key.to_vec());
    root.put(
        &[
            &uint_segment(fixture::TABLE),
            &[1],
            &uint_segment(fixture::NAME),
            &[0],
        ],
        // In key order, as a file stores them; `ff 80` holds record 1 as
        // FileMaker lists a long key there too, which OpenMaker's rules
        // would not give "Write tests".
        OwnedNode::list(
            vec![0],
            vec![
                entry(&stale, 2),
                entry(&written, 1),
                entry(&[0xff, 0x80], 1),
            ],
        ),
    );
    let file = fixture::TempFile::from_tree("stale-index", &root);
    let as_stored = field_index_on_disk(&file, fixture::NAME);
    let (port, handle) = server::start_with_handle(&file, om_server::ScriptLimits::DEFAULT);
    let path = std::path::absolute(&file.0).unwrap();
    let id = handle
        .files()
        .into_iter()
        .find(|f| f.path == path)
        .unwrap()
        .id;
    let (mut session, _) = server::logged_in(port, "stale index");

    // A failed save: the rebuilt index goes back with the record. A
    // commit that leaves Name as it was then saves Name's whole index
    // subtree, its keys and counts, as it was stored.
    assert!(handle.failing_commit_saves(id, true));
    assert!(commit_cells(&mut session, 2, &[(fixture::NAME, "Shipped")]).is_err());
    assert!(handle.failing_commit_saves(id, false));
    commit_cells(
        &mut session,
        1,
        &[(fixture::NAME, "Write tests"), (fixture::COUNT, "9")],
    )
    .unwrap();
    assert_eq!(field_index_on_disk(&file, fixture::NAME), as_stored);
    // The same commit, saved, is accepted: the failure was the save's.

    // Saved, the write leaves record 2 under "Shipped" only. Record 1, which
    // no write touched, keeps every entry as stored, `ff 80` included.
    commit_name(&mut session, 2, "Shipped");
    assert_eq!(
        name_index_on_disk(&file),
        [
            (om_collate::text_key("Shipped"), vec![2]),
            (written, vec![1]),
            (vec![0xff, 0x80], vec![1]),
        ]
    );

    // A failed save once the commits above have checked Name's index: the
    // roll-back puts back the mark they left, exactly, which the checking
    // transaction checks (review of #811, round 1).
    assert!(handle.failing_commit_saves(id, true));
    assert!(commit_cells(&mut session, 1, &[(fixture::NAME, "Write more")]).is_err());
    assert!(handle.failing_commit_saves(id, false));
    commit_name(&mut session, 1, "Write more");
    session.disconnect().unwrap();
}

/// An upload (op13) into an index whose agreement with its records a
/// commit has checked: the index is checked again at the next commit,
/// which sweeps a posting the upload planted for a record under a key its
/// value does not give (review of #785, round 1). The check is kept per
/// index, and a write into the index or the records that is not a commit's
/// own upkeep forgets it (`Store::forget_verified`).
#[test]
fn an_upload_into_an_index_has_it_checked_again() {
    use om_hbam::slot::uint_segment;
    use om_hbam::{OwnedNode, RidList};
    let written = om_collate::text_key("Write tests");
    let ship = om_collate::text_key("Ship");
    let entry = |key: &[u8], id: u64| [id].into_iter().collect::<RidList>().to_node(key.to_vec());
    let table = uint_segment(fixture::TABLE);
    let name = uint_segment(fixture::NAME);
    let index: [&[u8]; 4] = [&table, &[1], &name, &[0]];
    let mut root = fixture::tree(&[], true);
    let mut entries = vec![entry(&written, 1), entry(&ship, 2)];
    entries.sort_by(|a, b| a.segment.cmp(&b.segment));
    root.put(&index, OwnedNode::list(vec![0], entries));
    let file = fixture::TempFile::from_tree("upload-into-index", &root);
    let port = server::start(&file);
    let (mut session, _) = server::logged_in(port, "upload into index");

    // An ordinary edit: the index agrees with its records, and the commit
    // keeps it so.
    commit_name(&mut session, 1, "Written");
    let written = om_collate::text_key("Written");
    // The index uploaded again with record 2 also under a key its value
    // does not give.
    let unrelated = om_collate::text_key("Unrelated");
    let mut entries = vec![entry(&written, 1), entry(&ship, 2), entry(&unrelated, 2)];
    entries.sort_by(|a, b| a.segment.cmp(&b.segment));
    session
        .upload_to_host(&index, &OwnedNode::list(vec![0], entries))
        .unwrap();
    // Record 2's edit finds the index disagreeing and sweeps the posting.
    commit_name(&mut session, 2, "Shipped");
    let mut expected = vec![
        (written, vec![1]),
        (om_collate::text_key("Shipped"), vec![2]),
    ];
    expected.sort();
    assert_eq!(name_index_on_disk(&file), expected);
    session.disconnect().unwrap();
}

/// A commit of a value its field's rules key to nothing, a date that does
/// not parse and a time past the hour's minutes, into a value index a find
/// built and the commit checks first: the record leaves the index, and the
/// index no longer agrees with the current rules, which would not account
/// for the value, so its verified mark is forgotten (review of #811,
/// round 1; `derived::record_keys`). The checking transaction checks every
/// mark that an index agrees when the commit ends.
#[test]
fn a_value_without_a_key_has_its_index_checked_again() {
    use om_hbam::serial;
    use om_hbam::slot::uint_segment;
    const CLOCK: u64 = 0x20;
    let mut root = fixture::tree(&[], true);
    fixture::add_field(&mut root, fixture::TABLE, CLOCK, "Clock", 1, 4, 0, 0, 0);
    root.put(
        &[
            &uint_segment(fixture::TABLE),
            &[5],
            &serial::encode(2),
            &uint_segment(CLOCK),
        ],
        om_scripts::text_node(&[], "10:30:00"),
    );
    let file = fixture::TempFile::from_tree("unkeyed-values", &root);
    let port = server::start(&file);
    let (mut session, file_id) = server::logged_in(port, "unkeyed values");
    // Record 1's Due, record 2's Clock.
    for (record, field, data_type, held, unkeyed) in [
        (1, fixture::DUE, DataType::Date, "10/1/2026", "99/99/2026"),
        (2, CLOCK, DataType::Time, "10:30:00", "10:99"),
    ] {
        // A find on the field builds its value index and stores it.
        let finders: Vec<Vec<u8>> = find::finders(
            fixture::TABLE,
            field,
            data_type,
            held,
            DateOrder::MonthDayYear,
        )
        .unwrap()
        .into_iter()
        .map(|finder| finder.bytes)
        .collect();
        let program = find::checked_program(file_id, fixture::TABLE, field, &finders, None);
        let reply = session.query(&program).unwrap();
        let found: Vec<u64> = find::found(&reply.slots).unwrap().iter().collect();
        assert_eq!(found, [record]);
        let indexed = |record| {
            index_on_disk(&file, field, 0)
                .iter()
                .any(|(key, ids)| key.as_slice() != [1] && ids.contains(&record))
        };
        assert!(indexed(record), "{held} is indexed");
        commit_cells(&mut session, record, &[(field, unkeyed)]).unwrap();
        assert!(!indexed(record), "{unkeyed} has no key");
    }
    session.disconnect().unwrap();
}

/// A German file's legacy Number index used a period as the decimal
/// separator. Both spellings now key as 15, so only the sweep can move
/// the edited record; the untouched record keeps its old membership.
#[test]
fn a_numeric_edit_sweeps_an_old_decimal_key() {
    use om_hbam::slot::uint_segment;
    use om_hbam::{serial, OwnedNode, RidList};
    let mut root = fixture::tree(&[], true);
    root.put(
        &[&[2], &[0x0d]],
        OwnedNode::leaf(Vec::new(), om_calc::Locale::de_de().to_record()),
    );
    let table = uint_segment(fixture::TABLE);
    let count = uint_segment(fixture::COUNT);
    for id in [1, 2] {
        root.put(
            &[&table, &[5], &serial::encode(id), &count],
            om_scripts::text_node(&[], "1.5"),
        );
    }
    let old_key = vec![0x80, 0x01, 0x26]; // 1.5
    let new_key = vec![0x80, 0x02, 0x26]; // 15
    let posting = [1, 2]
        .into_iter()
        .collect::<RidList>()
        .to_node(old_key.clone());
    root.put(
        &[&table, &[1], &count, &[0]],
        OwnedNode::list(vec![0], vec![posting]),
    );
    let file = fixture::TempFile::from_tree("legacy-decimal-index", &root);
    let port = server::start(&file);
    let (mut session, _) = server::logged_in(port, "legacy decimal index");

    commit_cells(&mut session, 2, &[(fixture::COUNT, "15,0")]).unwrap();
    assert_eq!(
        index_on_disk(&file, fixture::COUNT, 0),
        [(old_key, vec![1]), (new_key, vec![2])]
    );
    session.disconnect().unwrap();
}

/// A node of a subtree: its path below the subtree's root, and its value.
type Flat = (Vec<Vec<u8>>, Option<Vec<u8>>);

/// Every node under `field`'s index subtree `[Tasks, 01, field]` in the
/// saved file, keys, counts and record lists, as (path, value) pairs.
fn field_index_on_disk(file: &fixture::TempFile, field: u64) -> Vec<Flat> {
    use om_hbam::slot::uint_segment;
    fn flatten(node: &om_hbam::Node<'_>, path: &mut Vec<Vec<u8>>, out: &mut Vec<Flat>) {
        for child in &node.children {
            path.push(child.segment.0.to_vec());
            out.push((path.clone(), child.value.map(<[u8]>::to_vec)));
            flatten(child, path, out);
            path.pop();
        }
    }
    let hbam = om_hbam::HbamFile::open(&file.0).unwrap();
    let path: [&[u8]; 3] = [&uint_segment(fixture::TABLE), &[1], &uint_segment(field)];
    let mut out = Vec::new();
    if let Some(index) = hbam.subtree(&path).unwrap() {
        flatten(&index, &mut Vec::new(), &mut out);
    }
    out
}

/// A deleted record with a stored date the server cannot key leaves its
/// persisted posting (#448). The synthetic index holds a posting for the
/// short-year date, so only the sweep can remove it.
#[test]
fn a_deleted_record_leaves_entries_another_rule_wrote() {
    use om_hbam::slot::uint_segment;
    use om_hbam::{serial, OwnedNode, RidList};
    let delete = fixture::script(
        1,
        "Delete the last record",
        vec![
            fixture::go_to_record(2),
            om_scripts::ScriptStep::new(9),
            fixture::exit_with("Get ( LastError )"),
        ],
    );
    let mut root = fixture::tree(&[delete], true);
    let table = uint_segment(fixture::TABLE);
    let due = uint_segment(fixture::DUE);
    root.put(
        &[&table, &[5], &serial::encode(2), &due],
        om_scripts::text_node(&[], "10/2/26"),
    );
    let date_key = |value: &str| om_collate::date_key(value, DateOrder::MonthDayYear).unwrap();
    let entry = |value: &str, id: u64| {
        [id].into_iter()
            .collect::<RidList>()
            .to_node(date_key(value))
    };
    root.put(
        &[&table, &[1], &due, &[0]],
        OwnedNode::list(vec![0], vec![entry("10/1/2026", 1), entry("10/2/2026", 2)]),
    );
    // Counts of a shape this server does not write are left as they are.
    root.put(
        &[&table, &[1], &due, &[0x10], &[7]],
        OwnedNode::leaf(Vec::new(), vec![1, 2, 3]),
    );
    let file = fixture::TempFile::from_tree("purged-index", &root);
    let counts = |file: &fixture::TempFile| -> Vec<Flat> {
        field_index_on_disk(file, fixture::DUE)
            .into_iter()
            .filter(|(path, _)| path[0] == [0x10])
            .collect()
    };
    let stored_counts = counts(&file);
    assert!(!stored_counts.is_empty());
    let port = server::start(&file);
    let (mut session, _) = server::logged_in(port, "purged index");
    let reply = session.perform_script_on_server(1, "", true).unwrap();
    assert_eq!((reply.error, reply.result.as_str()), (0, "0"));
    let keys: Vec<(Vec<u8>, Option<Vec<u8>>)> = field_index_on_disk(&file, fixture::DUE)
        .into_iter()
        .filter(|(path, _)| path.len() == 2 && path[0] == [0])
        .map(|(path, value)| (path[1].clone(), value))
        .collect();
    assert_eq!(keys, [(date_key("10/1/2026"), None)]);
    assert_eq!(counts(&file), stored_counts);
    session.disconnect().unwrap();
}

/// One commit of several records (Commit Transaction) keeps an index the
/// first record emptied, though it has no counts leaf yet: the second
/// record is indexed in it (#448). Name's word index (which has no key for
/// an empty value) holds only record 1's words; record 1's Name is cleared,
/// which empties it, and record 2's set to "Beta".
#[test]
fn a_batch_keeps_an_index_its_first_record_emptied() {
    use om_hbam::slot::uint_segment;
    use om_hbam::{serial, OwnedNode};
    let batch = fixture::script(
        1,
        "Batch",
        vec![
            om_scripts::ScriptStep::new(205),
            fixture::go_to_record(1),
            fixture::set_field(fixture::NAME, "\"\""),
            fixture::go_to_record(2),
            fixture::set_field(fixture::NAME, "\"Beta\""),
            om_scripts::ScriptStep::new(206),
            fixture::exit_with("Get ( LastError )"),
        ],
    );
    let mut root = fixture::tree(&[batch], true);
    let table = uint_segment(fixture::TABLE);
    let name = uint_segment(fixture::NAME);
    root.remove(&[&table, &[5], &serial::encode(2), &name]);
    root.put(
        &[&table, &[1], &name, &[1]],
        OwnedNode::list(vec![1], words_of(&[(1, "Write tests")], TextKeys::Collated)),
    );
    let file = fixture::TempFile::from_tree("batch-index", &root);
    let port = server::start(&file);
    let (mut session, _) = server::logged_in(port, "batch index");
    let reply = session.perform_script_on_server(1, "", true).unwrap();
    assert_eq!((reply.error, reply.result.as_str()), (0, "0"));
    let words: Vec<Vec<Vec<u8>>> = field_index_on_disk(&file, fixture::NAME)
        .into_iter()
        .filter(|(path, _)| path.len() == 2 && path[0] == [1])
        .map(|(path, _)| path)
        .collect();
    assert_eq!(words, [vec![vec![1], om_collate::text_key("Beta")]]);
    session.disconnect().unwrap();
}

/// Word-index entries (`[key] = records`) for `records`' texts in `form`,
/// in key order.
fn words_of(records: &[(u64, &str)], form: TextKeys) -> Vec<om_hbam::OwnedNode> {
    let mut index: std::collections::BTreeMap<Vec<u8>, om_hbam::RidList> = Default::default();
    for (id, text) in records {
        for key in om_query::keys::word_entries(&[text.to_string()], form) {
            index.entry(key).or_default().insert(*id);
        }
    }
    index
        .into_iter()
        .map(|(key, ids)| ids.to_node(key))
        .collect()
}

/// The entries of Tasks::`field`'s index `kind` in the saved file, each
/// key with its records.
fn index_on_disk(file: &fixture::TempFile, field: u64, kind: u8) -> Vec<(Vec<u8>, Vec<u64>)> {
    use om_hbam::slot::uint_segment;
    let hbam = om_hbam::HbamFile::open(&file.0).unwrap();
    let path: [&[u8]; 4] = [
        &uint_segment(fixture::TABLE),
        &[1],
        &uint_segment(field),
        &[kind],
    ];
    hbam.subtree(&path)
        .unwrap()
        .map(|index| {
            index
                .children
                .iter()
                .map(|e| {
                    let ids = om_hbam::RidList::from_node(e).unwrap().iter().collect();
                    (e.segment.0.to_vec(), ids)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A failed save of a commit that empties a word index with no counts
/// leaf puts the field's whole index subtree back as it was (#448): the
/// emptied node is kept, so the record has its index to go back into.
#[test]
fn a_failed_save_puts_an_emptied_index_back() {
    use om_hbam::slot::uint_segment;
    use om_hbam::{serial, OwnedNode};
    let mut root = fixture::tree(&[], true);
    let table = uint_segment(fixture::TABLE);
    let name = uint_segment(fixture::NAME);
    root.remove(&[&table, &[5], &serial::encode(2), &name]);
    root.put(
        &[&table, &[1], &name, &[1]],
        OwnedNode::list(vec![1], words_of(&[(1, "Write tests")], TextKeys::Collated)),
    );
    let file = fixture::TempFile::from_tree("emptied-index", &root);
    let as_stored = field_index_on_disk(&file, fixture::NAME);
    let (port, handle) = server::start_with_handle(&file, om_server::ScriptLimits::DEFAULT);
    let path = std::path::absolute(&file.0).unwrap();
    let id = handle
        .files()
        .into_iter()
        .find(|f| f.path == path)
        .unwrap()
        .id;
    let (mut session, _) = server::logged_in(port, "emptied index");
    assert!(handle.failing_commit_saves(id, true));
    assert!(commit_cells(&mut session, 1, &[(fixture::NAME, "")]).is_err());
    assert!(handle.failing_commit_saves(id, false));
    commit_cells(&mut session, 2, &[(fixture::COUNT, "9")]).unwrap();
    assert_eq!(field_index_on_disk(&file, fixture::NAME), as_stored);
    assert_eq!(name_on_disk(&file, 1).as_deref(), Some("Write tests"));
    session.disconnect().unwrap();
}

/// In a Unicode raw word index whose words FileMaker broke otherwise
/// (`a_b` one word, query-program.md 7.1), a record edited to a value whose
/// computed words did not change is still put under them: the sweep adds
/// what the move did not (#448).
#[test]
fn a_record_whose_keys_did_not_move_gets_them_in_a_disagreeing_index() {
    use om_hbam::slot::uint_segment;
    use om_hbam::{serial, OwnedNode, RidList};
    let mut root = fixture::tree(&[], true);
    fixture::set_field_language(&mut root, fixture::TABLE, fixture::NAME, 2);
    let table = uint_segment(fixture::TABLE);
    let name = uint_segment(fixture::NAME);
    root.put(
        &[&table, &[5], &serial::encode(1), &name],
        om_scripts::text_node(&[], "a_b"),
    );
    // FileMaker's one word for record 1, "Ship"'s for record 2.
    let raw = |word: &str| TextKeys::Raw.key(word).unwrap();
    let mut entries = vec![
        [1u64].into_iter().collect::<RidList>().to_node(raw("a_b")),
        [2u64].into_iter().collect::<RidList>().to_node(raw("Ship")),
    ];
    entries.sort_by(|a, b| a.segment.cmp(&b.segment));
    root.put(
        &[&table, &[1], &name, &[1]],
        OwnedNode::list(vec![1], entries),
    );
    let file = fixture::TempFile::from_tree("raw-words", &root);
    let port = server::start(&file);
    let (mut session, _) = server::logged_in(port, "raw words");
    commit_name(&mut session, 1, "a b");
    let mut expected: Vec<(Vec<u8>, Vec<u64>)> =
        om_query::keys::word_entries(&["a b".to_string()], TextKeys::Raw)
            .into_iter()
            .map(|key| (key, vec![1]))
            .collect();
    expected.push((raw("Ship"), vec![2]));
    expected.sort();
    assert_eq!(index_on_disk(&file, fixture::NAME, 1), expected);
    session.disconnect().unwrap();
}

#[test]
fn a_commit_refuses_a_noncanonical_destination_posting() {
    use om_hbam::slot::uint_segment;
    use om_hbam::{serial, OwnedNode, RidList};
    let mut root = fixture::tree(&[], true);
    fixture::set_field_language(&mut root, fixture::TABLE, fixture::NAME, 2);
    let table = uint_segment(fixture::TABLE);
    let name = uint_segment(fixture::NAME);
    let raw = |word: &str| TextKeys::Raw.key(word).unwrap();
    for (id, text) in [(1, "a_b"), (2, "b")] {
        root.put(
            &[&table, &[5], &serial::encode(id), &name],
            om_scripts::text_node(&[], text),
        );
    }
    // The reader accepts this one-bit bitmap for record 2. The writer
    // would emit a singleton, so it cannot round-trip the stored posting.
    let bitmap = OwnedNode::list(
        raw("b"),
        vec![OwnedNode::leaf(vec![0], 4u32.to_be_bytes().to_vec())],
    );
    let stale = [1].into_iter().collect::<RidList>().to_node(raw("a_b"));
    root.put(
        &[&table, &[1], &name, &[1]],
        OwnedNode::list(vec![1], vec![stale, bitmap]),
    );
    let file = fixture::TempFile::from_tree("noncanonical-posting", &root);
    let before = std::fs::read(&file.0).unwrap();
    let index = field_index_on_disk(&file, fixture::NAME);
    assert_eq!(
        index_on_disk(&file, fixture::NAME, 1),
        [(raw("a_b"), vec![1]), (raw("b"), vec![2])]
    );
    let port = server::start(&file);
    let (mut session, _) = server::logged_in(port, "noncanonical posting");
    // Both values compute {a, b}, so only the sweep would add record 1
    // to b. Refuse the commit before replacing any of its memberships.
    assert!(matches!(
        commit_cells(&mut session, 1, &[(fixture::NAME, "a b")]),
        Err(om_session::SessionError::Protocol(_))
    ));
    assert!(std::fs::read(&file.0).unwrap() == before);

    // Publish an unrelated change to check the in-memory state too.
    commit_cells(
        &mut session,
        2,
        &[(fixture::NAME, "b"), (fixture::COUNT, "9")],
    )
    .unwrap();
    assert!(std::fs::read(&file.0).unwrap() != before);
    assert_eq!(field_index_on_disk(&file, fixture::NAME), index);
    assert_eq!(name_on_disk(&file, 1).as_deref(), Some("a_b"));
    session.disconnect().unwrap();
}
