use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use om_hbam::slot::uint_segment;
use om_hbam::{serial, OwnedNode, RidList};
use om_protocol::write::{
    attention, CreateRecord, GetSerial, KillAllLocks, Lock, LockReply, Unlock, COMMIT_DELETED,
    COMMIT_NEW, LOCK_CONFLICT, LOCK_WRITE, SERIAL_INCREMENT,
};
use om_scripts::text_node;

use super::*;
use crate::config::HostedFile;
use crate::eventlog::EventLog;
use crate::hosting::Hosting;
use crate::modcount::{serial_leaf, MOD_COUNT};
use crate::scripts::fixture;

/// A fixed clock and a counter for a random source, the same for every
/// engine made with it.
struct Seeded(AtomicU64);

impl Environment for Seeded {
    fn now(&self) -> jiff::Zoned {
        "2026-11-24T12:00:00[UTC]".parse().unwrap()
    }

    fn fill_random(&self, bytes: &mut [u8]) {
        for byte in bytes {
            *byte = self.0.fetch_add(1, Ordering::Relaxed) as u8;
        }
    }
}

fn engine() -> FileEngine {
    FileEngine::new(Box::new(Seeded(AtomicU64::new(0))))
}

/// The operations an engine ran, as it offered them ([`FileEngine::recording`]).
type Log = Arc<Mutex<Vec<Operation>>>;

/// [`engine`], recording each operation it runs.
fn recording() -> (FileEngine, Log) {
    let log = Log::default();
    let into = Arc::clone(&log);
    let engine = engine().recording(move |operation| {
        into.lock().unwrap().push(operation.clone());
    });
    (engine, log)
}

/// The last operation `log` recorded.
fn last(log: &Log) -> Operation {
    log.lock()
        .unwrap()
        .last()
        .cloned()
        .expect("an operation ran")
}

/// `tree` hosted from a file of its own, tagged `tag`.
fn host(tag: &str, tree: &OwnedNode) -> (fixture::TempFile, Arc<Hosted>) {
    let file = fixture::TempFile::from_tree(tag, tree);
    let hosted = load(&file);
    (file, hosted)
}

/// The file at `file` hosted as it is on disk.
fn load(file: &fixture::TempFile) -> Arc<Hosted> {
    load_with(file, &engine())
}

/// [`load`], with `engine` finishing and saving what the file records as
/// pending.
fn load_with(file: &fixture::TempFile, engine: &FileEngine) -> Arc<Hosted> {
    let hosted = HostedFile {
        name: fixture::FILE.into(),
        path: file.0.clone(),
    };
    let hosting = Hosting::load(&[hosted], false, &EventLog::default(), engine).unwrap();
    hosting.by_name(fixture::FILE).unwrap()
}

/// The node at `path` in `file`'s tree.
fn node_at(file: &Hosted, path: &[Vec<u8>]) -> Option<OwnedNode> {
    let path: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
    file.read_store().node(&path).cloned()
}

/// The keys of index `index` (`[T, 01, F, k]`), each with its records,
/// and its counts leaf (`[T, 01, F, k | 10]`) as (entries, distinct keys).
type Postings = (BTreeMap<Vec<u8>, Vec<u64>>, (u64, u64));

fn postings(file: &Hosted, index: &[Vec<u8>]) -> Postings {
    let keys = node_at(file, index).expect("the index");
    let records = keys
        .children
        .iter()
        .map(|key| {
            let ids = RidList::from_owned(key).unwrap().iter().collect();
            (key.segment.clone(), ids)
        })
        .collect();
    let mut counts = index.to_vec();
    let kind = counts.pop().unwrap()[0];
    counts.push(vec![kind | 0x10]);
    let value = node_at(file, &counts)
        .and_then(|n| n.value)
        .expect("counts");
    let number = |bytes: &[u8]| u64::from_be_bytes(bytes.try_into().unwrap());
    (records, (number(&value[..8]), number(&value[8..])))
}

/// A field catalog `[T, 03]` in which `field` has the formula `text` in
/// the Tasks context, as Specify Calculation stores it (schema-path.md
/// 5.5).
fn set_formula(catalog: &mut OwnedNode, field: u64, text: &str) {
    let calc = fixture::calc(text);
    let f = uint_segment(field);
    for (child, bytes) in [
        (4u8, om_formula::Key::from_id(fixture::OCCURRENCE).0),
        (5, calc.formatted.unwrap()),
        (6, calc.polish.unwrap()),
    ] {
        catalog.put(
            &[&[5], &f, &[5], &[child]],
            OwnedNode::leaf(Vec::new(), bytes),
        );
    }
}

/// docs/server-storage.md 5: operations are values, so a sequence of them
/// recorded with its outcomes replays against a fresh load of the same
/// file to the same replies, notices and tree. The sequence covers each
/// operation: op71 reserves ids 4 and 5, which another user then cannot
/// lock; op72 advances a serial; op70 creates record 4 and deletes 2 and
/// leaves record 1, which the other user holds, staged; that user
/// upgrades and releases its lock, telling of a change; op65 ends the
/// first user's locks; a second op72 reads the serial the first advanced.
#[test]
fn a_recorded_sequence_of_operations_replays_to_the_same_replies_and_tree() {
    let mut tree = fixture::tree(&[], true);
    fixture::add_tasks(&mut tree, 1..=3);
    let (_first, file) = host("engine-recorded", &tree);
    let a = LockUser {
        connection: 1,
        txn: 7,
    };
    let b = LockUser {
        connection: 2,
        txn: 9,
    };
    let table = uint_segment(fixture::TABLE);
    let record = |id: u64| vec![table.clone(), vec![0x05], serial::encode(id)];
    let lock = |user: LockUser, id: u64, lock_type: u16| Lock {
        waiter: 0,
        timeout: 0,
        want_mod_count: true,
        path: record(id),
        argument: 0,
        host_file: 1,
        txn: user.txn,
        lock_type,
    };
    let new = OwnedNode::list(
        serial::encode(4),
        vec![
            text_node(&uint_segment(fixture::NAME), "Task 4"),
            serial_leaf(MOD_COUNT, 1),
        ],
    );
    let edited = OwnedNode::list(
        serial::encode(1),
        vec![
            text_node(&uint_segment(fixture::NAME), "Task one"),
            serial_leaf(MOD_COUNT, 2),
        ],
    );
    let entries = vec![
        OwnedNode::list(
            serial::encode(1),
            vec![OwnedNode::leaf(uint_segment(fixture::NAME), Vec::new())],
        ),
        OwnedNode::leaf(serial::encode(2), vec![COMMIT_DELETED]),
        OwnedNode::leaf(serial::encode(4), vec![COMMIT_NEW]),
    ];
    let list = OwnedNode::list(
        Vec::new(),
        vec![OwnedNode::list(
            table.clone(),
            vec![OwnedNode::list(vec![0x05], entries)],
        )],
    );
    let access = file.account(fixture::ACCOUNT).unwrap().access.clone();
    let operations = vec![
        Operation::ReserveRecordIds(ReserveRecordIds {
            user: a,
            holder: "A (Admin)".into(),
            request: CreateRecord {
                table: vec![table.clone()],
                count: 2,
                txn: a.txn,
            },
            room: writes::MAX_LOCKS,
            durable: true,
        }),
        Operation::TakeLock(TakeLock {
            user: b,
            holder: "B (Admin)".into(),
            request: lock(b, 4, LOCK_WRITE),
            room: writes::MAX_LOCKS,
        }),
        Operation::TakeLock(TakeLock {
            user: b,
            holder: "B (Admin)".into(),
            request: lock(b, 1, LOCK_WRITE),
            room: writes::MAX_LOCKS,
        }),
        Operation::AdvanceSerial(AdvanceSerial {
            request: GetSerial {
                table: vec![table.clone()],
                field: vec![uint_segment(fixture::COUNT)],
                value: String::new(),
                increment: 1,
                flags: SERIAL_INCREMENT,
            },
        }),
        Operation::CommitRecords(CommitRecords {
            user: a,
            account: fixture::ACCOUNT.into(),
            access: access.unwrap(),
            list,
            staged: Staged::from([(record(1), edited.clone()), (record(4), new)]),
            withheld: Withheld::default(),
        }),
        Operation::UpgradeLock(UpgradeLock {
            user: b,
            holder: "B (Admin)".into(),
            request: Upgrade {
                waiter: 0,
                timeout: 0,
                path: record(1),
                host_file: 1,
                txn: b.txn,
                new_type: 10,
                current_type: LOCK_WRITE,
            },
            room: writes::MAX_LOCKS,
        }),
        Operation::ReleaseLock(ReleaseLock {
            connection: b.connection,
            request: Unlock {
                message_address: 0,
                attention_mask: attention::MODIFY | attention::RECORD,
                path: record(1),
                host_file: 1,
                txn: b.txn,
                lock_type: 10,
            },
        }),
        Operation::ReleaseLocks(ReleaseLocks {
            connection: a.connection,
            request: KillAllLocks {
                host_file: 1,
                txn: a.txn,
                all: true,
            },
        }),
        Operation::AdvanceSerial(AdvanceSerial {
            request: GetSerial {
                table: vec![table.clone()],
                field: vec![uint_segment(fixture::COUNT)],
                value: String::new(),
                increment: 1,
                flags: SERIAL_INCREMENT,
            },
        }),
    ];
    let recording = engine();
    let recorded: Vec<(Operation, Outcome)> = operations
        .into_iter()
        .map(|operation| (operation.clone(), recording.submit(&file, operation)))
        .collect();
    let replies: Vec<&Reply> = recorded.iter().map(|(_, outcome)| &outcome.reply).collect();
    assert!(
        matches!(replies[0], Reply::ReserveRecordIds(r) if r.status == 0 && r.record == [serial::encode(4)]),
        "{:?}",
        replies[0]
    );
    assert!(
        matches!(replies[1], Reply::TakeLock(LockReply::Refused { status, conflict })
            if *status == LOCK_CONFLICT && conflict.users == "A (Admin)"),
        "{:?}",
        replies[1]
    );
    assert!(matches!(
        replies[2],
        Reply::TakeLock(LockReply::Granted { mod_count: Some(_) })
    ));
    assert!(matches!(replies[3], Reply::AdvanceSerial(r) if r.status == 0 && r.value == "1"));
    // The first op72 stored the next value: an engine that only read the
    // serial would answer "1" again.
    assert!(
        matches!(replies[8], Reply::AdvanceSerial(r) if r.status == 0 && r.value == "2"),
        "{:?}",
        replies[8]
    );
    assert_eq!(
        *replies[4],
        Reply::CommitRecords(CommitReply {
            result: Ok(()),
            unused: Staged::from([(record(1), edited)]),
        })
    );
    let code = |kind| kind | attention::RECORD;
    assert_eq!(
        recorded[4].1.notices,
        Notices {
            from: a.connection,
            changes: vec![
                Change {
                    path: record(2),
                    code: code(attention::DELETE)
                },
                Change {
                    path: record(4),
                    code: code(attention::ADD)
                },
            ],
        }
    );
    assert_eq!(recorded[6].1.notices.changes.len(), 1);
    let (_second, again) = host("engine-replayed", &tree);
    let replaying = engine();
    for (operation, outcome) in &recorded {
        assert_eq!(
            &replaying.submit(&again, operation.clone()),
            outcome,
            "{operation:?}"
        );
    }
    assert_eq!(again.read_store().node(&[]), file.read_store().node(&[]));
    for hosted in [&file, &again] {
        let locks = hosted.lock_table();
        assert_eq!(
            (locks.held_by(a.connection), locks.held_by(b.connection)),
            (0, 0)
        );
    }
}

