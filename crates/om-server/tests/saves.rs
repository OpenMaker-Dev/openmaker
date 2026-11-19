//! End to end: what a commit stores (write-path.md 3.4, 5; hbam-writer.md
//! 6.2). OpenMaker Server writes every commit to the file before it
//! counts: when the file cannot be written, a server script's commit fails
//! with FileMaker's error (and Commit Transaction with it), a client's op70
//! changes nothing and fails with a system exception, and what the server
//! holds stays what the file holds.
//! Saves are made to fail by putting a directory where the server writes
//! its new copy of the file. A record whose modification count is not one
//! is not stored, and a lock reports such a count already in the file as
//! none.

#[path = "fixtures/tasks.rs"]
mod fixture;
#[path = "fixtures/server.rs"]
mod server;

use std::path::PathBuf;

use fixture::{exit_with, go_to_record, on, script, set_field, set_variable, TempFile};
use fixture::{COUNT, NAME, OCCURRENCE, TABLE};
use om_hbam::slot::uint_segment;
use om_hbam::{serial, HbamFile, OwnedNode};
use om_protocol::write;
use om_scripts::{FieldSpec, ScriptStep, StoredKey};
use server::{cell, records_of};

/// FileMaker's "Unable to create file on disk".
const CANNOT_WRITE: u16 = 800;

/// Saves of `file` fail while this lives: the server's new copy
/// (`<file>.omsave`) cannot be created where a directory stands.
struct BlockedSaves(PathBuf);

impl BlockedSaves {
    fn new(file: &TempFile) -> Self {
        let mut path = file.0.as_os_str().to_owned();
        path.push(".omsave");
        let path = PathBuf::from(path);
        std::fs::create_dir(&path).unwrap();
        BlockedSaves(path)
    }
}

impl Drop for BlockedSaves {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.0);
    }
}

/// Record `id`'s cell of `field` in the file on disk.
fn on_disk(file: &TempFile, id: u64, field: u64) -> Option<String> {
    let hbam = HbamFile::open(&file.0).unwrap();
    let (t, r, f) = (uint_segment(TABLE), serial::encode(id), uint_segment(field));
    let node = hbam.subtree(&[&t, &[5], &r]).unwrap()?;
    let cell = node.child(&f)?;
    om_hbam::Cell::from_node(cell).text()?.ok()
}

/// Tasks' last issued record id `[T, 00]` in the file on disk.
fn last_id_on_disk(file: &TempFile) -> u64 {
    let hbam = HbamFile::open(&file.0).unwrap();
    let table = hbam.subtree(&[&uint_segment(TABLE)]).unwrap().unwrap();
    table
        .child(&[0])
        .and_then(|node| node.value)
        .and_then(|value| value.split_first())
        .and_then(|(_, segment)| serial::decode(segment))
        .unwrap_or(0)
}

/// Set Next Serial Value [Tasks::Count; text].
fn set_next_serial(text: &str) -> ScriptStep {
    let mut step = fixture::with_calc(116, text);
    step.set_field_spec(
        0,
        Some(FieldSpec::new(
            StoredKey::from_id(OCCURRENCE).unwrap(),
            StoredKey::from_id(COUNT).unwrap(),
            0,
        )),
    );
    step
}

/// Tasks::Photo, a container field.
const PHOTO: u64 = 20;

/// The reference count of library member 1 of Tasks (containers.md 4): a
/// leaf, which a download returns as its value.
fn member_count(session: &mut om_session::Session) -> Option<u32> {
    let (t, m) = (uint_segment(TABLE), serial::encode(1));
    let path: [&[u8]; 5] = [&t, &[0x1f], &[5], &m, &[0x12]];
    let reply = session.download(&path).unwrap();
    (reply.node_state == om_protocol::NodeState::Terminal)
        .then(|| Some(u32::from_be_bytes(reply.value.as_slice().try_into().ok()?)))
        .flatten()
}

