//! End to end: stored calculations recomputed after Manage Database changes
//! them (schema-path.md 7.2, "Implementation notes (recalculation)"). A
//! client saves the Tasks field catalog with the requests FileMaker Pro
//! sends for a field change (schema-path.md 2.3, 2.3a: lock the schema and
//! the field catalog, op15, op82, the table lock, op61, op13 of the whole
//! catalog with its deferred work list, the attention-2 release, op73, the
//! releases), and the server recomputes the stale stored values of every
//! record, rebuilds their index, writes the file once, and the other
//! client is told through the releases.

#[path = "fixtures/tasks.rs"]
mod fixture;
#[path = "fixtures/server.rs"]
mod server;

use fixture::{TempFile, COUNT, DOUBLED, NAME, OCCURRENCE, TABLE};
use om_formula::Key;
use om_hbam::slot::uint_segment;
use om_hbam::{OwnedNode, RidList};
use om_protocol::authoring;
use om_protocol::schema::{self, Upgrade};
use om_protocol::write::{self, attention, Lock, Unlock, Upload};
use om_protocol::{DownloadRequest, Resume};
use om_session::Session;
use server::{cell, notifications_of, notified, path_key, records_of};

/// Manage Database's transaction ids for the schema lock and the field
/// catalog (schema-path.md 2.1 used `2b`, `2c`...).
const SCHEMA_TXN: u32 = 0x2b;
const CATALOG_TXN: u32 = 0x2c;

/// Deferred work bits (schema-path.md 7.2): formula changed, index rebuilt.
const FORMULA_CHANGED: u32 = 0x80;
const BUILD_INDEX: u32 = 0x10;

/// Save `catalog` as table `TABLE`'s field catalog `[T, 03]` the way
/// FileMaker Pro saves a field change (schema-path.md 2.3a), each reply
/// checked.
fn save_field_catalog(session: &mut Session, file: u32, catalog: &OwnedNode) {
    let reply = try_save_field_catalog(session, file, catalog);
    assert_eq!(reply.unwrap(), schema::update_table_reply());
}

fn try_save_field_catalog(
    session: &mut Session,
    file: u32,
    catalog: &OwnedNode,
) -> Result<Vec<u8>, om_session::SessionError> {
    let t = uint_segment(TABLE);
    let mut id = 1000;
    let mut next = || {
        id += 1;
        id
    };
    let granted = write::LockReply::Granted { mod_count: None }
        .to_result()
        .unwrap();
    let lock = |path: Vec<Vec<u8>>, timeout: u32, txn: u32| Lock {
        waiter: 0x7000_0000,
        timeout,
        want_mod_count: false,
        path,
        argument: 0,
        host_file: file as i32,
        txn,
        lock_type: write::LOCK_WRITE,
    };
    let unlock = |path: Vec<Vec<u8>>, lock_type: u16, attention_mask: u32, txn: u32| Unlock {
        message_address: 1,
        attention_mask,
        path,
        host_file: file as i32,
        txn,
        lock_type,
    };
    let fields = vec![t.clone(), vec![3]];
    // Open Manage Database and the table's field catalog.
    let open = [
        lock(vec![vec![8]], u32::MAX, SCHEMA_TXN),
        lock(fields.clone(), 0, CATALOG_TXN),
    ];
    for request in &open {
        let reply = session.perform(&write::build_lock(next(), file, request));
        assert_eq!(reply.unwrap(), granted, "{:02x?}", request.path);
    }
    let segments: Vec<&[u8]> = fields.iter().map(Vec::as_slice).collect();
    session
        .perform(&om_protocol::build_download_from_host(&DownloadRequest {
            transaction_id: next(),
            file_id: file,
            key_path: &segments,
            resume: Resume::start(),
        }))
        .unwrap();
    session
        .perform(&schema::build_host_timestamp(next(), file))
        .unwrap();
    // OK: the table lock, the catalog's exclusive lock, the whole catalog,
    // the release that notifies, the deferred work.
    let table_lock = lock(vec![t.clone(), vec![5]], 2, 0);
    let reply = session.perform(&write::build_lock(next(), file, &table_lock));
    assert_eq!(reply.unwrap(), granted);
    let upgrade = Upgrade {
        waiter: 0x7000_0000,
        timeout: 0,
        path: fields.clone(),
        host_file: file as i32,
        txn: CATALOG_TXN,
        new_type: 10,
        current_type: write::LOCK_WRITE,
    };
    let reply = session.perform(&schema::build_upgrade(next(), file, &upgrade));
    assert_eq!(reply.unwrap(), granted);
    let upload = Upload {
        payload: catalog.to_wire().unwrap(),
        resume: Resume::start(),
        terminal: Vec::new(),
        flags: write::UPLOAD_NODE,
        dest: fields.clone(),
    };
    session
        .perform(&write::build_upload_to_host(next(), file, &upload))
        .unwrap();
    session
        .perform(&write::build_unlock(
            next(),
            file,
            &unlock(fields.clone(), 10, 2, CATALOG_TXN),
        ))
        .unwrap();
    let reply = session.perform(&schema::build_update_table(next(), file, &vec![t.clone()]));
    for release in [
        unlock(vec![vec![8]], write::LOCK_WRITE, 3, SCHEMA_TXN),
        unlock(
            vec![t.clone(), vec![5]],
            100,
            attention::MODIFY | attention::RECORD,
            0,
        ),
    ] {
        session
            .perform(&write::build_unlock(next(), file, &release))
            .unwrap();
    }
    reply
}

/// The Tasks field catalog `[T, 03]` of `root`.
fn field_catalog(root: &OwnedNode) -> OwnedNode {
    root.at(&[&uint_segment(TABLE), &[3]]).unwrap().clone()
}

/// Give `field` the formula `text` (in the Tasks context) in a field
/// catalog, as Specify Calculation stores it (schema-path.md 5.5).
fn set_formula(catalog: &mut OwnedNode, field: u64, text: &str) {
    let calc = fixture::calc(text);
    let f = uint_segment(field);
    for (child, bytes) in [
        (4u8, Key::from_id(OCCURRENCE).0),
        (5, calc.formatted.unwrap()),
        (6, calc.polish.unwrap()),
    ] {
        catalog.put(
            &[&[5], &f, &[5], &[child]],
            OwnedNode::leaf(Vec::new(), bytes),
        );
    }
}

/// Add a field: its name and 26-byte definition (kind, type, option word;
/// one repetition, English index), schema-path.md 5.5.
fn add_field(
    catalog: &mut OwnedNode,
    field: u64,
    name: &str,
    kind: u8,
    data_type: u8,
    options: u32,
) {
    let mut definition = vec![0; 26];
    definition[0] = kind;
    definition[1] = data_type;
    definition[4..8].copy_from_slice(&21u32.to_be_bytes());
    definition[8..12].copy_from_slice(&options.to_be_bytes());
    definition[24..26].copy_from_slice(&1u16.to_be_bytes());
    let f = uint_segment(field);
    catalog.put(&[&[5], &f, &[2]], OwnedNode::leaf(Vec::new(), definition));
    catalog.put(&[&[5], &f, &[16]], om_scripts::text_node(&[], name));
}

/// Rename a field (the name only, schema-path.md 5.5).
fn rename(catalog: &mut OwnedNode, field: u64, name: &str) {
    let f = uint_segment(field);
    catalog.put(&[&[5], &f, &[16]], om_scripts::text_node(&[], name));
}

/// List deferred work for `field` in the catalog's index (7.2).
fn defer(catalog: &mut OwnedNode, field: u64, bits: u32) {
    catalog.put(
        &[&[1], &[0xfd], &uint_segment(field)],
        OwnedNode::leaf(Vec::new(), bits.to_be_bytes().to_vec()),
    );
}

/// Value-index option bit (schema-path.md 5.5).
const VALUE_INDEX: u32 = 0x8000_0000;

/// The Tasks file with Doubled (`Count * 2`) value-indexed: the option bit
/// and its index over records 1 (6) and 2 (10), as a find would have left
/// it.
fn indexed_tasks() -> OwnedNode {
    let mut root = fixture::tree(&[], true);
    fixture::set_field_options(&mut root, TABLE, DOUBLED, VALUE_INDEX);
    let (t, f) = (uint_segment(TABLE), uint_segment(DOUBLED));
    let mut keys: Vec<OwnedNode> = [("6", 1u64), ("10", 2)]
        .into_iter()
        .map(|(value, record)| {
            let ids: RidList = [record].into_iter().collect();
            ids.to_node(om_collate::number_key(value).unwrap())
        })
        .collect();
    keys.sort_by(|a, b| a.segment.cmp(&b.segment));
    root.put(&[&t, &[1], &f, &[0]], OwnedNode::list(Vec::new(), keys));
    let mut counts = 2u64.to_be_bytes().to_vec();
    counts.extend(2u64.to_be_bytes());
    root.put(
        &[&t, &[1], &f, &[0x10]],
        OwnedNode::leaf(Vec::new(), counts),
    );
    root
}

