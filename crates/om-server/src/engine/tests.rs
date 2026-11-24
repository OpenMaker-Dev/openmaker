use std::sync::atomic::{AtomicU64, Ordering};

use om_hbam::slot::uint_segment;
use om_hbam::{serial, OwnedNode};
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
fn host(tag: &str, tree: &OwnedNode) -> (fixture::TempFile, std::sync::Arc<Hosted>) {
    let file = fixture::TempFile::from_tree(tag, tree);
    let hosted = HostedFile {
        name: fixture::FILE.into(),
        path: file.0.clone(),
    };
    let hosting = Hosting::load(&[hosted], false, &EventLog::default()).unwrap();
    (file, hosting.by_name(fixture::FILE).unwrap())
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

/// The schema operations replay the same way (docs/server-storage.md 5):
/// what a find and GenerateMemberKey left waiting in the store travels in
/// the operation, so a fresh load with nothing waiting gets the same tree
/// after each step. A find builds Name's value index, which is stored; op13
/// uploads a table's deferred field work (drop Name's indexes; Count
/// changed, so Doubled, stored stale on record 1, is recomputed), which
/// op73 does; an op86 program
/// renames Tasks, deletes Notes and its catalog member, writes the layout
/// key GenerateMemberKey issued and stores the word index another find
/// built. Each step is checked for its effect, so an operation that did
/// nothing fails here.
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
        store.indexes_on_demand()
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
    let mut renamed = file
        .read_store()
        .node(&[&[3], &[0x10], &[5], &table])
        .unwrap()
        .clone();
    renamed.set_child(text_node(&[0x10], "Jobs"));
    let mut recorded: Vec<(Operation, Outcome, OwnedNode)> = Vec::new();
    let recording = engine();
    let mut submit = |operation: Operation| {
        let outcome = recording.submit(&file, operation.clone());
        let after = file.read_store().node(&[]).unwrap().clone();
        recorded.push((operation, outcome.clone(), after));
        outcome
    };
    let node = |path: &[Vec<u8>]| {
        let path: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        file.read_store().node(&path).cloned()
    };

    let indexes = find(0);
    assert_eq!(indexes.len(), 1);
    let outcome = submit(Operation::StoreIndexes(StoreIndexes { indexes }));
    assert_eq!(outcome.reply, Reply::StoreIndexes);
    assert!(node(&index(0x10)).is_some(), "the value index is stored");

    let upload = UploadToCatalog {
        path: deferred.clone(),
        node: work,
    };
    let outcome = submit(Operation::UploadToCatalog(upload));
    assert_eq!(outcome.reply, Reply::UploadToCatalog(true));
    assert!(node(&deferred).is_some());

    let update = UpdateTable {
        table: table.clone(),
        account: fixture::ACCOUNT.into(),
    };
    let outcome = submit(Operation::UpdateTable(update));
    assert_eq!(outcome.reply, Reply::UpdateTable(Ok(true)));
    assert!(node(&deferred).is_none(), "the work list is done");
    assert!(node(&index(0x10)).is_none(), "Name's indexes are dropped");
    assert_eq!(
        node(&doubled).and_then(|n| n.value),
        text_node(&[], "2").value,
        "Doubled is recomputed"
    );

    let words = find(1);
    let issued = file.read_store().generate_member_key(&[vec![4]]).unwrap();
    let member_keys = file.read_store().issued_member_keys();
    let change = ChangeSchema {
        writes: vec![
            Write::Put {
                path: tasks.clone(),
                node: renamed.clone(),
            },
            Write::Remove {
                path: vec![vec![3], vec![0x10], vec![5], notes.clone()],
            },
            Write::Remove {
                path: vec![notes.clone()],
            },
        ],
        member_keys,
        indexes: words,
    };
    let outcome = submit(Operation::ChangeSchema(change));
    assert_eq!(outcome.reply, Reply::ChangeSchema(Ok(())));
    assert_eq!(
        node(&tasks).map(|n| n.child(&[0x10]).cloned()),
        Some(renamed.child(&[0x10]).cloned())
    );
    assert!(node(&[notes]).is_none(), "Notes is deleted");
    assert_eq!(
        node(&[vec![4], vec![1], vec![0]]).and_then(|n| n.value),
        Some(issued.0),
        "the issued layout id is written"
    );
    assert!(node(&index(0x11)).is_some(), "the word index is stored");
    assert!(recorded
        .iter()
        .all(|(_, outcome, _)| outcome.notices.changes.is_empty()));
    {
        let store = file.read_store();
        assert!(store.indexes_on_demand().is_empty());
        assert!(store.issued_member_keys().is_empty());
    }

    let (_second, again) = host("engine-schema-replayed", &tree);
    let replaying = engine();
    let mut before = again.read_store().node(&[]).unwrap().clone();
    for (operation, outcome, after) in &recorded {
        assert_eq!(
            &replaying.submit(&again, operation.clone()),
            outcome,
            "{operation:?}"
        );
        let now = again.read_store().node(&[]).unwrap().clone();
        assert_eq!(&now, after, "the tree after {operation:?}");
        assert_ne!(now, before, "{operation:?} changed nothing");
        before = now;
    }
}