fn scripts() -> Vec<om_scripts::Script> {
    vec![
        fixture::server_work(),
        script(
            5,
            "Serial",
            vec![
                on(86, true),
                set_next_serial("40"),
                exit_with("Get ( LastError )"),
            ],
        ),
        script(
            6,
            "Duplicate",
            vec![
                on(86, true),
                go_to_record(1),
                ScriptStep::new(8),
                ScriptStep::new(75),
                set_variable("$commit", "Get ( LastError )"),
                ScriptStep::new(51),
                exit_with("$commit & \"|\" & Get ( FoundCount )"),
            ],
        ),
        script(
            7,
            "Serial at New Record",
            vec![
                on(86, true),
                ScriptStep::new(7),
                // The serial is the record's from New Record on.
                set_field(NAME, "\"copied:\" & Tasks::Count"),
                ScriptStep::new(75),
                exit_with("Get ( LastError ) & \"|\" & Tasks::Name & \"|\" & Tasks::Count"),
            ],
        ),
        script(
            4,
            "Saves",
            vec![
                on(86, true),
                go_to_record(1),
                set_field(NAME, "\"Unsaved\""),
                ScriptStep::new(75),
                set_variable("$commit", "Get ( LastError )"),
                // The record stayed open with the change.
                set_variable("$open", "Get ( RecordOpenState ) & Tasks::Name"),
                ScriptStep::new(51),
                ScriptStep::new(205),
                set_field(COUNT, "99"),
                ScriptStep::new(206),
                set_variable("$transaction", "Get ( LastError )"),
                exit_with(
                    "$commit & \"|\" & $open & \"|\" & $transaction & \"|\" & Tasks::Name & \
                     \"|\" & Tasks::Count",
                ),
            ],
        ),
    ]
}

#[test]
fn a_script_commit_that_cannot_be_saved_fails_and_changes_nothing() {
    let file = TempFile::new("save-script", &scripts(), true);
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "saves");
    let blocked = BlockedSaves::new(&file);
    let reply = client.perform_script_on_server(4, "", true).unwrap();
    // Commit Records and Commit Transaction fail with the save's error;
    // the record and the transaction are reverted, in memory too.
    assert_eq!(
        (reply.error, reply.result),
        (
            0,
            format!("{CANNOT_WRITE}|2Unsaved|{CANNOT_WRITE}|Write tests|3")
        )
    );
    let records = records_of(&mut client, TABLE);
    assert_eq!(cell(&records, 1, NAME), Some("Write tests"));
    assert_eq!(cell(&records, 1, COUNT), Some("3"));
    assert_eq!(cell(&records, 1, 0xfc), None);
    drop(blocked);
    assert_eq!(on_disk(&file, 1, NAME).as_deref(), Some("Write tests"));

    // Saves work again: the next commit is written, and only its own
    // change.
    assert_eq!(
        client.perform_script_on_server(1, "", true).unwrap().error,
        0
    );
    assert_eq!(on_disk(&file, 1, COUNT).as_deref(), Some("13"));
    assert_eq!(on_disk(&file, 1, NAME).as_deref(), Some("Write tests"));
    client.disconnect().unwrap();
}

/// Tasks with a container field Photo whose record 1 holds library member
/// 1 (count 1), and Count a serial number on creation.
fn with_photo() -> OwnedNode {
    let mut root = fixture::tree(&scripts(), true);
    fixture::set_field_options(&mut root, TABLE, COUNT, 0x2);
    fixture::set_field_serial_increment(&mut root, TABLE, COUNT, 1);
    let (t, f) = (uint_segment(TABLE), uint_segment(PHOTO));
    let mut definition = vec![0; 26];
    definition[0] = 1;
    definition[1] = 6;
    definition[24..26].copy_from_slice(&1u16.to_be_bytes());
    root.put(
        &[&t, &[3], &[5], &f, &[2]],
        OwnedNode::leaf(Vec::new(), definition),
    );
    root.put(
        &[&t, &[3], &[5], &f, &[16]],
        om_scripts::text_node(&[], "Photo"),
    );
    let member = serial::encode(1);
    let hex: String = member.iter().map(|b| format!("{b:02x}")).collect();
    root.put(
        &[&t, &[5], &serial::encode(1), &f],
        om_scripts::text_node(&[], &format!("{hex}\u{e000}")),
    );
    root.put(
        &[&t, &[0x1f], &[5], &member, &[0x12]],
        OwnedNode::leaf(Vec::new(), 1u32.to_be_bytes().to_vec()),
    );
    root
}