/// What a step of a recording submits: a completed operation, or a draft
/// the engine completes.
enum Step {
    Operation(Operation),
    Draft(Draft),
}

/// The schema operations replay the same way (docs/server-storage.md 5).
/// Drafts are recorded as the engine completed them, with what reads left
/// waiting, and a fresh load with nothing waiting replays those to the same
/// replies and tree after each step. A find builds Name's value index,
/// which is stored; op13 uploads a table's deferred field work (drop Name's
/// indexes; Count changed, so Doubled, stored stale on record 1, is
/// recomputed), which op73 does; an op86 program renames Tasks, deletes
/// Notes and its catalog member, writes the layout key GenerateMemberKey
/// issued and stores the word index another find built. Each effect is
/// checked against the fixture.
#[test]
fn a_recorded_sequence_of_schema_operations_replays_to_the_same_replies_and_tree() {
    use om_query::{Source as _, Write};
    let table = uint_segment(fixture::TABLE);
    let doubled = vec![
        table.clone(),
        vec![0x05],
        serial::encode(1),
        uint_segment(fixture::DOUBLED),
    ];
    let mut tree = fixture::tree(&[], true);
    fixture::add_tasks(&mut tree, 1..=3);
    fixture::index_layouts(&mut tree);
    let path: Vec<&[u8]> = doubled.iter().map(Vec::as_slice).collect();
    tree.put(&path, text_node(&[], "99"));
    let (_first, file) = host("engine-schema-recorded", &tree);
    let notes = uint_segment(fixture::NOTES);
    let name = uint_segment(fixture::NAME);
    // `[T, 01, F, k]`: field F's value (0) or word (1) index.
    let index = |kind: u8| vec![table.clone(), vec![0x01], name.clone(), vec![kind]];
    let find = |kind: u8| {
        let store = file.read_store();
        assert!(store
            .index_on_demand(&table, &name, kind)
            .unwrap()
            .is_some());
    };
    // `[T, 03, 01, fd]`: the deferred field work (schema-path.md 7.2).
    let deferred = vec![table.clone(), vec![0x03], vec![0x01], vec![0xfd]];
    let work = OwnedNode::list(
        vec![0xfd],
        vec![
            OwnedNode::leaf(name.clone(), 0x20u32.to_be_bytes().to_vec()),
            OwnedNode::leaf(uint_segment(fixture::COUNT), 0x40u32.to_be_bytes().to_vec()),
        ],
    );
    let tasks = vec![vec![3], vec![0x10], vec![5], table.clone()];
    let notes_member = vec![vec![3], vec![0x10], vec![5], notes.clone()];
    let mut renamed = node_at(&file, &tasks).unwrap();
    renamed.set_child(text_node(&[0x10], "Jobs"));
    let layouts_index = |key: u8| vec![vec![4], vec![1], vec![key]];
    assert!(node_at(&file, &notes_member).is_some());
    // The keys Name's value and word indexes hold for "Task 1" to "Task 3"
    // (`om_collate`, the one definition of an index key).
    let mut values = BTreeMap::new();
    let mut words: BTreeMap<Vec<u8>, Vec<u64>> = BTreeMap::new();
    for id in 1..=3u64 {
        let text = format!("Task {id}");
        values.insert(om_collate::text_index_key(&text), vec![id]);
        for word in om_collate::words(&text) {
            words
                .entry(om_collate::text_index_key(word))
                .or_default()
                .push(id);
        }
    }
    let mut recorded: Vec<(Operation, Outcome, OwnedNode)> = Vec::new();
    let (recording, log) = recording();
    // The operation the engine ran, completed, and its outcome.
    let mut submit = |step: Step| {
        let outcome = match step {
            Step::Operation(operation) => {
                let outcome = recording.submit(&file, operation.clone());
                assert_eq!(last(&log), operation);
                outcome
            }
            Step::Draft(draft) => recording.submit_draft(&file, draft),
        };
        let operation = last(&log);
        let after = file.read_store().node(&[]).unwrap().clone();
        recorded.push((operation.clone(), outcome.clone(), after));
        (operation, outcome)
    };

    find(0);
    let (operation, outcome) = submit(Step::Draft(Draft::StoreIndexes));
    assert_eq!(
        operation,
        Operation::StoreIndexes(StoreIndexes {
            indexes: vec![(table.clone(), name.clone(), 0)],
        })
    );
    assert_eq!(outcome.reply, Reply::StoreIndexes);
    assert_eq!(postings(&file, &index(0)), (values, (3, 3)));

    let upload = UploadToCatalog {
        path: deferred.clone(),
        node: work,
    };
    let (_, outcome) = submit(Step::Operation(Operation::UploadToCatalog(upload)));
    assert_eq!(outcome.reply, Reply::UploadToCatalog(true));
    assert!(node_at(&file, &deferred).is_some());

    let update = UpdateTable {
        table: table.clone(),
        account: fixture::ACCOUNT.into(),
    };
    let (_, outcome) = submit(Step::Operation(Operation::UpdateTable(update)));
    assert_eq!(outcome.reply, Reply::UpdateTable(Ok(true)));
    assert!(node_at(&file, &deferred).is_none(), "the work list is done");
    assert!(
        node_at(&file, &index(0x10)).is_none(),
        "Name's indexes are dropped"
    );
    assert_eq!(
        node_at(&file, &doubled).and_then(|n| n.value),
        text_node(&[], "2").value,
        "Doubled is recomputed"
    );

    find(1);
    let issued = file.read_store().generate_member_key(&[vec![4]]).unwrap();
    let writes = vec![
        Write::Put {
            path: tasks.clone(),
            node: renamed.clone(),
        },
        Write::Remove {
            path: notes_member.clone(),
        },
        Write::Remove {
            path: vec![notes.clone()],
        },
    ];
    let (operation, outcome) = submit(Step::Draft(Draft::ChangeSchema {
        writes: writes.clone(),
    }));
    assert_eq!(outcome.reply, Reply::ChangeSchema(Ok(())));
    let Operation::ChangeSchema(completed) = &operation else {
        panic!("{operation:?}");
    };
    assert_eq!(completed.writes, writes);
    assert_eq!(completed.indexes, [(table.clone(), name.clone(), 1)]);
    assert_eq!(completed.member_keys.len(), 1);
    assert_eq!(node_at(&file, &tasks), Some(renamed));
    assert!(
        node_at(&file, &notes_member).is_none(),
        "Notes' member is gone"
    );
    assert!(node_at(&file, &[notes]).is_none(), "Notes is deleted");
    let value = |path: Vec<Vec<u8>>| node_at(&file, &path).and_then(|n| n.value);
    assert_eq!(value(layouts_index(0x00)), Some(issued.0), "the layout id");
    assert_eq!(value(layouts_index(0xfc)), Some(issued.1), "its mod count");
    // "task" in every record, and each number in one.
    assert_eq!(words.len(), 4);
    assert_eq!(postings(&file, &index(1)), (words, (6, 4)));
    assert!(recorded
        .iter()
        .all(|(_, outcome, _)| outcome.notices.changes.is_empty()));
    {
        let store = file.read_store();
        assert!(!store.has_indexes_on_demand());
        assert!(!store.has_issued_member_keys());
    }

    let (_second, again) = host("engine-schema-replayed", &tree);
    let replaying = engine();
    for (operation, outcome, after) in &recorded {
        assert_eq!(
            &replaying.submit(&again, operation.clone()),
            outcome,
            "{operation:?}"
        );
        let now = again.read_store().node(&[]).unwrap().clone();
        assert_eq!(&now, after, "the tree after {operation:?}");
    }
}