/// The value-index keys of Doubled the server holds, each with its records.
fn doubled_index(session: &mut Session) -> Vec<(Vec<u8>, Vec<u64>)> {
    let (t, f) = (uint_segment(TABLE), uint_segment(DOUBLED));
    let path: [&[u8]; 4] = [&t, &[1], &f, &[0]];
    let reply = session.download(&path).unwrap();
    let base = path.iter().map(|s| om_hbam::KeySegment(s)).collect();
    let tree =
        om_hbam::KeyTree::from_slots(om_hbam::SlotReader::with_path(&reply.slots, base)).unwrap();
    tree.node(&path)
        .map(|index| {
            index
                .children
                .iter()
                .map(|key| {
                    let ids = RidList::from_node(key).unwrap();
                    (key.segment.0.to_vec(), ids.iter().collect())
                })
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn a_formula_change_recomputes_stored_values_and_their_index() {
    let file = TempFile::from_tree("recalc-formula", &indexed_tasks());
    let port = server::start(&file);
    let (mut designer, id) = server::logged_in(port, "designer");
    let (mut other, _) = server::logged_in(port, "other");
    let before = records_of(&mut other, TABLE);
    assert_eq!(cell(&before, 1, DOUBLED), Some("6"));

    // Doubled becomes Count * 3; FileMaker Pro lists the formula change and
    // the index rebuild of the indexed field.
    let mut catalog = field_catalog(&indexed_tasks());
    set_formula(&mut catalog, DOUBLED, "Count * 3");
    defer(&mut catalog, DOUBLED, FORMULA_CHANGED | BUILD_INDEX);
    save_field_catalog(&mut designer, id, &catalog);
    let t = uint_segment(TABLE);
    // The other client was told what FileMaker Server tells it: the field
    // catalog changed (attention 2) and the table's records (0x201 on
    // `[T, 05]`), not each record (schema-path.md 7.3, 9).
    let mut notices = other.take_notifications();
    notices.extend(notifications_of(&mut other));
    let notices: Vec<(u32, Vec<u8>)> = notices.iter().map(notified).collect();
    assert!(
        notices.contains(&(2, path_key(&[&t, &[3]]))),
        "{notices:02x?}"
    );
    assert!(
        notices.contains(&(attention::MODIFY | attention::RECORD, path_key(&[&t, &[5]]))),
        "{notices:02x?}"
    );
    assert!(
        notices.iter().all(|(_, path)| path.len() <= 6),
        "no record notices: {notices:02x?}"
    );

    let after = records_of(&mut other, TABLE);
    assert_eq!(cell(&after, 1, DOUBLED), Some("9"));
    assert_eq!(cell(&after, 2, DOUBLED), Some("15"));
    assert_eq!(
        cell(&after, 1, NAME),
        Some("Write tests"),
        "other values kept"
    );
    // Each record changed once (its modification count, write-path.md 5.4).
    assert_eq!(cell(&after, 1, 0xfc), Some("1"));
    assert_eq!(cell(&after, 2, 0xfc), Some("1"));
    // The index holds the new values only.
    let key = |v: &str| om_collate::number_key(v).unwrap();
    assert_eq!(
        doubled_index(&mut other),
        [(key("9"), vec![1]), (key("15"), vec![2])]
    );
    // The work list is gone.
    let list: [&[u8]; 4] = [&t, &[3], &[1], &[0xfd]];
    assert_eq!(
        other.download(&list).unwrap().node_state,
        om_protocol::NodeState::Missing
    );
    // The file on disk has it all.
    let saved = om_hbam::HbamFile::open(&file.0).unwrap();
    let tree = saved.key_tree().unwrap();
    let value = |record: u8| {
        tree.node(&[&t, &[5], &[record], &uint_segment(DOUBLED)])
            .and_then(|n| om_hbam::Cell::from_node(n).text())
            .and_then(Result::ok)
    };
    assert_eq!(value(1).as_deref(), Some("9"));
    assert_eq!(value(2).as_deref(), Some("15"));

    // A new stored calculation that reads Doubled, with no deferred entry
    // (the server sees it is new), and one this server cannot evaluate (it
    // reads a global field): the first is computed, the second left empty.
    const LABEL: u64 = 8;
    const ODD: u64 = 9;
    add_field(&mut catalog, LABEL, "Label", 2, 1, 0);
    set_formula(&mut catalog, LABEL, "Name & \":\" & Doubled");
    add_field(&mut catalog, ODD, "Odd", 2, 1, 0);
    set_formula(&mut catalog, ODD, "Setting & \"!\"");
    catalog.remove(&[&[1], &[0xfd]]);
    save_field_catalog(&mut designer, id, &catalog);
    let after = records_of(&mut other, TABLE);
    assert_eq!(cell(&after, 1, LABEL), Some("Write tests:9"));
    assert_eq!(cell(&after, 2, LABEL), Some("Ship:15"));
    assert_eq!(cell(&after, 1, ODD), None);
    assert_eq!(cell(&after, 1, 0xfc), Some("2"));

    // Then the formula Label reads changes: Label follows.
    set_formula(&mut catalog, DOUBLED, "Count * 10");
    defer(&mut catalog, DOUBLED, FORMULA_CHANGED | BUILD_INDEX);
    save_field_catalog(&mut designer, id, &catalog);
    let after = records_of(&mut other, TABLE);
    assert_eq!(cell(&after, 1, DOUBLED), Some("30"));
    assert_eq!(cell(&after, 2, LABEL), Some("Ship:50"));
    assert_eq!(cell(&after, 1, 0xfc), Some("3"));

    // Renaming is not a change of value: nothing is recomputed.
    rename(&mut catalog, DOUBLED, "Times Ten");
    catalog.remove(&[&[1], &[0xfd]]);
    defer(&mut catalog, DOUBLED, 0x200);
    save_field_catalog(&mut designer, id, &catalog);
    let after = records_of(&mut other, TABLE);
    assert_eq!(cell(&after, 1, 0xfc), Some("3"));

    // Deleting the field Doubled reads (Count) drops Count's values and
    // makes Doubled stale, but its formula now names a missing field (what
    // FileMaker Server stores then is not specified): its values are left
    // as they were and the server logs it.
    catalog.remove(&[&[5], &uint_segment(COUNT)]);
    catalog.remove(&[&[1], &[0xfd]]);
    defer(&mut catalog, COUNT, 0x1e1);
    save_field_catalog(&mut designer, id, &catalog);
    let after = records_of(&mut other, TABLE);
    assert_eq!(cell(&after, 1, COUNT), None);
    assert_eq!(cell(&after, 1, DOUBLED), Some("30"));
    assert_eq!(cell(&after, 1, LABEL), Some("Write tests:30"));
}

/// A record whose modification count is the largest a serial key holds (a
/// client's op70 may store any count, write-path.md 5.4) cannot count a
/// change: recomputing a stored calculation leaves its value as it was, and
/// deleting a field leaves that field's values, every repetition, while
/// the other record changes and counts it. Changed without a new count, a
/// client's copy of the record would not show as stale (#361).
#[test]
fn a_record_at_the_largest_count_keeps_its_values() {
    let mut root = indexed_tasks();
    let t = uint_segment(TABLE);
    let mut largest = vec![9];
    largest.extend(om_hbam::serial::encode(u64::MAX));
    root.put(
        &[&t, &[5], &[2], &[0xfc]],
        OwnedNode::leaf(Vec::new(), largest),
    );
    // A second repetition of Count in both records: the cell `Count` then
    // the byte 2 (calc-values.md, "Repetitions in records").
    let second = uint_segment(COUNT * 256 + 2);
    for record in [1u8, 2] {
        root.put(
            &[&t, &[5], &[record], &second],
            om_scripts::text_node(&[], "7"),
        );
    }
    let file = TempFile::from_tree("recalc-largest-count", &root);
    let port = server::start(&file);
    let (mut designer, id) = server::logged_in(port, "designer");
    let largest = u64::MAX.to_string();
    let mut catalog = field_catalog(&root);
    set_formula(&mut catalog, DOUBLED, "Count * 3");
    defer(&mut catalog, DOUBLED, FORMULA_CHANGED | BUILD_INDEX);
    save_field_catalog(&mut designer, id, &catalog);
    let after = records_of(&mut designer, TABLE);
    assert_eq!(cell(&after, 1, DOUBLED), Some("9"));
    assert_eq!(cell(&after, 1, 0xfc), Some("1"));
    assert_eq!(cell(&after, 2, DOUBLED), Some("10"));
    assert_eq!(cell(&after, 2, 0xfc), Some(largest.as_str()));

    catalog.remove(&[&[5], &uint_segment(COUNT)]);
    catalog.remove(&[&[1], &[0xfd]]);
    defer(&mut catalog, COUNT, 0x1e1);
    save_field_catalog(&mut designer, id, &catalog);
    let after = records_of(&mut designer, TABLE);
    assert_eq!(cell(&after, 1, COUNT), None);
    assert_eq!(cell(&after, 1, COUNT * 256 + 2), None);
    assert_eq!(cell(&after, 1, 0xfc), Some("2"));
    assert_eq!(cell(&after, 2, COUNT), Some("5"));
    assert_eq!(cell(&after, 2, COUNT * 256 + 2), Some("7"));
    assert_eq!(cell(&after, 2, 0xfc), Some(largest.as_str()));
}

/// A server that stopped between a client's op13 (which saved the new
/// formula and the work list) and its op73 finishes the work when it next
/// hosts the file, before serving it.
#[test]
fn deferred_work_a_stop_interrupted_is_finished_at_startup() {
    let mut root = fixture::tree(&[], true);
    let t = uint_segment(TABLE);
    let mut catalog = field_catalog(&root);
    set_formula(&mut catalog, DOUBLED, "Count * 4");
    defer(&mut catalog, DOUBLED, FORMULA_CHANGED);
    root.put(&[&t, &[3]], catalog.clone());
    let file = TempFile::from_tree("recalc-recover", &root);
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "client");
    let records = records_of(&mut client, TABLE);
    assert_eq!(cell(&records, 1, DOUBLED), Some("12"));
    assert_eq!(cell(&records, 2, DOUBLED), Some("20"));
    // Saved at once, without the work list.
    let saved = om_hbam::HbamFile::open(&file.0).unwrap();
    let tree = saved.key_tree().unwrap();
    assert!(tree.node(&[&t, &[3], &[1], &[0xfd]]).is_none());
    let value = tree
        .node(&[&t, &[5], &[1], &uint_segment(DOUBLED)])
        .and_then(|n| om_hbam::Cell::from_node(n).text())
        .and_then(Result::ok);
    assert_eq!(value.as_deref(), Some("12"));

    // A pending removal has no old field definition after restart. Leave
    // all pending work intact, including the otherwise replayable formula.
    catalog.remove(&[&[5], &uint_segment(NAME)]);
    defer(&mut catalog, NAME, 0x1e1);
    root.put(&[&t, &[3]], catalog);
    let file = TempFile::from_tree("recalc-recover-unknown-old-type", &root);
    let before = std::fs::read(&file.0).unwrap();
    let (mut client, _) = server::logged_in(server::start(&file), "client");
    let records = records_of(&mut client, TABLE);
    assert!(cell(&records, 1, NAME).is_some());
    assert_eq!(cell(&records, 1, DOUBLED), Some("6"));
    assert_eq!(std::fs::read(&file.0).unwrap(), before);
}

/// What a request was answered: its reply, or the system exception it got.
type Answer = Result<Vec<u8>, String>;

/// How a row's request is sent, on the session to the file with an id.
type SendRequest = Box<dyn Fn(&mut Session, u32) -> Answer>;

/// Whether a row's request made its change, read back on the session.
type Made = Box<dyn Fn(&mut Session, &Answer) -> bool>;

/// A request that would change a file, as one row of the tests below: what
/// it needs in place first (a lock and a staged edit, the work op73 does),
/// how it is sent, how its refusal reads, and, on a writable file, the
/// change it made, read back. No refusal may be the reply of a write that
/// was done.
struct Row {
    what: &'static str,
    prepare: Box<dyn Fn(&mut Session)>,
    send: SendRequest,
    refused: fn(&Answer) -> bool,
    made: Made,
    /// Admitted, it reaches the engine, which decides again: the transition
    /// test runs it.
    engine: bool,
}

/// The Tasks file with Photo and the scripts the rows run: writable, as
/// each row first runs on it; with `read_only`, field work deferred for a
/// field whose definition is gone (Name's), which no server can do, so
/// that it is hosted read-only from the start. Its field catalog comes back
/// too.
fn tasks_for_writes(tag: &str, read_only: bool) -> (TempFile, OwnedNode) {
    let (file, root) = tasks_tree_for_writes(tag, read_only);
    (file, field_catalog(&root))
}

/// [`tasks_for_writes`], with the file's whole tree.
fn tasks_tree_for_writes(tag: &str, read_only: bool) -> (TempFile, OwnedNode) {
    use fixture::{exit_with, go_to_record, on, script, set_field, set_variable};
    // Set Field's error and the commit's, as `set|commit`.
    let edit = script(
        4,
        "Edit",
        vec![
            on(86, true),
            go_to_record(1),
            set_field(COUNT, "99"),
            set_variable("$set", "Get ( LastError )"),
            om_scripts::ScriptStep::new(75),
            exit_with("$set & \"|\" & Get ( LastError )"),
        ],
    );
    let left_open = script(
        6,
        "Left open",
        vec![go_to_record(1), set_field(COUNT, "98")],
    );
    let mut root = fixture::tree(&[edit, left_open], true);
    fixture::add_photo(&mut root);
    let mut catalog = field_catalog(&root);
    if read_only {
        catalog.remove(&[&[5], &uint_segment(NAME)]);
        defer(&mut catalog, NAME, 0x1e1);
        root.put(&[&uint_segment(TABLE), &[3]], catalog);
    }
    (TempFile::from_tree(tag, &root), root)
}

/// The deferred-work list of Tasks `[T, 03, 01, fd]`.
fn deferred_path() -> Vec<Vec<u8>> {
    vec![uint_segment(TABLE), vec![3], vec![1], vec![0xfd]]
}

/// The node at `path`, as a download answers: its state and value.
fn read_back(session: &mut Session, path: &[Vec<u8>]) -> (om_protocol::NodeState, Vec<u8>) {
    let path: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
    let reply = session.download(&path).unwrap();
    (reply.node_state, reply.value)
}

/// An upload of `payload` to `dest` with `flags`, in one part.
fn upload(flags: u8, dest: Vec<Vec<u8>>, payload: Vec<u8>) -> Upload {
    Upload {
        payload,
        resume: Resume::start(),
        terminal: 0x10u32.to_be_bytes().to_vec(),
        flags,
        dest,
    }
}

/// Count's build-index work `0x10` as a deferred-work list: a change of
/// Tasks' field catalog no file of these tests holds.
fn count_work() -> OwnedNode {
    OwnedNode::list(
        vec![0xfd],
        vec![OwnedNode::leaf(
            uint_segment(COUNT),
            0x10u32.to_be_bytes().to_vec(),
        )],
    )
}

/// Whether Count's work `0x10` is in the deferred-work list.
fn count_work_listed(session: &mut Session) -> bool {
    let mut path = deferred_path();
    path.push(uint_segment(COUNT));
    read_back(session, &path)
        == (
            om_protocol::NodeState::Terminal,
            0x10u32.to_be_bytes().to_vec(),
        )
}

/// An edit of record 1's Name as staged records, `name` its new value.
fn staged_name(name: &str) -> OwnedNode {
    OwnedNode::list(
        Vec::new(),
        vec![OwnedNode::list(
            om_hbam::serial::encode(1),
            vec![OwnedNode::leaf(
                uint_segment(NAME),
                om_scsu::encode_scsu(name),
            )],
        )],
    )
}

/// The op70 list that commits record 1's staged Name.
fn name_commit_list() -> Vec<u8> {
    let changed = OwnedNode::list(
        om_hbam::serial::encode(1),
        vec![OwnedNode::leaf(uint_segment(NAME), Vec::new())],
    );
    write::commit_list_tables(vec![(uint_segment(TABLE), vec![changed])]).unwrap()
}

/// Lock record 1 for transaction 0x31, as FileMaker Pro does before it
/// stages an edit (op60); its reply unchecked, for a read-only file's
/// preparation too.
fn lock_record_1(session: &mut Session) {
    let (t, record) = (uint_segment(TABLE), om_hbam::serial::encode(1));
    let path: [&[u8]; 3] = [&t, &[5], &record];
    let _ = session.lock(&path, 0x31, true);
}

/// Record 1's Name, as a client reads it.
fn name_of_record_1(session: &mut Session) -> Option<String> {
    cell(&records_of(session, TABLE), 1, NAME).map(str::to_owned)
}

/// An op88 payload holding `bytes` as one stream, with its directory and
/// MD5 (containers.md 2, 3.1).
fn binary_payload(bytes: &[u8]) -> Vec<u8> {
    let mut directory = 1u16.to_be_bytes().to_vec();
    directory.extend(b"FILE");
    directory.extend((bytes.len() as u32).to_be_bytes());
    let md5 = om_data::containers::md5([(&b"FILE"[..], bytes)]);
    OwnedNode::list(
        Vec::new(),
        vec![
            OwnedNode::leaf(vec![2], directory),
            OwnedNode::leaf(vec![4], md5.to_vec()),
            OwnedNode::leaf(b"FILE".to_vec(), bytes.to_vec()),
        ],
    )
    .to_wire()
    .unwrap()
}

/// op86 with a schema instruction (DeleteTable of Notes), keyed for the
/// session as FileMaker Pro keys it.
fn delete_notes_program(session: &Session, file_id: u32) -> Vec<u8> {
    use om_query::program::{leaf, opcode};
    let t = uint_segment(fixture::NOTES);
    let mut program = OwnedNode::list(Vec::new(), Vec::new());
    for (key, value) in [
        (leaf::OPCODE, vec![opcode::DELETE_TABLE]),
        (leaf::SRC, [vec![t.len() as u8], t].concat()),
        (leaf::FILE_ID, file_id.to_be_bytes().to_vec()),
        (
            leaf::ACCESS_KEY,
            om_crypto::query_access_key(1, session.connection_key(), file_id).to_vec(),
        ),
    ] {
        program.put(&[&[1], &[1], &[key]], OwnedNode::leaf(Vec::new(), value));
    }
    program.to_wire().unwrap()
}

/// A server script's run as an answer: its error and result, `error|result`.
fn run_script(session: &mut Session, script: u64) -> Answer {
    let reply = session
        .perform_script_on_server(script, "", true)
        .map_err(|e| e.to_string())?;
    Ok(format!("{}|{}", reply.error, reply.result).into_bytes())
}

/// A script's answer (`error|result`) with an error in it: the run's, or one
/// its result reports (script 4's `set|commit`).
fn script_failed(answer: &Answer) -> bool {
    answer.as_deref().is_ok_and(|bytes| {
        String::from_utf8_lossy(bytes)
            .split('|')
            .any(|part| part.parse::<i64>().is_ok_and(|code| code != 0))
    })
}

/// Every request a client sends that changes a file, each with how its
/// refusal reads and the change it makes on a writable file. `root` is the
/// file's tree (an authoring edit names the revision of what it edits).
fn write_rows(root: &OwnedNode) -> Vec<Row> {
    use om_protocol::container;
    let exception = |answer: &Answer| answer.is_err();
    let status = |answer: &Answer| {
        let bytes = answer.as_deref().unwrap_or_default();
        bytes.get(..4).is_some_and(|code| code != [0; 4])
    };
    let none = || Box::new(|_: &mut Session| {}) as Box<dyn Fn(&mut Session)>;
    let into_file = |op13: bool, flags: u8| {
        move |session: &mut Session, id: u32| {
            let payload = match flags {
                write::UPLOAD_VALUE => Vec::new(),
                _ => count_work().to_wire().unwrap(),
            };
            let mut dest = deferred_path();
            if flags == write::UPLOAD_VALUE {
                dest.push(uint_segment(COUNT));
            }
            let request = upload(flags, dest, payload);
            let bytes = match op13 {
                true => write::build_upload_to_host(1, id, &request),
                false => write::build_upload(1, id, &request),
            };
            session.perform(&bytes).map_err(|e| e.to_string())
        }
    };
    let to_temp = |op13: bool| {
        move |session: &mut Session, id: u32| {
            let dest = vec![uint_segment(TABLE), vec![5]];
            let request = upload(
                write::UPLOAD_FLAGS,
                dest,
                staged_name("Staged").to_wire().unwrap(),
            );
            let bytes = match op13 {
                true => write::build_upload_to_host(2, id, &request),
                false => write::build_upload(2, id, &request),
            };
            session.perform(&bytes).map_err(|e| e.to_string())
        }
    };
    // What a staged edit of Name becomes once committed.
    let staged_then_committed = |session: &mut Session, _: &Answer| {
        let committed = session.perform(&write::build_commit(
            3,
            session.opened_file().unwrap().reply.file_id,
            &write::Commit {
                list: name_commit_list(),
                resume: Resume::start(),
                flags: write::COMMIT_COMPLETE_BEFORE_RETURNING,
                txn: 0x31,
            },
        ));
        committed.is_ok() && name_of_record_1(session).as_deref() == Some("Staged")
    };
    let left_open = [vec![17], vec![5], om_hbam::serial::encode(6)];
    let revision = {
        let path: Vec<&[u8]> = left_open.iter().map(Vec::as_slice).collect();
        authoring::content_revision(root.at(&path).expect("script 6"))
    };
    vec![
        Row {
            what: "op13 node into the file",
            prepare: none(),
            send: Box::new(into_file(true, write::UPLOAD_NODE)),
            refused: exception,
            made: Box::new(|session, _| count_work_listed(session)),
            engine: true,
        },
        Row {
            what: "op13 value into the file",
            prepare: none(),
            send: Box::new(into_file(true, write::UPLOAD_VALUE)),
            refused: exception,
            made: Box::new(|session, _| count_work_listed(session)),
            engine: true,
        },
        Row {
            what: "op14 node into the file (temporary-area flag clear)",
            prepare: none(),
            send: Box::new(into_file(false, write::UPLOAD_NODE)),
            refused: exception,
            made: Box::new(|session, _| count_work_listed(session)),
            engine: true,
        },
        Row {
            what: "op13 into the temporary area",
            prepare: Box::new(lock_record_1),
            send: Box::new(to_temp(true)),
            refused: exception,
            made: Box::new(staged_then_committed),
            engine: false,
        },
        Row {
            what: "op14 into the temporary area",
            prepare: Box::new(lock_record_1),
            send: Box::new(to_temp(false)),
            refused: exception,
            made: Box::new(staged_then_committed),
            engine: false,
        },
        Row {
            what: "op70 commit",
            prepare: Box::new(|session| {
                lock_record_1(session);
                let t = uint_segment(TABLE);
                let _ = session.upload_records(&t, staged_name("Changed").children.clone());
            }),
            send: Box::new(|session, id| {
                let commit = write::Commit {
                    list: name_commit_list(),
                    resume: Resume::start(),
                    flags: write::COMMIT_COMPLETE_BEFORE_RETURNING,
                    txn: 0x31,
                };
                session
                    .perform(&write::build_commit(4, id, &commit))
                    .map_err(|e| e.to_string())
            }),
            refused: exception,
            made: Box::new(|session, _| name_of_record_1(session).as_deref() == Some("Changed")),
            engine: true,
        },
        Row {
            what: "op71 create a record",
            prepare: none(),
            send: Box::new(|session, id| {
                let create = write::CreateRecord {
                    table: vec![uint_segment(TABLE)],
                    count: 1,
                    txn: 0x32,
                };
                session
                    .perform(&write::build_create_record(5, id, &create))
                    .map_err(|e| e.to_string())
            }),
            refused: status,
            made: Box::new(|session, answer| {
                let Ok(reply) =
                    write::parse_create_record_reply(answer.as_deref().unwrap_or_default())
                else {
                    return false;
                };
                // The new last id is saved before the id is answered.
                let last = read_back(session, &[uint_segment(TABLE), vec![0]]).1;
                reply.status == 0 && reply.record.first().is_some_and(|id| last.ends_with(id))
            }),
            engine: true,
        },
        Row {
            what: "op72 advance a serial",
            prepare: none(),
            send: Box::new(|session, id| {
                let serial = write::GetSerial {
                    table: vec![uint_segment(TABLE)],
                    field: vec![uint_segment(COUNT)],
                    value: String::new(),
                    increment: 1,
                    flags: write::SERIAL_INCREMENT,
                };
                session
                    .perform(&write::build_get_serial(6, id, &serial))
                    .map_err(|e| e.to_string())
            }),
            refused: status,
            made: Box::new(|session, answer| {
                let Ok(reply) =
                    write::parse_get_serial_reply(answer.as_deref().unwrap_or_default())
                else {
                    return false;
                };
                let next: Option<u64> = reply.value.parse().ok().map(|v: u64| v + 1);
                let path = [
                    uint_segment(TABLE),
                    vec![3],
                    vec![5],
                    uint_segment(COUNT),
                    vec![0x0c],
                ];
                let stored = om_scsu::decode_scsu(&read_back(session, &path).1).ok();
                reply.status == 0 && next.map(|n| n.to_string()) == stored
            }),
            engine: true,
        },
        Row {
            what: "op73 update a table (Count's work listed first)",
            prepare: Box::new(|session| {
                let dest = deferred_path();
                let dest: Vec<&[u8]> = dest.iter().map(Vec::as_slice).collect();
                let _ = session.upload_to_host(&dest, &count_work());
            }),
            send: Box::new(|session, id| {
                session
                    .perform(&schema::build_update_table(
                        7,
                        id,
                        &vec![uint_segment(TABLE)],
                    ))
                    .map_err(|e| e.to_string())
            }),
            refused: exception,
            made: Box::new(|session, _| {
                read_back(session, &deferred_path()).0 == om_protocol::NodeState::Missing
            }),
            engine: true,
        },
        Row {
            what: "op86 a program that deletes a table",
            prepare: none(),
            send: Box::new(|session, id| {
                let program = delete_notes_program(session, id);
                session
                    .query(&program)
                    .map(|reply| reply.slots)
                    .map_err(|e| e.to_string())
            }),
            refused: |answer| {
                answer
                    .as_deref()
                    .is_ok_and(|slots| slots.windows(3).any(|s| s == [2, 0, 200]))
            },
            made: Box::new(|session, _| {
                read_back(session, &[uint_segment(fixture::NOTES)]).0
                    == om_protocol::NodeState::Missing
            }),
            engine: true,
        },
        Row {
            what: "op87 adjust a reference count",
            prepare: none(),
            send: Box::new(|session, id| {
                let adjust = container::AdjustReferenceCount {
                    path: vec![
                        uint_segment(TABLE),
                        vec![0x1f],
                        vec![5],
                        om_hbam::serial::encode(fixture::PHOTO_MEMBER),
                    ],
                    delta: 1,
                };
                session
                    .perform(&container::build_adjust_reference_count(8, id, &adjust))
                    .map_err(|e| e.to_string())
            }),
            refused: |answer| {
                answer
                    .as_deref()
                    .is_ok_and(|b| b == container::COUNT_FAILED.to_be_bytes())
            },
            made: Box::new(|session, answer| {
                let path = [
                    uint_segment(TABLE),
                    vec![0x1f],
                    vec![5],
                    om_hbam::serial::encode(fixture::PHOTO_MEMBER),
                    vec![0x12],
                ];
                answer.as_deref().is_ok_and(|b| b == 2u32.to_be_bytes())
                    && read_back(session, &path).1 == 2u32.to_be_bytes()
            }),
            engine: true,
        },
        Row {
            what: "op88 upload container data",
            prepare: none(),
            send: Box::new(|session, id| {
                let binary = container::UploadBinary {
                    payload: binary_payload(b"kept"),
                    resume: Resume::start(),
                    table: vec![uint_segment(TABLE)],
                    field: vec![uint_segment(fixture::PHOTO)],
                    solved_path: String::new(),
                    reserved: [0; 16],
                };
                session
                    .perform(&container::build_upload_binary(9, id, &binary))
                    .map_err(|e| e.to_string())
            }),
            refused: |answer| {
                answer.as_ref().is_ok_and(|bytes| {
                    container::parse_upload_binary_reply(bytes)
                        .is_ok_and(|reply| reply.status != 0 && reply.member.is_empty())
                })
            },
            made: Box::new(|session, answer| {
                let Some(member) = answer.as_ref().ok().and_then(|bytes| {
                    let reply = container::parse_upload_binary_reply(bytes).ok()?;
                    (reply.status == 0)
                        .then(|| reply.member.first().cloned())
                        .flatten()
                }) else {
                    return false;
                };
                let path = [uint_segment(TABLE), vec![0x1f], vec![5], member];
                read_back(session, &path).0 == om_protocol::NodeState::NonTerminal
            }),
            engine: true,
        },
        Row {
            what: "op240 Save",
            prepare: none(),
            send: Box::new(|session, id| {
                session
                    .perform(&authoring::build_request(10, id, &authoring::Request::Save).unwrap())
                    .map_err(|e| e.to_string())
            }),
            refused: |answer| {
                answer.as_ref().is_ok_and(|bytes| {
                    matches!(
                        authoring::parse_reply(&authoring::Request::Save, bytes),
                        Ok(authoring::Reply::Refused { .. })
                    )
                })
            },
            made: Box::new(|_, answer| {
                answer.as_ref().is_ok_and(|bytes| {
                    authoring::parse_reply(&authoring::Request::Save, bytes)
                        == Ok(authoring::Reply::Saved)
                })
            }),
            engine: true,
        },
        Row {
            what: "op240 replace a script's steps",
            prepare: none(),
            send: Box::new(move |session, id| {
                session
                    .perform(&authoring::build_request(11, id, &replace_steps(revision)).unwrap())
                    .map_err(|e| e.to_string())
            }),
            refused: |answer| {
                answer.as_ref().is_ok_and(|bytes| {
                    matches!(
                        authoring::parse_reply(&replace_steps([0; 32]), bytes),
                        Ok(authoring::Reply::Refused { .. })
                    )
                })
            },
            made: Box::new(|session, answer| {
                let replaced = answer.as_ref().is_ok_and(|bytes| {
                    matches!(
                        authoring::parse_reply(&replace_steps([0; 32]), bytes),
                        Ok(authoring::Reply::Replaced(authoring::Replaced {
                            changed: true,
                            ..
                        }))
                    )
                });
                // The script runs its new steps.
                replaced && run_script(session, 6).as_deref() == Ok(&b"0|replaced"[..])
            }),
            engine: true,
        },
        Row {
            what: "op30 a server script that sets a field and commits",
            prepare: none(),
            send: Box::new(|session, _| run_script(session, 4)),
            refused: script_failed,
            made: Box::new(|session, answer| {
                !script_failed(answer) && cell(&records_of(session, TABLE), 1, COUNT) == Some("99")
            }),
            engine: true,
        },
        Row {
            what: "op30 a server script that sets a field and ends",
            prepare: none(),
            send: Box::new(|session, _| run_script(session, 6)),
            refused: script_failed,
            made: Box::new(|session, answer| {
                !script_failed(answer) && cell(&records_of(session, TABLE), 1, COUNT) == Some("98")
            }),
            engine: true,
        },
    ]
}

/// op240's request to replace script 6's steps with one that exits with
/// "replaced", at the script's revision `expected`.
fn replace_steps(expected: authoring::Revision) -> authoring::Request {
    let steps = vec![fixture::exit_with("\"replaced\"")];
    authoring::Request::ReplaceScriptSteps(authoring::ReplaceScriptSteps {
        script: 6,
        expected,
        definition: fixture::script(6, "Left open", steps).encode(),
    })
}

/// #737 and the writes like it: every request a client sends that changes
/// a file is first sent to a writable copy, where it makes its change,
/// read back; then to the same file hosted read-only (here its deferred
/// work cannot be done at startup), where it is refused and none is
/// answered as a write that was done: a system exception where the reply
/// has no status (op13 and op14, into the file or the temporary area;
/// op70; op73), otherwise the status or count of a refusal (and a server
/// script's error). The connection carries on, and the file is as it was.
/// What FileMaker Server answers for a file it serves read-only is
/// unmeasured (#790).
#[test]
fn every_write_to_a_file_hosted_read_only_is_refused() {
    let (writable, root) = tasks_tree_for_writes("recalc-writes-writable", false);
    let (read_only, read_only_root) = tasks_tree_for_writes("recalc-writes-read-only", true);
    let before = std::fs::read(&read_only.0).unwrap();
    let rows = write_rows(&root);
    let read_only_rows = write_rows(&read_only_root);
    for (row, read_only_row) in rows.iter().zip(&read_only_rows) {
        // Each row on a writable copy of its own: it makes its change.
        let copy = TempFile(
            writable
                .0
                .with_extension(format!("{}.fmp12", row.what.len())),
        );
        std::fs::copy(&writable.0, &copy.0).unwrap();
        let (mut client, id) = server::logged_in(server::start(&copy), "writer");
        (row.prepare)(&mut client);
        let answer = (row.send)(&mut client, id);
        assert!(
            !(row.refused)(&answer),
            "{}: refused on a writable file: {answer:02x?}",
            row.what
        );
        assert!(
            (row.made)(&mut client, &answer),
            "{}: made no change: {answer:02x?}",
            row.what
        );
        client.disconnect().unwrap();
        // The same on the read-only file: refused, nothing made.
        let (mut client, id) = server::logged_in(server::start(&read_only), "designer");
        (read_only_row.prepare)(&mut client);
        let answer = (read_only_row.send)(&mut client, id);
        assert!(
            (read_only_row.refused)(&answer),
            "{}: {answer:02x?}",
            row.what
        );
        assert_eq!(std::fs::read(&read_only.0).unwrap(), before, "{}", row.what);
        client.disconnect().unwrap();
    }
    let (mut client, _) = server::logged_in(server::start(&read_only), "reader");
    let records = records_of(&mut client, TABLE);
    assert_eq!(cell(&records, 1, DOUBLED), Some("6"));
    assert_eq!(cell(&records, 1, COUNT), Some("3"));
    assert_eq!(std::fs::read(&read_only.0).unwrap(), before);
}

/// On a file hosted read-only a server script opens no record for editing:
/// Set Field fails with 3 where the script sees it (script 4 keeps Set
/// Field's error before its commit), and a script that sets a field and
/// ends reports 3, never 0 with its edit dropped when the session ends.
#[test]
fn a_server_script_opens_no_record_on_a_file_hosted_read_only() {
    let (read_only, _) = tasks_tree_for_writes("recalc-script-read-only", true);
    let (mut client, _) = server::logged_in(server::start(&read_only), "scripter");
    let edit = run_script(&mut client, 4).unwrap();
    assert!(
        String::from_utf8_lossy(&edit).ends_with("|3|0"),
        "{}",
        String::from_utf8_lossy(&edit)
    );
    let left_open = run_script(&mut client, 6).unwrap();
    assert!(
        left_open.starts_with(b"3|"),
        "{}",
        String::from_utf8_lossy(&left_open)
    );
    assert_eq!(cell(&records_of(&mut client, TABLE), 1, COUNT), Some("3"));
}

/// An op13 upload the file cannot hold where it is sent (here under
/// `[T, 00]`, which holds a value: the file writer cannot write a list
/// there, `om_hbam::writer::put_refusals`) is not stored, and fails as an
/// upload whose save failed does, rather than answer done.
#[test]
fn an_upload_the_file_cannot_hold_fails() {
    let (writable, _) = tasks_tree_for_writes("recalc-cannot-hold", false);
    let before = std::fs::read(&writable.0).unwrap();
    let (mut client, id) = server::logged_in(server::start(&writable), "uploader");
    let dest = vec![uint_segment(TABLE), vec![0], vec![5]];
    let request = upload(write::UPLOAD_NODE, dest, count_work().to_wire().unwrap());
    let answer = client.perform(&write::build_upload_to_host(1, id, &request));
    assert!(answer.is_err(), "{answer:02x?}");
    assert_eq!(std::fs::read(&writable.0).unwrap(), before);
}

/// A write admitted while its file was writable, whose file becomes
/// read-only before the engine takes the write lock (as another
/// connection's op73 can make it), is refused by the engine and answered
/// as not stored, never with the reply of a write that was done: every row
/// that reaches the engine once admitted, each on a server of its own (a
/// file stays read-only once it is), its change not made and the file on
/// disk as it was.
#[test]
fn a_write_whose_file_becomes_read_only_before_it_is_stored_fails() {
    let (writable, root) = tasks_tree_for_writes("recalc-read-only-meanwhile", false);
    for row in write_rows(&root).iter().filter(|row| row.engine) {
        let copy = TempFile(
            writable
                .0
                .with_extension(format!("{}.fmp12", row.what.len())),
        );
        std::fs::copy(&writable.0, &copy.0).unwrap();
        let (port, handle) = server::start_with_handle(&copy, om_server::ScriptLimits::DEFAULT);
        let (mut client, id) = server::logged_in(port, "designer");
        (row.prepare)(&mut client);
        // What the preparation stored (op73's listed work) is the file's.
        let before = std::fs::read(&copy.0).unwrap();
        handle.read_only_before_next_write();
        let answer = (row.send)(&mut client, id);
        assert!((row.refused)(&answer), "{}: {answer:02x?}", row.what);
        assert!(!(row.made)(&mut client, &answer), "{}: made", row.what);
        assert_eq!(std::fs::read(&copy.0).unwrap(), before, "{}", row.what);
    }
}

/// An op70 refused because its file became read-only after the request
/// was admitted leaves the session's staged edit, and what it is charged,
/// where a refusal at admission leaves them: the engine checks before the
/// commit takes anything from staging.
#[test]
fn a_late_refused_commit_leaves_the_staged_edit_as_an_early_one_does() {
    let (writable, _) = tasks_for_writes("recalc-staged-kept", false);
    for late in [false, true] {
        let copy = TempFile(writable.0.with_extension(format!("{late}.fmp12")));
        std::fs::copy(&writable.0, &copy.0).unwrap();
        let (port, handle) = server::start_with_handle(&copy, om_server::ScriptLimits::DEFAULT);
        let (mut client, id) = server::logged_in(port, "stager");
        lock_record_1(&mut client);
        let t = uint_segment(TABLE);
        client
            .upload_records(&t, staged_name("Staged").children.clone())
            .unwrap();
        let staged = handle.upload_memory_in_use();
        assert!(staged > 0);
        match late {
            false => assert!(handle.hold_read_only(1)),
            true => handle.read_only_before_next_write(),
        }
        let commit = write::Commit {
            list: name_commit_list(),
            resume: Resume::start(),
            flags: write::COMMIT_COMPLETE_BEFORE_RETURNING,
            txn: 0x31,
        };
        assert!(client
            .perform(&write::build_commit(1, id, &commit))
            .is_err());
        assert_eq!(handle.upload_memory_in_use(), staged, "late {late}");
        assert_eq!(
            name_of_record_1(&mut client).as_deref(),
            Some("Write tests")
        );
    }
}

/// A later part of an upload, refused because its file became read-only
/// since its first part (op13 into the file, op70's list, op88's
/// container data), ends the upload it belongs to: the parts held and what
/// they are charged are given back at once, not kept until evicted.
#[test]
fn a_refused_continuation_gives_back_the_parts_it_follows() {
    use om_protocol::container;
    let (writable, catalog) = tasks_for_writes("recalc-continuations", false);
    let whole = catalog.to_wire().unwrap();
    let (first, rest) = whole.split_at(whole.len() / 2);
    let commit_list = {
        let changed: Vec<OwnedNode> = (1..=40)
            .map(|id| {
                OwnedNode::list(
                    om_hbam::serial::encode(id),
                    vec![OwnedNode::leaf(uint_segment(NAME), Vec::new())],
                )
            })
            .collect();
        write::commit_list_tables(vec![(uint_segment(TABLE), changed)]).unwrap()
    };
    let (list_first, list_rest) = commit_list.split_at(commit_list.len() / 2);
    let binary = binary_payload(&[7; 4096]);
    let (binary_first, binary_rest) = binary.split_at(binary.len() / 2);
    for what in ["op13", "op70", "op88"] {
        let copy = TempFile(writable.0.with_extension(format!("{what}.fmp12")));
        std::fs::copy(&writable.0, &copy.0).unwrap();
        let (port, handle) = server::start_with_handle(&copy, om_server::ScriptLimits::DEFAULT);
        let (mut client, id) = server::logged_in(port, "uploader");
        let dest = vec![uint_segment(TABLE), vec![3]];
        let upload = |payload: &[u8], resume: Resume| Upload {
            payload: payload.to_vec(),
            resume,
            terminal: Vec::new(),
            flags: write::UPLOAD_NODE,
            dest: dest.clone(),
        };
        let commit = |list: &[u8], resume: Resume| write::Commit {
            list: list.to_vec(),
            resume,
            flags: write::COMMIT_COMPLETE_BEFORE_RETURNING,
            txn: 0x31,
        };
        let binary_part = |payload: &[u8], resume: Resume| container::UploadBinary {
            payload: payload.to_vec(),
            resume,
            table: vec![uint_segment(TABLE)],
            field: vec![uint_segment(fixture::PHOTO)],
            solved_path: String::new(),
            reserved: [0; 16],
        };
        // The first part, while the file is writable: held, a cursor back.
        let resume = match what {
            "op13" => write::parse_upload_reply(
                &client
                    .perform(&write::build_upload_to_host(
                        1,
                        id,
                        &upload(first, Resume::start()),
                    ))
                    .unwrap(),
            )
            .unwrap(),
            "op70" => write::parse_upload_reply(
                &client
                    .perform(&write::build_commit(
                        1,
                        id,
                        &commit(list_first, Resume::start()),
                    ))
                    .unwrap(),
            )
            .unwrap(),
            _ => {
                container::parse_upload_binary_reply(
                    &client
                        .perform(&container::build_upload_binary(
                            1,
                            id,
                            &binary_part(binary_first, Resume::start()),
                        ))
                        .unwrap(),
                )
                .unwrap()
                .resume
            }
        };
        assert!(resume.has_more(), "{what}: the first part was not held");
        assert!(handle.upload_memory_in_use() > 0, "{what}");
        assert!(handle.hold_read_only(1));
        let resume = Resume {
            depth: -1,
            ..resume
        };
        let answer = match what {
            "op13" => client
                .perform(&write::build_upload_to_host(2, id, &upload(rest, resume)))
                .map(|_| ()),
            "op70" => client
                .perform(&write::build_commit(2, id, &commit(list_rest, resume)))
                .map(|_| ()),
            _ => client
                .perform(&container::build_upload_binary(
                    2,
                    id,
                    &binary_part(binary_rest, resume),
                ))
                .map(|bytes| {
                    let reply = container::parse_upload_binary_reply(&bytes).unwrap();
                    assert!(reply.status != 0 && reply.member.is_empty(), "{reply:?}");
                }),
        };
        if what != "op88" {
            assert!(answer.is_err(), "{what}: the continuation was answered");
        }
        assert_eq!(handle.upload_memory_in_use(), 0, "{what}: parts still held");
    }
}

#[test]
fn refused_live_deferred_work_keeps_the_host_read_only() {
    let root = fixture::tree(&[], true);
    let file = TempFile::from_tree("recalc-live-unknown-old-type", &root);
    let port = server::start(&file);
    let (mut designer, id) = server::logged_in(port, "designer");
    let (mut writer, writer_id) = server::logged_in(port, "writer");
    let mut catalog = field_catalog(&root);
    set_formula(&mut catalog, DOUBLED, "Count * 4");
    defer(&mut catalog, DOUBLED, FORMULA_CHANGED);
    // op13 saves the new catalog, but op73 cannot classify this removal.
    defer(&mut catalog, 46, 0x1e1);
    assert!(try_save_field_catalog(&mut designer, id, &catalog).is_err());

    let table = uint_segment(TABLE);
    let pending = [&table[..], &[3], &[1], &[0xfd]];
    let saved = om_hbam::HbamFile::open(&file.0).unwrap();
    assert!(saved.key_tree().unwrap().node(&pending).is_some());
    let before = std::fs::read(&file.0).unwrap();
    let request = write::CreateRecord {
        table: vec![table],
        count: 1,
        txn: 0x51,
    };
    let reply = writer
        .perform(&write::build_create_record(1, writer_id, &request))
        .unwrap();
    assert_eq!(
        write::parse_create_record_reply(&reply).unwrap().status,
        200
    );
    let records = records_of(&mut writer, TABLE);
    assert_eq!(cell(&records, 1, DOUBLED), Some("6"));
    assert_eq!(std::fs::read(&file.0).unwrap(), before);
}

/// A script that runs while Manage Database changes a stored calculation
/// writes with the new definition: its session reads the schema again
/// before each step, so the record it changes after the save gets the new
/// formula's value rather than the old one's over the server's
/// recomputation (script-runtime.md "Implementation notes (PSOS)").
#[test]
fn a_running_script_writes_with_the_rules_of_a_schema_saved_meanwhile() {
    use std::time::{Duration, Instant};

    use fixture::{exit_with, go_to_record, script, set_field, with_calc};
    use om_scripts::ScriptStep;

    // The synthetic second record is a handshake: the script commits
    // "ready" with the old schema, then waits until the other connection
    // releases it. The short pause only paces polling; it cannot release
    // the script, however long the schema save takes.
    let mut pause = with_calc(62, "0.01");
    pause.set_flag(om_scripts::flag::CALCULATION, true);
    let waiting = script(
        4,
        "Across a save",
        vec![
            go_to_record(2),
            set_field(NAME, "\"ready\""),
            ScriptStep::new(75),
            ScriptStep::new(71),
            with_calc(72, "Tasks::Name = \"released\""),
            pause,
            ScriptStep::new(73),
            go_to_record(1),
            set_field(COUNT, "4"),
            ScriptStep::new(75),
            exit_with("Get ( LastError ) & \"|\" & Tasks::Doubled"),
        ],
    );
    let release = script(
        5,
        "Release after save",
        vec![
            go_to_record(2),
            set_field(NAME, "\"released\""),
            ScriptStep::new(75),
            exit_with("Get ( LastError )"),
        ],
    );
    let root = fixture::tree(&[waiting, release], true);
    let file = TempFile::from_tree("recalc-running", &root);
    let port = server::start_with_limits(
        &file,
        om_server::ScriptLimits {
            duration: Duration::from_secs(30),
            ..om_server::ScriptLimits::DEFAULT
        },
    );
    let (mut designer, id) = server::logged_in(port, "designer");
    let (mut scripter, _) = server::logged_in(port, "scripter");
    let started = scripter
        .perform_script_on_server_with_callback(4, "", 7)
        .unwrap();
    assert_eq!(started.error, 0);

    // Acceptance of PSOS is not readiness. Observe the script's committed
    // marker before saving, so its session definitely started with Count * 2.
    let deadline = Instant::now() + Duration::from_secs(30);
    let ready = loop {
        let records = records_of(&mut designer, TABLE);
        if cell(&records, 2, NAME) == Some("ready") {
            break records;
        }
        scripter.poll(Duration::from_millis(10)).unwrap();
        assert!(
            scripter.take_psos_results().is_empty(),
            "script ended before readiness"
        );
        assert!(Instant::now() < deadline, "script did not become ready");
    };
    assert_eq!(cell(&ready, 1, COUNT), Some("3"));
    assert_eq!(cell(&ready, 1, DOUBLED), Some("6"));

    // While the script waits for release, Doubled becomes Count * 3.
    let mut catalog = field_catalog(&root);
    set_formula(&mut catalog, DOUBLED, "Count * 3");
    defer(&mut catalog, DOUBLED, FORMULA_CHANGED);
    save_field_catalog(&mut designer, id, &catalog);
    let recomputed = records_of(&mut designer, TABLE);
    assert_eq!(cell(&recomputed, 1, COUNT), Some("3"));
    assert_eq!(cell(&recomputed, 1, DOUBLED), Some("9"));
    assert_eq!(cell(&recomputed, 2, NAME), Some("ready"));
    scripter.poll(Duration::from_millis(10)).unwrap();
    assert!(
        scripter.take_psos_results().is_empty(),
        "script ended before release"
    );

    let released = designer.perform_script_on_server(5, "", true).unwrap();
    assert_eq!((released.error, released.result.as_str()), (0, "0"));

    let deadline = Instant::now() + Duration::from_secs(30);
    let result = loop {
        scripter.poll(Duration::from_millis(200)).unwrap();
        if let Some(result) = scripter.take_psos_results().pop() {
            break result;
        }
        assert!(Instant::now() < deadline, "no callback");
    };
    assert_eq!(result.callback, 7);
    assert_eq!((result.error, result.result.as_str()), (0, "0|12"));
    let after = records_of(&mut designer, TABLE);
    assert_eq!(cell(&after, 1, COUNT), Some("4"));
    assert_eq!(cell(&after, 1, DOUBLED), Some("12"));
    assert_eq!(cell(&after, 2, DOUBLED), Some("15"));
}

/// A field's type change rewrites what it stores as FileMaker Server does
/// (schema-path.md 7.2.1, #441). Name, a text field of two repetitions
/// holding `1/2/03` and `01-02-03`, becomes a date of one repetition:
/// record 1's second repetition `4/5/06` is deleted (`0x4`, 7.2), the
/// incomplete date is written again as `1/2/2003` and its record counts
/// one more modification; the other, rendered at the same length, stays.
/// Photo, a container holding a reference to library data, becomes text:
/// its value goes and the data is released. Setting, a global of two
/// repetitions cut to one, loses its second from the table's global
/// record (`0x4 | 0x2`) and keeps its first (#491).
#[test]
fn a_type_change_to_date_rewrites_values_and_one_from_container_deletes_them() {
    let mut root = fixture::tree(&[], true);
    fixture::add_photo(&mut root);
    fixture::set_field_repetitions(&mut root, TABLE, NAME, 2);
    fixture::set_field_repetitions(&mut root, TABLE, fixture::SETTING, 2);
    let t = uint_segment(TABLE);
    // Repetition n of field F is the cell F followed by the byte n
    // (calc-values.md, "Repetitions in records").
    let second = |field| [uint_segment(field), vec![2]].concat();
    for (record, cell, text) in [
        (1u64, uint_segment(NAME), "1/2/03"),
        (1, second(NAME), "4/5/06"),
        (2, uint_segment(NAME), "01-02-03"),
    ] {
        root.put(
            &[&t, &[5], &om_hbam::serial::encode(record), &cell],
            om_scripts::text_node(&[], text),
        );
    }
    for (cell, text) in [
        (uint_segment(fixture::SETTING), "on"),
        (second(fixture::SETTING), "off"),
    ] {
        root.put(&[&t, &[8], &cell], om_scripts::text_node(&[], text));
    }
    let file = TempFile::from_tree("recalc-type-change", &root);
    let port = server::start(&file);
    let (mut designer, id) = server::logged_in(port, "designer");
    let member = |session: &mut Session| {
        let m = om_hbam::serial::encode(fixture::PHOTO_MEMBER);
        let path: [&[u8]; 5] = [&t, &[0x1f], &[5], &m, &[0x12]];
        let reply = session.download(&path).unwrap();
        (reply.node_state == om_protocol::NodeState::Terminal)
            .then(|| {
                reply
                    .value
                    .as_slice()
                    .try_into()
                    .ok()
                    .map(u32::from_be_bytes)
            })
            .flatten()
    };
    let global = |session: &mut Session, cell: &[u8]| {
        let path: [&[u8]; 3] = [&t, &[8], cell];
        session.download(&path).unwrap().node_state != om_protocol::NodeState::Missing
    };
    let before = records_of(&mut designer, TABLE);
    assert_eq!(member(&mut designer), Some(1));
    assert!(cell(&before, 1, fixture::PHOTO).is_some());
    assert_eq!(cell(&before, 1, 0x0102), Some("4/5/06"));
    assert!(global(&mut designer, &second(fixture::SETTING)));
    let count = |records: &_, record| cell(records, record, 0xfc).map(str::to_owned);

    // Data type byte 1 of the definition (schema-path.md 5.5): Name to date
    // (3), Photo from container to text (1), with the work Manage Database
    // lists for them (7.2: 0x400 or 0x8, and 0x40 for any type change).
    let mut catalog = field_catalog(&root);
    for (field, data_type) in [(NAME, 3u8), (fixture::PHOTO, 1)] {
        let path: [&[u8]; 3] = [&[5], &uint_segment(field), &[2]];
        let mut definition = catalog.at(&path).unwrap().value.clone().unwrap();
        definition[1] = data_type;
        if field == NAME {
            definition[24..26].copy_from_slice(&1u16.to_be_bytes());
        }
        catalog.put(&path, OwnedNode::leaf(Vec::new(), definition));
    }
    // Name's repetitions cut too: 0x4 and 0x1000 beside the conversion.
    defer(&mut catalog, NAME, 0x400 | 0x1000 | 0x4 | 0x40 | 0x80);
    defer(&mut catalog, fixture::PHOTO, 0x8 | 0x40 | 0x80);
    // Setting's only: the global's work (`0x2`) with the cut.
    let path: [&[u8]; 3] = [&[5], &uint_segment(fixture::SETTING), &[2]];
    let mut definition = catalog.at(&path).unwrap().value.clone().unwrap();
    definition[24..26].copy_from_slice(&1u16.to_be_bytes());
    catalog.put(&path, OwnedNode::leaf(Vec::new(), definition));
    defer(&mut catalog, fixture::SETTING, 0x4 | 0x2 | 0x1000 | 0x40);
    save_field_catalog(&mut designer, id, &catalog);

    let after = records_of(&mut designer, TABLE);
    assert_eq!(cell(&after, 1, NAME), Some("1/2/2003"));
    assert_eq!(cell(&after, 1, 0x0102), None, "the second repetition goes");
    assert_eq!(cell(&after, 2, NAME), Some("01-02-03"));
    assert!(global(&mut designer, &uint_segment(fixture::SETTING)));
    assert!(!global(&mut designer, &second(fixture::SETTING)));
    assert_eq!(cell(&after, 1, fixture::PHOTO), None);
    assert_ne!(
        member(&mut designer),
        Some(1),
        "the library data is released"
    );
    // Record 1 changed (Name and Photo), record 2 did not.
    assert_ne!(count(&after, 1), count(&before, 1));
    assert_eq!(count(&after, 2), count(&before, 2));
}

/// New stored calculations over the container Photo are computed from the
/// data its library member holds, as FileMaker Server's engine computes
/// them (script oracle s34-containers-calc): record 1's photo gives its
/// file name and byte length, and record 2, with no photo, stays empty
/// ("do not evaluate if all referenced fields are empty"). Reading the
/// data leaves the library as it was. Before, the recompute skipped any
/// calculation reading a container and left its values empty (#82). A
/// photo naming a member the library lost is no data to compute from: the
/// calculation fails on that record, which keeps its value, rather than
/// reading the reference's own text.
#[test]
fn stored_calculations_over_a_container_read_its_data() {
    let mut root = fixture::tree(&[], true);
    fixture::add_photo(&mut root);
    let t = uint_segment(TABLE);
    let lost = om_data::containers::reference_text(&om_hbam::serial::encode(99));
    let record = |id: u64| om_hbam::serial::encode(id);
    root.put(
        &[&t, &[5], &record(3), &uint_segment(fixture::PHOTO)],
        om_scripts::text_node(&[], &lost),
    );
    root.put(
        &[&t, &[5], &record(3), &uint_segment(22)],
        om_scripts::text_node(&[], "75"),
    );
    let file = TempFile::from_tree("recalc-container-input", &root);
    let port = server::start(&file);
    let (mut designer, id) = server::logged_in(port, "designer");
    let member_count = |session: &mut Session| {
        let m = om_hbam::serial::encode(fixture::PHOTO_MEMBER);
        let path: [&[u8]; 5] = [&t, &[0x1f], &[5], &m, &[0x12]];
        session.download(&path).unwrap().value
    };
    let count_before = member_count(&mut designer);

    // PhotoName and PhotoLength of the oracle's fields, added as Manage
    // Database adds new fields: no deferred work, the server sees they are
    // new.
    let mut with_calculations = root.clone();
    let names = fixture::photo_names();
    for (field, name, kind, data_type, options, text) in fixture::PHOTO_CALCULATIONS {
        if kind == 2 && field != 23 {
            fixture::add_field(
                &mut with_calculations,
                TABLE,
                field,
                name,
                kind,
                data_type,
                0,
                options,
                0,
            );
            fixture::add_formula(
                &mut with_calculations,
                &names,
                (TABLE, OCCURRENCE),
                field,
                5,
                text,
            );
        }
    }
    save_field_catalog(&mut designer, id, &field_catalog(&with_calculations));

    let after = records_of(&mut designer, TABLE);
    assert_eq!(cell(&after, 1, 21), Some("photo2.png"));
    let length = fixture::png().len().to_string();
    assert_eq!(cell(&after, 1, 22), Some(length.as_str()));
    assert_eq!(cell(&after, 2, 21), None);
    assert_eq!(cell(&after, 2, 22), None);
    assert_eq!(cell(&after, 3, 21), None);
    assert_eq!(
        cell(&after, 3, 22),
        Some("75"),
        "kept, not the reference's length"
    );
    assert_eq!(member_count(&mut designer), count_before);
    assert!(cell(&after, 1, fixture::PHOTO).is_some(), "Photo is kept");
}

/// A repeating stored calculation is computed in each of its repetitions
/// (calc-values.md 8.6): Name repeats three times; Shout, of three
/// repetitions, reads Name's repetition n in repetition n and leaves a
/// repetition whose Name is empty empty ("do not evaluate if all
/// referenced fields are empty"); Counts, of two, reads Count, which does
/// not repeat, plainly (empty in repetition 2) and through `Extend` (its
/// value in each). A formula change rewrites every repetition and drops one
/// that became empty. Before, the recompute skipped repeating
/// calculations and left their values as they were (#82).
#[test]
fn repeating_stored_calculations_are_computed_in_each_repetition() {
    const SHOUT: u64 = 40;
    const COUNTS: u64 = 41;
    let mut root = fixture::tree(&[], true);
    fixture::set_field_repetitions(&mut root, TABLE, NAME, 3);
    let t = uint_segment(TABLE);
    let repetition = |field: u64, n: u64| field * 256 + n;
    root.put(
        &[
            &t,
            &[5],
            &om_hbam::serial::encode(1),
            &uint_segment(repetition(NAME, 2)),
        ],
        om_scripts::text_node(&[], "docs"),
    );
    let file = TempFile::from_tree("recalc-repetitions", &root);
    let port = server::start(&file);
    let (mut designer, id) = server::logged_in(port, "designer");

    let names = fixture::names();
    let mut designed = root.clone();
    for (field, name, repetitions, text) in [
        (
            SHOUT,
            "Shout",
            3,
            "Upper ( Name ) & Get ( CalculationRepetitionNumber )",
        ),
        (COUNTS, "Counts", 2, "Count & \"/\" & Extend ( Count )"),
    ] {
        fixture::add_field(&mut designed, TABLE, field, name, 2, 1, 0, 0, 0);
        fixture::add_formula(&mut designed, &names, (TABLE, OCCURRENCE), field, 5, text);
        fixture::set_field_repetitions(&mut designed, TABLE, field, repetitions);
    }
    let mut catalog = field_catalog(&designed);
    save_field_catalog(&mut designer, id, &catalog);

    let after = records_of(&mut designer, TABLE);
    assert_eq!(cell(&after, 1, SHOUT), Some("WRITE TESTS1"));
    assert_eq!(cell(&after, 1, repetition(SHOUT, 2)), Some("DOCS2"));
    assert_eq!(cell(&after, 1, repetition(SHOUT, 3)), None);
    assert_eq!(cell(&after, 2, SHOUT), Some("SHIP1"));
    assert_eq!(cell(&after, 2, repetition(SHOUT, 2)), None);
    assert_eq!(cell(&after, 1, COUNTS), Some("3/3"));
    assert_eq!(cell(&after, 1, repetition(COUNTS, 2)), Some("/3"));
    assert_eq!(cell(&after, 2, COUNTS), Some("5/5"));
    assert_eq!(cell(&after, 2, repetition(COUNTS, 2)), Some("/5"));

    // Shout answers in repetition 1 only now: its second repetition
    // empties; `Name[1]` reads the first repetition in each.
    set_formula(
        &mut catalog,
        SHOUT,
        "If ( Get ( CalculationRepetitionNumber ) = 1 ; Lower ( Name ) ; Name[1] & \"?\" )",
    );
    defer(&mut catalog, SHOUT, FORMULA_CHANGED);
    save_field_catalog(&mut designer, id, &catalog);
    let after = records_of(&mut designer, TABLE);
    assert_eq!(cell(&after, 1, SHOUT), Some("write tests"));
    assert_eq!(cell(&after, 1, repetition(SHOUT, 2)), Some("Write tests?"));
    assert_eq!(cell(&after, 1, repetition(SHOUT, 3)), Some("Write tests?"));
    assert_eq!(cell(&after, 2, repetition(SHOUT, 2)), Some("Ship?"));
    assert_eq!(cell(&after, 1, COUNTS), Some("3/3"), "Counts is not stale");

    // Then only the plain reading is left: a repetition whose Name is
    // empty is not evaluated, and one that held a value is emptied.
    set_formula(
        &mut catalog,
        SHOUT,
        "If ( Get ( CalculationRepetitionNumber ) = 1 ; Lower ( Name ) )",
    );
    defer(&mut catalog, SHOUT, FORMULA_CHANGED);
    save_field_catalog(&mut designer, id, &catalog);
    let after = records_of(&mut designer, TABLE);
    assert_eq!(cell(&after, 1, SHOUT), Some("write tests"));
    assert_eq!(cell(&after, 1, repetition(SHOUT, 2)), None);
    assert_eq!(cell(&after, 1, repetition(SHOUT, 3)), None);
    assert_eq!(cell(&after, 2, repetition(SHOUT, 2)), None);
}

/// The recompute evaluates a stored calculation as the file's own, with no
/// session: Design functions answer empty but `DatabaseNames`, as
/// FileMaker Server's engine answers them there (calc-functions.md,
/// "Design functions on FileMaker Server", battery `fn-design`). Before,
/// they answered from the file's schema (#82).
#[test]
fn design_functions_answer_as_the_file_context_does() {
    const TABLES: u64 = 42;
    const FILES: u64 = 43;
    let root = fixture::tree(&[], true);
    let file = TempFile::from_tree("recalc-design", &root);
    let port = server::start(&file);
    let (mut designer, id) = server::logged_in(port, "designer");
    let names = fixture::names();
    let mut designed = root.clone();
    for (field, name, text) in [
        (TABLES, "Tables", "TableNames ( \"\" ) & \"|\""),
        (FILES, "Files", "DatabaseNames"),
    ] {
        fixture::add_field(&mut designed, TABLE, field, name, 2, 1, 0, 0, 0);
        fixture::add_formula(&mut designed, &names, (TABLE, OCCURRENCE), field, 5, text);
    }
    save_field_catalog(&mut designer, id, &field_catalog(&designed));
    let after = records_of(&mut designer, TABLE);
    assert_eq!(cell(&after, 1, TABLES), Some("|"));
    assert_eq!(cell(&after, 1, FILES), Some("Tasks"));
}

/// Stored calculations with a container result are recomputed and stored
/// as a server script's commit stores container data (containers.md,
/// "Implementation notes (OpenMaker Server scripts)"; #384). PhotoCopy
/// (`Photo`) names Photo's own member, one more reference to it; record 2,
/// with no photo, stays empty. Decoded (`Base64Decode`, the same data on
/// both records) is stored once as a new member with two references. When
/// PhotoCopy's formula then answers nothing, its reference is released.
/// Before, the recompute skipped container results and left them as they
/// were.
#[test]
fn stored_container_calculations_store_their_data() {
    const COPY: u64 = 44;
    const DECODED: u64 = 45;
    let mut root = fixture::tree(&[], true);
    fixture::add_photo(&mut root);
    let file = TempFile::from_tree("recalc-container-results", &root);
    let port = server::start(&file);
    let (mut designer, id) = server::logged_in(port, "designer");
    let t = uint_segment(TABLE);
    let references = |session: &mut Session, text: &str| {
        let member = om_data::containers::reference(text).unwrap();
        let path: [&[u8]; 5] = [&t, &[0x1f], &[5], &member, &[0x12]];
        let value = session.download(&path).unwrap().value;
        u32::from_be_bytes(value.try_into().unwrap())
    };

    let mut names = fixture::photo_names();
    names.add_field(TABLE, COPY, "PhotoCopy");
    names.add_field(TABLE, DECODED, "Decoded");
    let mut designed = root.clone();
    for (field, name, text) in [
        (COPY, "PhotoCopy", "Photo"),
        (
            DECODED,
            "Decoded",
            "Base64Decode ( \"aGVsbG8=\" ; \"a.txt\" )",
        ),
    ] {
        // A stored calculation (2) with a container result (6).
        fixture::add_field(&mut designed, TABLE, field, name, 2, 6, 0, 0, 0);
        fixture::add_formula(&mut designed, &names, (TABLE, OCCURRENCE), field, 5, text);
    }
    let mut catalog = field_catalog(&designed);
    save_field_catalog(&mut designer, id, &catalog);

    let after = records_of(&mut designer, TABLE);
    let photo = cell(&after, 1, fixture::PHOTO).unwrap().to_owned();
    assert_eq!(cell(&after, 1, COPY), Some(photo.as_str()));
    assert_eq!(cell(&after, 2, COPY), None);
    assert_eq!(references(&mut designer, &photo), 2);
    let decoded = cell(&after, 1, DECODED).unwrap().to_owned();
    assert_ne!(decoded, photo);
    assert_eq!(cell(&after, 2, DECODED), Some(decoded.as_str()));
    assert_eq!(references(&mut designer, &decoded), 2);

    set_formula(&mut catalog, COPY, "\"\"");
    defer(&mut catalog, COPY, FORMULA_CHANGED);
    save_field_catalog(&mut designer, id, &catalog);
    let after = records_of(&mut designer, TABLE);
    assert_eq!(cell(&after, 1, COPY), None);
    assert_eq!(references(&mut designer, &photo), 1);
    assert_eq!(
        cell(&after, 1, DECODED),
        Some(decoded.as_str()),
        "not stale"
    );
    assert_eq!(references(&mut designer, &decoded), 2);
}

/// A calculation whose result type changes from text to container keeps
/// its old text until it is recomputed; text shaped like a library
/// reference there owns no reference. Emptying it must not release the
/// member Photo names (#427's review: Photo's member was deleted while
/// Photo still named it). A container result whose text has that shape is
/// refused: the record keeps its value.
#[test]
fn a_result_changed_to_container_releases_no_reference_its_text_did_not_own() {
    const SHAPED: u64 = 46;
    const FAKE: u64 = 47;
    let mut root = fixture::tree(&[], true);
    fixture::add_photo(&mut root);
    let file = TempFile::from_tree("recalc-result-type", &root);
    let port = server::start(&file);
    let (mut designer, id) = server::logged_in(port, "designer");
    let t = uint_segment(TABLE);
    let photo_member = |session: &mut Session| {
        let m = om_hbam::serial::encode(fixture::PHOTO_MEMBER);
        let path: [&[u8]; 5] = [&t, &[0x1f], &[5], &m, &[0x12]];
        session.download(&path).unwrap()
    };
    // Photo's member is `01`; its reference text is "01" then U+E000.
    let reference = om_data::containers::reference_text(&om_hbam::serial::encode(1));
    let shaped = "\"01\" & Char ( 57344 )";
    let names = fixture::photo_names();
    let mut designed = root.clone();
    // Shaped: a stored text calculation (1); Fake: a container one (6).
    for (field, name, data_type) in [(SHAPED, "Shaped", 1u8), (FAKE, "Fake", 6)] {
        fixture::add_field(&mut designed, TABLE, field, name, 2, data_type, 0, 0x20, 0);
        fixture::add_formula(&mut designed, &names, (TABLE, OCCURRENCE), field, 5, shaped);
    }
    let mut catalog = field_catalog(&designed);
    save_field_catalog(&mut designer, id, &catalog);
    let after = records_of(&mut designer, TABLE);
    assert_eq!(cell(&after, 1, SHAPED), Some(reference.as_str()));
    assert_eq!(
        cell(&after, 1, FAKE),
        None,
        "reference-shaped text is refused"
    );

    // Shaped becomes a container calculation, its formula kept: the
    // shaped text it answers is refused, and its old text, a value of the
    // old type, is dropped with the type change rather than kept uncounted.
    let path: [&[u8]; 3] = [&[5], &uint_segment(SHAPED), &[2]];
    let mut definition = catalog.at(&path).unwrap().value.clone().unwrap();
    definition[1] = 6;
    catalog.put(&path, OwnedNode::leaf(Vec::new(), definition));
    defer(&mut catalog, SHAPED, 0x40);
    save_field_catalog(&mut designer, id, &catalog);
    let after = records_of(&mut designer, TABLE);
    assert_eq!(cell(&after, 1, SHAPED), None);
    // A later save empties it: nothing it never counted is released.
    catalog.remove(&[&[1], &[0xfd]]);
    set_formula(&mut catalog, SHAPED, "\"\"");
    defer(&mut catalog, SHAPED, FORMULA_CHANGED);
    save_field_catalog(&mut designer, id, &catalog);
    let after = records_of(&mut designer, TABLE);
    assert_eq!(cell(&after, 1, SHAPED), None);
    let member = photo_member(&mut designer);
    assert_eq!(
        member.node_state,
        om_protocol::NodeState::Terminal,
        "Photo's data is kept"
    );
    assert_eq!(member.value, 1u32.to_be_bytes().to_vec());
    assert_eq!(cell(&after, 1, fixture::PHOTO), Some(reference.as_str()));
}

/// Startup recovery without the schema it changed from (#427's review: a
/// stop after the schema saved and before its recompute): a container
/// calculation still holds text shaped like Photo's reference, which it
/// never counted. Recomputing it to empty releases nothing, so Photo's
/// member keeps its one reference, and the cell is emptied.
#[test]
fn recovery_releases_no_reference_a_cell_did_not_count() {
    const SHAPED: u64 = 46;
    let mut root = fixture::tree(&[], true);
    fixture::add_photo(&mut root);
    let t = uint_segment(TABLE);
    let names = fixture::photo_names();
    fixture::add_field(&mut root, TABLE, SHAPED, "Shaped", 2, 6, 0, 0x20, 0);
    fixture::add_formula(&mut root, &names, (TABLE, OCCURRENCE), SHAPED, 5, "\"\"");
    let reference = om_data::containers::reference_text(&om_hbam::serial::encode(1));
    root.put(
        &[&t, &[5], &[1], &uint_segment(SHAPED)],
        om_scripts::text_node(&[], &reference),
    );
    let mut catalog = field_catalog(&root);
    defer(&mut catalog, SHAPED, FORMULA_CHANGED);
    root.put(&[&t, &[3]], catalog);
    let file = TempFile::from_tree("recalc-recover-shaped", &root);
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "client");
    let after = records_of(&mut client, TABLE);
    assert_eq!(cell(&after, 1, SHAPED), None);
    assert_eq!(cell(&after, 1, fixture::PHOTO), Some(reference.as_str()));
    let m = om_hbam::serial::encode(fixture::PHOTO_MEMBER);
    let path: [&[u8]; 5] = [&t, &[0x1f], &[5], &m, &[0x12]];
    let member = client.download(&path).unwrap();
    assert_eq!(
        member.node_state,
        om_protocol::NodeState::Terminal,
        "Photo's data is kept"
    );
    assert_eq!(member.value, 1u32.to_be_bytes().to_vec());
}

/// Two old text cells name the key a generated file will receive. Startup
/// must keep the new cell's acquisition even when both old names go away.
#[test]
fn recovery_keeps_new_container_acquisitions_when_old_names_are_removed() {
    const SHAPED_A: u64 = 46;
    const SHAPED_B: u64 = 47;
    const GENERATED: u64 = 48;
    let mut root = fixture::tree(&[], true);
    let table = uint_segment(TABLE);
    let reference = om_data::containers::reference_text(&[1]);
    let names = fixture::names();
    for (field, name, formula) in [
        (SHAPED_A, "ShapedA", "\"\""),
        (SHAPED_B, "ShapedB", "\"\""),
        (
            GENERATED,
            "Generated",
            "If ( Count = 3 ; Base64Decode ( \"aGVsbG8=\" ; \"a.txt\" ) ; \"\" )",
        ),
    ] {
        fixture::add_field(&mut root, TABLE, field, name, 2, 6, 0, 0x20, 0);
        fixture::add_formula(&mut root, &names, (TABLE, OCCURRENCE), field, 5, formula);
        if field != GENERATED {
            root.put(
                &[&table, &[5], &[1], &uint_segment(field)],
                om_scripts::text_node(&[], &reference),
            );
        }
    }
    let mut catalog = field_catalog(&root);
    for field in [SHAPED_A, SHAPED_B, GENERATED] {
        defer(&mut catalog, field, 0x40 | FORMULA_CHANGED);
    }
    root.put(&[&table, &[3]], catalog);
    assert!(root.at(&[&table, &[0x1f]]).is_none());
    let file = TempFile::from_tree("recalc-recover-new-member", &root);
    let (mut client, _) = server::logged_in(server::start(&file), "client");
    let records = records_of(&mut client, TABLE);
    assert_eq!(cell(&records, 1, SHAPED_A), None);
    assert_eq!(cell(&records, 1, SHAPED_B), None);
    assert_eq!(cell(&records, 1, GENERATED), Some(reference.as_str()));
    assert_eq!(cell(&records, 2, GENERATED), None);
    let count = client
        .download(&[&table, &[0x1f], &[5], &[1], &[0x12]])
        .unwrap();
    assert_eq!(count.node_state, om_protocol::NodeState::Terminal);
    assert_eq!(count.value, 1u32.to_be_bytes());
}

/// A skipped Text-to-Container calculation keeps the entered value.
/// Deleting that field later must leave Photo's sole counted reference.
#[test]
fn deleting_a_converted_field_preserves_the_member_owned_by_photo() {
    const ENTERED: u64 = 46;
    let mut root = fixture::tree(&[], true);
    let table = uint_segment(TABLE);
    let field = uint_segment(ENTERED);
    fixture::add_photo(&mut root);
    fixture::add_field(&mut root, TABLE, ENTERED, "Entered", 1, 1, 0, 0, 0);
    let reference = om_data::containers::reference_text(&[1]);
    root.put(
        &[&table, &[5], &[1], &field],
        om_scripts::text_node(&[], &reference),
    );
    let file = TempFile::from_tree("recalc-delete-converted", &root);
    let (mut designer, id) = server::logged_in(server::start(&file), "designer");
    fixture::add_field(&mut root, TABLE, ENTERED, "Entered", 2, 6, 0, 0x20, 0);
    fixture::add_formula(
        &mut root,
        &fixture::names(),
        (TABLE, OCCURRENCE),
        ENTERED,
        5,
        "Notes::Body",
    );
    let mut catalog = field_catalog(&root);
    defer(&mut catalog, ENTERED, 0x40 | FORMULA_CHANGED);
    save_field_catalog(&mut designer, id, &catalog);
    let records = records_of(&mut designer, TABLE);
    assert_eq!(cell(&records, 1, ENTERED), Some(reference.as_str()));

    catalog.remove(&[&[5], &field]);
    defer(&mut catalog, ENTERED, 0x1e9);
    save_field_catalog(&mut designer, id, &catalog);
    let records = records_of(&mut designer, TABLE);
    assert_eq!(cell(&records, 1, ENTERED), None);
    assert_eq!(cell(&records, 1, fixture::PHOTO), Some(reference.as_str()));
    let count = designer
        .download(&[&table, &[0x1f], &[5], &[1], &[0x12]])
        .unwrap();
    assert_eq!(count.node_state, om_protocol::NodeState::Terminal);
    assert_eq!(count.value, 1u32.to_be_bytes());
}

#[test]
fn deleting_a_record_preserves_the_member_named_by_a_surviving_cell() {
    use fixture::{exit_with, go_to_record, on, script};
    use om_scripts::ScriptStep;

    const ENTERED: u64 = 46;
    const GLOBAL: u64 = 47;
    let table = uint_segment(TABLE);
    let field = uint_segment(ENTERED);
    let photo = uint_segment(fixture::PHOTO);
    let global = uint_segment(GLOBAL);
    let reference = om_data::containers::reference_text(&[1]);

    for global_owner in [false, true] {
        let delete = script(
            4,
            "Delete converted row",
            vec![
                on(86, true),
                go_to_record(2),
                ScriptStep::new(9),
                exit_with("Get ( LastError )"),
            ],
        );
        let mut root = fixture::tree(&[delete], true);
        fixture::add_photo(&mut root);
        if global_owner {
            fixture::add_field(
                &mut root,
                TABLE,
                GLOBAL,
                "Global Photo",
                1,
                6,
                0,
                0x1_0000,
                0,
            );
            root.remove(&[&table, &[5], &[1], &photo]);
            root.put(
                &[&table, &[8], &global],
                om_scripts::text_node(&[], &reference),
            );
        }
        fixture::add_field(&mut root, TABLE, ENTERED, "Entered", 1, 1, 0, 0, 0);
        root.put(
            &[&table, &[5], &[2], &field],
            om_scripts::text_node(&[], &reference),
        );
        let file = TempFile::from_tree("recalc-delete-record", &root);
        let (mut designer, id) = server::logged_in(server::start(&file), "designer");
        fixture::add_field(&mut root, TABLE, ENTERED, "Entered", 2, 6, 0, 0x20, 0);
        fixture::add_formula(
            &mut root,
            &fixture::names(),
            (TABLE, OCCURRENCE),
            ENTERED,
            5,
            "Notes::Body",
        );
        let mut catalog = field_catalog(&root);
        defer(&mut catalog, ENTERED, 0x40 | FORMULA_CHANGED);
        save_field_catalog(&mut designer, id, &catalog);
        let records = records_of(&mut designer, TABLE);
        assert_eq!(cell(&records, 2, ENTERED), Some(reference.as_str()));

        let result = designer.perform_script_on_server(4, "", true).unwrap();
        assert_eq!((result.error, result.result.as_str()), (0, "0"));
        let saved = om_hbam::HbamFile::open(&file.0).unwrap();
        let tree = saved.key_tree().unwrap();
        assert!(tree.node(&[&table, &[5], &[2]]).is_none());
        let owner = if global_owner {
            tree.node(&[&table, &[8], &global])
        } else {
            tree.node(&[&table, &[5], &[1], &photo])
        };
        let value = owner
            .and_then(|n| om_hbam::Cell::from_node(n).text())
            .unwrap()
            .unwrap();
        assert_eq!(value, reference);
        let count = designer
            .download(&[&table, &[0x1f], &[5], &[1], &[0x12]])
            .unwrap();
        assert_eq!(count.node_state, om_protocol::NodeState::Terminal);
        assert_eq!(count.value, 1u32.to_be_bytes());
    }
}