#[test]
fn a_duplicate_holds_its_containers_and_a_failed_save_takes_it_back() {
    let file = TempFile::from_tree("save-duplicate", &with_photo());
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "duplicate");
    let blocked = BlockedSaves::new(&file);
    // Set Next Serial Value and the duplicate's commit fail; the serial and
    // the member's count are as they were.
    let serial_value = |client: &mut om_session::Session| {
        let (t, f) = (uint_segment(TABLE), uint_segment(COUNT));
        let path: [&[u8]; 5] = [&t, &[3], &[5], &f, &[0x0c]];
        let reply = client.download(&path).unwrap();
        (reply.node_state == om_protocol::NodeState::Terminal)
            .then(|| om_scsu::decode_scsu(&reply.value).ok())
            .flatten()
    };
    let run = |client: &mut om_session::Session, script| {
        let reply = client.perform_script_on_server(script, "", true).unwrap();
        reply.result
    };
    assert_eq!(run(&mut client, 5), CANNOT_WRITE.to_string());
    assert_eq!(serial_value(&mut client), None);
    assert_eq!(run(&mut client, 6), format!("{CANNOT_WRITE}|2"));
    assert_eq!(member_count(&mut client), Some(1));
    assert_eq!(records_of(&mut client, TABLE).len(), 2);
    drop(blocked);

    // Saved: the copy holds the member too (op87 +1, containers.md 5.1).
    assert_eq!(run(&mut client, 5), "0");
    assert_eq!(serial_value(&mut client).as_deref(), Some("40"));
    assert_eq!(run(&mut client, 6), "0|3");
    assert_eq!(member_count(&mut client), Some(2));
    // (Record 3's id went to the copy the failed save took back.)
    let records = records_of(&mut client, TABLE);
    let ids: Vec<u64> = records.iter().map(|(id, _)| *id).collect();
    assert_eq!(ids, [1, 2, 4]);
    assert_eq!(cell(&records, 4, PHOTO), cell(&records, 1, PHOTO));
    // The copy takes the next serial number, not the original's.
    assert_eq!(cell(&records, 4, COUNT), Some("40"));
    assert_eq!(serial_value(&mut client).as_deref(), Some("41"));
    // A script reads a new record's serial before the commit.
    assert_eq!(run(&mut client, 7), "0|copied:41|41");
    assert_eq!(serial_value(&mut client).as_deref(), Some("42"));
    client.disconnect().unwrap();
}

