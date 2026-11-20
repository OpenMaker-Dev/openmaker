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
    assert_eq!(reply.unwrap(), schema::update_table_reply());
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
    root.put(&[&t, &[3]], catalog);
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
