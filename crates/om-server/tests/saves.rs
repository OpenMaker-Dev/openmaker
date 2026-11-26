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
                set_variable("$duplicate", "Get ( LastError )"),
                ScriptStep::new(75),
                set_variable("$commit", "Get ( LastError )"),
                ScriptStep::new(51),
                exit_with("$duplicate & \"|\" & $commit & \"|\" & Get ( FoundCount )"),
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
            8,
            "Reverted",
            vec![
                on(86, true),
                ScriptStep::new(7),
                ScriptStep::new(51),
                exit_with("Get ( LastError )"),
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
    // Set Next Serial Value fails, and so does Duplicate Record, whose
    // serial number cannot be saved (docs/server-storage.md 5), so there is
    // no commit; the serial and the member's count are as they were.
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
    assert_eq!(run(&mut client, 6), format!("{CANNOT_WRITE}|0|2"));
    assert_eq!(member_count(&mut client), Some(1));
    assert_eq!(records_of(&mut client, TABLE).len(), 2);
    drop(blocked);

    // Saved: the copy holds the member too (op87 +1, containers.md 5.1).
    assert_eq!(run(&mut client, 5), "0");
    assert_eq!(serial_value(&mut client).as_deref(), Some("40"));
    assert_eq!(run(&mut client, 6), "0|0|3");
    assert_eq!(member_count(&mut client), Some(2));
    // (Record 3's id went to the copy whose serial could not be saved.)
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
    let path: [&[u8]; 3] = [&table, &[5], &serial::encode(1)];
    let lock = |client: &mut om_session::Session, txn: u32| {
        assert!(matches!(
            client.lock(&path, txn, true).unwrap(),
            write::LockReply::Granted { .. }
        ));
    };

    // op70 fails with a system exception, never its done page (its reply
    // has no status, 3.4); the record keeps its values in memory and on
    // disk, and the client can still release its lock.
    let blocked = BlockedSaves::new(&file);
    lock(&mut client, 0x31);
    client
        .upload_records(&table, vec![record("Not saved")])
        .unwrap();
    let error = client
        .commit_records(&table, vec![changed()], 0x31)
        .unwrap_err();
    assert!(error.to_string().contains("SystemException"), "{error}");
    let records = records_of(&mut client, TABLE);
    assert_eq!(cell(&records, 1, NAME), Some("Write tests"));
    assert_eq!(cell(&records, 1, 0xfc), None);
    drop(blocked);
    assert_eq!(on_disk(&file, 1, NAME).as_deref(), Some("Write tests"));

    // The failed save took the staged record. The same op70 sent again
    // without its upload is refused, not answered done with nothing stored
    // (#839 of github.com/OpenMaker-Dev/openmaker).
    let error = client
        .commit_records(&table, vec![changed()], 0x31)
        .unwrap_err();
    assert!(error.to_string().contains("SystemException"), "{error}");
    assert_eq!(on_disk(&file, 1, NAME).as_deref(), Some("Write tests"));

    // Uploaded again, the same commit applies.
    client
        .upload_records(&table, vec![record("Saved")])
        .unwrap();
    client
        .commit_records(&table, vec![changed()], 0x31)
        .unwrap();
    client.kill_all_locks(0x31).unwrap();
    let records = records_of(&mut client, TABLE);
    assert_eq!(cell(&records, 1, NAME), Some("Saved"));
    assert_eq!(on_disk(&file, 1, NAME).as_deref(), Some("Saved"));
    client.disconnect().unwrap();
}