#[test]
fn a_client_commit_that_cannot_be_saved_changes_nothing() {
    let file = TempFile::new("save-op70", &[], true);
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "op70");
    let table = uint_segment(TABLE);
    let text = |s: &str| om_scsu::encode_scsu(s);
    let record = |name: &str| {
        OwnedNode::list(
            serial::encode(1),
            vec![
                OwnedNode::leaf(uint_segment(NAME), text(name)),
                OwnedNode::leaf(uint_segment(COUNT), text("3")),
                OwnedNode::leaf(vec![0xfc], vec![0x01, 0x01]),
            ],
        )
    };
    let changed = || {
        OwnedNode::list(
            serial::encode(1),
            vec![OwnedNode::leaf(uint_segment(NAME), Vec::new())],
        )
    };
    let commit = |client: &mut om_session::Session, name: &str| {
        let path: [&[u8]; 3] = [&table, &[5], &serial::encode(1)];
        assert!(matches!(
            client.lock(&path, 0x31, true).unwrap(),
            write::LockReply::Granted { .. }
        ));
        client.upload_records(&table, vec![record(name)]).unwrap();
        let committed = client.commit_records(&table, vec![changed()], 0x31);
        client.kill_all_locks(0x31).unwrap();
        committed
    };

    // op70 fails with a system exception, never its done page (its reply
    // has no status, 3.4); the record keeps its values in memory and on
    // disk, and the client can still release its lock.
    let blocked = BlockedSaves::new(&file);
    let error = commit(&mut client, "Not saved").unwrap_err();
    assert!(error.to_string().contains("SystemException"), "{error}");
    let records = records_of(&mut client, TABLE);
    assert_eq!(cell(&records, 1, NAME), Some("Write tests"));
    assert_eq!(cell(&records, 1, 0xfc), None);
    drop(blocked);
    assert_eq!(on_disk(&file, 1, NAME).as_deref(), Some("Write tests"));

    // Once saves work, the same commit applies.
    commit(&mut client, "Saved").unwrap();
    let records = records_of(&mut client, TABLE);
    assert_eq!(cell(&records, 1, NAME), Some("Saved"));
    assert_eq!(on_disk(&file, 1, NAME).as_deref(), Some("Saved"));
    client.disconnect().unwrap();
}

/// A commit writes each table's record list and each changed index's
/// counts once, at its end; when its save fails they are put back with
/// the records: the record list `[T, 0d]` and the indexes `[T, 01]` are
/// what they were, byte for byte.
#[test]
fn a_failed_save_puts_back_record_lists_and_indexes() {
    let find = {
        let request = om_scripts::specs::StoredRequest {
            omit: false,
            criteria: vec![(
                om_scripts::specs::SpecField {
                    occurrence: Some(OCCURRENCE),
                    field: NAME,
                    repetition: 1,
                },
                "Ship".to_string(),
            )],
        };
        let mut step = ScriptStep::new(28);
        step.set_data_leaf(0x06, Some(om_scripts::specs::encode_requests(&[request])));
        step.set_flag(om_scripts::flag::RESTORE, true);
        step
    };
    let scripts = vec![
        script(
            1,
            "Find",
            vec![on(86, true), find, exit_with("Get ( FoundCount )")],
        ),
        script(
            2,
            "Many",
            vec![
                on(86, true),
                ScriptStep::new(205),
                ScriptStep::new(7),
                set_field(NAME, "\"New one\""),
                ScriptStep::new(7),
                set_field(NAME, "\"New two\""),
                go_to_record(1),
                ScriptStep::new(9),
                ScriptStep::new(206),
                exit_with("Get ( LastError )"),
            ],
        ),
    ];
    let mut root = fixture::tree(&scripts, true);
    // Name has a value index, which the find builds.
    fixture::set_field_options(&mut root, TABLE, NAME, 0x8000_0000);
    let file = TempFile::from_tree("save-indexes", &root);
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "indexes");
    assert_eq!(
        client.perform_script_on_server(1, "", true).unwrap().result,
        "1"
    );
    let t = uint_segment(TABLE);
    let snapshot = |client: &mut om_session::Session| {
        [&[0x0du8][..], &[0x01]].map(|key| {
            let path: [&[u8]; 2] = [&t, key];
            client.download(&path).unwrap().slots
        })
    };
    let before = snapshot(&mut client);
    assert!(!before[1].is_empty(), "the find stored Name's index");
    let blocked = BlockedSaves::new(&file);
    let reply = client.perform_script_on_server(2, "", true).unwrap();
    assert_eq!(reply.result, CANNOT_WRITE.to_string());
    assert_eq!(snapshot(&mut client), before);
    drop(blocked);
    let reply = client.perform_script_on_server(2, "", true).unwrap();
    assert_eq!(reply.result, "0");
    assert_ne!(snapshot(&mut client), before);
    let records = records_of(&mut client, TABLE);
    let names: Vec<&str> = records
        .iter()
        .filter_map(|(id, _)| cell(&records, *id, NAME))
        .collect();
    assert_eq!(names, ["Ship", "New one", "New two"]);
}