/// `table`'s field catalog `[T, 03]` given the index `[T, 03, 01]` every
/// file FileMaker made has (schema-path.md 5.1, 5.5): the last issued id,
/// the name list, the name list by type, the display order and the count,
/// so that a field can be added to it.
fn index_fields(root: &mut OwnedNode, table: u64) {
    use om_data::field_catalog;
    let key_string = |key: &[u8]| [&[key.len() as u8][..], key].concat();
    let t = uint_segment(table);
    let catalog = root.at(&[&t, &[3]]).cloned().expect("a field catalog");
    let members = field_catalog::members(&catalog);
    let mut index = OwnedNode::list(vec![1], Vec::new());
    let mut last = 0;
    for (n, member) in members.iter().enumerate() {
        last = last.max(serial::decode(&member.segment).expect("a serial key"));
        let name = field_catalog::member_name(member).expect("a name");
        let definition = field_catalog::member_definition(member).expect("a definition");
        let key = field_catalog::name_key(&name);
        let id = key_string(&member.segment);
        let listed = [&key[..], &[0, 0, 0]].concat();
        let typed = [&[definition[0], definition[1] << 2][..], &key, &[0, 0]].concat();
        index.put(&[&[1], &listed], OwnedNode::leaf(Vec::new(), id.clone()));
        index.put(&[&[6], &typed], OwnedNode::leaf(Vec::new(), id.clone()));
        let position = (n as u32 + 1).to_be_bytes();
        index.put(&[&[3], &position], OwnedNode::leaf(Vec::new(), id));
    }
    let count = (members.len() as u32).to_be_bytes().to_vec();
    index.set_child(OwnedNode::leaf(vec![0], key_string(&serial::encode(last))));
    index.set_child(OwnedNode::leaf(vec![4], count));
    root.put(&[&t, &[3], &[1]], index);
}

/// Container data holding `bytes` as its one stream, `FILE`
/// (containers.md 2).
fn file_data(bytes: &[u8]) -> crate::library::BinaryData {
    use om_data::containers::{md5, stream_node};
    let mut directory = 1u16.to_be_bytes().to_vec();
    directory.extend(b"FILE");
    directory.extend((bytes.len() as u32).to_be_bytes());
    let md5 = md5([(&b"FILE"[..], bytes)]);
    let keys = vec![
        OwnedNode::leaf(vec![2], directory),
        OwnedNode::leaf(vec![4], md5.to_vec()),
        stream_node(b"FILE", bytes),
    ];
    crate::library::BinaryData::from_tree(keys, md5)
}