/// A commit naming a record with nothing staged is refused only when a
/// failed commit of its transaction took the record's upload (#839 of
/// github.com/OpenMaker-Dev/openmaker); any other is answered done, with
/// nothing stored, as before: a record never uploaded, one whose lock the
/// client released since (op62, op65), and one another transaction's
/// failed commit took.
#[test]
fn only_a_commit_whose_upload_a_failed_save_took_is_refused_without_it() {
    let file = TempFile::new("save-op70-lost", &[], true);
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "op70-lost");
    let table = uint_segment(TABLE);
    let path: [&[u8]; 3] = [&table, &[5], &serial::encode(1)];
    let changed = || {
        OwnedNode::list(
            serial::encode(1),
            vec![OwnedNode::leaf(uint_segment(NAME), Vec::new())],
        )
    };
    // Record 1 locked for `txn`, uploaded and committed while saves fail.
    let fail = |client: &mut om_session::Session, txn: u32| {
        let blocked = BlockedSaves::new(&file);
        assert!(matches!(
            client.lock(&path, txn, true).unwrap(),
            write::LockReply::Granted { .. }
        ));
        let record = OwnedNode::list(
            serial::encode(1),
            vec![
                OwnedNode::leaf(uint_segment(NAME), om_scsu::encode_scsu("Lost")),
                OwnedNode::leaf(vec![0xfc], vec![0x01, 0x01]),
            ],
        );
        client.upload_records(&table, vec![record]).unwrap();
        let error = client
            .commit_records(&table, vec![changed()], txn)
            .unwrap_err();
        assert!(error.to_string().contains("SystemException"), "{error}");
        drop(blocked);
    };
    let unchanged = |file: &TempFile| {
        assert_eq!(on_disk(file, 1, NAME).as_deref(), Some("Write tests"));
    };

    // Never uploaded: done, nothing stored.
    assert!(matches!(
        client.lock(&path, 0x41, true).unwrap(),
        write::LockReply::Granted { .. }
    ));
    client
        .commit_records(&table, vec![changed()], 0x41)
        .unwrap();
    client.kill_all_locks(0x41).unwrap();
    unchanged(&file);

    // After the failed save, its lock released with op62: done.
    fail(&mut client, 0x42);
    client.unlock(&path, 0x42, write::LOCK_WRITE, 0).unwrap();
    client
        .commit_records(&table, vec![changed()], 0x42)
        .unwrap();
    unchanged(&file);

    // ... or its transaction's locks with op65: done.
    fail(&mut client, 0x43);
    client.kill_all_locks(0x43).unwrap();
    client
        .commit_records(&table, vec![changed()], 0x43)
        .unwrap();
    unchanged(&file);

    // Another transaction's commit of the record: done; the failed one's
    // own, sent again, refused.
    fail(&mut client, 0x44);
    client
        .commit_records(&table, vec![changed()], 0x45)
        .unwrap();
    let error = client
        .commit_records(&table, vec![changed()], 0x44)
        .unwrap_err();
    assert!(error.to_string().contains("SystemException"), "{error}");
    client.kill_all_locks(0x44).unwrap();
    unchanged(&file);
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
/// lock reported; the commit must be answered done.
fn edit(client: &mut om_session::Session, txn: u32, name: &str, count: Vec<u8>) -> Vec<u8> {
    let (locked, committed) = edit_answered(client, txn, name, count);
    committed.unwrap();
    locked
}

/// [`edit`], with the commit's answer.
fn edit_answered(
    client: &mut om_session::Session,
    txn: u32,
    name: &str,
    count: Vec<u8>,
) -> (Vec<u8>, Result<(), om_session::SessionError>) {
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
    let committed = client.commit_records(&table, vec![changed], txn);
    client.kill_all_locks(txn).unwrap();
    (locked, committed)
}