/// Tasks record 1's path, `[T, 05, 1]`.
fn record_path() -> [Vec<u8>; 3] {
    [uint_segment(TABLE), vec![5], serial::encode(1)]
}

/// Lock Tasks record 1 for `txn` without waiting.
fn lock(client: &mut om_session::Session, txn: u32, want_mod_count: bool) -> write::LockReply {
    let path = record_path();
    let path: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
    client.lock(&path, txn, want_mod_count).unwrap()
}

/// Edit Tasks record 1 as a client does (write-path.md 0): lock it asking
/// for its modification count, upload it with Name `name` and `count` as
/// its `fc`, commit the change of Name and release. Returns the count the
/// lock reported.
fn edit(client: &mut om_session::Session, txn: u32, name: &str, count: Vec<u8>) -> Vec<u8> {
    let write::LockReply::Granted {
        mod_count: Some(locked),
    } = lock(client, txn, true)
    else {
        panic!("the lock for {name:?} was refused");
    };
    let table = uint_segment(TABLE);
    let record = OwnedNode::list(
        serial::encode(1),
        vec![
            OwnedNode::leaf(uint_segment(NAME), om_scsu::encode_scsu(name)),
            OwnedNode::leaf(uint_segment(COUNT), om_scsu::encode_scsu("3")),
            OwnedNode::leaf(vec![0xfc], count),
        ],
    );
    let changed = OwnedNode::list(
        serial::encode(1),
        vec![OwnedNode::leaf(uint_segment(NAME), Vec::new())],
    );
    client.upload_records(&table, vec![record]).unwrap();
    client.commit_records(&table, vec![changed], txn).unwrap();
    client.kill_all_locks(txn).unwrap();
    locked
}

/// A modification count is a length byte and a serial key (write-path.md
/// 5.4). A record committed with anything else is not stored, so no lock
/// has to report it; the record stays as it was, and other sessions edit
/// it as before.
#[test]
fn a_commit_with_a_malformed_modification_count_is_refused() {
    let file = TempFile::new("mod-count-commit", &[], true);
    let port = server::start(&file);
    let (mut a, _) = server::logged_in(port, "a");
    let (mut b, _) = server::logged_in(port, "b");
    // 300 bytes, more than op60's reply could carry; then a length byte
    // that does not match its key.
    assert_eq!(edit(&mut a, 0x31, "Poisoned", vec![0x55; 300]), [1, 0]);
    assert_eq!(edit(&mut a, 0x32, "Mislabeled", vec![2, 1]), [1, 0]);
    let records = records_of(&mut a, TABLE);
    assert_eq!(cell(&records, 1, NAME), Some("Write tests"));
    assert_eq!(cell(&records, 1, 0xfc), None);

    assert_eq!(edit(&mut b, 0x41, "Edited", vec![1, 1]), [1, 0]);
    assert_eq!(edit(&mut a, 0x33, "Edited again", vec![1, 2]), [1, 1]);
    let records = records_of(&mut b, TABLE);
    assert_eq!(cell(&records, 1, NAME), Some("Edited again"));
    assert_eq!(cell(&records, 1, 0xfc), Some("2"));
    assert_eq!(on_disk(&file, 1, NAME).as_deref(), Some("Edited again"));
}