/// docs/server-storage.md 5: an authoring edit takes the time of its
/// stamps and the UUIDs of what it adds from the engine's environment, so
/// a recorded sequence of edits and container operations replays against a
/// fresh load to the same replies, notices and tree. Each edit carries the
/// revision the file had after the edits before it, so a replay that went
/// differently would be refused as a conflict. The sequence: one edit of
/// each kind (a script's steps, a new field, a new layout and on it a field
/// with its label, a text, a portal, a button, a button step, a move, new
/// words, a control, a deletion, a new part, part options, a width and
/// Layout Setup; the layout duplicated and the copy deleted; a value list's
/// values), a server script's reservation that the authoring extension's
/// Save writes to disk, container data stored and a library member
/// released. Each step's effect is checked against the fixture, not only
/// against the replay, so an operation that does part of its work the same
/// way twice fails too: the members and catalog indexes each edit writes,
/// with the stamp of a save by the session at the environment's time
/// (schema-path.md 8.1), and the library and member op88 stored. Every step
/// but the Save must change the tree, and the Save must save the file;
/// every edit's locks are released after it.
#[test]
fn a_recorded_sequence_of_design_and_container_operations_replays_to_the_same_replies_and_tree() {
    use om_data::layout_objects::{self, ObjectFrame};
    use om_data::layout_placement;
    use om_protocol::authoring::{self as design, catalog_revision, content_revision};
    use om_protocol::container::AdjustReferenceCount as Adjust;

    use crate::library::LIBRARY;
    const SCRIPT: u64 = 5;
    const DESIGNER: u32 = 3;
    let script = fixture::script(SCRIPT, "Greeting", vec![fixture::exit_with("1")]);
    let mut tree = fixture::tree(std::slice::from_ref(&script), true);
    fixture::index_scripts(&mut tree);
    fixture::index_layouts(&mut tree);
    fixture::add_themes(&mut tree, &[1]);
    fixture::add_photo(&mut tree);
    fixture::add_relationship(
        &mut tree,
        1,
        (fixture::OCCURRENCE, fixture::NAME),
        0,
        (fixture::NOTES_OCCURRENCE, fixture::BODY),
    );
    index_fields(&mut tree, fixture::TABLE);
    // A list of custom values (file-metadata.md 6.1) and the value-list
    // catalog's index.
    tree.put(
        &[&[0x21], &[5], &[1]],
        OwnedNode::list(
            vec![1],
            vec![
                text_node(&[3], "Open\rDone"),
                text_node(&[0x10], "Statuses"),
            ],
        ),
    );
    tree.put(&[&[0x21], &[1], &[0]], OwnedNode::leaf(vec![0], vec![1, 1]));
    let (_first, file) = host("engine-design-recorded", &tree);
    let edit = |edit: DesignEdit| {
        Operation::EditDesign(Box::new(EditDesign {
            connection: DESIGNER,
            session_name: "Designer".into(),
            account: fixture::ACCOUNT.into(),
            edit,
        }))
    };
    let node = |path: &[Vec<u8>]| {
        let path: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        file.read_store().node(&path).cloned()
    };
    let revision = |path: &[Vec<u8>]| catalog_revision(&node(path).expect("a catalog member"));
    let recording = engine();
    let mut recorded: Vec<(Operation, Outcome, OwnedNode, u64)> = Vec::new();
    let mut submit = |operation: Operation| {
        let outcome = recording.submit(&file, operation.clone());
        let store = file.read_store();
        let after = store.node(&[]).unwrap().clone();
        recorded.push((operation, outcome.clone(), after, store.saves()));
        outcome
    };
    let answer = |outcome: Outcome| match outcome.reply {
        Reply::EditDesign(edited) => edited.reply,
        other => panic!("not an edit's reply: {other:?}"),
    };
    let changed = |reply: &design::Reply| match reply {
        design::Reply::Replaced(r) => r.changed,
        design::Reply::Created(r) => r.changed,
        design::Reply::Placed(r) => r.changed,
        design::Reply::LayoutCreated(r) => r.changed,
        design::Reply::Framed(r) => r.changed,
        design::Reply::Deleted(r) => r.changed,
        _ => false,
    };
    let frame =
        |top: f64, left: f64, height: f64, width: f64| [top, left, top + height, left + width];
    let layout_path = |layout: u64| vec![vec![4], vec![5], uint_segment(layout)];
    let at = |path: &[Vec<u8>], below: &[&[u8]]| {
        let mut path = path.to_vec();
        path.extend(below.iter().map(|s| s.to_vec()));
        path
    };
    let text_at = |path: &[Vec<u8>]| node(path).as_ref().and_then(om_scripts::node_text);
    // A member's stamp `fc`: its count, user, account and time.
    let time = crate::connection::host_time_key_at(&Seeded(AtomicU64::new(0)).now());
    type Stamp = (Option<u64>, Option<String>, Option<String>, Option<Vec<u8>>);
    let stamp = |path: &[Vec<u8>]| -> Stamp {
        let member = node(path).unwrap_or_else(|| panic!("no member at {path:02x?}"));
        let value = |key: &[u8]| member.child(key).and_then(|n| n.value.clone());
        let text = |key: &[u8]| member.child(key).and_then(om_scripts::node_text);
        (
            value(design::MOD_COUNT).and_then(|v| serial::decode(v.get(1..)?)),
            text(design::MOD_USER),
            text(design::MOD_ACCOUNT),
            value(design::MOD_TIME).and_then(|v| v.get(1..).map(<[u8]>::to_vec)),
        )
    };
    let count = |path: &[Vec<u8>]| stamp(path).0.unwrap_or(0);
    // Saved by the designer at the environment's time, with `count`.
    let stamped = |path: &[Vec<u8>], count: Option<u64>| {
        let by_edit = (
            count,
            Some("Designer".to_owned()),
            Some(fixture::ACCOUNT.to_owned()),
            Some(time.clone()),
        );
        assert_eq!(stamp(path), by_edit, "the stamp of {path:02x?}");
    };
    // An object's attribute record `[L, 05, O]` with both its UUIDs.
    let has_uuids = |layout: &[Vec<u8>], object: u64| {
        let record = at(layout, &[&[5], &serial::encode(object)]);
        for key in [0xd8, 0xd9] {
            let uuid = node(&at(&record, &[&[key]])).and_then(|n| n.value);
            assert_eq!(uuid.map(|u| u.len()), Some(16), "{record:02x?} {key:02x}");
        }
    };
    let rect = |frame: [f64; 4]| om_layout::Rect {
        top: frame[0],
        left: frame[1],
        bottom: frame[2],
        right: frame[3],
        other: Vec::new(),
    };

    // The script's steps, and the script catalog's index stamped with it.
    let definition = vec![vec![17], vec![5], serial::encode(SCRIPT)];
    let scripts_index = vec![vec![17], vec![1]];
    let counts = (count(&definition), count(&scripts_index));
    let steps = fixture::script(SCRIPT, "Greeting", vec![fixture::exit_with("2")]).encode();
    let replace = design::ReplaceScriptSteps {
        script: SCRIPT,
        expected: content_revision(&node(&definition).unwrap()),
        definition: steps.clone(),
    };
    let outcome = submit(edit(DesignEdit::ReplaceScriptSteps(replace)));
    assert_eq!(
        outcome.notices,
        Notices {
            from: DESIGNER,
            changes: vec![
                Change {
                    path: definition.clone(),
                    code: 2
                },
                Change {
                    path: vec![vec![17], vec![1]],
                    code: 2
                },
            ],
        }
    );
    assert!(changed(&answer(outcome)));
    assert_eq!(
        content_revision(&node(&definition).unwrap()),
        content_revision(&steps)
    );
    stamped(&definition, Some(counts.0 + 1));
    stamped(&scripts_index, Some(counts.1 + 1));

    // A new field: its member, new and so without a count, and its name in
    // the catalog's index, counted and stamped.
    let fields = vec![uint_segment(fixture::TABLE), vec![3]];
    let fields_index = at(&fields, &[&[1]]);
    let index_count = count(&fields_index);
    let create = design::CreateField {
        table: fixture::TABLE,
        expected: revision(&fields),
        data_type: 1,
        name: "Priority".into(),
        comment: String::new(),
    };
    let design::Reply::Created(created) = answer(submit(edit(DesignEdit::CreateField(create))))
    else {
        panic!("no field created");
    };
    assert!(created.changed);
    let new_field = at(&fields, &[&[5], &serial::encode(created.field)]);
    assert_eq!(
        node(&new_field).and_then(|m| om_data::field_catalog::member_name(&m)),
        Some("Priority".to_owned())
    );
    stamped(&new_field, None);
    stamped(&fields_index, Some(index_count + 1));
    let listed = om_data::field_catalog::check_name("Priority").unwrap();
    let id = serial::encode(created.field);
    assert_eq!(
        node(&at(&fields_index, &[&[1], &listed])).and_then(|n| n.value),
        Some([&[id.len() as u8][..], &id].concat()),
        "the new field in the index's name list"
    );

    // A new layout, and edits of it.
    let layouts_index = vec![vec![4], vec![1]];
    let index_count = count(&layouts_index);
    let create = design::CreateLayout {
        expected: revision(&layouts_index),
        occurrence: fixture::OCCURRENCE,
        like: fixture::LAYOUT,
        name: "Entry".into(),
        uuid: [9; 16],
    };
    let reply = answer(submit(edit(DesignEdit::CreateLayout(create))));
    let design::Reply::LayoutCreated(design::LayoutCreated { layout, .. }) = reply else {
        panic!("no layout created: {reply:?}");
    };
    let member = layout_path(layout);
    assert_eq!(text_at(&at(&member, &[&[0x10]])), Some("Entry".to_owned()));
    assert_eq!(
        node(&at(&member, &[&[0xd8]])).and_then(|n| n.value),
        Some(vec![9; 16])
    );
    stamped(&member, None);
    stamped(&layouts_index, Some(index_count + 1));
    let entry = |layout: u64| at(&layouts_index, &[&[7], &uint_segment(layout)]);
    assert!(
        node(&entry(layout)).is_some(),
        "the new layout's index entry"
    );
    // Each edit of the layout stamps its member, counted up.
    let mut edits = count(&member);
    let mut edited = |path: &[Vec<u8>]| {
        edits += 1;
        stamped(path, Some(edits));
    };
    let place = design::PlaceField {
        layout,
        expected: revision(&member),
        table: fixture::TABLE,
        occurrence: fixture::OCCURRENCE,
        field: fixture::NAME,
        frame: frame(80.0, 110.0, 20.0, 200.0),
        label: Some(("Name".into(), frame(80.0, 20.0, 20.0, 80.0))),
    };
    let reply = answer(submit(edit(DesignEdit::PlaceField(place))));
    let design::Reply::Placed(field) = reply else {
        panic!("no field placed: {reply:?}");
    };
    edited(&member);
    has_uuids(&member, field.field_object);
    has_uuids(&member, field.label_object);
    let text = design::PlaceText {
        layout,
        expected: revision(&member),
        frame: frame(10.0, 20.0, 20.0, 120.0),
        text: "Entry".into(),
    };
    let reply = answer(submit(edit(DesignEdit::PlaceText(text))));
    let design::Reply::Placed(text) = reply else {
        panic!("no text placed: {reply:?}");
    };
    edited(&member);
    has_uuids(&member, text.field_object);
    let portal = design::PlacePortal {
        layout,
        expected: revision(&member),
        table: fixture::NOTES,
        occurrence: fixture::NOTES_OCCURRENCE,
        frame: frame(120.0, 20.0, 90.0, 300.0),
        rows: 3,
        fields: vec![fixture::BODY],
    };
    let reply = answer(submit(edit(DesignEdit::PlacePortal(portal))));
    let design::Reply::Placed(portal) = reply else {
        panic!("no portal placed: {reply:?}");
    };
    edited(&member);
    has_uuids(&member, portal.field_object);
    let button = design::PlaceButton {
        layout,
        expected: revision(&member),
        script: SCRIPT,
        frame: frame(240.0, 20.0, 20.0, 80.0),
        label: "Greet".into(),
        parameter: None,
    };
    let reply = answer(submit(edit(DesignEdit::PlaceButton(button))));
    let design::Reply::Placed(button) = reply else {
        panic!("no button placed: {reply:?}");
    };
    edited(&member);
    has_uuids(&member, button.field_object);
    let step = fixture::go_to_record(1);
    let button = design::PlaceButtonStep {
        layout,
        expected: revision(&member),
        frame: frame(240.0, 120.0, 20.0, 80.0),
        label: "First".into(),
        id: step.id(),
        value: step.value,
        flags: step.flags,
        reserved: step.reserved,
        data: step
            .data
            .as_ref()
            .filter(|data| !data.is_empty())
            .map(|data| data.encode(&[om_data::layout_placement::STEP_DATA])),
    };
    let reply = answer(submit(edit(DesignEdit::PlaceButtonStep(button))));
    let design::Reply::Placed(button) = reply else {
        panic!("no button step placed: {reply:?}");
    };
    edited(&member);
    has_uuids(&member, button.field_object);
    let to = frame(10.0, 40.0, 20.0, 120.0);
    let moved = design::SetObjectFrames {
        layout,
        expected: revision(&member),
        frames: vec![(text.field_object, to)],
    };
    let reply = answer(submit(edit(DesignEdit::SetObjectFrames(moved))));
    assert!(changed(&reply), "{reply:?}");
    edited(&member);
    let framed = ObjectFrame {
        object: text.field_object,
        frame: rect(to),
    };
    assert!(layout_objects::already_framed(
        &node(&member).unwrap(),
        &[framed]
    ));
    let words = design::SetText {
        expected: revision(&member),
        layout,
        object: field.label_object,
        text: "Task".into(),
    };
    let reply = answer(submit(edit(DesignEdit::SetText(words))));
    assert!(changed(&reply), "{reply:?}");
    edited(&member);
    assert!(layout_placement::already_words(
        &node(&member).unwrap(),
        field.label_object,
        "Task"
    ));
    let control = design::FieldControl {
        expected: revision(&member),
        layout,
        object: field.field_object,
        // A pop-up menu (`layout_objects::FIELD_CONTROLS`).
        control: 2,
        value_list: Some(1),
    };
    let reply = answer(submit(edit(DesignEdit::FieldControl(control))));
    assert!(changed(&reply), "{reply:?}");
    edited(&member);
    assert!(layout_objects::already_control(
        &node(&member).unwrap(),
        field.field_object,
        2,
        Some(1)
    ));
    let deleted = design::DeleteObjects {
        layout,
        expected: revision(&member),
        objects: vec![text.field_object],
    };
    let reply = answer(submit(edit(DesignEdit::DeleteObjects(deleted))));
    assert!(changed(&reply), "{reply:?}");
    edited(&member);
    assert!(layout_objects::already_deleted(
        &node(&member).unwrap(),
        &[text.field_object]
    ));
    let parts = |path: &[Vec<u8>]| -> Vec<u64> {
        let mut parts = path.to_vec();
        parts.push(vec![3]);
        node(&parts)
            .map(|parts| {
                // Each part `[L, 03, P]` holds its definition `02`.
                parts
                    .children
                    .iter()
                    .filter(|p| p.child(&[2]).is_some())
                    .filter_map(|p| om_hbam::slot::be_uint(&p.segment))
                    .collect()
            })
            .unwrap_or_default()
    };
    let body = parts(&member);
    let header = design::ChangeParts {
        expected: revision(&member),
        layout,
        action: design::PARTS_ADD,
        part: 0,
        part_type: 1,
        size: 40.0,
    };
    let reply = answer(submit(edit(DesignEdit::ChangeParts(header))));
    assert!(changed(&reply), "{reply:?}");
    edited(&member);
    assert_eq!(parts(&member).len(), body.len() + 1);
    let options = design::PartOptions {
        expected: revision(&member),
        layout,
        part: body[0],
        flags: 0,
        page_break_after: 3,
        break_field: None,
    };
    let reply = answer(submit(edit(DesignEdit::PartOptions(options))));
    assert!(changed(&reply), "{reply:?}");
    edited(&member);
    assert!(layout_placement::already_part_options(
        &node(&member).unwrap(),
        body[0],
        (0, 3, None)
    ));
    let width = design::LayoutWidth {
        expected: revision(&member),
        layout,
        width: 640.0,
    };
    let reply = answer(submit(edit(DesignEdit::LayoutWidth(width))));
    assert!(changed(&reply), "{reply:?}");
    edited(&member);
    assert!(layout_placement::already_width(
        &node(&member).unwrap(),
        640.0
    ));
    let views = {
        let mut flags = member.clone();
        flags.push(vec![0xf1]);
        node(&flags)
            .and_then(|n| n.value)
            .and_then(|v| om_hbam::slot::be_uint(&v))
            .unwrap_or(0)
    };
    let setup = design::LayoutSetup {
        expected: revision(&member),
        layout,
        name: "Task entry".into(),
        flags: views,
        hidden: false,
    };
    let index_count = count(&layouts_index);
    let reply = answer(submit(edit(DesignEdit::LayoutSetup(setup))));
    assert!(changed(&reply), "{reply:?}");
    edited(&member);
    stamped(&layouts_index, Some(index_count + 1));
    let renamed = Some("Task entry".to_owned());
    assert_eq!(text_at(&at(&member, &[&[0x10]])), renamed);
    assert_eq!(text_at(&at(&entry(layout), &[&[0x10]])), renamed);
    // The copy keeps its source's count, with who copied it when.
    let (index_count, source_count) = (count(&layouts_index), count(&member));
    let copy = design::DuplicateLayout {
        expected: revision(&layouts_index),
        layout,
        uuid: [10; 16],
    };
    let reply = answer(submit(edit(DesignEdit::DuplicateLayout(copy))));
    let design::Reply::LayoutCreated(copy) = reply else {
        panic!("no copy: {reply:?}");
    };
    let copied = layout_path(copy.layout);
    assert_eq!(
        node(&at(&copied, &[&[0xd8]])).and_then(|n| n.value),
        Some(vec![10; 16])
    );
    stamped(&copied, Some(source_count));
    stamped(&layouts_index, Some(index_count + 1));
    assert!(
        node(&entry(copy.layout)).is_some(),
        "the copy's index entry"
    );
    let index_count = count(&layouts_index);
    let delete = design::DeleteLayout {
        expected: revision(&layouts_index),
        layout: copy.layout,
    };
    let reply = answer(submit(edit(DesignEdit::DeleteLayout(delete))));
    assert!(changed(&reply), "{reply:?}");
    assert!(node(&copied).is_none());
    assert!(
        node(&entry(copy.layout)).is_none(),
        "the copy's index entry"
    );
    stamped(&layouts_index, Some(index_count + 1));

    // A value list's values, the list and the catalog's index stamped.
    let list = vec![vec![0x21], vec![5], vec![1]];
    let lists_index = vec![vec![0x21], vec![1]];
    let counts = (count(&list), count(&lists_index));
    let values = design::ValueListValues {
        expected: revision(&list),
        list: 1,
        values: vec!["Open".into(), "Done".into(), "Later".into()],
    };
    let reply = answer(submit(edit(DesignEdit::ValueListValues(values))));
    assert!(changed(&reply), "{reply:?}");
    assert_eq!(
        text_at(&at(&list, &[&[3]])),
        Some("Open\rDone\rLater".to_owned())
    );
    stamped(&list, Some(counts.0 + 1));
    stamped(&lists_index, Some(counts.1 + 1));

    // A server script's reservation waits for its commit to be saved; the
    // extension's Save writes it.
    let reserve = ReserveRecordIds {
        user: LockUser {
            connection: 4,
            txn: 1,
        },
        holder: "Script (Admin)".into(),
        request: CreateRecord {
            table: vec![uint_segment(fixture::TABLE)],
            count: 1,
            txn: 1,
        },
        room: writes::UNCAPPED,
        durable: false,
    };
    let outcome = submit(Operation::ReserveRecordIds(reserve));
    assert!(matches!(outcome.reply, Reply::ReserveRecordIds(r) if r.status == 0));
    assert!(file.read_store().has_unsaved_changes());
    let outcome = submit(Operation::SaveFile);
    assert_eq!(outcome.reply, Reply::SaveFile(Ok(())));
    assert!(!file.read_store().has_unsaved_changes());

    // Container data stored in Tasks' library, and Photo's member released.
    let tasks = uint_segment(fixture::TABLE);
    let data = file_data(b"a stored file");
    let md5 = data.md5;
    let store = StoreContainerData {
        table: vec![tasks.clone()],
        data: Ok(data),
        bytes: 64,
    };
    let outcome = submit(Operation::StoreContainerData(store));
    let Reply::StoreContainerData(stored) = outcome.reply else {
        panic!("not op88's reply: {:?}", outcome.reply);
    };
    assert_eq!(stored.status, 0);
    // A new member of Tasks' library `[T, 1f]`, the next after Photo's,
    // holding the data and listed under its MD5; the file's own library
    // `[1f]` does not list it.
    let [member] = stored.member.as_slice() else {
        panic!("no member: {stored:?}");
    };
    assert_eq!(*member, serial::encode(fixture::PHOTO_MEMBER + 1));
    let library = vec![tasks.clone(), vec![LIBRARY]];
    let new_member = at(&library, &[&[5], member]);
    assert_eq!(
        node(&at(&new_member, &[&[4]])).and_then(|n| n.value),
        Some(md5.to_vec())
    );
    assert!(node(&at(&library, &[&[1], &[8], &md5, member])).is_some());
    assert!(node(&[vec![LIBRARY], vec![1], vec![8], md5.to_vec()]).is_none());
    let photo = vec![
        tasks.clone(),
        vec![LIBRARY],
        vec![5],
        serial::encode(fixture::PHOTO_MEMBER),
    ];
    assert!(node(&photo).is_some());
    let release = AdjustContainerCount {
        request: Adjust {
            path: photo.clone(),
            delta: -1,
        },
    };
    let outcome = submit(Operation::AdjustContainerCount(release));
    assert_eq!(outcome.reply, Reply::AdjustContainerCount(0));
    assert!(node(&photo).is_none(), "the released member is deleted");
    assert_eq!(file.lock_table().held_by(DESIGNER), 0);

    let (_second, again) = host("engine-design-replayed", &tree);
    let replaying = engine();
    let mut before = again.read_store().node(&[]).unwrap().clone();
    for (operation, outcome, after, saves) in &recorded {
        let saved = again.read_store().saves();
        assert_eq!(
            &replaying.submit(&again, operation.clone()),
            outcome,
            "{operation:?}"
        );
        let store = again.read_store();
        let now = store.node(&[]).unwrap().clone();
        assert_eq!(&now, after, "the tree after {operation:?}");
        assert_eq!(store.saves(), *saves, "the saves after {operation:?}");
        if *operation == Operation::SaveFile {
            assert!(store.saves() > saved, "the Save saved nothing");
        } else {
            assert_ne!(now, before, "{operation:?} changed nothing");
        }
        before = now;
    }
    assert_eq!(again.lock_table().held_by(DESIGNER), 0);
}

