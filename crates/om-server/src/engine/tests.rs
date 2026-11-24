use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

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

/// `tree` hosted from a file of its own, tagged `tag`.
fn host(tag: &str, tree: &OwnedNode) -> (fixture::TempFile, Arc<Hosted>) {
    let file = fixture::TempFile::from_tree(tag, tree);
    let hosted = load(&file);
    (file, hosted)
}

/// The file at `file` hosted as it is on disk.
fn load(file: &fixture::TempFile) -> Arc<Hosted> {
    let hosted = HostedFile {
        name: fixture::FILE.into(),
        path: file.0.clone(),
    };
    let hosting = Hosting::load(&[hosted], false, &EventLog::default()).unwrap();
    hosting.by_name(fixture::FILE).unwrap()
}

/// The node at `path` in `file`'s tree.
fn node_at(file: &Hosted, path: &[Vec<u8>]) -> Option<OwnedNode> {
    let path: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
    file.read_store().node(&path).cloned()
}

/// Each key's records in index `index` (`[T, 01, F, k]`), and its counts
/// leaf (`[T, 01, F, k | 10]`) as (entries, distinct keys).
fn postings(file: &Hosted, index: &[Vec<u8>]) -> (Vec<Vec<u64>>, (u64, u64)) {
    let keys = node_at(file, index).expect("the index");
    let records = keys
        .children
        .iter()
        .map(|key| RidList::from_owned(key).unwrap().iter().collect())
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
    let mut recorded: Vec<(Operation, Outcome, OwnedNode)> = Vec::new();
    let recording = engine();
    let mut submit = |step: Step| {
        let submitted = match step {
            Step::Operation(operation) => Submitted {
                outcome: recording.submit(&file, operation.clone()),
                operation,
            },
            Step::Draft(draft) => recording.submit_draft(&file, draft),
        };
        let after = file.read_store().node(&[]).unwrap().clone();
        recorded.push((
            submitted.operation.clone(),
            submitted.outcome.clone(),
            after,
        ));
        submitted
    };

    find(0);
    let stored = submit(Step::Draft(Draft::StoreIndexes));
    assert_eq!(
        stored.operation,
        Operation::StoreIndexes(StoreIndexes {
            indexes: vec![(table.clone(), name.clone(), 0)],
        })
    );
    assert_eq!(stored.outcome.reply, Reply::StoreIndexes);
    // "Task 1" to "Task 3": one key each.
    assert_eq!(
        postings(&file, &index(0)),
        (vec![vec![1], vec![2], vec![3]], (3, 3))
    );

    let upload = UploadToCatalog {
        path: deferred.clone(),
        node: work,
    };
    let outcome = submit(Step::Operation(Operation::UploadToCatalog(upload))).outcome;
    assert_eq!(outcome.reply, Reply::UploadToCatalog(true));
    assert!(node_at(&file, &deferred).is_some());

    let update = UpdateTable {
        table: table.clone(),
        account: fixture::ACCOUNT.into(),
    };
    let outcome = submit(Step::Operation(Operation::UpdateTable(update))).outcome;
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
    let changed = submit(Step::Draft(Draft::ChangeSchema {
        writes: writes.clone(),
    }));
    assert_eq!(changed.outcome.reply, Reply::ChangeSchema(Ok(())));
    let Operation::ChangeSchema(completed) = &changed.operation else {
        panic!("{:?}", changed.operation);
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
    assert_eq!(
        postings(&file, &index(1)),
        (vec![vec![1], vec![2], vec![3], vec![1, 2, 3]], (6, 4))
    );
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
    let engine = engine();
    let nothing = Operation::StoreIndexes(StoreIndexes {
        indexes: Vec::new(),
    });

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
    assert_eq!(a.outcome.reply, Reply::ChangeSchema(Ok(())));
    let b = engine.submit_draft(&file, Draft::ChangeSchema { writes: Vec::new() });
    assert_eq!(b.operation, nothing, "A wrote B's key before the delete");
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
    let a = engine.submit_draft(&file, Draft::ChangeSchema { writes: Vec::new() });
    assert_eq!(a.operation, nothing);
    // The next check for the index finds none.
    let counts = vec![table.clone(), vec![0x01], name.clone(), vec![0x10]];
    assert!(node_at(&file, &counts).is_none());

    // A layout key was issued, and two requests saw it waiting: one save.
    let (saved, file) = host("drafts-one-save", &tree);
    file.read_store().generate_member_key(&[vec![4]]).unwrap();
    let saves = file.read_store().saves();
    let first = engine.submit_draft(&file, Draft::ChangeSchema { writes: Vec::new() });
    assert!(matches!(&first.operation, Operation::ChangeSchema(op) if op.member_keys.len() == 1));
    let bytes = std::fs::read(&saved.0).unwrap();
    let second = engine.submit_draft(&file, Draft::ChangeSchema { writes: Vec::new() });
    assert_eq!(second.operation, nothing);
    assert_eq!(file.read_store().saves(), saves + 1);
    assert_eq!(
        std::fs::read(&saved.0).unwrap(),
        bytes,
        "the file, and its change number, as the first save left them"
    );
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