/// A file that already holds a malformed modification count (an earlier
/// server's commit, another writer): a lock asking for the count is
/// granted with none (`01 00`) and held by a session that is told so,
/// which releases it as usual. The next edit stores a count again.
#[test]
fn a_stored_malformed_modification_count_is_reported_as_none() {
    let mut root = fixture::tree(&[], true);
    let [t, records, r] = record_path();
    root.put(
        &[&t, &records, &r, &[0xfc]],
        OwnedNode::leaf(Vec::new(), vec![0x55; 300]),
    );
    let file = TempFile::from_tree("mod-count-stored", &root);
    let port = server::start(&file);
    let (mut a, _) = server::logged_in(port, "a");
    let (mut b, _) = server::logged_in(port, "b");
    assert_eq!(
        lock(&mut a, 0x31, true),
        write::LockReply::Granted {
            mod_count: Some(vec![1, 0])
        }
    );
    match lock(&mut b, 0x41, false) {
        write::LockReply::Refused { status, conflict } => {
            assert_eq!(status, write::LOCK_CONFLICT);
            assert!(!conflict.self_conflict);
            assert_eq!(conflict.users, "a (Admin)");
        }
        other => panic!("b was not refused: {other:?}"),
    }
    a.kill_all_locks(0x31).unwrap();
    assert_eq!(
        lock(&mut b, 0x41, true),
        write::LockReply::Granted {
            mod_count: Some(vec![1, 0])
        }
    );
    b.kill_all_locks(0x41).unwrap();

    assert_eq!(edit(&mut a, 0x32, "Repaired", vec![1, 1]), [1, 0]);
    let records = records_of(&mut b, TABLE);
    assert_eq!(cell(&records, 1, NAME), Some("Repaired"));
    assert_eq!(cell(&records, 1, 0xfc), Some("1"));
}

/// A Full Access account's catalog upload (op13) that the file could not
/// hold is refused before it reaches the file's contents: a key segment
/// over 219 bytes, a path deeper than 16 segments (hbam-writer.md 2.2, 2.3),
/// or a child beneath a committed value, which would make a list of the
/// value (2.2). The committed value stays byte for byte, in memory and on
/// disk, and later commits are saved (issue #303).
#[test]
fn an_upload_the_file_cannot_hold_leaves_the_file_saveable() {
    use om_protocol::write::Upload;
    use om_protocol::Resume;
    let commit = script(
        1,
        "Commit",
        vec![
            on(86, true),
            go_to_record(1),
            set_field(NAME, "\"Saved after uploads\""),
            ScriptStep::new(75),
            exit_with("Get ( LastError )"),
        ],
    );
    // A committed value of a key no catalog reads: opaque to the server.
    let committed: Vec<u8> = (0..40u8).collect();
    let mut root = fixture::tree(&[commit], true);
    root.put(&[&[0x41]], OwnedNode::leaf(Vec::new(), committed.clone()));
    let file = TempFile::from_tree("save-unwritable-upload", &root);
    let port = server::start(&file);
    let (mut client, file_id) = server::logged_in(port, "uploads");
    let mut upload = |dest: Vec<Vec<u8>>, child: OwnedNode| {
        let upload = Upload {
            payload: OwnedNode::list(Vec::new(), vec![child]).to_wire().unwrap(),
            resume: Resume::start(),
            terminal: Vec::new(),
            flags: write::UPLOAD_NODE,
            dest,
        };
        client
            .perform(&write::build_upload_to_host(7, file_id, &upload))
            .unwrap();
    };
    upload(vec![vec![0x40]], OwnedNode::leaf(vec![7; 230], vec![1]));
    upload(vec![vec![0x40]; 16], OwnedNode::leaf(vec![7], vec![1]));
    upload(
        vec![vec![0x41], vec![0x01]],
        OwnedNode::leaf(vec![7], vec![1]),
    );
    let value = |client: &mut om_session::Session| {
        let reply = client.download(&[&[0x41]]).unwrap();
        (reply.node_state == om_protocol::NodeState::Terminal).then_some(reply.value)
    };
    assert_eq!(value(&mut client), Some(committed.clone()));

    let reply = client.perform_script_on_server(1, "", true).unwrap();
    assert_eq!(reply.result, "0");
    assert_eq!(
        on_disk(&file, 1, NAME).as_deref(),
        Some("Saved after uploads")
    );
    let hbam = HbamFile::open(&file.0).unwrap();
    assert!(hbam.subtree(&[&[0x40]]).unwrap().is_none());
    let on_disk = hbam
        .subtree(&[&[0x41]])
        .unwrap()
        .expect("the committed value");
    assert_eq!(
        (on_disk.value, on_disk.children.len()),
        (Some(&committed[..]), 0)
    );
    assert_eq!(value(&mut client), Some(committed));
    client.disconnect().unwrap();
}