/// What reads leave waiting is taken by the first write that holds the
/// store, as before the engine interface (the module doc's "Drafts"), in
/// three schedules a snapshot taken as each query ended got wrong:
/// GenerateMemberKey for a table's field catalog followed by its
/// DeleteTable, an index build that a refused query took back, and two
/// requests that both saw the same key waiting.
#[test]
fn drafts_take_what_reads_left_waiting_when_they_hold_the_store() {
    use om_query::{Source as _, Write};
    let mut tree = fixture::tree(&[], true);
    fixture::add_tasks(&mut tree, 1..=3);
    fixture::index_layouts(&mut tree);
    let table = uint_segment(fixture::TABLE);
    let name = uint_segment(fixture::NAME);
    let (engine, log) = recording();
    let nothing = Operation::StoreIndexes(StoreIndexes {
        indexes: Vec::new(),
    });
    let changes_nothing = || Draft::ChangeSchema { writes: Vec::new() };

    // A ran a DeleteTable program for Tasks; B then had a key issued for
    // Tasks' field catalog, and A's write holds the store first.
    let (_deleted, file) = host("drafts-delete-table", &tree);
    file.read_store()
        .generate_member_key(&[table.clone(), vec![3]])
        .unwrap();
    let a = engine.submit_draft(
        &file,
        Draft::ChangeSchema {
            writes: vec![Write::Remove {
                path: vec![table.clone()],
            }],
        },
    );
    assert_eq!(a.reply, Reply::ChangeSchema(Ok(())));
    engine.submit_draft(&file, changes_nothing());
    assert_eq!(last(&log), nothing, "A wrote B's key before the delete");
    // What NodeExists reads (`om_query::exec`, NodeExists).
    let store = file.read_store();
    assert!(store.node(&[&table]).is_none());
    assert!(om_query::Source::node(&*store, &[&table])
        .unwrap()
        .is_none());
    drop(store);

    // B built Name's value index, A saw it waiting, and B, refused for an
    // index it cannot key, took it back before A's write.
    let (_refused, file) = host("drafts-refused-index", &tree);
    let built = (table.clone(), name.clone(), 0);
    file.read_store()
        .index_on_demand(&table, &name, 0)
        .unwrap()
        .unwrap();
    assert!(file.read_store().has_indexes_on_demand());
    file.read_store()
        .forget_indexes_on_demand(std::slice::from_ref(&built));
    engine.submit_draft(&file, changes_nothing());
    assert_eq!(last(&log), nothing);
    // The next check for the index finds none.
    let counts = vec![table.clone(), vec![0x01], name.clone(), vec![0x10]];
    assert!(node_at(&file, &counts).is_none());

    // A layout key was issued, and two requests saw it waiting: one save.
    let (saved, file) = host("drafts-one-save", &tree);
    file.read_store().generate_member_key(&[vec![4]]).unwrap();
    let saves = file.read_store().saves();
    engine.submit_draft(&file, changes_nothing());
    assert!(matches!(last(&log), Operation::ChangeSchema(op) if op.member_keys.len() == 1));
    let bytes = std::fs::read(&saved.0).unwrap();
    engine.submit_draft(&file, changes_nothing());
    assert_eq!(last(&log), nothing);
    assert_eq!(file.read_store().saves(), saves + 1);
    assert_eq!(
        std::fs::read(&saved.0).unwrap(),
        bytes,
        "the file, and its change number, as the first save left them"
    );
}