/// A modification count is a length byte and a serial key (write-path.md
/// 5.4). A record committed with anything else is not stored, so no lock
/// has to report it: the commit fails, the record stays as it was, and
/// other sessions edit it as before.
#[test]
fn a_commit_with_a_malformed_modification_count_is_refused() {
    let file = TempFile::new("mod-count-commit", &[], true);
    let port = server::start(&file);
    let (mut a, _) = server::logged_in(port, "a");
    let (mut b, _) = server::logged_in(port, "b");
    // 300 bytes, more than op60's reply could carry; then a length byte
    // that does not match its key.
    for (txn, name, count) in [
        (0x31, "Poisoned", vec![0x55; 300]),
        (0x32, "Mislabeled", vec![2, 1]),
    ] {
        let (locked, committed) = edit_answered(&mut a, txn, name, count);
        assert_eq!(locked, [1, 0]);
        assert!(committed.is_err(), "{name}");
    }
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
/// value (2.2). Each fails as an upload whose save failed does, rather than
/// answer done (#737's rule). The committed value stays byte for byte, in
/// memory and on disk, and later commits are saved (issue #303).
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

    // An upload the file can hold that leaves a table's name undecodable
    // (masked SCSU that ends inside a quote), before anything of this load
    // is read: the store keeps the catalog it last decoded, and the first
    // read, which a test server answers through both views and compares
    // (`Server::checking_reads`), answers as the file engine does.
    assert!(om_scsu::decode_scsu(&[0x5b]).is_err());
    let (mut first, _) = server::logged_in_unread(port, "first");
    let table = uint_segment(TABLE);
    let entry: [&[u8]; 4] = [&[3], &[16], &[5], &table];
    let named = |name: OwnedNode| OwnedNode::list(Vec::new(), vec![name]);
    first
        .upload_to_host(&entry, &named(OwnedNode::leaf(vec![16], vec![0x5b])))
        .unwrap();
    let reply = first.download(&[&[0x41]]).unwrap();
    assert_eq!(reply.value, committed);
    first
        .upload_to_host(&entry, &named(om_scripts::text_node(&[16], "Tasks")))
        .unwrap();
    first.disconnect().unwrap();

    let (mut client, file_id) = server::logged_in(port, "uploads");
    let mut upload = |dest: Vec<Vec<u8>>, child: OwnedNode| {
        let upload = Upload {
            payload: OwnedNode::list(Vec::new(), vec![child]).to_wire().unwrap(),
            resume: Resume::start(),
            terminal: Vec::new(),
            flags: write::UPLOAD_NODE,
            dest,
        };
        let answer = client.perform(&write::build_upload_to_host(7, file_id, &upload));
        assert!(answer.is_err(), "{answer:02x?}");
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

    // A record committed as new: over record 1 it is refused (the commit
    // fails) and record 1 keeps its values; with the id op71 handed out it
    // is stored.
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
        client.commit_records(&table, vec![entry], 0x51)
    };
    assert!(commit_new(&mut client, 1, "Replaced").is_err());
    commit_new(&mut client, u64::MAX, "Last").unwrap();
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

/// Tasks' next serial value of Count (`[T, 03, 05, F, 0c]`) as a client
/// downloads it.
fn serial_of(session: &mut om_session::Session) -> Option<String> {
    let (t, f) = (uint_segment(TABLE), uint_segment(COUNT));
    let path: [&[u8]; 5] = [&t, &[3], &[5], &f, &[0x0c]];
    let reply = session.download(&path).unwrap();
    (reply.node_state == om_protocol::NodeState::Terminal)
        .then(|| om_scsu::decode_scsu(&reply.value).ok())
        .flatten()
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

/// An op86 program of one instruction, `opcode`, with `table` as its
/// source, signed for `session` (query-program.md 2, 11.2): what Manage
/// Database sends to delete a table.
fn schema_program(
    session: &om_session::Session,
    file_id: u32,
    opcode: u8,
    table: &[u8],
) -> Vec<u8> {
    use om_query::program::leaf;
    let mut program = OwnedNode::list(Vec::new(), Vec::new());
    for (key, value) in [
        (leaf::OPCODE, vec![opcode]),
        (
            leaf::SRC,
            [vec![table.len() as u8], table.to_vec()].concat(),
        ),
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

/// op72 that sets a field's next serial value to the one it holds changes
/// nothing: with saves failing, it answers status 0 with the value, as a
/// request with nothing to store does, and the file keeps it (review of
/// #785, round 1: a write that leaves the tree as it was is no change,
/// `Store::write_in`).
#[test]
fn a_serial_set_to_the_one_held_answers_with_it_while_saves_fail() {
    let mut root = with_photo();
    let (t, f) = (uint_segment(TABLE), uint_segment(COUNT));
    root.put(
        &[&t, &[3], &[5], &f, &[0x0c]],
        OwnedNode::leaf(Vec::new(), om_scsu::encode_scsu("7")),
    );
    let file = TempFile::from_tree("save-same-serial", &root);
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "same serial");
    let blocked = BlockedSaves::new(&file);
    let serial = client
        .get_serial(&t, &f, "7", 0, write::SERIAL_REPLACE)
        .unwrap();
    assert_eq!((serial.status, serial.value.as_str()), (0, "7"));
    drop(blocked);
    assert_eq!(serial_of(&mut client).as_deref(), Some("7"));
    client.disconnect().unwrap();
}

/// op13 that uploads a top-level catalog as the file holds it, as the
/// first write to a freshly hosted file, changes nothing: with saves
/// failing it succeeds, and the file is not rewritten (review of #785,
/// round 2: the root of a tree loaded from disk is not flagged a list, and
/// that flag is no content, `Store::write_in`).
#[test]
fn an_upload_of_a_catalog_as_it_is_changes_nothing_while_saves_fail() {
    let mut root = with_photo();
    fixture::add_themes(&mut root, &[1]);
    let file = TempFile::from_tree("save-same-catalog", &root);
    let bytes = std::fs::read(&file.0).unwrap();
    let themes = HbamFile::open(&file.0)
        .unwrap()
        .subtree(&[&[6]])
        .unwrap()
        .unwrap()
        .to_owned_node();
    assert!(!themes.children.is_empty());
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "same catalog");
    let blocked = BlockedSaves::new(&file);
    client.upload_to_host(&[&[6]], &themes).unwrap();
    drop(blocked);
    assert!(
        std::fs::read(&file.0).unwrap() == bytes,
        "the file is rewritten"
    );
    client.disconnect().unwrap();
}

/// docs/server-storage.md 5, "not stored": a request whose save fails
/// before its rename is answered with its storage error, as each reply can
/// carry it (op72 and op88 in their status, 800 "Unable to create file on
/// disk"; op86 in its error slot; op87 with its failure count; op13 and
/// op73, whose replies have no status, with a system exception, as op70),
/// and nothing of it is kept: another client reads the file as it was, the
/// file on disk is unchanged, and no notice arrives. Once saves work, each
/// request is done.
#[test]
fn each_request_answers_a_save_that_failed_with_its_storage_error_and_keeps_nothing() {
    let file = TempFile::from_tree("save-requests", &with_photo());
    let port = server::start(&file);
    let (mut client, file_id) = server::logged_in(port, "requests");
    let (mut other, _) = server::logged_in(port, "other");
    let table = uint_segment(TABLE);
    let deferred: [&[u8]; 4] = [&table, &[3], &[1], &[0xfd]];
    let work = OwnedNode::list(
        vec![0xfd],
        vec![OwnedNode::leaf(
            uint_segment(NAME),
            0x10u32.to_be_bytes().to_vec(),
        )],
    );
    let photo = vec![table.clone(), vec![0x1f], vec![5], serial::encode(1)];
    let notes = uint_segment(fixture::NOTES);
    let state = |session: &mut om_session::Session| {
        (
            serial_of(session),
            session.download(&deferred).unwrap().node_state,
            member_count(session),
            session.download(&[&notes]).unwrap().node_state,
        )
    };
    let before = state(&mut other);
    let bytes = std::fs::read(&file.0).unwrap();

    let blocked = BlockedSaves::new(&file);
    let serial = client
        .get_serial(&table, &uint_segment(COUNT), "", 1, write::SERIAL_INCREMENT)
        .unwrap();
    assert_eq!(
        (serial.status, serial.value.as_str()),
        (u32::from(CANNOT_WRITE), "")
    );
    assert!(client.upload_to_host(&deferred, &work).is_err(), "op13");
    let stored = client
        .upload_binary(
            &table,
            &uint_segment(fixture::PHOTO),
            binary_payload(b"not kept"),
        )
        .unwrap();
    assert_eq!(
        (stored.status, stored.member.len()),
        (CANNOT_WRITE, 0),
        "{stored:?}"
    );
    let count = client.adjust_reference_count(photo.clone(), 1).unwrap();
    assert_eq!(count, om_protocol::container::COUNT_FAILED);
    let program = schema_program(
        &client,
        file_id,
        om_query::program::opcode::DELETE_TABLE,
        &notes,
    );
    let reply = client.query(&program).unwrap();
    assert_eq!(
        reply.slots,
        [0x80, 0x06, 0x00, 0x03, 0x02, 0x03, 0x20, 0x40]
    );
    assert_eq!(state(&mut other), before);
    assert_eq!(std::fs::read(&file.0).unwrap(), bytes, "the file on disk");
    assert!(other.take_notifications().is_empty());
    drop(blocked);

    // Saves work: each is done.
    let serial = client
        .get_serial(&table, &uint_segment(COUNT), "", 1, write::SERIAL_INCREMENT)
        .unwrap();
    assert_eq!((serial.status, serial.value.as_str()), (0, "1"));
    client.upload_to_host(&deferred, &work).unwrap();
    // op73 does the work only now; it was not stored before.
    let blocked = BlockedSaves::new(&file);
    let bytes = std::fs::read(&file.0).unwrap();
    assert!(client.update_table(&table).is_err(), "op73");
    assert_eq!(
        other.download(&deferred).unwrap().node_state,
        om_protocol::NodeState::NonTerminal,
        "the work waits"
    );
    assert_eq!(std::fs::read(&file.0).unwrap(), bytes);
    drop(blocked);
    client.update_table(&table).unwrap();
    assert_eq!(
        other.download(&deferred).unwrap().node_state,
        om_protocol::NodeState::Missing
    );
    let stored = client
        .upload_binary(
            &table,
            &uint_segment(fixture::PHOTO),
            binary_payload(b"kept"),
        )
        .unwrap();
    assert_eq!(stored.status, 0);
    assert_eq!(client.adjust_reference_count(photo, 1).unwrap(), 2);
    let reply = client.query(&program).unwrap();
    assert!(
        !reply.slots.starts_with(&[0x80, 0x06, 0x00]),
        "{:02x?}",
        reply.slots
    );
    assert_eq!(
        other.download(&[&notes]).unwrap().node_state,
        om_protocol::NodeState::Missing,
        "Notes is deleted"
    );
    let notifications = other.take_notifications();
    assert!(notifications.is_empty(), "{notifications:?}");
    client.disconnect().unwrap();
    other.disconnect().unwrap();
}

/// The status of hosted file `id`, waited for until it is `want`.
fn wait_for(handle: &om_server::ServerHandle, id: u32, want: om_server::FileStatus) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if handle
            .files()
            .iter()
            .any(|f| f.id == id && f.status == want)
        {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("file {id} not {want:?}");
}

/// A server script that sets a field and ends with its record open has the
/// record committed as the session ends; when that commit is not stored
/// (here its save fails), the edit is lost, and the script's result is the
/// commit's error, never 0.
#[test]
fn a_script_whose_last_record_is_not_stored_reports_the_error() {
    let left_open = script(
        4,
        "Left open",
        vec![go_to_record(1), set_field(COUNT, "99")],
    );
    let file = TempFile::new("save-left-open", &[left_open], true);
    let (port, handle) = server::start_with_handle(&file, om_server::ScriptLimits::DEFAULT);
    let id = handle.files()[0].id;
    let (mut client, _) = server::logged_in(port, "left open");
    assert!(handle.failing_saves(id, Some(om_server::SaveFault::BeforeRename)));
    let reply = client.perform_script_on_server(4, "", true).unwrap();
    assert!(handle.failing_saves(id, None));
    assert_eq!(reply.error, 800, "{reply:?}");
    let records = records_of(&mut client, TABLE);
    assert_eq!(cell(&records, 1, COUNT), Some("3"));
}

/// docs/server-storage.md 5, "unknown": a save that renamed its new copy
/// over the file and could not flush the folder is answered with the
/// storage error; the file keeps the change, as the renamed file holds it,
/// and takes no more: the next write is refused with an error, while reads
/// and locks go on. One alert says so, and the administrator's list of
/// files says why it takes no more changes. Closed and opened again, it
/// takes changes again.
#[test]
fn a_save_whose_outcome_is_unknown_leaves_the_file_taking_no_more_changes_until_hosted_again() {
    let file = TempFile::from_tree("save-unknown", &with_photo());
    let (port, handle) = server::start_with_handle(&file, om_server::ScriptLimits::DEFAULT);
    let id = handle.files()[0].id;
    let (mut client, _) = server::logged_in(port, "unknown");
    let table = uint_segment(TABLE);
    let count = uint_segment(COUNT);
    let alerts = handle.alerts().len();
    assert!(handle.failing_saves(id, Some(om_server::SaveFault::FolderFlush)));
    let serial = client
        .get_serial(&table, &count, "", 1, write::SERIAL_INCREMENT)
        .unwrap();
    assert!(handle.failing_saves(id, None));
    assert_eq!(
        (serial.status, serial.value.as_str()),
        (u32::from(CANNOT_WRITE), "")
    );
    // The file keeps the change, which the renamed file holds.
    assert_eq!(serial_of(&mut client).as_deref(), Some("2"));
    let hbam = HbamFile::open(&file.0).unwrap();
    let on_disk = hbam
        .subtree(&[&table, &[3], &[5], &count, &[0x0c]])
        .unwrap();
    assert!(on_disk.is_some());
    let [info] = handle.files().try_into().unwrap();
    assert_eq!(info.status, om_server::FileStatus::Open);
    let why = info.error.expect("why it takes no more changes");
    assert!(why.contains("takes no more changes"), "{why}");
    let raised = handle.alerts();
    assert_eq!(raised.len(), alerts + 1, "{raised:?}");
    assert!(
        raised[0].message.contains("takes no more changes"),
        "{}",
        raised[0].message
    );

    // The next write is refused; reads and locks go on.
    let bytes = std::fs::read(&file.0).unwrap();
    let serial = client
        .get_serial(&table, &count, "", 1, write::SERIAL_INCREMENT)
        .unwrap();
    assert_eq!(serial.status, u32::from(CANNOT_WRITE));
    let request = write::CreateRecord {
        table: vec![table.clone()],
        count: 1,
        txn: 0x71,
    };
    let reply = client
        .perform(&write::build_create_record(1, 1, &request))
        .unwrap();
    let created = write::parse_create_record_reply(&reply).unwrap();
    assert_eq!(created.status, u32::from(CANNOT_WRITE));
    let path: [&[u8]; 3] = [&table, &[5], &serial::encode(1)];
    assert!(matches!(
        client.lock(&path, 0x72, false).unwrap(),
        write::LockReply::Granted { .. }
    ));
    client.kill_all_locks(0x72).unwrap();
    assert_eq!(records_of(&mut client, TABLE).len(), 2);
    assert_eq!(serial_of(&mut client).as_deref(), Some("2"));
    assert_eq!(std::fs::read(&file.0).unwrap(), bytes, "nothing written");
    assert_eq!(handle.alerts().len(), alerts + 1, "one alert");
    client.disconnect().unwrap();

    // Hosted again, it takes changes.
    handle
        .close_file(id, &om_server::Notice::default())
        .unwrap();
    wait_for(&handle, id, om_server::FileStatus::Closed);
    handle.open_file(id).unwrap();
    wait_for(&handle, id, om_server::FileStatus::Open);
    assert_eq!(handle.files()[0].error, None);
    let (mut client, _) = server::logged_in(port, "unknown-again");
    let serial = client
        .get_serial(&table, &count, "", 1, write::SERIAL_INCREMENT)
        .unwrap();
    assert_eq!((serial.status, serial.value.as_str()), (0, "2"));
    client.disconnect().unwrap();
}

/// A save that stops part way leaves `<file>.omsave` beside the file,
/// which stops every later save. Nothing removes it, nor anything else at
/// that name: a file hosted with something there (a copy a save left, a
/// folder, a link to another file) is hosted as any file is, with an alert
/// that names the entry, which is left as it is, and each change fails at
/// its save, taken back, with the storage error. Something put there while
/// the file is hosted does the same, and the alert of the failed save says
/// what is in the way. Once it is removed the file saves, without being
/// opened again (docs/server-storage.md 5).
#[cfg(unix)]
#[test]
fn anything_at_a_files_save_copy_name_is_left_and_its_changes_fail_until_it_is_removed() {
    let table = uint_segment(TABLE);
    let count = uint_segment(COUNT);
    let advance = |client: &mut om_session::Session| {
        client
            .get_serial(&table, &count, "", 1, write::SERIAL_INCREMENT)
            .unwrap()
            .status
    };
    let copy_of = |file: &TempFile| {
        let mut path = file.0.as_os_str().to_owned();
        path.push(".omsave");
        PathBuf::from(path)
    };

    for (tag, kind) in [
        ("save-leftover", "copy"),
        ("save-leftover-folder", "folder"),
        ("save-leftover-link", "link"),
    ] {
        let file = TempFile::from_tree(tag, &with_photo());
        let copy = copy_of(&file);
        let victim = file.0.with_extension("victim");
        std::fs::write(&victim, b"not to be removed").unwrap();
        match kind {
            "copy" => std::fs::write(&copy, b"half a save").unwrap(),
            "folder" => std::fs::create_dir(&copy).unwrap(),
            _ => std::os::unix::fs::symlink(&victim, &copy).unwrap(),
        }
        let (port, handle) = server::start_with_handle(&file, om_server::ScriptLimits::DEFAULT);
        let [info] = handle.files().try_into().unwrap();
        assert_eq!(info.status, om_server::FileStatus::Open, "{tag}: hosted");
        let named = copy.display().to_string();
        let alerts = handle.alerts();
        assert!(
            alerts
                .iter()
                .any(|a| a.message.contains(&named) && a.message.contains("not saved")),
            "{tag}: {alerts:?}"
        );
        let bytes = std::fs::read(&file.0).unwrap();
        let (mut client, _) = server::logged_in(port, tag);
        assert_eq!(advance(&mut client), u32::from(CANNOT_WRITE), "{tag}");
        assert_eq!(serial_of(&mut client), None, "{tag}: taken back");
        assert_eq!(std::fs::read(&file.0).unwrap(), bytes);
        let kept = std::fs::symlink_metadata(&copy).unwrap();
        assert_eq!(
            (kept.is_file(), kept.is_dir(), kept.is_symlink()),
            (kind == "copy", kind == "folder", kind == "link"),
            "{tag}: left as it is"
        );
        if kind == "copy" {
            assert_eq!(std::fs::read(&copy).unwrap(), b"half a save");
        }
        assert_eq!(std::fs::read(&victim).unwrap(), b"not to be removed");
        // Removed, the next change is saved.
        if kind == "folder" {
            std::fs::remove_dir(&copy).unwrap();
        } else {
            std::fs::remove_file(&copy).unwrap();
        }
        assert_eq!(advance(&mut client), 0, "{tag}");
        assert_eq!(serial_of(&mut client).as_deref(), Some("2"), "{tag}");
        client.disconnect().unwrap();
        let _ = std::fs::remove_file(&victim);
    }

    // Put there while the file is hosted: the next save is not stored, and
    // its alert says why; the entry is left as it is.
    let file = TempFile::from_tree("save-leftover-later", &with_photo());
    let copy = copy_of(&file);
    let (port, handle) = server::start_with_handle(&file, om_server::ScriptLimits::DEFAULT);
    let (mut client, _) = server::logged_in(port, "leftover-later");
    let bytes = std::fs::read(&file.0).unwrap();
    std::fs::write(&copy, b"put there").unwrap();
    assert_eq!(advance(&mut client), u32::from(CANNOT_WRITE));
    assert_eq!(serial_of(&mut client), None, "taken back");
    assert_eq!(std::fs::read(&file.0).unwrap(), bytes);
    assert_eq!(std::fs::read(&copy).unwrap(), b"put there");
    let alerts = handle.alerts();
    assert!(
        alerts
            .iter()
            .any(|a| a.message.contains("in the way")
                && a.message.contains(&copy.display().to_string())),
        "{alerts:?}"
    );
    std::fs::remove_file(&copy).unwrap();
    assert_eq!(advance(&mut client), 0);
    assert_eq!(serial_of(&mut client).as_deref(), Some("2"));
    client.disconnect().unwrap();
}

/// A server script's end saves what it left unsaved (the record id New
/// Record reserved for a record it reverted): a save that fails there is
/// the script's error (docs/server-storage.md 5), not dropped.
#[test]
fn a_script_whose_end_save_fails_ends_with_the_save_error() {
    let file = TempFile::from_tree("save-script-end", &with_photo());
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "script-end");
    let blocked = BlockedSaves::new(&file);
    let reply = client.perform_script_on_server(8, "", true).unwrap();
    assert_eq!((reply.error, reply.result.as_str()), (CANNOT_WRITE, "0"));
    drop(blocked);
    let reply = client.perform_script_on_server(8, "", true).unwrap();
    assert_eq!((reply.error, reply.result.as_str()), (0, "0"));
    client.disconnect().unwrap();
}

/// An op86 program of one instruction with `operands` (leaf, value),
/// signed for `session` (query-program.md 2).
fn program(session: &om_session::Session, file_id: u32, operands: &[(u8, Vec<u8>)]) -> Vec<u8> {
    use om_query::program::leaf;
    let mut program = OwnedNode::list(Vec::new(), Vec::new());
    let signed = [
        (leaf::FILE_ID, file_id.to_be_bytes().to_vec()),
        (
            leaf::ACCESS_KEY,
            om_crypto::query_access_key(1, session.connection_key(), file_id).to_vec(),
        ),
    ];
    for (key, value) in operands.iter().cloned().chain(signed) {
        program.put(&[&[1], &[1], &[key]], OwnedNode::leaf(Vec::new(), value));
    }
    program.to_wire().unwrap()
}

/// Whether op86's reply is the error slot with 800 alone
/// (query-program.md 11.2).
fn failed_to_store(slots: &[u8]) -> bool {
    slots == [0x80, 0x06, 0x00, 0x03, 0x02, 0x03, 0x20, 0x40]
}

/// The review's P1 on op86: a program that reserves a member key whose
/// storing fails answers its error slot, and the key waits for the next
/// write; a program that only reads keeps its answer meanwhile, though the
/// key it finds waiting cannot be stored either. On a file that takes no
/// more changes a program that would change it is refused before it runs,
/// so it reserves nothing, and programs that read go on answering.
#[test]
fn a_program_that_only_reads_keeps_its_answer_when_what_waits_cannot_be_stored() {
    use om_query::program::{leaf, opcode};
    let file = TempFile::from_tree("save-op86-waiting", &with_photo());
    let (port, handle) = server::start_with_handle(&file, om_server::ScriptLimits::DEFAULT);
    let id = handle.files()[0].id;
    let (mut client, file_id) = server::logged_in(port, "op86-waiting");
    let table = uint_segment(TABLE);
    let generate = program(
        &client,
        file_id,
        &[
            (leaf::OPCODE, vec![opcode::GENERATE_MEMBER_KEY]),
            (leaf::OPERAND1, vec![1, 4]),
        ],
    );
    let read = program(
        &client,
        file_id,
        &[
            (leaf::OPCODE, vec![opcode::GET_KEY]),
            (leaf::SRC, [vec![table.len() as u8], table.clone()].concat()),
        ],
    );
    let answer = client.query(&read).unwrap().slots;
    assert!(!failed_to_store(&answer));

    let blocked = BlockedSaves::new(&file);
    let reply = client.query(&generate).unwrap();
    assert!(failed_to_store(&reply.slots), "{:02x?}", reply.slots);
    // The key waits, and cannot be stored: the read keeps its answer.
    assert_eq!(client.query(&read).unwrap().slots, answer);
    drop(blocked);
    assert_eq!(client.query(&read).unwrap().slots, answer, "and stores it");

    // A file that takes no more changes.
    assert!(handle.failing_saves(id, Some(om_server::SaveFault::FolderFlush)));
    let serial = client
        .get_serial(&table, &uint_segment(COUNT), "", 1, write::SERIAL_INCREMENT)
        .unwrap();
    assert!(handle.failing_saves(id, None));
    assert_eq!(serial.status, u32::from(CANNOT_WRITE));
    let layouts = [vec![4]];
    let keys = handle.member_keys(id, &layouts).unwrap();
    assert_eq!(keys.0, None, "nothing waits");
    let reply = client.query(&generate).unwrap();
    assert!(failed_to_store(&reply.slots), "{:02x?}", reply.slots);
    assert_eq!(
        handle.member_keys(id, &layouts).unwrap(),
        keys,
        "no key reserved: nothing waits, the next key is the same"
    );
    assert_eq!(client.query(&read).unwrap().slots, answer);
    client.disconnect().unwrap();
}

/// An op70 entry in the edit form for a record the file does not hold
/// creates nothing (#840 of github.com/OpenMaker-Dev/openmaker): only an
/// entry marked new (`01`) creates a record, and only under an issued, free
/// id. So a client cannot plant a record under an id an honest client's
/// next new record gets, nor bring back a record another client deleted.
/// Each refused commit fails whole and leaves its staged record staged.
#[test]
fn an_edit_of_a_record_that_is_not_there_creates_nothing() {
    let file = TempFile::new("edit-of-missing", &[], true);
    let (port, handle) = server::start_with_handle(&file, om_server::ScriptLimits::DEFAULT);
    let table = uint_segment(TABLE);
    let record = |id: u64, name: &str| {
        OwnedNode::list(
            serial::encode(id),
            vec![
                OwnedNode::leaf(uint_segment(NAME), om_scsu::encode_scsu(name)),
                OwnedNode::leaf(vec![0xfc], vec![1, 1]),
            ],
        )
    };
    let edited = |id: u64| {
        OwnedNode::list(
            serial::encode(id),
            vec![OwnedNode::leaf(uint_segment(NAME), Vec::new())],
        )
    };

    // X plants record 5, past the last issued id 2, in the edit form.
    let (mut x, _) = server::logged_in(port, "x");
    x.upload_records(&table, vec![record(5, "Planted")])
        .unwrap();
    let staged = handle.upload_memory_in_use();
    assert!(x.commit_records(&table, vec![edited(5)], 0x31).is_err());
    assert_eq!(handle.upload_memory_in_use(), staged, "still staged");
    assert_eq!(last_id_on_disk(&file), 2);
    assert_eq!(on_disk(&file, 5, NAME), None);

    // H is issued 3 to 5, and its new record 5 is stored.
    let (mut h, h_file) = server::logged_in(port, "h");
    let create = write::CreateRecord {
        table: vec![table.clone()],
        count: 3,
        txn: 0x41,
    };
    let reply = h
        .perform(&write::build_create_record(1, h_file, &create))
        .unwrap();
    assert_eq!(write::parse_create_record_reply(&reply).unwrap().status, 0);
    h.upload_records(&table, vec![record(5, "Honest5")])
        .unwrap();
    let new = OwnedNode::leaf(serial::encode(5), vec![write::COMMIT_NEW]);
    h.commit_records(&table, vec![new], 0x41).unwrap();
    assert_eq!(on_disk(&file, 5, NAME).as_deref(), Some("Honest5"));

    // B stages record 1 without a lock; A deletes it; B's edit of it then
    // brings nothing back.
    let (mut b, _) = server::logged_in(port, "b");
    b.upload_records(&table, vec![record(1, "Ghost")]).unwrap();
    let (mut a, _) = server::logged_in(port, "a");
    assert!(matches!(
        lock(&mut a, 0x51, true),
        write::LockReply::Granted { .. }
    ));
    let deleted = OwnedNode::leaf(serial::encode(1), vec![write::COMMIT_DELETED]);
    a.commit_records(&table, vec![deleted], 0x51).unwrap();
    a.kill_all_locks(0x51).unwrap();
    assert_eq!(on_disk(&file, 1, NAME), None);
    let staged = handle.upload_memory_in_use();
    assert!(b.commit_records(&table, vec![edited(1)], 0x61).is_err());
    assert_eq!(handle.upload_memory_in_use(), staged, "still staged");
    assert_eq!(on_disk(&file, 1, NAME), None, "not back");

    // An ordinary edit, of record 2, still stores.
    a.upload_records(&table, vec![record(2, "Edited")]).unwrap();
    a.commit_records(&table, vec![edited(2)], 0x52).unwrap();
    assert_eq!(on_disk(&file, 2, NAME).as_deref(), Some("Edited"));
}