/// A table whose last issued record id `[T, 00]` is near the largest a
/// serial key holds (the file says so, or a catalog upload made it so):
/// op71 hands out the ids that remain and then refuses as it refuses a
/// count it cannot reserve, rather than wrapping around to ids records
/// have. A new record is never committed over an existing one.
#[test]
fn record_ids_near_the_largest_do_not_wrap_around() {
    let mut root = fixture::tree(&[], true);
    let last = serial::encode(u64::MAX - 1);
    let mut value = vec![last.len() as u8];
    value.extend(last);
    root.put(
        &[&uint_segment(TABLE), &[0]],
        OwnedNode::leaf(Vec::new(), value),
    );
    let file = TempFile::from_tree("record-id-overflow", &root);
    let port = server::start(&file);
    let (mut client, file_id) = server::logged_in(port, "ids");
    let table = uint_segment(TABLE);
    let create = |client: &mut om_session::Session, count: u32| {
        let request = write::CreateRecord {
            table: vec![table.clone()],
            count,
            txn: 0x51,
        };
        let result = client
            .perform(&write::build_create_record(1, file_id, &request))
            .unwrap();
        write::parse_create_record_reply(&result).unwrap()
    };
    let refused = create(&mut client, 2);
    assert_eq!((refused.status, refused.record), (3, vec![vec![0]]));
    let granted = create(&mut client, 1);
    assert_eq!(granted.status, 0);
    assert_eq!(serial::decode(&granted.record[0]), Some(u64::MAX));
    assert_eq!(create(&mut client, 1).status, 3);

    // A record committed as new: over record 1 it is refused and record 1
    // keeps its values; with the id op71 handed out it is stored.
    let table = uint_segment(TABLE);
    let commit_new = |client: &mut om_session::Session, id: u64, name: &str| {
        let record = OwnedNode::list(
            serial::encode(id),
            vec![OwnedNode::leaf(
                uint_segment(NAME),
                om_scsu::encode_scsu(name),
            )],
        );
        let entry = OwnedNode::leaf(serial::encode(id), vec![write::COMMIT_NEW]);
        client.upload_records(&table, vec![record]).unwrap();
        client.commit_records(&table, vec![entry], 0x51).unwrap();
    };
    commit_new(&mut client, 1, "Replaced");
    commit_new(&mut client, u64::MAX, "Last");
    client.kill_all_locks(0x51).unwrap();
    let records = records_of(&mut client, TABLE);
    assert_eq!(cell(&records, 1, NAME), Some("Write tests"));
    assert_eq!(cell(&records, u64::MAX, NAME), Some("Last"));
    assert_eq!(on_disk(&file, 1, NAME).as_deref(), Some("Write tests"));
    assert_eq!(on_disk(&file, u64::MAX, NAME).as_deref(), Some("Last"));
}

/// A record whose modification count is the largest a serial key holds
/// (a client's op70 may store any count, write-path.md 5.4): a server
/// script's edit cannot count it, so its commit is refused with error 3 and
/// writes nothing, rather than wrapping the count around or leaving it as it
/// was, which would hide the change from clients holding a copy (#361). The
/// client that runs the script keeps its connection.
#[test]
fn a_script_edit_of_the_largest_count_is_refused() {
    let edit_script = script(
        7,
        "Edit",
        vec![
            on(86, true),
            go_to_record(1),
            set_field(NAME, "\"Edited by a script\""),
            ScriptStep::new(75),
            exit_with("Get ( LastError )"),
        ],
    );
    let file = TempFile::new("largest-count", &[edit_script], true);
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "client");
    let mut largest = vec![9];
    largest.extend(serial::encode(u64::MAX));
    edit(&mut client, 0x31, "At the largest count", largest);
    let stored = |client: &mut om_session::Session| {
        let records = records_of(client, TABLE);
        [NAME, 0xfc].map(|f| cell(&records, 1, f).map(str::to_string))
    };
    let before = [
        Some("At the largest count".to_string()),
        Some(u64::MAX.to_string()),
    ];
    assert_eq!(stored(&mut client), before);

    let reply = client.perform_script_on_server(7, "", true).unwrap();
    assert_eq!((reply.error, reply.result.as_str()), (3, "3"));
    assert_eq!(stored(&mut client), before);
    assert_eq!(
        on_disk(&file, 1, NAME).as_deref(),
        Some("At the largest count")
    );
}