/// Member keys a draft took go back to waiting when its run is left by a
/// panic before it writes them, as they stayed waiting before the engine
/// interface when storing the indexes panicked; the indexes it took do not
/// (they had been taken then too). The next write writes the keys.
#[test]
fn member_keys_a_draft_took_wait_again_when_its_run_panics_before_writing_them() {
    use om_query::Source as _;
    let mut tree = fixture::tree(&[], true);
    fixture::add_tasks(&mut tree, 1..=3);
    fixture::index_layouts(&mut tree);
    let (_file, file) = host("drafts-unwind", &tree);
    let table = uint_segment(fixture::TABLE);
    let name = uint_segment(fixture::NAME);
    let issued = file.read_store().generate_member_key(&[vec![4]]).unwrap();
    file.read_store()
        .index_on_demand(&table, &name, 0)
        .unwrap()
        .unwrap();
    let engine = engine();
    super::transaction::PANIC_STORING_INDEXES.with(|panics| panics.set(true));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        engine.submit_draft(&file, Draft::ChangeSchema { writes: Vec::new() })
    }));
    super::transaction::PANIC_STORING_INDEXES.with(|panics| panics.set(false));
    assert!(ran.is_err());
    {
        let store = file.read_store();
        assert!(store.has_issued_member_keys());
        assert!(!store.has_indexes_on_demand());
    }
    engine.submit_draft(&file, Draft::ChangeSchema { writes: Vec::new() });
    let id = node_at(&file, &[vec![4], vec![1], vec![0]]).and_then(|n| n.value);
    assert_eq!(id, Some(issued.0));
    assert!(!file.read_store().has_issued_member_keys());
}

/// op73 finds the stored calculations to recompute by comparing the field
/// definitions with the basis the store keeps, which a fresh load takes
/// from the file as loaded (`Store::load`): so a server that stops between
/// op13 and op73 loses what changed in between. A table with deferred
/// work gets no basis, and its stored calculations are recomputed when it
/// is loaded (`Store::recover`). A formula change always lists
/// `0x40 | 0x80` (schema-path.md 7.2; `om_data::schema_draft` writes the
/// same), so Doubled changed to `Count * 3` is recomputed after a reload.
#[test]
fn a_formula_change_is_recomputed_when_the_file_is_loaded_between_op13_and_op73() {
    let mut tree = fixture::tree(&[], true);
    fixture::add_tasks(&mut tree, 1..=3);
    let table = uint_segment(fixture::TABLE);
    let (saved, file) = host("engine-formula-reloaded", &tree);
    let mut catalog = tree.at(&[&table, &[3]]).unwrap().clone();
    set_formula(&mut catalog, fixture::DOUBLED, "Count * 3");
    catalog.put(
        &[&[1], &[0xfd], &uint_segment(fixture::DOUBLED)],
        OwnedNode::leaf(Vec::new(), 0xc0u32.to_be_bytes().to_vec()),
    );
    let upload = UploadToCatalog {
        path: vec![table.clone(), vec![3]],
        node: catalog,
    };
    let engine = engine();
    let outcome = engine.submit(&file, Operation::UploadToCatalog(upload));
    assert_eq!(outcome.reply, Reply::UploadToCatalog(true));
    drop(file);
    let file = load(&saved);
    let update = UpdateTable {
        table: table.clone(),
        account: fixture::ACCOUNT.into(),
    };
    engine.submit(&file, Operation::UpdateTable(update));
    for (record, value) in [(1, "3"), (2, "6"), (3, "9")] {
        let path = vec![
            table.clone(),
            vec![0x05],
            serial::encode(record),
            uint_segment(fixture::DOUBLED),
        ];
        assert_eq!(
            node_at(&file, &path).and_then(|n| n.value),
            text_node(&[], value).value,
            "record {record}"
        );
    }
}

/// A new stored calculation comes with no deferred work (the server sees
/// it is new by its basis; `tests/recalc.rs`, as `om_data::schema_draft`
/// writes it), so a server that stops between op13 and op73 never computes
/// it: the reload's basis already has it.
#[test]
#[ignore = "requires https://github.com/OpenMaker-Dev/openmaker/issues/707: a reload takes the new field into the basis"]
fn a_new_stored_calculation_is_computed_when_the_file_is_loaded_between_op13_and_op73() {
    const TRIPLED: u64 = 18;
    let mut tree = fixture::tree(&[], true);
    fixture::add_tasks(&mut tree, 1..=3);
    let table = uint_segment(fixture::TABLE);
    let (saved, file) = host("engine-new-calculation-reloaded", &tree);
    let mut catalog = tree.at(&[&table, &[3]]).unwrap().clone();
    let mut definition = vec![0; 26];
    definition[0] = 2;
    definition[1] = 2;
    definition[4..8].copy_from_slice(&21u32.to_be_bytes());
    definition[24..26].copy_from_slice(&1u16.to_be_bytes());
    let f = uint_segment(TRIPLED);
    catalog.put(&[&[5], &f, &[2]], OwnedNode::leaf(Vec::new(), definition));
    catalog.put(&[&[5], &f, &[16]], text_node(&[], "Tripled"));
    set_formula(&mut catalog, TRIPLED, "Count * 3");
    let upload = UploadToCatalog {
        path: vec![table.clone(), vec![3]],
        node: catalog,
    };
    let engine = engine();
    engine.submit(&file, Operation::UploadToCatalog(upload));
    drop(file);
    let file = load(&saved);
    let update = UpdateTable {
        table: table.clone(),
        account: fixture::ACCOUNT.into(),
    };
    engine.submit(&file, Operation::UpdateTable(update));
    let path = vec![table.clone(), vec![0x05], serial::encode(1), f.clone()];
    assert_eq!(
        node_at(&file, &path).and_then(|n| n.value),
        text_node(&[], "3").value
    );
}

/// The passwords a test step submits beside its operation
/// ([`Credentials`]), kept as fixture text so that a replay can supply them
/// again: a recording holds the operation alone.
#[derive(Clone, Copy)]
enum Given {
    Nothing,
    Change(&'static str, &'static str),
    Reset(&'static str),
}

impl Given {
    fn credentials(self) -> Credentials {
        match self {
            Given::Nothing => Credentials::None,
            Given::Change(old, new) => Credentials::Change {
                old: Password::new(old),
                new: Password::new(new),
            },
            Given::Reset(new) => Credentials::Reset {
                new: Password::new(new),
            },
        }
    }
}

/// The salt and the verifier digest of every account's stored password
/// in `file` (`[23, 1, 5, A, 06]`: the version, a 10-byte salt, the
/// 20-byte digest, check bytes; om_crypto::account).
fn verifier_parts(file: &Hosted) -> Vec<Vec<u8>> {
    let accounts = node_at(file, &[vec![23], vec![1], vec![5]]).expect("accounts");
    accounts
        .children
        .iter()
        .filter_map(|account| account.child(&[6]).and_then(|n| n.value.clone()))
        .flat_map(|stored| [stored[1..11].to_vec(), stored[11..31].to_vec()])
        .collect()
}

/// docs/server-storage.md 5 and 9: the account edits, the deferred field
/// work a file is opened with and a window's saved found set replay the
/// same way, and neither a recording of them nor their outcomes shows a
/// password or a stored verifier. The sequence: the desktop's Change
/// Password of Admin (no stamp) and of Newcomer, whose change is due;
/// a script's Change Password of Clerk, whose set does not let it (206);
/// a script's Reset Account Password of Clerk, Enable Account of Former
/// twice (the second changes nothing) and Delete Account of Clerk; a reset
/// of an account the file lacks (213); the deferred work an op13 upload
/// left (Count's dependents recomputed), finished; and Tasks' found set
/// kept for a window. The passwords travel beside the operations, and the
/// replay supplies them again from the fixture. Each effect is checked
/// against the fixture, the salts are the environment's, and each step
/// that is not refused must change the tree. A file opened with deferred
/// work pending has it finished and saved by the engine that loads it.
#[test]
fn a_recorded_sequence_of_account_deferred_work_and_found_set_operations_replays() {
    use om_protocol::authoring::{MOD_COUNT as STAMP_COUNT, MOD_USER as STAMP_USER};

    use crate::accounts::AccountStatus;
    const ADMIN_NEW: &str = "a new horse";
    const NEWCOMER_NEW: &str = "settled in";
    const CLERK_NEW: &str = "clerk's new secret";
    const SCRIPT: u32 = 4;
    let passwords = [
        fixture::PASSWORD,
        ADMIN_NEW,
        fixture::NEWCOMER_PASSWORD,
        NEWCOMER_NEW,
        fixture::CLERK_PASSWORD,
        CLERK_NEW,
    ];
    let table = uint_segment(fixture::TABLE);
    let occurrence = uint_segment(fixture::OCCURRENCE);
    let doubled = vec![
        table.clone(),
        vec![0x05],
        serial::encode(1),
        uint_segment(fixture::DOUBLED),
    ];
    let leaf = |value: Vec<u8>| OwnedNode::leaf(Vec::new(), value);
    let mut tree = fixture::tree(&[], true);
    fixture::add_tasks(&mut tree, 1..=3);
    fixture::add_local_accounts(&mut tree);
    // The account catalog's index as FileMaker keeps it (local-files.md
    // 3), which Delete Account rewrites and every stamped edit stamps.
    let accounts = [
        (2u8, fixture::ACCOUNT),
        (3, fixture::CLERK),
        (4, fixture::FORMER),
        (5, fixture::NEWCOMER),
    ];
    for (place, (key, name)) in (1u32..).zip(accounts) {
        let listed = om_data::field_catalog::listed_key(name).unwrap();
        tree.put(&[&[23], &[1], &[1], &[1], &listed], leaf(vec![1, key]));
        let place = place.to_be_bytes();
        tree.put(&[&[23], &[1], &[1], &[3], &place], leaf(vec![1, key]));
        tree.put(&[&[23], &[1], &[1], &[7], &[key]], leaf(Vec::new()));
    }
    tree.put(
        &[&[23], &[1], &[1], &[4]],
        leaf(4u32.to_be_bytes().to_vec()),
    );
    // Tasks' data version (schema-path.md 5.8), which a kept found set
    // names, and Doubled stored stale on record 1.
    tree.put(
        &[&[3], &[0x10], &[5], &table, &[0xfb]],
        leaf(7u32.to_be_bytes().to_vec()),
    );
    let path: Vec<&[u8]> = doubled.iter().map(Vec::as_slice).collect();
    tree.put(&path, text_node(&[], "99"));
    // `[T, 03, 01, fd]`: Count's type changed (schema-path.md 7.2).
    let deferred = vec![table.clone(), vec![0x03], vec![0x01], vec![0xfd]];
    let work = OwnedNode::list(
        vec![0xfd],
        vec![OwnedNode::leaf(
            uint_segment(fixture::COUNT),
            0x40u32.to_be_bytes().to_vec(),
        )],
    );

    let (_first, file) = host("engine-accounts-recorded", &tree);
    let (recorder, log) = recording();
    let tree_of = |file: &Hosted| file.read_store().node(&[]).unwrap().clone();
    // What a step shows (its operation as recorded and its outcome) holds
    // no password, and no salt or digest of a stored verifier, in any form
    // a debug print gives: decimal, hex list, hex text.
    let assert_hidden = |shown: &str, parts: &[Vec<u8>]| {
        for password in passwords {
            assert!(!shown.contains(password), "a password is shown: {shown}");
        }
        for part in parts {
            let hex: String = part.iter().map(|b| format!("{b:02x}")).collect();
            for form in [format!("{part:?}"), format!("{part:02x?}"), hex] {
                assert!(!shown.contains(&form), "a verifier is shown: {shown}");
            }
        }
    };
    let mut recorded: Vec<(Operation, Given, Outcome, OwnedNode, bool)> = Vec::new();
    // Submit `operation` with the passwords `given`; it must change the
    // tree when `changes`.
    let mut submit = |operation: Operation, given: Given, changes: bool| {
        let before = tree_of(&file);
        let mut parts = verifier_parts(&file);
        let outcome = recorder.submit_with(&file, operation, given.credentials());
        let operation = last(&log);
        parts.extend(verifier_parts(&file));
        assert_hidden(&format!("{:?}", (&operation, &outcome)), &parts);
        let after = tree_of(&file);
        assert_eq!(after != before, changes, "{operation:?}");
        recorded.push((operation, given, outcome.clone(), after, changes));
        outcome
    };
    let account = |key: u8| node_at(&file, &[vec![23], vec![1], vec![5], vec![key]]);
    let status = |key: u8| {
        let account = account(key).expect("the account");
        AccountStatus::decode(account.child(&[0x0a]).and_then(|n| n.value.as_deref()))
            .expect("a status")
    };
    // The salt of an account's stored password, which the environment's
    // random source drew (`Seeded`: 0, 1, 2, ... from the engine's start).
    let salt = |key: u8| {
        let account = account(key).expect("the account");
        let stored = account.child(&[6]).and_then(|n| n.value.clone()).unwrap();
        stored[1..11].to_vec()
    };
    let drawn = |from: u8| (from..from + 10).collect::<Vec<u8>>();
    let stamp = AccountStamp {
        connection: SCRIPT,
        user: fixture::ACCOUNT.into(),
        account: fixture::ACCOUNT.into(),
    };
    let catalog_changed = Notices {
        from: SCRIPT,
        changes: [vec![vec![23], vec![1]], vec![vec![23], vec![1], vec![1]]]
            .into_iter()
            .map(|path| Change { path, code: 2 })
            .collect(),
    };
    let nothing_told = Notices {
        from: SCRIPT,
        changes: Vec::new(),
    };
    let stamp_count = |node: Option<OwnedNode>| {
        node.and_then(|n| n.child(STAMP_COUNT).and_then(|c| c.value.clone()))
    };
    let index = vec![vec![23], vec![1], vec![1]];
    let change = |account: AccountRef, stamp: Option<AccountStamp>| {
        Operation::ChangePassword(ChangePassword {
            account,
            manager: false,
            stamp,
        })
    };

    // The desktop's own Change Password: the next login needs the new
    // password; the status is set at the environment's time (12:00:00,
    // 43,200 s) with no change due, the salt is the first the
    // environment drew, and nothing is stamped.
    let outcome = submit(
        change(AccountRef::Name(fixture::ACCOUNT.into()), None),
        Given::Change(fixture::PASSWORD, ADMIN_NEW),
        true,
    );
    assert_eq!(outcome.reply, Reply::ChangePassword(Ok(())));
    assert!(outcome.notices.changes.is_empty());
    assert!(file.verify_password(fixture::ACCOUNT, ADMIN_NEW).is_some());
    assert!(file
        .verify_password(fixture::ACCOUNT, fixture::PASSWORD)
        .is_none());
    let admin = status(2);
    assert_eq!(
        (admin.date, admin.seconds),
        (jiff::civil::date(2026, 11, 24), 43_200)
    );
    assert!(admin.is_active() && !admin.must_change_password());
    assert_eq!(salt(2), drawn(0));
    assert_eq!(stamp_count(account(2)), None);
    assert_eq!(stamp_count(node_at(&file, &index)), None);

    // Newcomer's change was due: a new password clears it.
    assert!(status(5).must_change_password());
    let outcome = submit(
        change(AccountRef::Name(fixture::NEWCOMER.into()), None),
        Given::Change(fixture::NEWCOMER_PASSWORD, NEWCOMER_NEW),
        true,
    );
    assert_eq!(outcome.reply, Reply::ChangePassword(Ok(())));
    assert!(!status(5).must_change_password(), "no change is due");
    assert!(status(5).is_active());
    assert_eq!(salt(5), drawn(10));
    assert!(file
        .verify_password(fixture::NEWCOMER, NEWCOMER_NEW)
        .is_some());

    // Clerk's set does not let it change its own password.
    let outcome = submit(
        change(AccountRef::Key(vec![3]), Some(stamp.clone())),
        Given::Change(fixture::CLERK_PASSWORD, CLERK_NEW),
        false,
    );
    assert_eq!(
        outcome.reply,
        Reply::ChangePassword(Err(PasswordChangeError::NotAllowed))
    );
    assert_eq!(outcome.notices, nothing_told);

    // A script resets Clerk's password: stamped by its account, the index
    // too, and the other clients told the catalog changed.
    let reset = |account: &str| {
        Operation::ResetPassword(ResetPassword {
            account: account.into(),
            expire: false,
            stamp: Some(stamp.clone()),
        })
    };
    let outcome = submit(reset(fixture::CLERK), Given::Reset(CLERK_NEW), true);
    assert_eq!(outcome.reply, Reply::ResetPassword(Ok(())));
    assert_eq!(outcome.notices, catalog_changed);
    assert!(file.verify_password(fixture::CLERK, CLERK_NEW).is_some());
    assert!(file
        .verify_password(fixture::CLERK, fixture::CLERK_PASSWORD)
        .is_none());
    let clerk = account(3).unwrap();
    let user = clerk.child(STAMP_USER).and_then(|n| n.value.clone());
    assert_eq!(user, text_node(&[], fixture::ACCOUNT).value);
    assert!(stamp_count(Some(clerk)).is_some());
    let index_stamp = stamp_count(node_at(&file, &index));
    assert!(
        index_stamp.is_some(),
        "the account catalog's index is stamped"
    );
    assert!(!status(3).must_change_password());
    assert_eq!(salt(3), drawn(20));

    // Former made active, then again, which changes nothing.
    assert!(!status(4).is_active());
    let enable = Operation::EnableAccount(EnableAccount {
        account: fixture::FORMER.into(),
        active: true,
        stamp: Some(stamp.clone()),
    });
    let outcome = submit(enable.clone(), Given::Nothing, true);
    assert_eq!(outcome.reply, Reply::EnableAccount(Ok(true)));
    assert_eq!(outcome.notices, catalog_changed);
    assert!(status(4).is_active());
    assert_ne!(stamp_count(node_at(&file, &index)), index_stamp);
    let outcome = submit(enable, Given::Nothing, false);
    assert_eq!(outcome.reply, Reply::EnableAccount(Ok(false)));
    assert_eq!(outcome.notices, nothing_told);

    // Clerk deleted: its member, its index entries and one from the count.
    let outcome = submit(
        Operation::DeleteAccount(DeleteAccount {
            account: fixture::CLERK.into(),
            stamp: Some(stamp.clone()),
        }),
        Given::Nothing,
        true,
    );
    assert_eq!(outcome.reply, Reply::DeleteAccount(Ok(())));
    assert_eq!(outcome.notices, catalog_changed);
    assert!(account(3).is_none());
    assert!(file.verify_password(fixture::CLERK, CLERK_NEW).is_none());
    let count = node_at(&file, &[vec![23], vec![1], vec![1], vec![4]]);
    assert_eq!(
        count.and_then(|n| n.value),
        Some(3u32.to_be_bytes().to_vec())
    );

    // An account the file lacks.
    let outcome = submit(reset("Nobody"), Given::Reset(CLERK_NEW), false);
    assert_eq!(
        outcome.reply,
        Reply::ResetPassword(Err(AccountEditError::Missing))
    );

    // Deferred work an op13 upload left, finished: the list is gone and
    // Doubled, which reads Count, is recomputed.
    let upload = UploadToCatalog {
        path: deferred.clone(),
        node: work.clone(),
    };
    let outcome = submit(Operation::UploadToCatalog(upload), Given::Nothing, true);
    assert_eq!(outcome.reply, Reply::UploadToCatalog(true));
    let outcome = submit(Operation::FinishDeferredWork, Given::Nothing, true);
    assert_eq!(outcome.reply, Reply::FinishDeferredWork(true));
    assert!(node_at(&file, &deferred).is_none(), "the work list is done");
    assert_eq!(
        node_at(&file, &doubled).and_then(|n| n.value),
        text_node(&[], "2").value,
        "Doubled is recomputed"
    );

    // A window's found set, records 1 and 3 with 3 current, kept at
    // `[TO, 07]` with the table's data version.
    let found = RidList::from_iter([1, 3]);
    let outcome = submit(
        Operation::SaveFoundSet(SaveFoundSet {
            occurrence: occurrence.clone(),
            table: table.clone(),
            records: Some(found.clone()),
            current: Some(3),
        }),
        Given::Nothing,
        true,
    );
    assert_eq!(outcome.reply, Reply::SaveFoundSet(true));
    let state = |key: Vec<u8>| node_at(&file, &[occurrence.clone(), vec![7], vec![1], key]);
    let kept = state(vec![5]).expect("the found records");
    assert_eq!(RidList::from_owned(&kept).unwrap(), found);
    let version = state(vec![0xfe]).and_then(|n| n.child(&table).and_then(|v| v.value.clone()));
    assert_eq!(version, Some(7u32.to_be_bytes().to_vec()));
    assert!(state(vec![3]).is_some(), "the current record");

    // Nothing the recorder kept, nor the passwords' own debug form, shows
    // a password or a verifier.
    let parts = verifier_parts(&file);
    assert_hidden(&format!("{:?}", log.lock().unwrap()), &parts);
    for password in passwords {
        let given = Given::Change(password, password).credentials();
        assert_hidden(&format!("{:?}", (Password::new(password), given)), &parts);
    }

    let (_second, again) = host("engine-accounts-replayed", &tree);
    let replaying = engine();
    for (operation, given, outcome, after, changes) in &recorded {
        let before = tree_of(&again);
        let replayed = replaying.submit_with(&again, operation.clone(), given.credentials());
        assert_eq!(&replayed, outcome, "{operation:?}");
        let now = tree_of(&again);
        assert_eq!(&now, after, "the tree after {operation:?}");
        assert_eq!(now != before, *changes, "{operation:?}");
    }

    // A file opened with that work pending has it finished, and saved, by
    // the engine that loads it.
    let mut pending = tree.clone();
    let path: Vec<&[u8]> = deferred.iter().map(Vec::as_slice).collect();
    pending.put(&path, work);
    let saved = fixture::TempFile::from_tree("engine-deferred-at-load", &pending);
    let (loading, log) = recording();
    let file = load_with(&saved, &loading);
    assert_eq!(
        *log.lock().unwrap(),
        [Operation::FinishDeferredWork, Operation::SaveFile]
    );
    assert!(node_at(&file, &deferred).is_none());
    let on_disk = om_hbam::HbamFile::open(&saved.0).unwrap();
    assert!(on_disk.subtree(&path).unwrap().is_none(), "saved");
}

/// An environment whose random source fails ([`Environment::try_fill_random`]).
struct NoRandomSource;

impl Environment for NoRandomSource {
    fn now(&self) -> jiff::Zoned {
        "2026-11-24T12:00:00[UTC]".parse().unwrap()
    }