/// A new record's id op71 issues is on disk before op71 answers (#927).
/// FileMaker Pro keeps a new record and its id through a server restart and
/// commits it later, and op70's reply cannot report a record it leaves out
/// (write-path.md 3.4), so the id must be one the file counts as issued
/// whatever the server held only in memory. Here the restart is a second
/// server hosting a copy of the file as the disk holds it right after
/// op71, as after a crash: it takes the new record's commit. While the
/// file cannot be saved, op71 is refused with the save's error and issues
/// nothing: the next reservation gets the same id.
#[test]
fn an_issued_record_id_outlasts_a_restart_and_one_that_cannot_be_saved_is_refused() {
    let file = TempFile::new("save-op71", &[], true);
    let port = server::start(&file);
    let (mut client, file_id) = server::logged_in(port, "op71");
    let table = uint_segment(TABLE);
    let create = |client: &mut om_session::Session| {
        let request = write::CreateRecord {
            table: vec![table.clone()],
            count: 1,
            txn: 0x61,
        };
        let result = client
            .perform(&write::build_create_record(1, file_id, &request))
            .unwrap();
        write::parse_create_record_reply(&result).unwrap()
    };
    let before = last_id_on_disk(&file);

    let blocked = BlockedSaves::new(&file);
    let refused = create(&mut client);
    assert_eq!(
        (refused.status, refused.record),
        (u32::from(CANNOT_WRITE), vec![vec![0]])
    );
    drop(blocked);
    assert_eq!(last_id_on_disk(&file), before);

    let granted = create(&mut client);
    assert_eq!(granted.status, 0);
    let id = serial::decode(&granted.record[0]).unwrap();
    assert_eq!(id, before + 1, "the refused reservation issued nothing");
    assert_eq!(last_id_on_disk(&file), id, "on disk before the reply");

    // The server restarts from what the disk holds; the client opens the
    // file again and commits the record it kept, with its id.
    let restarted = TempFile(std::env::temp_dir().join(format!(
        "om-psos-save-op71-restarted-{}-{:?}.fmp12",
        std::process::id(),
        std::thread::current().id()
    )));
    std::fs::copy(&file.0, &restarted.0).unwrap();
    let port = server::start(&restarted);
    let (mut again, _) = server::logged_in(port, "op71-again");
    let path: [&[u8]; 3] = [&table, &[5], &serial::encode(id)];
    assert!(matches!(
        again.lock(&path, 0x62, false).unwrap(),
        write::LockReply::Granted { .. }
    ));
    let record = OwnedNode::list(
        serial::encode(id),
        vec![OwnedNode::leaf(
            uint_segment(NAME),
            om_scsu::encode_scsu("Typed before the restart"),
        )],
    );
    let entry = OwnedNode::leaf(serial::encode(id), vec![write::COMMIT_NEW]);
    again.upload_records(&table, vec![record]).unwrap();
    again.commit_records(&table, vec![entry], 0x62).unwrap();
    again.kill_all_locks(0x62).unwrap();
    let records = records_of(&mut again, TABLE);
    assert_eq!(cell(&records, id, NAME), Some("Typed before the restart"));
    assert_eq!(
        on_disk(&restarted, id, NAME).as_deref(),
        Some("Typed before the restart")
    );
    again.disconnect().unwrap();
    client.disconnect().unwrap();
}