    fn fill_random(&self, _: &mut [u8]) {
        panic!("a test's random source that fails");
    }

    fn try_fill_random(&self, _: &mut [u8]) -> Result<(), NoRandom> {
        Err(NoRandom)
    }
}

/// A password's salt that cannot be drawn refuses Change Password and Reset
/// Account Password as unwritten ("no random salt", a script step's 3),
/// with the file unchanged, as before the engine interface: it neither
/// panics nor drops the connection that asked.
#[test]
fn an_account_edit_whose_salt_cannot_be_drawn_is_refused_and_changes_nothing() {
    let (_file, file) = host("engine-no-random", &fixture::tree(&[], true));
    let engine = FileEngine::new(Box::new(NoRandomSource));
    let before = file.read_store().node(&[]).unwrap().clone();
    let change = ChangePassword {
        account: AccountRef::Name(fixture::ACCOUNT.into()),
        manager: false,
        stamp: None,
    };
    let given = Given::Change(fixture::PASSWORD, "a new horse").credentials();
    let outcome = engine.submit_with(&file, Operation::ChangePassword(change), given);
    let unwritten = PasswordChangeError::Unwritten("no random salt".into());
    assert_eq!(outcome.reply, Reply::ChangePassword(Err(unwritten)));
    let reset = ResetPassword {
        account: fixture::CLERK.into(),
        expire: false,
        stamp: None,
    };
    let given = Given::Reset("clerk's new secret").credentials();
    let outcome = engine.submit_with(&file, Operation::ResetPassword(reset), given);
    let unwritten = AccountEditError::Unwritten("no random salt".into());
    assert_eq!(outcome.reply, Reply::ResetPassword(Err(unwritten)));
    assert_eq!(file.read_store().node(&[]).unwrap(), &before);
    assert!(file
        .verify_password(fixture::ACCOUNT, fixture::PASSWORD)
        .is_some());
}
